//! Mission stores (mission, fence, rally), the upload protocol's state, and the AUTO walker.
//!
//! The walker follows ArduPilot's AP_Mission and each firmware's start_command/verify_command
//! (ArduPlane/commands_logic.cpp, Rover/mode_auto.cpp, ArduSub/commands_logic.cpp):
//! "Mission: N Type" as each command starts (AP_Mission::start_command, again when AUTO is
//! re-entered or the GCS sets the current item), DO_ and CONDITION_ items run as they are
//! passed and are never flown to (with the firmware's own text, Plane's "Set airspeed N m/s",
//! Rover's "speed: N.N m/s"), navigation items a firmware doesn't support are skipped with
//! "Skipping invalid cmd #<id>", and when the mission runs out each firmware does its own
//! thing (Plane: RTL; Rover and Sub: hold at the last waypoint, in AUTO).

use std::collections::HashMap;

use mavlink::dialects::ardupilotmega::{
    MISSION_ITEM_INT_DATA, MavCmd, MavFrame, MavMissionResult, MavMissionType, MavSeverity,
    MissionState,
};

use crate::behavior::{Firmware, apmission};
use crate::geo::distance_m;
use crate::kinematics::{Out, State, Target, TargetKind};

/// ArduPilot re-requests a missing item after 1 s and gives up 8 s after the last one arrived
/// (MissionItemProtocol.cpp: wp_recv_timeout_ms, upload_timeout_ms).
pub const UPLOAD_RETRY_S: f64 = 1.0;
pub const UPLOAD_TIMEOUT_S: f64 = 8.0;

/// An upload in progress. As ArduPilot does, items are requested strictly in order.
#[derive(Debug, Clone)]
pub struct Upload {
    pub mission_type: MavMissionType,
    pub count: u16,
    pub items: Vec<MISSION_ITEM_INT_DATA>,
    /// (sysid, compid) of the GCS that started it; others are refused
    pub from: (u8, u8),
    pub last_request: f64,
    pub last_receive: f64,
}

impl Upload {
    pub fn next(&self) -> u16 {
        self.items.len() as u16
    }
}

/// What the walker needs from the vehicle, refreshed every physics tick.
#[derive(Debug, Clone, Copy)]
pub struct NavCfg {
    pub firmware: Firmware,
    /// seconds since boot (sim clock)
    pub now: f64,
    /// waypoint acceptance radius, m (for loiter-time arrival)
    pub wp_radius: f64,
    /// smallest circle it can fly (fixed wing), m
    pub min_radius: f64,
    pub max_speed: f64,
}

impl Default for NavCfg {
    fn default() -> Self {
        NavCfg {
            firmware: Firmware::Rover,
            now: 0.0,
            wp_radius: 10.0,
            min_radius: 0.0,
            max_speed: f64::INFINITY,
        }
    }
}

#[derive(Debug, Default)]
pub struct Mission {
    pub items: Vec<MISSION_ITEM_INT_DATA>,
    pub seq: u16,
    /// how often each DO_JUMP has run, by the jump item's seq (AP_Mission's jump tracking)
    pub jumps: HashMap<u16, i32>,
    pub upload: Option<Upload>,
    pub fence: Vec<MISSION_ITEM_INT_DATA>,
    pub rally: Vec<MISSION_ITEM_INT_DATA>,
    pub cfg: NavCfg,
    /// DO_CHANGE_SPEED, m/s (None = the platform's cruise)
    pub speed: Option<f64>,
    /// AUTO ran out of items (AP_Mission MISSION_COMPLETE)
    pub complete: bool,
    /// DO_SET_HOME passed: (lat, lon, alt)
    pub new_home: Option<(f64, f64, f64)>,
    /// the next AUTO resumes at the current item whatever MIS_RESTART says (AP_Mission's
    /// set_force_resume, as ArduPlane's RTL_AUTOLAND sets it for its landing sequence)
    pub force_resume: bool,
    /// the navigation item whose start AP_Mission has announced ("Mission: N WP")
    announced: Option<u16>,
    /// timed item in progress: (seq, arrived at)
    loiter: Option<(u16, f64)>,
    last_nav: Option<(f64, f64, f64)>,
    end_point: Option<(f64, f64, f64)>,
}

/// MAV_CMD_NAV_LAST: ids below it are navigation commands (AP_Mission::is_nav_cmd).
const NAV_LAST: u32 = 95;

pub fn is_nav(cmd: MavCmd) -> bool {
    (cmd as u32) < NAV_LAST
}

/// Navigation commands each firmware's start_command handles; the rest are skipped.
fn supported(fw: Firmware, cmd: MavCmd) -> bool {
    use MavCmd::*;
    if fw == Firmware::Blimp {
        // ModeAuto::start_command: waypoints only; everything else is "Command not supported"
        return cmd == MAV_CMD_NAV_WAYPOINT;
    }
    let common = matches!(
        cmd,
        MAV_CMD_NAV_WAYPOINT
            | MAV_CMD_NAV_LOITER_UNLIM
            | MAV_CMD_NAV_LOITER_TURNS
            | MAV_CMD_NAV_LOITER_TIME
            | MAV_CMD_NAV_RETURN_TO_LAUNCH
            | MAV_CMD_NAV_DELAY
            | MAV_CMD_NAV_GUIDED_ENABLE
    );
    common
        || match fw {
            Firmware::Plane => matches!(
                cmd,
                MAV_CMD_NAV_TAKEOFF
                    | MAV_CMD_NAV_LAND
                    | MAV_CMD_NAV_LOITER_TO_ALT
                    | MAV_CMD_NAV_CONTINUE_AND_CHANGE_ALT
                    | MAV_CMD_NAV_VTOL_TAKEOFF
                    | MAV_CMD_NAV_VTOL_LAND
            ),
            Firmware::Copter => matches!(
                cmd,
                MAV_CMD_NAV_TAKEOFF
                    | MAV_CMD_NAV_LAND
                    | MAV_CMD_NAV_SPLINE_WAYPOINT
                    | MAV_CMD_NAV_VTOL_TAKEOFF
                    | MAV_CMD_NAV_VTOL_LAND
            ),
            // ArduSub: NAV_LAND = surface at the location
            Firmware::Sub => matches!(cmd, MAV_CMD_NAV_LAND),
            Firmware::Rover | Firmware::Tracker | Firmware::Blimp => false,
        }
}

// GLOBAL_RELATIVE_ALT_INT is deprecated for COMMAND_INT, still the mission item frame
#[allow(deprecated)]
fn item_alt(it: &MISSION_ITEM_INT_DATA, st: &State) -> f64 {
    use MavFrame::*;
    match it.frame {
        MAV_FRAME_GLOBAL_RELATIVE_ALT
        | MAV_FRAME_GLOBAL_RELATIVE_ALT_INT
        | MAV_FRAME_GLOBAL_TERRAIN_ALT
        | MAV_FRAME_GLOBAL_TERRAIN_ALT_INT => st.home.2 + f64::from(it.z),
        _ => f64::from(it.z),
    }
}

/// Download frames: ArduPilot stores locations without the _INT distinction and reports the
/// plain frames (AP_Mission::mission_cmd_to_mavlink_int).
#[allow(deprecated)]
fn plain_frame(f: MavFrame) -> MavFrame {
    use MavFrame::*;
    match f {
        MAV_FRAME_GLOBAL_INT => MAV_FRAME_GLOBAL,
        MAV_FRAME_GLOBAL_RELATIVE_ALT_INT => MAV_FRAME_GLOBAL_RELATIVE_ALT,
        MAV_FRAME_GLOBAL_TERRAIN_ALT_INT => MAV_FRAME_GLOBAL_TERRAIN_ALT,
        other => other,
    }
}

impl Mission {
    pub fn advance(&mut self, out: &mut Vec<Out>) {
        self.seq = self.seq.saturating_add(1);
        self.loiter = None;
        // past the last item the mission is over, and AP_Mission keeps reporting the last one
        // (the walker settles on it in `finish`)
        out.push(Out::MissionCurrent(self.seq.min(self.last())));
    }

    /// The last item's seq (0 when the mission is empty).
    fn last(&self) -> u16 {
        self.items.len().saturating_sub(1) as u16
    }

    /// A waypoint reached at `dist` metres: MISSION_ITEM_REACHED and the firmware's text.
    pub fn reached_at(&mut self, st: &State, out: &mut Vec<Out>, dist: f64) {
        let text = match self.cfg.firmware {
            Firmware::Plane => format!("Reached waypoint #{} dist {}m", self.seq, dist.round()),
            Firmware::Sub => format!("Reached command #{}", self.seq),
            _ => format!("Reached waypoint #{}", self.seq),
        };
        self.nav_done(st, out, Some(text));
    }

    /// A takeoff item done at `rel_alt` metres above home: ArduPlane's text (every firmware
    /// sends MISSION_ITEM_REACHED for it, as for any navigation command).
    pub fn takeoff_done(&mut self, st: &State, out: &mut Vec<Out>, rel_alt: f64) {
        let text = (self.cfg.firmware == Firmware::Plane)
            .then(|| format!("Takeoff complete at {rel_alt:.2}m"));
        self.nav_done(st, out, text);
    }

    /// The current navigation item is done: MISSION_ITEM_REACHED, the firmware's text, and the
    /// DO commands right after it, which AP_Mission starts and completes in the same update
    /// (seen in SITL: their MISSION_ITEM_REACHED takes the waypoint's place, GCS_MAVLINK
    /// keeping one deferred index). Then the next navigation item is the current one.
    pub fn nav_done(&mut self, st: &State, out: &mut Vec<Out>, text: Option<String>) {
        let at = out.len();
        let mut reached = self.seq;
        if let Some(text) = text {
            out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, text));
        }
        self.seq = self.seq.saturating_add(1);
        self.loiter = None;
        while let Some(it) = self.items.get(usize::from(self.seq)).cloned() {
            if is_nav(it.command) || it.command == MavCmd::MAV_CMD_DO_JUMP {
                break;
            }
            apmission::started(self.seq, it.command, out);
            if let Some(text) = self.do_command(&it, st) {
                out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, text));
            }
            reached = self.seq;
            self.seq += 1;
        }
        out.insert(at, Out::ItemReached(reached));
        // past the last item the mission is over, and AP_Mission keeps reporting the last one
        // (the walker settles on it in `finish`)
        out.push(Out::MissionCurrent(self.seq.min(self.last())));
    }

    /// Install a completed upload. Same bookkeeping as ArduPilot: the current item stays 0
    /// (home) until the mission starts; AUTO then begins at item 1.
    pub fn install(&mut self, items: Vec<MISSION_ITEM_INT_DATA>) {
        self.seq = 0;
        self.items = items;
        self.restart_state();
    }

    fn restart_state(&mut self) {
        self.jumps.clear();
        self.speed = None;
        self.complete = false;
        self.announced = None;
        self.loiter = None;
        self.last_nav = None;
        self.end_point = None;
    }

    /// AP_Mission::present: more than home.
    pub fn present(&self) -> bool {
        self.items.len() > 1
    }

    /// Entering AUTO (AP_Mission::start_or_resume): a finished mission, or MIS_RESTART=1,
    /// starts again from item 1; otherwise it resumes where it was.
    pub fn start_or_resume(&mut self, restart: bool) {
        let forced = std::mem::take(&mut self.force_resume);
        if self.complete || (restart && !forced) {
            self.seq = if self.present() { 1 } else { 0 };
            self.restart_state();
        }
        // resuming restarts the current command (AP_Mission::resume): it is announced again
        self.announced = None;
        self.loiter = None;
    }

    /// Entering AUTO, AP_Mission starts the current navigation command in the mode change
    /// itself, so its "Mission: N Type" precedes the COMMAND_ACK (and anything the GCS sends
    /// next); the walker on the following tick then has nothing to announce.
    pub fn announce_current(&mut self, out: &mut Vec<Out>) {
        if self.complete || !self.present() {
            return;
        }
        let seq = self.seq.max(1);
        if self.announced != Some(seq)
            && let Some(it) = self.items.get(usize::from(seq))
            && is_nav(it.command)
        {
            self.announced = Some(seq);
            apmission::started(seq, it.command, out);
        }
    }

    /// DO_SET_MISSION_CURRENT / MISSION_SET_CURRENT.
    pub fn set_current(&mut self, seq: u16) {
        self.seq = seq;
        self.complete = false;
        self.announced = None;
        self.loiter = None;
        self.end_point = None;
    }

    /// MISSION_CURRENT.mission_state (GCS_MAVLINK::mission_state).
    pub fn state(&self, in_auto: bool) -> MissionState {
        if !self.present() {
            MissionState::MISSION_STATE_NO_MISSION
        } else if self.complete {
            MissionState::MISSION_STATE_COMPLETE
        } else if in_auto {
            MissionState::MISSION_STATE_ACTIVE
        } else {
            MissionState::MISSION_STATE_NOT_STARTED
        }
    }

    /// The current navigation item's location, for NAV_CONTROLLER_OUTPUT.
    pub fn current_location(&self) -> Option<(f64, f64)> {
        let it = self.items.get(self.seq as usize)?;
        (is_nav(it.command) && (it.x != 0 || it.y != 0))
            .then(|| (f64::from(it.x) / 1e7, f64::from(it.y) / 1e7))
    }

    /// The landing sequence nearest (lat, lon) (AP_Mission::get_landing_sequence_start): the
    /// DO_LAND_START whose own location, or else the next navigation item's, is closest; the
    /// first of equals. None without one.
    pub fn landing_sequence_start(&self, lat: f64, lon: f64) -> Option<u16> {
        let located = |it: &MISSION_ITEM_INT_DATA| {
            (it.x != 0 || it.y != 0).then(|| (f64::from(it.x) / 1e7, f64::from(it.y) / 1e7))
        };
        let mut best: Option<(u16, f64)> = None;
        for (i, it) in self.items.iter().enumerate().skip(1) {
            if it.command != MavCmd::MAV_CMD_DO_LAND_START {
                continue;
            }
            let at = located(it).or_else(|| {
                self.items[i + 1..]
                    .iter()
                    .find(|n| is_nav(n.command))
                    .and_then(located)
            });
            let Some((la, lo)) = at else { continue };
            let d = distance_m(lat, lon, la, lo);
            if best.is_none_or(|(_, b)| d < b) {
                best = Some((i as u16, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Whether the current item is a landing (the behavior watches for touchdown).
    pub fn landing(&self) -> bool {
        self.items.get(self.seq as usize).is_some_and(|it| {
            matches!(
                it.command,
                MavCmd::MAV_CMD_NAV_LAND | MavCmd::MAV_CMD_NAV_VTOL_LAND
            )
        })
    }

    /// The target for AUTO: run DO items and DO_JUMPs up to the current navigation item.
    /// `hold_r` is the behavior's circle for holding (fixed wing) or 0 (it can stop),
    /// `loiter_default` its loiter radius when an item gives none. What AP_Mission says and
    /// does on the way (texts, MISSION_CURRENT, a mode change at the end) goes to `out`, in
    /// order with the behavior's own events.
    pub fn auto_target(
        &mut self,
        st: &State,
        cruise: f64,
        hold_r: f64,
        loiter_default: f64,
        out: &mut Vec<Out>,
    ) -> Target {
        if self.complete {
            return self.end_hold(st, cruise, hold_r);
        }
        let fw = self.cfg.firmware;
        let entry = self.seq;
        let n = self.items.len();
        // AP_Mission gives up on DO_JUMP cycles with no navigation item; so does this
        let mut budget = 4 * n + 16;
        // DO commands passed on the way complete at once; one MISSION_ITEM_REACHED for the
        // last of them (see `complete_nav`)
        let mut reached = None;
        let target = loop {
            if (self.seq as usize) >= n || budget == 0 {
                break None;
            }
            budget -= 1;
            if self.seq == 0 {
                self.seq = 1;
                continue;
            }
            let it = self.items[self.seq as usize].clone();
            if it.command == MavCmd::MAV_CMD_DO_JUMP {
                // AP_Mission::get_next_cmd: run it while it has repeats left, saying so each
                // time; a finished loop's counter is zeroed, so it repeats if reached again
                let times = it.param2 as i32;
                let runs = self.jumps.entry(self.seq).or_insert(0);
                if times < 0 || *runs < times {
                    *runs += 1;
                    let text = apmission::jump_text(self.seq, *runs, times);
                    out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, text));
                    self.seq = it.param1 as u16;
                } else {
                    *runs = 0;
                    self.seq += 1;
                }
                continue;
            }
            if !is_nav(it.command) {
                apmission::started(self.seq, it.command, out);
                if let Some(text) = self.do_command(&it, st) {
                    out.push(Out::Text(MavSeverity::MAV_SEVERITY_INFO, text));
                }
                reached = Some(self.seq);
                self.seq += 1;
                continue;
            }
            if self.announced != Some(self.seq) {
                self.announced = Some(self.seq);
                apmission::started(self.seq, it.command, out);
            }
            if !supported(fw, it.command) {
                out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_WARNING,
                    format!("Skipping invalid cmd #{}", it.command as u32),
                ));
                self.seq += 1;
                continue;
            }
            if it.command == MavCmd::MAV_CMD_NAV_GUIDED_ENABLE {
                self.seq += 1;
                continue;
            }
            break Some(self.nav_target(&it, st, cruise, hold_r, loiter_default, out));
        };
        if let Some(seq) = reached {
            out.push(Out::ItemReached(seq));
        }
        let Some(target) = target else {
            self.finish(st, out);
            return self.end_hold(st, cruise, hold_r);
        };
        self.last_nav = Some((target.lat, target.lon, target.alt));
        if self.seq != entry {
            out.push(Out::MissionCurrent(self.seq));
        }
        target
    }

    fn nav_target(
        &mut self,
        it: &MISSION_ITEM_INT_DATA,
        st: &State,
        cruise: f64,
        hold_r: f64,
        loiter_default: f64,
        out: &mut Vec<Out>,
    ) -> Target {
        use MavCmd::*;
        let fw = self.cfg.firmware;
        // a location of 0,0 means "here" (Location::sanitize)
        let (lat, lon) = if it.x == 0 && it.y == 0 {
            (st.lat, st.lon)
        } else {
            (f64::from(it.x) / 1e7, f64::from(it.y) / 1e7)
        };
        let alt = item_alt(it, st);
        let speed = self.speed.unwrap_or(cruise);
        let t = |lat, lon, alt, radius, kind| Target {
            lat,
            lon,
            alt,
            radius,
            speed,
            kind,
        };
        let min_r = self.cfg.min_radius;
        // the item's radius (param3), else the default; a vehicle that stops holds the point
        let circle = |p3: f32| {
            let r = if p3 != 0.0 {
                f64::from(p3).abs()
            } else {
                loiter_default
            };
            r.max(min_r)
        };
        match it.command {
            MAV_CMD_NAV_TAKEOFF | MAV_CMD_NAV_VTOL_TAKEOFF => t(
                st.lat,
                st.lon,
                st.home.2 + f64::from(it.z),
                0.0,
                TargetKind::Takeoff,
            ),
            MAV_CMD_NAV_LAND | MAV_CMD_NAV_VTOL_LAND if fw == Firmware::Sub => {
                t(lat, lon, 0.0, 0.0, TargetKind::Waypoint)
            }
            MAV_CMD_NAV_LAND | MAV_CMD_NAV_VTOL_LAND => {
                t(lat, lon, st.home.2, 0.0, TargetKind::Land)
            }
            MAV_CMD_NAV_RETURN_TO_LAUNCH if fw == Firmware::Plane => {
                // ArduPlane switches to RTL mode
                out.push(Out::SetMode("RTL"));
                t(st.home.0, st.home.1, st.alt, hold_r, TargetKind::Rtl)
            }
            MAV_CMD_NAV_RETURN_TO_LAUNCH => {
                // Rover and Sub fly home inside AUTO (Sub to the home altitude: the surface)
                let alt = if fw == Firmware::Sub {
                    st.home.2
                } else {
                    st.alt
                };
                t(st.home.0, st.home.1, alt, 0.0, TargetKind::Waypoint)
            }
            MAV_CMD_NAV_LOITER_UNLIM => {
                let r = if hold_r == 0.0 {
                    0.0
                } else {
                    circle(it.param3)
                };
                t(lat, lon, alt, r, TargetKind::Loiter)
            }
            MAV_CMD_NAV_LOITER_TIME => {
                let r = if hold_r == 0.0 {
                    0.0
                } else {
                    circle(it.param3)
                };
                self.timed(
                    st,
                    t(lat, lon, alt, r, TargetKind::Loiter),
                    f64::from(it.param1),
                    out,
                )
            }
            MAV_CMD_NAV_LOITER_TURNS => {
                // everyone circles for this one (Rover and Sub: their circle modes)
                let r = circle(it.param3).max(1.0);
                let lap = std::f64::consts::TAU * r / speed.max(0.1);
                let tgt = t(lat, lon, alt, r, TargetKind::Loiter);
                self.timed(st, tgt, f64::from(it.param1).max(0.0) * lap, out)
            }
            MAV_CMD_NAV_DELAY => {
                let r = if hold_r == 0.0 { 0.0 } else { hold_r };
                let here = t(st.lat, st.lon, st.alt, r, TargetKind::Loiter);
                self.timed(st, here, f64::from(it.param1).max(0.0), out)
            }
            // Rover / Sub / Copter: param1 of a waypoint is a hold time there, s
            MAV_CMD_NAV_WAYPOINT if fw != Firmware::Plane && it.param1 > 0.0 => self.timed(
                st,
                t(lat, lon, alt, 0.0, TargetKind::Loiter),
                f64::from(it.param1),
                out,
            ),
            _ => t(lat, lon, alt, 0.0, TargetKind::Waypoint),
        }
    }

    /// Hold `tgt` for `secs` once there, then complete the item.
    fn timed(&mut self, st: &State, tgt: Target, secs: f64, out: &mut Vec<Out>) -> Target {
        let now = self.cfg.now;
        let dist = distance_m(st.lat, st.lon, tgt.lat, tgt.lon);
        let tol = self.cfg.wp_radius.max(tgt.radius * 0.25);
        let there = if tgt.radius > 0.0 {
            dist < tgt.radius + tol
        } else {
            dist < tol
        };
        match self.loiter {
            Some((seq, since)) if seq == self.seq => {
                if now - since >= secs {
                    self.nav_done(st, out, None);
                }
            }
            _ if there => {
                self.loiter = Some((self.seq, now));
                let it = self.items.get(self.seq as usize);
                let waits = it.is_some_and(|it| {
                    matches!(
                        it.command,
                        MavCmd::MAV_CMD_NAV_WAYPOINT | MavCmd::MAV_CMD_NAV_LOITER_TIME
                    )
                });
                if self.cfg.firmware == Firmware::Rover && waits {
                    out.push(Out::Text(
                        MavSeverity::MAV_SEVERITY_INFO,
                        format!(
                            "Reached waypoint #{}. Loiter for {} seconds",
                            self.seq,
                            secs.round()
                        ),
                    ));
                }
            }
            _ => {}
        }
        tgt
    }

    /// Run a DO_ / CONDITION_ item; the firmware's text about it, if it says one.
    fn do_command(&mut self, it: &MISSION_ITEM_INT_DATA, st: &State) -> Option<String> {
        match it.command {
            // param2: speed, m/s; -1 (or 0) = no change. Plane says what it set
            // (Plane::do_change_speed), Rover too (ModeAuto::do_change_speed), Sub nothing
            MavCmd::MAV_CMD_DO_CHANGE_SPEED if it.param2 > 0.0 => {
                self.speed = Some(f64::from(it.param2).min(self.cfg.max_speed));
                match self.cfg.firmware {
                    Firmware::Plane if it.param1 == 1.0 => {
                        Some(format!("Set groundspeed {}", it.param2 as u32))
                    }
                    Firmware::Plane => Some(format!("Set airspeed {} m/s", it.param2 as u32)),
                    Firmware::Rover => Some(format!("speed: {:.1} m/s", it.param2)),
                    _ => None,
                }
            }
            MavCmd::MAV_CMD_DO_SET_HOME => {
                self.new_home = Some(if it.param1 == 1.0 || (it.x == 0 && it.y == 0) {
                    (st.lat, st.lon, st.alt)
                } else {
                    (
                        f64::from(it.x) / 1e7,
                        f64::from(it.y) / 1e7,
                        f64::from(it.z),
                    )
                });
                None
            }
            // every other DO_ / CONDITION_ item: done (or not modelled), never flown
            _ => None,
        }
    }

    /// The mission ran out (exit_mission / exit_mission_callback). The current item stays the
    /// last one, as AP_Mission keeps reporting it.
    fn finish(&mut self, st: &State, out: &mut Vec<Out>) {
        self.complete = true;
        self.seq = self.last();
        self.end_point = self.last_nav.or(Some((st.lat, st.lon, st.alt)));
        match self.cfg.firmware {
            Firmware::Plane => {
                out.push(Out::SetMode("RTL"));
                out.push(Out::Text(
                    MavSeverity::MAV_SEVERITY_INFO,
                    "Mission complete, changing mode to RTL".into(),
                ));
            }
            Firmware::Rover => out.push(Out::Text(
                MavSeverity::MAV_SEVERITY_NOTICE,
                "Mission Complete".into(),
            )),
            _ => {}
        }
    }

    /// After the mission: hold (or circle) at the last waypoint.
    fn end_hold(&self, st: &State, cruise: f64, hold_r: f64) -> Target {
        let (lat, lon, alt) = self.end_point.unwrap_or((st.lat, st.lon, st.alt));
        let alt = if self.cfg.firmware == Firmware::Plane {
            st.alt
        } else {
            alt
        };
        Target {
            lat,
            lon,
            alt,
            radius: hold_r,
            speed: self.speed.unwrap_or(cruise),
            kind: TargetKind::Hold,
        }
    }

    // ---- the mission protocol's stores ----

    /// Items for a download: MISSION_COUNT's count (None: no such mission type).
    pub fn download_count(&self, ty: MavMissionType) -> Option<u16> {
        Some(match ty {
            // ArduPilot always has item 0 (home)
            MavMissionType::MAV_MISSION_TYPE_MISSION => self.items.len().max(1) as u16,
            MavMissionType::MAV_MISSION_TYPE_FENCE => self.fence.len() as u16,
            MavMissionType::MAV_MISSION_TYPE_RALLY => self.rally.len() as u16,
            _ => return None,
        })
    }

    /// One item for a download, as AP_Mission::mission_cmd_to_mavlink_int reports what it
    /// stored: item 0 of the mission is the current home (absolute altitude), commands without
    /// a location are in the global frame, and NAV_LAND's param4 is its yaw direction (+-1).
    pub fn download_item(
        &self,
        ty: MavMissionType,
        seq: u16,
        home: (f64, f64, f64),
    ) -> Option<MISSION_ITEM_INT_DATA> {
        let mut it = match ty {
            MavMissionType::MAV_MISSION_TYPE_MISSION if seq == 0 => MISSION_ITEM_INT_DATA {
                command: MavCmd::MAV_CMD_NAV_WAYPOINT,
                frame: MavFrame::MAV_FRAME_GLOBAL,
                x: (home.0 * 1e7) as i32,
                y: (home.1 * 1e7) as i32,
                z: home.2 as f32,
                autocontinue: 1,
                ..Default::default()
            },
            MavMissionType::MAV_MISSION_TYPE_MISSION => self.items.get(seq as usize)?.clone(),
            MavMissionType::MAV_MISSION_TYPE_FENCE => self.fence.get(seq as usize)?.clone(),
            MavMissionType::MAV_MISSION_TYPE_RALLY => self.rally.get(seq as usize)?.clone(),
            _ => return None,
        };
        it.seq = seq;
        it.mission_type = ty;
        it.current = 0;
        it.frame = if is_nav(it.command) {
            plain_frame(it.frame)
        } else {
            MavFrame::MAV_FRAME_GLOBAL
        };
        if it.command == MavCmd::MAV_CMD_NAV_LAND {
            it.param4 = if it.param4 < 0.0 { -1.0 } else { 1.0 };
        }
        Some(it)
    }

    /// Check one uploaded item as ArduPilot's protocol handlers do.
    pub fn validate(
        ty: MavMissionType,
        it: &MISSION_ITEM_INT_DATA,
        count: u16,
    ) -> MavMissionResult {
        let id = it.command as u32;
        match ty {
            MavMissionType::MAV_MISSION_TYPE_MISSION => {
                if it.command == MavCmd::MAV_CMD_DO_JUMP {
                    let target = it.param1 as i64;
                    if target <= 0 || target >= i64::from(count) {
                        return MavMissionResult::MAV_MISSION_ERROR;
                    }
                }
                MavMissionResult::MAV_MISSION_ACCEPTED
            }
            // NAV_FENCE_RETURN_POINT .. NAV_FENCE_CIRCLE_EXCLUSION
            MavMissionType::MAV_MISSION_TYPE_FENCE if (5000..=5004).contains(&id) => {
                MavMissionResult::MAV_MISSION_ACCEPTED
            }
            MavMissionType::MAV_MISSION_TYPE_RALLY if id == 5100 => {
                MavMissionResult::MAV_MISSION_ACCEPTED
            }
            _ => MavMissionResult::MAV_MISSION_UNSUPPORTED,
        }
    }

    /// A finished upload replaces that store.
    pub fn store(&mut self, ty: MavMissionType, items: Vec<MISSION_ITEM_INT_DATA>) {
        match ty {
            MavMissionType::MAV_MISSION_TYPE_MISSION => self.install(items),
            MavMissionType::MAV_MISSION_TYPE_FENCE => self.fence = items,
            MavMissionType::MAV_MISSION_TYPE_RALLY => self.rally = items,
            _ => {}
        }
    }

    /// MISSION_CLEAR_ALL.
    pub fn clear(&mut self, ty: MavMissionType) {
        let all = ty == MavMissionType::MAV_MISSION_TYPE_ALL;
        if all || ty == MavMissionType::MAV_MISSION_TYPE_MISSION {
            self.install(Vec::new());
        }
        if all || ty == MavMissionType::MAV_MISSION_TYPE_FENCE {
            self.fence.clear();
        }
        if all || ty == MavMissionType::MAV_MISSION_TYPE_RALLY {
            self.rally.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(deprecated)]
    fn wp(seq: u16, command: MavCmd, p1: f32, p2: f32) -> MISSION_ITEM_INT_DATA {
        MISSION_ITEM_INT_DATA {
            seq,
            command,
            param1: p1,
            param2: p2,
            x: 378_000_000 + seq as i32 * 10_000,
            y: -1_225_000_000,
            z: 50.0,
            frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
            ..Default::default()
        }
    }

    fn state() -> State {
        State {
            lat: 37.8,
            lon: -122.5,
            alt: 0.0,
            heading: 0.0,
            speed: 0.0,
            vz: 0.0,
            ve: 0.0,
            vn: 0.0,
            home: (37.8, -122.5, 10.0),
            armed: true,
            mode: "AUTO",
            hold_point: (37.7, -122.4),
            aground: false,
            demand: 0.0,
        }
    }

    fn mission(fw: Firmware, items: Vec<MISSION_ITEM_INT_DATA>) -> Mission {
        let mut m = Mission::default();
        m.cfg.firmware = fw;
        m.install(items);
        m
    }

    const NAV: MavCmd = MavCmd::MAV_CMD_NAV_WAYPOINT;

    #[test]
    fn do_jump_repeats_then_falls_through() {
        let mut out = Vec::new();
        let jump = MavCmd::MAV_CMD_DO_JUMP;
        let mut m = mission(
            Firmware::Rover,
            vec![
                wp(0, NAV, 0., 0.),
                wp(1, NAV, 0., 0.),
                wp(2, jump, 1., 1.),
                wp(3, NAV, 0., 0.),
            ],
        );
        let st = state();
        assert_eq!(m.auto_target(&st, 5.0, 0.0, 200.0, &mut out).alt, 60.0);
        m.seq = 2;
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 1, "first pass jumps back");
        m.seq = 2;
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 3, "repeat count used up");
    }

    #[test]
    fn jump_cycle_without_navigation_does_not_hang() {
        let mut out = Vec::new();
        let jump = MavCmd::MAV_CMD_DO_JUMP;
        let mut m = mission(
            Firmware::Rover,
            vec![wp(0, NAV, 0., 0.), wp(1, jump, 1., -1.)],
        );
        let t = m.auto_target(&state(), 5.0, 0.0, 200.0, &mut out);
        assert_eq!(t.kind, TargetKind::Hold);
        assert!(m.complete);
    }

    #[test]
    fn do_items_are_run_not_flown() {
        let mut out = Vec::new();
        let mut speed = wp(1, MavCmd::MAV_CMD_DO_CHANGE_SPEED, 1., 3.5);
        speed.x = 0;
        speed.y = 0;
        let mut servo = wp(2, MavCmd::MAV_CMD_DO_SET_SERVO, 9., 1500.);
        servo.x = 0;
        servo.y = 0;
        let mut m = mission(
            Firmware::Rover,
            vec![wp(0, NAV, 0., 0.), speed, servo, wp(3, NAV, 0., 0.)],
        );
        let t = m.auto_target(&state(), 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 3);
        assert_eq!(t.kind, TargetKind::Waypoint);
        assert!((t.lat - 37.803).abs() < 1e-9, "flies to item 3, not to 0,0");
        assert_eq!(t.speed, 3.5, "DO_CHANGE_SPEED applied");
        assert!(out.contains(&Out::MissionCurrent(3)));
    }

    #[test]
    fn unsupported_nav_items_are_skipped_with_ardupilots_text() {
        let mut out = Vec::new();
        let mut m = mission(
            Firmware::Rover,
            vec![
                wp(0, NAV, 0., 0.),
                wp(1, MavCmd::MAV_CMD_NAV_TAKEOFF, 0., 0.),
                wp(2, NAV, 0., 0.),
            ],
        );
        m.auto_target(&state(), 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 2);
        assert!(out.contains(&Out::Text(
            MavSeverity::MAV_SEVERITY_WARNING,
            "Skipping invalid cmd #22".into()
        )));
    }

    #[test]
    fn loiter_time_holds_then_moves_on() {
        let mut out = Vec::new();
        let mut st = state();
        let mut m = mission(
            Firmware::Rover,
            vec![
                wp(0, NAV, 0., 0.),
                wp(1, MavCmd::MAV_CMD_NAV_LOITER_TIME, 30., 0.),
                wp(2, NAV, 0., 0.),
            ],
        );
        st.lat = 37.801; // at item 1
        let t = m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(
            (t.kind, t.radius),
            (TargetKind::Loiter, 0.0),
            "a boat holds the point"
        );
        m.cfg.now = 29.0;
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 1);
        m.cfg.now = 30.5;
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 2);
        assert!(out.contains(&Out::ItemReached(1)));
    }

    #[test]
    fn a_hold_time_waypoint_before_a_jump_is_reported_reached() {
        let mut out = Vec::new();
        let mut st = state();
        let mut m = mission(
            Firmware::Rover,
            vec![
                wp(0, NAV, 0., 0.),
                wp(1, NAV, 5., 0.),
                wp(2, MavCmd::MAV_CMD_DO_JUMP, 1., 1.),
            ],
        );
        st.lat = 37.801; // at item 1
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 1);
        m.cfg.now = 6.0;
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert!(out.contains(&Out::ItemReached(1)), "{out:?}");
        assert_eq!(m.seq, 2, "on the jump, run next tick");
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 1, "jumped back");
        assert!(out.contains(&Out::Text(
            MavSeverity::MAV_SEVERITY_INFO,
            "Mission: 2 Jump 1/1".into()
        )));
    }

    #[test]
    fn mission_end_per_firmware() {
        let mut out = Vec::new();
        let items = || vec![wp(0, NAV, 0., 0.), wp(1, NAV, 0., 0.)];
        let st = state();
        // Rover: holds at the last waypoint, not where AUTO started
        let mut m = mission(Firmware::Rover, items());
        m.seq = 2;
        let t = m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(t.kind, TargetKind::Hold);
        assert!(m.complete);
        assert!((t.lat - st.lat).abs() < 1e-9, "no waypoint flown yet: here");
        let mut m = mission(Firmware::Rover, items());
        let first = m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        m.seq = 2;
        let t = m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!((t.lat, t.lon, t.radius), (first.lat, first.lon, 0.0));
        assert!(
            out
                .iter()
                .any(|o| matches!(o, Out::Text(_, s) if s == "Mission Complete"))
        );
        // Plane: RTL
        let mut m = mission(Firmware::Plane, items());
        m.seq = 2;
        m.auto_target(&st, 5.0, 300.0, 300.0, &mut out);
        assert!(out.contains(&Out::SetMode("RTL")));
        // entering AUTO again restarts a finished mission
        m.start_or_resume(false);
        assert_eq!((m.seq, m.complete), (1, false));
    }

    #[test]
    fn a_mission_is_at_item_0_until_it_starts_and_keeps_its_last_item_at_the_end() {
        let mut out = Vec::new();
        let items = || vec![wp(0, NAV, 0., 0.), wp(1, NAV, 0., 0.), wp(2, NAV, 0., 0.)];
        let st = state();
        let mut m = mission(Firmware::Rover, items());
        assert_eq!(
            m.seq, 0,
            "installed, not started: AP_Mission has no current command"
        );
        m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!(m.seq, 1, "AUTO starts at item 1");
        assert!(out.contains(&Out::MissionCurrent(1)));
        // both items done: the walker runs off the end, the current item stays the last one
        let mut out = Vec::new();
        m.advance(&mut out);
        m.advance(&mut out);
        assert_eq!(
            out.last(),
            Some(&Out::MissionCurrent(2)),
            "never reports past the end"
        );
        let t = m.auto_target(&st, 5.0, 0.0, 200.0, &mut out);
        assert_eq!((m.seq, m.complete, t.kind), (2, true, TargetKind::Hold));
    }

    #[test]
    fn plane_mission_rtl_item_switches_mode_rover_flies_home() {
        let mut out = Vec::new();
        let items = || {
            vec![
                wp(0, NAV, 0., 0.),
                wp(1, MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH, 0., 0.),
            ]
        };
        let st = state();
        let mut p = mission(Firmware::Plane, items());
        let t = p.auto_target(&st, 5.0, 300.0, 300.0, &mut out);
        assert_eq!(t.kind, TargetKind::Rtl);
        assert!(out.contains(&Out::SetMode("RTL")));
        let mut r = mission(Firmware::Rover, items());
        let t = r.auto_target(&st, 5.0, 0.0, 300.0, &mut out);
        assert_eq!((t.kind, t.lat, t.lon), (TargetKind::Waypoint, 37.8, -122.5));
    }

    #[test]
    fn download_returns_home_and_plain_frames() {
        let m = mission(
            Firmware::Rover,
            vec![wp(0, NAV, 0., 0.), wp(1, NAV, 0., 0.)],
        );
        let ty = MavMissionType::MAV_MISSION_TYPE_MISSION;
        assert_eq!(m.download_count(ty), Some(2));
        let home = m.download_item(ty, 0, (1.0, 2.0, 3.0)).expect("home");
        assert_eq!((home.x, home.y, home.z), (10_000_000, 20_000_000, 3.0));
        let one = m.download_item(ty, 1, (1.0, 2.0, 3.0)).expect("item");
        #[allow(deprecated)]
        let rel = MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT;
        assert_eq!(one.frame, rel);
        assert!(m.download_item(ty, 2, (1.0, 2.0, 3.0)).is_none());
        assert_eq!(Mission::default().download_count(ty), Some(1));
    }

    #[test]
    fn upload_validation() {
        let mission = MavMissionType::MAV_MISSION_TYPE_MISSION;
        let fence = MavMissionType::MAV_MISSION_TYPE_FENCE;
        let jump = wp(1, MavCmd::MAV_CMD_DO_JUMP, 5., 1.);
        assert_eq!(
            Mission::validate(mission, &jump, 3),
            MavMissionResult::MAV_MISSION_ERROR
        );
        assert_eq!(
            Mission::validate(fence, &wp(0, NAV, 0., 0.), 1),
            MavMissionResult::MAV_MISSION_UNSUPPORTED
        );
        let vertex = wp(
            0,
            MavCmd::MAV_CMD_NAV_FENCE_POLYGON_VERTEX_INCLUSION,
            4.,
            0.,
        );
        assert_eq!(
            Mission::validate(fence, &vertex, 4),
            MavMissionResult::MAV_MISSION_ACCEPTED
        );
    }
}
