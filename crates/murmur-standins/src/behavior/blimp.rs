//! Small airship (ArduPilot Blimp): slow, holonomic, pushed around by the wind.
//!
//! Wire, as Blimp in ~/ardupilot: MAV_TYPE_AIRSHIP, modes LAND 0, MANUAL 1, VELOCITY 2,
//! LOITER 3, RTL 4, AUTO 5, HOLD 6; boots in LOITER. AUTO accepts any mission (or none) and
//! runs only NAV_WAYPOINT: AP_Mission announces each command it starts ("Mission: 2 WP"),
//! Blimp skips every other one ("Command not supported. Skipped.", TAKEOFF and LOITER_UNLIM
//! included) and AP_Mission walks DO_JUMPs ("Mission: 5 Jump 2/unlimited" from the second
//! pass). Waypoints are not reported reached (no text, no MISSION_ITEM_REACHED): the mission
//! just moves on. At the end: "Mission complete.", holding at the last target. Blimp has no
//! in-flight disarm check, so a GCS disarm in the air is accepted (the envelope floats on).
//! RTL flies to home and down to it (its target is the home position, altitude included, so
//! from far away it reaches the ground first and skims the rest of the way).
//!
//! Movement: the autopilot commands a ground velocity toward the target (at most WP_VEL in
//! AUTO, LOIT_MAX_VELX otherwise, both seeded from the catalog's cruise speed; slowing as it
//! closes in) and the fins supply whatever air velocity that takes against the wind, up to the
//! catalog's max_mps. When the wind is stronger than that it cannot make headway and drifts
//! downwind while pushing. MANUAL and HOLD give no thrust (no RC), and a disarmed blimp in
//! the air drifts with the wind; on the ground without thrust it stays put (moored). Wind from
//! tidewater (wants_conditions() gives the samples); without it, still air. Climbs at LOIT_MAX_VELZ
//! (the catalog's climb_mps); LAND descends at half of it, as Blimp's ModeLand.

use mavlink::dialects::ardupilotmega::{MavCmd, MavFrame, MavParamType, MavSeverity, MavType};

use super::apmission::{self, Jumps};
use super::{Behavior, Tick, modes};
use crate::config::Spec;
use crate::geo::{M_PER_DEG, bearing_deg, distance_m};
use crate::kinematics::{Out, State};

/// position loop gain: commanded speed = gain x distance, capped at cruise
const POS_GAIN: f64 = 0.5;
const ACCEL_MPS2: f64 = 0.5;

pub struct Blimp {
    mode: &'static str,
    /// altitude to hold in LOITER (taken on entering it)
    hold_alt: f64,
    /// the item whose start has been announced
    started: Option<u16>,
    /// AP_Mission's jump tracking: times each DO_JUMP has run
    jumps: Jumps,
    /// the mission had finished as of the last step (entering AUTO then starts it over)
    was_complete: bool,
    /// air velocity (east, north) the fins are giving it
    air: (f64, f64),
    /// AUTO's target once the mission is done, or while it has none
    last_target: Option<(f64, f64, f64)>,
}

impl Blimp {
    pub fn new() -> Blimp {
        Blimp {
            mode: "",
            hold_alt: 0.0,
            started: None,
            jumps: Jumps::default(),
            was_complete: false,
            air: (0.0, 0.0),
            last_target: None,
        }
    }

    /// AP_Mission plus Blimp's ModeAuto: the current waypoint, announcing and skipping as it goes.
    // GLOBAL_RELATIVE_ALT_INT is deprecated for COMMAND_INT, still the mission item frame
    #[allow(deprecated)]
    fn auto_target(&mut self, t: &mut Tick) -> Option<(f64, f64, f64)> {
        let n = t.mission.items.len();
        let mut budget = 4 * n + 16;
        if t.mission.complete {
            return None;
        }
        let wp_radius = t.params.f("WP_RADIUS", 1.0);
        if t.mission.seq == 0 {
            t.mission.seq = 1; // item 0 is home
        }
        while budget > 0 {
            budget -= 1;
            let seq = t.mission.seq;
            let Some(it) = t.mission.items.get(usize::from(seq)).cloned() else {
                // done: AP_Mission keeps the last command as the current one
                t.mission.seq = n.saturating_sub(1) as u16;
                t.mission.complete = true;
                t.out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    "Mission complete.".into(),
                ));
                return None;
            };
            if it.command == MavCmd::MAV_CMD_DO_JUMP {
                let times = it.param2 as i32;
                if times < 0 || self.jumps.runs(seq) < times {
                    self.jumps.ran(seq, times, t.out);
                    t.mission.seq = it.param1 as u16;
                } else {
                    t.mission.seq = seq + 1;
                }
                continue;
            }
            if self.started != Some(seq) {
                self.started = Some(seq);
                apmission::started(seq, it.command, t.out);
                if it.command == MavCmd::MAV_CMD_NAV_WAYPOINT {
                    t.out.push(Out::MissionCurrent(seq));
                }
                if it.command != MavCmd::MAV_CMD_NAV_WAYPOINT {
                    t.out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_CRITICAL,
                        "Command not supported. Skipped.".into(),
                    ));
                }
            }
            if it.command != MavCmd::MAV_CMD_NAV_WAYPOINT {
                t.mission.seq = seq + 1; // skipped within the same update
                continue;
            }
            let st = &*t.st;
            // zero lat/lon or altitude: keep the current one (ModeAuto::loc_from_cmd)
            let (lat, lon) = if it.x == 0 && it.y == 0 {
                (st.lat, st.lon)
            } else {
                (f64::from(it.x) / 1e7, f64::from(it.y) / 1e7)
            };
            let alt = if it.z == 0.0 {
                st.alt
            } else if it.frame == MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT {
                st.home.2 + f64::from(it.z)
            } else {
                f64::from(it.z)
            };
            let dist = distance_m(st.lat, st.lon, lat, lon).hypot(st.alt - alt);
            if dist <= wp_radius {
                t.mission.seq = seq + 1; // reached: Blimp says nothing, the next one starts
                continue;
            }
            return Some((lat, lon, alt));
        }
        None
    }
}

impl Default for Blimp {
    fn default() -> Self {
        Blimp::new()
    }
}

/// Ground under the blimp: tidewater's land or water surface if known, else home.
fn ground(t: &Tick) -> f64 {
    match t.water.filter(|s| s.in_area()) {
        Some(s) => s
            .elevation_m
            .unwrap_or(0.0)
            .max(s.water_level_m.unwrap_or(f64::MIN)),
        None => t.st.home.2,
    }
}

fn wind(t: &Tick) -> (f64, f64) {
    t.water
        .and_then(|s| s.wind_east.zip(s.wind_north))
        .unwrap_or((0.0, 0.0))
}

impl Behavior for Blimp {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_AIRSHIP
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::BLIMP
    }

    fn hold_mode(&self) -> &'static str {
        "LOITER"
    }

    fn spawn(&self, _spec: &Spec, st: &mut State) {
        st.mode = "LOITER"; // Blimp boots in LOITER
    }

    /// Blimp has no in-flight disarm check: a GCS disarm in the air is accepted.
    fn airborne(&self, _st: &State) -> bool {
        false
    }

    /// It needs tidewater's wind.
    fn wants_conditions(&self) -> bool {
        true
    }

    /// Blimp's waypoint and loiter limits, seeded from the catalog (SITL Blimp's defaults are
    /// for a small indoor envelope: WP_VEL 0.4, LOIT_MAX_VEL* 0.5 / 0.4 m/s)
    fn extra_params(&self, spec: &Spec) -> Vec<(&'static str, MavParamType, f32)> {
        use MavParamType::MAV_PARAM_TYPE_REAL32 as F32;
        let cruise = spec.cruise_mps as f32;
        let climb = spec.climb_mps().max(0.1) as f32;
        vec![
            ("WP_RADIUS", F32, 1.0),
            ("WP_VEL", F32, cruise),
            ("WP_ACCEL", F32, ACCEL_MPS2 as f32),
            ("LOIT_MAX_VELX", F32, cruise),
            ("LOIT_MAX_VELY", F32, cruise),
            ("LOIT_MAX_VELZ", F32, climb),
        ]
    }

    fn step(&mut self, t: &mut Tick) {
        if self.was_complete && !t.mission.complete {
            // a new mission, DO_SET_MISSION_CURRENT, or AUTO again after the end (the shared
            // store starts it over): AP_Mission's jump counts start over too
            self.started = None;
            self.jumps.clear();
        }
        if t.st.mode != self.mode {
            self.mode = t.st.mode;
            self.hold_alt = t.st.alt;
            if self.mode == "AUTO" {
                self.started = None; // the current command is announced again
                self.last_target = Some((t.st.lat, t.st.lon, t.st.alt));
            }
        }
        let (we, wn) = wind(t);
        let floor = ground(t);
        let cruise = if self.mode == "AUTO" {
            t.params.f("WP_VEL", t.spec.cruise_mps)
        } else {
            t.params.f("LOIT_MAX_VELX", t.spec.cruise_mps)
        };
        let max_air = t.spec.max_mps.max(t.spec.cruise_mps);
        let climb = t.params.f("LOIT_MAX_VELZ", t.spec.climb_mps()).max(0.1);
        // what the autopilot asks for: a ground velocity (east, north, up), or no thrust at all
        let mut want: Option<(f64, f64, f64)> = None;
        let mut goto = |st: &State, lat: f64, lon: f64, alt: f64| {
            let d = distance_m(st.lat, st.lon, lat, lon);
            // brake in time: the air velocity changes at ACCEL_MPS2 at most
            let v = (POS_GAIN * d)
                .min((1.4 * ACCEL_MPS2 * d).sqrt())
                .min(cruise);
            let b = bearing_deg(st.lat, st.lon, lat, lon).to_radians();
            let vz = (alt - st.alt).clamp(-climb, climb);
            want = Some((v * b.sin(), v * b.cos(), vz));
        };
        if self.mode == "AUTO" {
            // the mission walks (and talks) armed or not; it only flies armed
            if let Some(tgt) = self.auto_target(t) {
                self.last_target = Some(tgt);
            }
        }
        self.was_complete = t.mission.complete;
        let st = &*t.st;
        if st.armed {
            match self.mode {
                "AUTO" => {
                    let (lat, lon, alt) = self.last_target.unwrap_or((st.lat, st.lon, st.alt));
                    goto(st, lat, lon, alt);
                }
                "LOITER" => goto(st, st.hold_point.0, st.hold_point.1, self.hold_alt),
                "RTL" => goto(st, st.home.0, st.home.1, st.home.2),
                "VELOCITY" => want = Some((0.0, 0.0, 0.0)), // no RC: zero velocity demand
                "LAND" => want = Some((0.0, 0.0, -0.5 * climb)),
                _ => {} // MANUAL (no RC), HOLD (motors off)
            }
        }
        let st = &mut *t.st;
        let on_ground = st.alt <= floor + 0.2;
        let thrust = want.is_some_and(|(ge, gn, _)| ge.hypot(gn) > 0.05);
        // air velocity the fins must supply, within what they can
        let (ae, an, vz) = match want {
            Some((ge, gn, vz)) => {
                let (mut ae, mut an) = (ge - we, gn - wn);
                let need = ae.hypot(an);
                if need > max_air {
                    (ae, an) = (ae * max_air / need, an * max_air / need);
                }
                (ae, an, vz)
            }
            None => (0.0, 0.0, 0.0),
        };
        // the current air velocity eases toward it
        let (ce, cn) = self.air;
        let (de, dn) = (ae - ce, an - cn);
        let dv = de.hypot(dn);
        let step = ACCEL_MPS2 * t.dt;
        let k = if dv > step { step / dv } else { 1.0 };
        self.air = (ce + de * k, cn + dn * k);
        let (ne, nn) = self.air;
        st.speed = ne.hypot(nn);
        st.demand = ae.hypot(an);
        if st.speed > 0.2 {
            st.heading = ne.atan2(nn).to_degrees().rem_euclid(360.0); // nose into the airflow
        }
        st.vz = vz;
        st.alt = (st.alt + vz * t.dt).max(floor);
        if st.alt <= floor {
            st.vz = 0.0;
        }
        // on the ground without thrust it sits (moored); otherwise the wind carries it
        let (ve, vn) = if on_ground && vz <= 0.0 && !thrust {
            self.air = (0.0, 0.0);
            st.speed = 0.0;
            (0.0, 0.0)
        } else {
            (ne + we, nn + wn)
        };
        st.ve = ve;
        st.vn = vn;
        st.lat += vn * t.dt / M_PER_DEG;
        st.lon += ve * t.dt / (M_PER_DEG * st.lat.to_radians().cos());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::Firmware;
    use crate::conditions::Sample;
    use crate::geo::Gauss;
    use crate::mission::Mission;
    use crate::params::Params;
    use mavlink::dialects::ardupilotmega::MISSION_ITEM_INT_DATA;

    fn spec() -> Spec {
        Spec {
            name: "lta".into(),
            behavior: "blimp".into(),
            cruise_mps: 3.0,
            max_mps: 5.0,
            climb_mps: Some(1.0),
            turn_dps: None,
            loiter_radius_m: None,
            endurance_h: None,
            link: None,
        }
    }

    fn state(mode: &'static str) -> State {
        State {
            lat: 37.8,
            lon: -122.5,
            alt: 20.0,
            heading: 0.0,
            speed: 0.0,
            vz: 0.0,
            ve: 0.0,
            vn: 0.0,
            home: (37.8, -122.5, 0.0),
            armed: true,
            mode,
            hold_point: (37.8, -122.5),
            aground: false,
            demand: 0.0,
        }
    }

    fn run(
        b: &mut Blimp,
        st: &mut State,
        m: &mut Mission,
        water: Option<&Sample>,
        secs: f64,
    ) -> Vec<Out> {
        let spec = spec();
        let mut params = Params::seed(Firmware::Blimp, &spec, 1);
        params.extend(b.extra_params(&spec));
        let mut rng = Gauss::new(1);
        let mut out = Vec::new();
        for _ in 0..(secs * 10.0) as usize {
            b.step(&mut Tick {
                spec: &spec,
                params: &params,
                st,
                mission: m,
                water,
                rng: &mut rng,
                out: &mut out,
                dt: 0.1,
            });
        }
        out
    }

    #[allow(deprecated)]
    fn item(seq: u16, command: MavCmd, north_m: f64, z: f32) -> MISSION_ITEM_INT_DATA {
        MISSION_ITEM_INT_DATA {
            seq,
            command,
            x: ((37.8 + north_m / 111_320.0) * 1e7) as i32,
            y: (-122.5 * 1e7) as i32,
            z,
            frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
            ..Default::default()
        }
    }

    fn texts(out: &[Out]) -> Vec<String> {
        out.iter()
            .filter_map(|o| match o {
                Out::Text(_, s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn auto_skips_what_blimp_cannot_fly_and_says_so() {
        let mut b = Blimp::new();
        let mut st = state("AUTO");
        let mut m = Mission::default();
        m.install(vec![
            item(0, MavCmd::MAV_CMD_NAV_WAYPOINT, 0.0, 0.0),
            item(1, MavCmd::MAV_CMD_NAV_TAKEOFF, 0.0, 20.0),
            item(2, MavCmd::MAV_CMD_NAV_WAYPOINT, 30.0, 20.0),
            item(3, MavCmd::MAV_CMD_NAV_LOITER_UNLIM, 30.0, 20.0),
        ]);
        let out = run(&mut b, &mut st, &mut m, None, 40.0);
        assert_eq!(
            texts(&out),
            [
                "Mission: 1 Takeoff",
                "Command not supported. Skipped.",
                "Mission: 2 WP",
                "Mission: 3 LoitUnlim",
                "Command not supported. Skipped.",
                "Mission complete.",
            ]
        );
        assert!(!out.iter().any(|o| matches!(o, Out::ItemReached(_))));
        let d = distance_m(st.lat, st.lon, 37.8 + 30.0 / 111_320.0, -122.5);
        assert!(d < 2.0, "holds at the last waypoint, {d:.1} m off");
    }

    #[test]
    fn drifts_downwind_when_the_wind_beats_it() {
        let mut b = Blimp::new();
        let mut st = state("LOITER");
        let mut m = Mission::default();
        let calm = Sample {
            elevation_m: Some(0.0),
            wind_east: Some(4.0),
            wind_north: Some(0.0),
            ..Default::default()
        };
        run(&mut b, &mut st, &mut m, Some(&calm), 60.0);
        let held = distance_m(st.lat, st.lon, 37.8, -122.5);
        assert!(held < 3.0, "holds station in a 4 m/s wind, {held:.1} m off");
        let gale = Sample {
            wind_east: Some(8.0),
            ..calm
        };
        run(&mut b, &mut st, &mut m, Some(&gale), 60.0);
        let lost = distance_m(st.lat, st.lon, 37.8, -122.5);
        // 8 m/s wind against 5 m/s of air speed: about 3 m/s downwind
        assert!(lost > 150.0 && lost < 200.0, "drifted {lost:.0} m");
        assert!(st.lon > -122.5, "downwind is east");
    }
}
