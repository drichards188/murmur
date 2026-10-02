//! ArduPilot's AP_Mission as the copter and VTOL stand-ins run it: the current navigation item
//! (DO_JUMPs resolved, with their "Mission: N Jump i/n" texts), the "Mission: N Type" text when
//! an item starts, MISSION_ITEM_REACHED and "Reached command #N" when it completes, and the
//! reset on disarm. The shared walker in `mission` speaks standins.py's dialect; this one
//! follows ArduPilot 4.8 as SITL shows it (sim/tests/standins_vs_sitl_air.py).

use mavlink::dialects::ardupilotmega::{MISSION_ITEM_INT_DATA, MavCmd, MavFrame, MavSeverity};

use crate::kinematics::{Out, State};
use crate::mission::Mission;

/// MAV_CMD ids below this are navigation commands (MAV_CMD_NAV_LAST).
const NAV_LAST: u32 = 95;

#[derive(Debug, Default)]
pub struct Auto {
    /// the item whose start was announced; None = announce the current one again
    started: Option<u16>,
    /// which upload `started` belongs to
    upload: u64,
    /// the seq this walker last left the mission at (anything else was set by the GCS)
    seen: u16,
    /// the last item is done; the vehicle holds until the mission restarts
    pub finished: bool,
    /// reset by a disarm: not running until AUTO is entered, the GCS sets an item, or a new
    /// mission arrives
    stopped: bool,
}

/// Motors off (disarmed, or a flat battery): an aircraft above the ground falls. Returns
/// whether it is on the ground.
pub fn fall(st: &mut State, dt: f64) -> bool {
    let ground = st.home.2;
    if st.alt - ground < 0.5 {
        st.vz = 0.0;
        return true;
    }
    st.vz = (st.vz - 9.8 * dt).max(-30.0);
    st.alt = (st.alt + st.vz * dt).max(ground);
    false
}

/// A navigation item, decoded.
#[derive(Debug, Clone)]
pub struct Nav {
    pub seq: u16,
    pub command: MavCmd,
    /// None when the item leaves the position out (0, 0): "here"
    pub pos: Option<(f64, f64)>,
    /// metres above MSL
    pub alt: f64,
    pub item: MISSION_ITEM_INT_DATA,
}

pub fn is_nav(c: MavCmd) -> bool {
    (c as u32) < NAV_LAST
}

/// AP_Mission::Mission_Command::type() for the commands the stand-ins see.
pub fn type_name(c: MavCmd) -> &'static str {
    use MavCmd::*;
    match c {
        MAV_CMD_NAV_WAYPOINT => "WP",
        MAV_CMD_NAV_SPLINE_WAYPOINT => "SplineWP",
        MAV_CMD_NAV_RETURN_TO_LAUNCH => "RTL",
        MAV_CMD_NAV_LOITER_UNLIM => "LoitUnlim",
        MAV_CMD_NAV_LOITER_TIME => "LoitTime",
        MAV_CMD_NAV_LOITER_TURNS => "LoitTurns",
        MAV_CMD_NAV_LOITER_TO_ALT => "LoitAltitude",
        MAV_CMD_NAV_TAKEOFF => "Takeoff",
        MAV_CMD_NAV_LAND => "Land",
        MAV_CMD_NAV_VTOL_TAKEOFF => "VTOLTakeoff",
        MAV_CMD_NAV_VTOL_LAND => "VTOLLand",
        MAV_CMD_NAV_DELAY => "Delay",
        MAV_CMD_DO_CHANGE_SPEED => "ChangeSpeed",
        MAV_CMD_DO_SET_HOME => "SetHome",
        MAV_CMD_DO_JUMP => "Jump",
        MAV_CMD_DO_LAND_START => "LandStart",
        MAV_CMD_DO_VTOL_TRANSITION => "VTOLTransition",
        MAV_CMD_CONDITION_DELAY => "CondDelay",
        _ => "?",
    }
}

/// Altitude of a mission item, metres above MSL.
// GLOBAL_RELATIVE_ALT_INT is deprecated for COMMAND_INT, still the mission item frame
#[allow(deprecated)]
pub fn item_alt(st: &State, it: &MISSION_ITEM_INT_DATA) -> f64 {
    match it.frame {
        MavFrame::MAV_FRAME_GLOBAL | MavFrame::MAV_FRAME_GLOBAL_INT => f64::from(it.z),
        _ => st.home.2 + f64::from(it.z),
    }
}

fn fingerprint(m: &Mission) -> u64 {
    m.items.iter().fold(m.items.len() as u64, |h, it| {
        h.wrapping_mul(0x100_0000_01B3)
            ^ (it.x as u64)
            ^ ((it.y as u64) << 17)
            ^ u64::from(it.z.to_bits())
            ^ (it.command as u64)
    })
}

impl Auto {
    /// Disarming resets the mission (AP_Mission::reset): back to the first item.
    pub fn reset(&mut self, m: &mut Mission, out: &mut Vec<Out>) {
        m.seq = 0;
        m.jumps.clear();
        m.complete = false;
        self.seen = 0;
        self.started = None;
        self.finished = false;
        self.stopped = true;
        out.push(Out::MissionCurrent(0));
    }

    /// Whether the mission runs (a disarm stops it until something restarts it).
    pub fn running(&mut self, m: &Mission) -> bool {
        if self.stopped && (fingerprint(m) != self.upload || m.seq != self.seen) {
            self.stopped = false;
        }
        !self.stopped
    }

    /// Whether the mission has items to fly (AP_Mission::present: more than home).
    pub fn present(m: &Mission) -> bool {
        m.items.len() > 1
    }

    /// Entering AUTO: a finished mission starts over, as ArduPilot's start_or_resume does.
    pub fn enter(&mut self, m: &mut Mission, out: &mut Vec<Out>) {
        self.started = None;
        self.stopped = false;
        if self.finished || usize::from(m.seq) >= m.items.len() || m.seq == 0 {
            self.finished = false;
            m.complete = false;
            m.seq = 1;
            m.jumps.clear();
            out.push(Out::MissionCurrent(m.seq));
        }
        self.seen = m.seq;
    }

    /// The current navigation item, after any DO_JUMPs and DO commands before it; None when
    /// the mission is finished or empty. Announces the item when it starts.
    pub fn current(&mut self, st: &State, m: &mut Mission, out: &mut Vec<Out>) -> Option<Nav> {
        let fp = fingerprint(m);
        if fp != self.upload || m.seq != self.seen {
            // a new upload, or DO_SET_MISSION_CURRENT: start that item
            self.upload = fp;
            self.started = None;
            self.finished = false;
        }
        if self.finished {
            return None;
        }
        let n = m.items.len();
        if m.seq == 0 && n > 1 {
            m.seq = 1;
        }
        let nav = self.walk(st, m, out);
        self.seen = m.seq;
        nav
    }

    fn walk(&mut self, st: &State, m: &mut Mission, out: &mut Vec<Out>) -> Option<Nav> {
        let n = m.items.len();
        let mut budget = 4 * n + 16;
        while usize::from(m.seq) < n && budget > 0 {
            budget -= 1;
            let it = m.items[usize::from(m.seq)].clone();
            if it.command == MavCmd::MAV_CMD_DO_JUMP {
                let target = it.param1 as u16;
                let times = it.param2 as i32;
                if target == 0 || usize::from(target) >= n {
                    return None; // invalid jump ends the mission
                }
                let run = m.jumps.entry(m.seq).or_insert(0);
                if times < 0 || *run < times {
                    *run += 1;
                    let text = if times < 0 {
                        format!("Mission: {} Jump {}/unlimited", m.seq, *run)
                    } else {
                        format!("Mission: {} Jump {}/{}", m.seq, *run, times)
                    };
                    out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, text));
                    m.seq = target;
                } else {
                    *run = 0; // a finished loop counts again if re-entered
                    m.seq += 1;
                }
                out.push(Out::MissionCurrent(m.seq));
                self.started = None;
                continue;
            }
            if !is_nav(it.command) {
                if self.started != Some(m.seq) {
                    out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_INFO,
                        format!("Mission: {} {}", m.seq, type_name(it.command)),
                    ));
                }
                m.seq += 1;
                self.started = None;
                continue;
            }
            if self.started != Some(m.seq) {
                self.started = Some(m.seq);
                out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    format!("Mission: {} {}", m.seq, type_name(it.command)),
                ));
            }
            let pos =
                (it.x != 0 || it.y != 0).then(|| (f64::from(it.x) / 1e7, f64::from(it.y) / 1e7));
            return Some(Nav {
                seq: m.seq,
                command: it.command,
                pos,
                alt: item_alt(st, &it),
                item: it,
            });
        }
        None
    }

    /// The current item is done: MISSION_ITEM_REACHED, then `text` (the firmware's own
    /// "Reached ..." line, if it says one), then the next item.
    pub fn done(&mut self, m: &mut Mission, out: &mut Vec<Out>, text: Option<String>) {
        out.push(Out::ItemReached(m.seq));
        if let Some(text) = text {
            out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, text));
        }
        self.started = None;
        if usize::from(m.seq) + 1 < m.items.len() {
            m.seq += 1;
            out.push(Out::MissionCurrent(m.seq));
        } else {
            // ArduPilot keeps reporting the last item once the mission is complete
            self.finished = true;
            m.complete = true;
        }
        self.seen = m.seq;
    }
}

/// A vehicle without a link, for the behaviors' tests: commands in, ticks, what it said.
#[cfg(test)]
pub(super) mod testkit {
    use mavlink::dialects::ardupilotmega::{
        MISSION_ITEM_INT_DATA, MavCmd, MavFrame, MavModeFlag, MavResult,
    };

    use crate::behavior::{Behavior, Command, Ctx, Tick};
    use crate::config::Spec;
    use crate::geo::Gauss;
    use crate::kinematics::{Out, State};
    use crate::mission::Mission;
    use crate::params::Params;

    pub const LAT: f64 = 37.795;
    pub const LON: f64 = -122.465;

    pub struct Sim {
        pub b: Box<dyn Behavior>,
        pub spec: Spec,
        pub params: Params,
        pub st: State,
        pub mission: Mission,
        pub said: Vec<String>,
        pub reached: Vec<u16>,
        rng: Gauss,
    }

    impl Sim {
        pub fn new(behavior: &str, cruise: f64, turn_dps: Option<f64>) -> Sim {
            let b = crate::behavior::create(behavior).expect("behavior");
            let spec = Spec {
                name: behavior.into(),
                behavior: behavior.into(),
                cruise_mps: cruise,
                max_mps: cruise,
                climb_mps: None,
                turn_dps,
                loiter_radius_m: None,
                endurance_h: None,
                link: None,
            };
            let mut params = Params::seed(b.firmware(), &spec, 1);
            params.extend(b.extra_params(&spec));
            let mut st = State {
                lat: LAT,
                lon: LON,
                alt: 20.0,
                heading: 0.0,
                speed: 0.0,
                vz: 0.0,
                ve: 0.0,
                vn: 0.0,
                home: (LAT, LON, 20.0),
                armed: false,
                mode: b.hold_mode(),
                hold_point: (LAT, LON),
                aground: false,
                demand: 0.0,
            };
            b.spawn(&spec, &mut st);
            Sim {
                b,
                spec,
                params,
                st,
                mission: Mission::default(),
                said: Vec::new(),
                reached: Vec::new(),
                rng: Gauss::new(1),
            }
        }

        fn take(&mut self, out: Vec<Out>) {
            for o in out {
                match o {
                    Out::Text(_, s) => self.said.push(s),
                    Out::ItemReached(seq) => self.reached.push(seq),
                    Out::MissionCurrent(_) => {}
                    Out::SetMode(name) => {
                        self.st.mode = name;
                        self.st.hold_point = (self.st.lat, self.st.lon);
                    }
                }
            }
        }

        pub fn command(&mut self, command: MavCmd, p: [f32; 7]) -> Option<MavResult> {
            let m = Command {
                command,
                p: [p[0], p[1], p[2], p[3]],
                x: f64::from(p[4]),
                y: f64::from(p[5]),
                z: p[6],
            };
            let mut out = Vec::new();
            let r = self.b.command(
                &mut Ctx {
                    spec: &self.spec,
                    params: &self.params,
                    st: &mut self.st,
                    mission: &mut self.mission,
                    out: &mut out,
                },
                &m,
            );
            self.take(out);
            r
        }

        pub fn mode(&mut self, number: u32) -> Option<MavResult> {
            let custom = f32::from(MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED.bits());
            self.command(
                MavCmd::MAV_CMD_DO_SET_MODE,
                [custom, number as f32, 0., 0., 0., 0., 0.],
            )
        }

        pub fn arm(&mut self, on: bool) -> Option<MavResult> {
            let p1 = if on { 1.0 } else { 0.0 };
            self.command(
                MavCmd::MAV_CMD_COMPONENT_ARM_DISARM,
                [p1, 0., 0., 0., 0., 0., 0.],
            )
        }

        pub fn run(&mut self, seconds: f64) {
            for _ in 0..(seconds * 10.0) as usize {
                let mut out = Vec::new();
                self.b.step(&mut Tick {
                    spec: &self.spec,
                    params: &self.params,
                    st: &mut self.st,
                    mission: &mut self.mission,
                    water: None,
                    rng: &mut self.rng,
                    out: &mut out,
                    dt: 0.1,
                });
                self.take(out);
            }
        }

        /// Run until `cond` or `max_s`; the seconds it took.
        pub fn until(&mut self, max_s: f64, cond: impl Fn(&Sim) -> bool) -> Option<f64> {
            let mut t = 0.0;
            while t < max_s {
                if cond(self) {
                    return Some(t);
                }
                self.run(0.1);
                t += 0.1;
            }
            None
        }

        pub fn rel_alt(&self) -> f64 {
            self.st.alt - self.st.home.2
        }

        pub fn said(&self, s: &str) -> bool {
            self.said.iter().any(|t| t.starts_with(s))
        }

        /// Install a mission: (command, north m, east m, alt rel m) per item after home.
        #[allow(deprecated)]
        pub fn mission(&mut self, items: &[(MavCmd, f64, f64, f32)]) {
            let mut all = vec![(MavCmd::MAV_CMD_NAV_WAYPOINT, 0.0, 0.0, 0.0)];
            all.extend_from_slice(items);
            let items = all
                .iter()
                .enumerate()
                .map(|(i, (command, n, e, z))| {
                    let (lat, lon) = offset(*n, *e);
                    let here = *n == 0.0 && *e == 0.0 && i > 0;
                    MISSION_ITEM_INT_DATA {
                        seq: i as u16,
                        command: *command,
                        frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
                        x: if here { 0 } else { (lat * 1e7) as i32 },
                        y: if here { 0 } else { (lon * 1e7) as i32 },
                        z: *z,
                        ..Default::default()
                    }
                })
                .collect();
            self.mission.install(items);
        }

        pub fn dist_to(&self, north: f64, east: f64) -> f64 {
            let (lat, lon) = offset(north, east);
            crate::geo::distance_m(self.st.lat, self.st.lon, lat, lon)
        }
    }

    pub fn offset(north: f64, east: f64) -> (f64, f64) {
        let m = crate::geo::M_PER_DEG;
        (LAT + north / m, LON + east / (m * LAT.to_radians().cos()))
    }
}
