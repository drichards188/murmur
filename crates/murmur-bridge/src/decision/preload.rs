//! Rules 6: an engagement's fallback preloaded as its platform's own lost-link action.
//!
//! A fallback the bridge commands after a stop can't arrive over a dead link, so at dispatch
//! the engine's order carries a [`LostLinkPreload`]: the platform's GCS failsafe set to the
//! fallback (ArduPilot 4.8 parameters, `murmur_decide::engage::preload_for`), anything that
//! would override it cleared, its timer no longer than the approval's link-gap limit. This
//! module does it on the vehicle's link, as a GCS does: read every parameter the preload
//! touches (the platform's own values, kept to put back: [`start_read`]), and once they are
//! in the log as the write-ahead (`engage.preload` intent), set the preload's values (the
//! timer only if the platform's own is longer) and read each back ([`start_write`]). A bridge
//! that stops between the write and its report still finds the values owed in its log. The engine logs the
//! result (`engage.preload`) and decides from the read-back: the intercept goes out only once
//! it confirms, nothing is released before, and one that does not confirm stops the
//! engagement with its fallback. Near the release [`start_verify`] reads it again (the
//! release needs a confirmed read-back at most 10 s old). When no engagement holds the
//! platform any more, [`start_restore`] puts its own values back and reads them back,
//! retried until the vehicle answers.
//!
//! The engagement loop (`super::engage`) runs at most one of these jobs at a time on a
//! platform's link, so a restore can never interleave with a newer engagement's set.
//!
//! Rules 30 (docs/NODE-LOSS.md section 7, the owner's decision D4): the vehicle's own lost-link
//! plan, the vehicle fact `lost_link`, is read from it with the same parameter reads, as a
//! GCS reads its vehicles' parameters: at first contact and after each reboot
//! ([`read_lost_link`]), its firmware's GCS failsafe parameters ([`lost_link_plan`] says which
//! and what they make it do). Only where the link cannot carry a read (an AUV under water on its
//! acoustic modem) is it configured, from the parameters Murmur gives that firmware and the
//! link profile's timer ([`configured_params`]); the fact says which (`source`). An
//! engagement's jobs go first on the link: the read waits while one runs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use murmur_decide::{AutopilotKind, LostLinkAction, LostLinkPreload};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::{Instant, timeout_at};

use crate::mav::{self, Kind, MavMessage, apm};
use crate::vehicle::{LostLinkParams, VehicleCell};
use crate::world::World;

/// Tries per parameter read or write before the job gives up (as the task layer's ACK
/// retries; each waits the link's measured round trip, backing off).
const ATTEMPTS: u32 = 4;

/// What a set or restore job found: the platform's own values (set: read first), what it
/// reported after, and what went wrong on the way.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobResult {
    pub before: BTreeMap<String, f64>,
    pub readback: BTreeMap<String, f64>,
    pub error: Option<String>,
}

/// A job on a vehicle's link; its result once it has finished. It always finishes (each
/// read and write is bounded by its attempts), so an engagement that ended meanwhile waits
/// for its report before its restore.
pub struct Job {
    result: Arc<Mutex<Option<JobResult>>>,
}

impl Job {
    /// The result, once (None while it runs, and after it was taken).
    pub fn take(&self) -> Option<JobResult> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn spawn(f: impl std::future::Future<Output = JobResult> + Send + 'static) -> Job {
        let result = Arc::new(Mutex::new(None));
        let out = result.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    let r = f.await;
                    *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                });
            }
            Err(_) => {
                *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(JobResult {
                    error: Some("no runtime to talk to the vehicle on".into()),
                    ..JobResult::default()
                });
            }
        }
        Job { result }
    }
}

/// Read the preload's parameters, set them, read them back. `known`: the platform's own
/// values still owed back to it from an earlier preload (the vehicle may still hold that
/// preload): those are its own, and are not read again (what it holds now is not its own).
pub fn start_read(
    cell: Arc<VehicleCell>,
    preload: LostLinkPreload,
    known: BTreeMap<String, f64>,
) -> Job {
    let job = cell.param_job();
    Job::spawn(async move {
        let _job = job;
        read_own(&cell, &preload, &known).await
    })
}

/// Write the preload (`before`: the platform's own values the write-ahead logged) and read it
/// back. Called only once the write-ahead is in the log.
pub fn start_write(cell: Arc<VehicleCell>, preload: LostLinkPreload, before: BTreeMap<String, f64>) -> Job {
    let job = cell.param_job();
    Job::spawn(async move {
        let _job = job;
        write_preload(&cell, &preload, before).await
    })
}

/// Read the preload's parameters again (near the release: is it still in force?).
pub fn start_verify(cell: Arc<VehicleCell>, preload: LostLinkPreload) -> Job {
    let job = cell.param_job();
    Job::spawn(async move {
        let _job = job;
        let mut out = JobResult::default();
        for name in preload.names() {
            match read(&cell, name).await {
                Ok(v) => {
                    out.readback.insert(name.to_string(), v);
                }
                Err(e) if e.starts_with(MISSING) && preload.clear_bits.iter().any(|b| b.name == name) => {}
                Err(e) => {
                    out.error = Some(e);
                    break;
                }
            }
        }
        out
    })
}

/// Put `values` back on the vehicle and read them back.
pub fn start_restore(cell: Arc<VehicleCell>, values: BTreeMap<String, f64>) -> Job {
    let job = cell.param_job();
    Job::spawn(async move {
        let _job = job;
        let mut out = JobResult::default();
        for (name, v) in &values {
            if let Err(e) = write(&cell, name, *v).await {
                out.error = Some(e);
                break;
            }
        }
        if out.error.is_none() {
            for name in values.keys() {
                match read(&cell, name).await {
                    Ok(v) => {
                        out.readback.insert(name.clone(), v);
                    }
                    Err(e) => {
                        out.error = Some(e);
                        break;
                    }
                }
            }
        }
        out
    })
}

async fn read_own(cell: &VehicleCell, preload: &LostLinkPreload, known: &BTreeMap<String, f64>) -> JobResult {
    let mut out = JobResult::default();
    // the platform's own values first: what a restore puts back (those still owed from an
    // earlier preload as they were, never read off a platform that may hold it)
    for name in preload.names() {
        if let Some(v) = known.get(name) {
            out.before.insert(name.to_string(), *v);
            continue;
        }
        match read(cell, name).await {
            Ok(v) => {
                out.before.insert(name.to_string(), v);
            }
            // a bitmask the firmware does not have is clear (nothing to override the action)
            Err(e) if e.starts_with(MISSING) && preload.clear_bits.iter().any(|b| b.name == name) => {}
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        }
    }
    out
}

async fn write_preload(cell: &VehicleCell, preload: &LostLinkPreload, before: BTreeMap<String, f64>) -> JobResult {
    let mut out = JobResult {
        before,
        ..JobResult::default()
    };
    // the values: the failsafe's action exactly, the bits cleared, the timer shortened only
    let mut want: Vec<(String, f64)> = preload
        .set
        .iter()
        .map(|p| (p.name.clone(), p.value))
        .collect();
    for b in &preload.clear_bits {
        if let Some(v) = out.before.get(&b.name) {
            let cleared = (v.round() as i64) & !i64::from(b.clear);
            want.push((b.name.clone(), cleared as f64));
        }
    }
    let timer = out
        .before
        .get(&preload.timer)
        .copied()
        .unwrap_or(preload.timer_max_s)
        .min(preload.timer_max_s);
    want.push((preload.timer.clone(), timer));
    for (name, v) in &want {
        if let Err(e) = write(cell, name, *v).await {
            out.error = Some(e);
            return out;
        }
    }
    // and read back what the vehicle holds now (an echo is not the stored value)
    for (name, _) in &want {
        match read(cell, name).await {
            Ok(v) => {
                out.readback.insert(name.clone(), v);
            }
            Err(e) => {
                out.error = Some(e);
                return out;
            }
        }
    }
    out
}

/// How an error for a parameter the vehicle does not have begins.
const MISSING: &str = "no such parameter";

/// PARAM_REQUEST_READ by name until the vehicle answers with its value (PARAM_VALUE) or says it
/// has no such parameter (PARAM_ERROR), `ATTEMPTS` times over the link's timing.
async fn read(cell: &VehicleCell, name: &str) -> Result<f64, String> {
    let mut rx = cell.subscribe();
    for attempt in 0..ATTEMPTS {
        cell.send(&MavMessage::PARAM_REQUEST_READ(apm::PARAM_REQUEST_READ_DATA {
            param_index: -1,
            target_system: cell.sysid,
            target_component: mav::AUTOPILOT_COMPID,
            param_id: mav::char_array(name),
        }));
        match answer(&mut rx, name, cell.link_timing().ack_timeout(attempt)).await {
            Some(Ok(v)) => return Ok(v),
            Some(Err(e)) => return Err(e),
            None => {}
        }
    }
    Err(format!("no answer from the vehicle to PARAM_REQUEST_READ {name} after {ATTEMPTS} attempts"))
}

/// PARAM_SET until the vehicle echoes the parameter (PARAM_VALUE), `ATTEMPTS` times.
async fn write(cell: &VehicleCell, name: &str, value: f64) -> Result<f64, String> {
    let mut rx = cell.subscribe();
    for attempt in 0..ATTEMPTS {
        cell.send(&MavMessage::PARAM_SET(apm::PARAM_SET_DATA {
            param_value: value as f32,
            target_system: cell.sysid,
            target_component: mav::AUTOPILOT_COMPID,
            param_id: mav::char_array(name),
            param_type: apm::MavParamType::MAV_PARAM_TYPE_REAL32,
        }));
        match answer(&mut rx, name, cell.link_timing().ack_timeout(attempt)).await {
            Some(Ok(v)) => return Ok(v),
            Some(Err(e)) => return Err(e),
            None => {}
        }
    }
    Err(format!("no answer from the vehicle to PARAM_SET {name} after {ATTEMPTS} attempts"))
}

/// The vehicle's answer about `name` within `wait`: its value, an error it reported, or none.
async fn answer(
    rx: &mut broadcast::Receiver<Arc<MavMessage>>,
    name: &str,
    wait: Duration,
) -> Option<Result<f64, String>> {
    let deadline = Instant::now() + wait;
    loop {
        match timeout_at(deadline, rx.recv()).await {
            Err(_) | Ok(Err(RecvError::Closed)) => return None,
            Ok(Err(RecvError::Lagged(_))) => {}
            Ok(Ok(m)) => match &*m {
                MavMessage::PARAM_VALUE(p) if mav::chars_to_string(&p.param_id) == name => {
                    return Some(Ok(f64::from(p.param_value)));
                }
                MavMessage::PARAM_ERROR(e) if mav::chars_to_string(&e.param_id) == name => {
                    return Some(Err(
                        if e.error == apm::MavParamError::MAV_PARAM_ERROR_DOES_NOT_EXIST {
                            format!("{MISSING} {name} on the vehicle")
                        } else {
                            format!("the vehicle refused {name}: {:?}", e.error)
                        },
                    ));
                }
                _ => {}
            },
        }
    }
}

// ---- Rules 30: the vehicle's own lost-link failsafe, read from it ----

/// Rules 30: lost-link reads under way at once, fleet-wide. Saturation's 3,602 vehicles meet
/// the bridge together at its start; a read is a few parameters one after another, a few round
/// trips (GUESS: enough to read Saturation's fleet in under a minute on its radios).
pub const LOST_LINK_READS_MAX: usize = 128;
/// Rules 30: seconds before a read that did not finish is tried again; each later try waits
/// twice as long, up to [`LOST_LINK_RETRY_MAX_S`] (as an engagement's restore).
pub const LOST_LINK_RETRY_S: f64 = 15.0;
pub const LOST_LINK_RETRY_MAX_S: f64 = 300.0;
/// How often a read waiting for an engagement's job on the same link looks again, s.
const LOST_LINK_YIELD_S: f64 = 0.5;

/// The firmware families whose GCS failsafes differ (ArduPilot 4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Firmware {
    Plane,
    Copter,
    Rover,
    Sub,
    Blimp,
    Tracker,
}

impl Firmware {
    /// By the autopilot a heartbeat reports (a QuadPlane is ArduPlane, a boat and a UGV are
    /// ArduRover).
    fn of_kind(kind: Kind) -> Option<Firmware> {
        Some(match kind {
            Kind::Plane | Kind::Vtol => Firmware::Plane,
            Kind::Copter => Firmware::Copter,
            Kind::Boat | Kind::Ugv => Firmware::Rover,
            Kind::Sub => Firmware::Sub,
            Kind::Blimp => Firmware::Blimp,
            Kind::Tracker => Firmware::Tracker,
            Kind::Unknown => return None,
        })
    }

    /// By the vehicle fact `autopilot` (a drifting buoy imitates ArduRover).
    fn of_autopilot(ap: AutopilotKind) -> Firmware {
        match ap {
            AutopilotKind::Plane | AutopilotKind::Vtol => Firmware::Plane,
            AutopilotKind::Copter => Firmware::Copter,
            AutopilotKind::Boat | AutopilotKind::Ugv | AutopilotKind::Drifter => Firmware::Rover,
            AutopilotKind::Sub => Firmware::Sub,
            AutopilotKind::Blimp => Firmware::Blimp,
            AutopilotKind::Tracker => Firmware::Tracker,
        }
    }

    /// The parameters that say what its GCS failsafe does, which the read asks for (names
    /// from `crate::vehicle::LOST_LINK_PARAMS`). AntennaTracker has no GCS failsafe: it is
    /// asked for Copter's switch, and its answer that it has none is the read.
    fn names(self) -> &'static [&'static str] {
        match self {
            Firmware::Plane => &["FS_GCS_ENABL", "FS_LONG_ACTN", "FS_LONG_TIMEOUT", "Q_OPTIONS"],
            Firmware::Copter => &["FS_GCS_ENABLE", "FS_OPTIONS", "FS_GCS_TIMEOUT"],
            Firmware::Rover => &["FS_GCS_ENABLE", "FS_ACTION", "FS_OPTIONS", "FS_GCS_TIMEOUT", "FS_TIMEOUT"],
            Firmware::Sub | Firmware::Blimp => &["FS_GCS_ENABLE", "FS_GCS_TIMEOUT"],
            Firmware::Tracker => &["FS_GCS_ENABLE"],
        }
    }
}

/// Rules 30: the GCS failsafe parameters of an autopilot's firmware, which [`lost_link_plan`]
/// reads.
pub fn lost_link_names(autopilot: AutopilotKind) -> &'static [&'static str] {
    Firmware::of_autopilot(autopilot).names()
}

/// Rules 30: what a vehicle's own GCS failsafe does if its link is lost now, and after how
/// long: the fact [`murmur_decide::LostLinkPlan`]'s action and timer. `param(name)` is a
/// parameter as the vehicle reported it (`Some(Some(v))`), or that it has no such parameter
/// (`Some(None)`), or nothing yet (`None`); None until every parameter of its firmware was
/// reported. The action depends on the mode and armed state it was last heard in, as each
/// firmware decides it (ArduPilot 4.8; the stand-ins' `failsafe.rs` acts the same):
///
/// - A failsafe switched off (or, ArduSub, set to warn only) is `off`, and so is any vehicle
///   last heard disarmed: Copter and Sub act only armed, and Plane's and Rover's mode switch
///   does not move a disarmed vehicle. A drifting buoy is `off` too: it cannot drive (its
///   stand-in takes no mode but HOLD).
/// - ArduPlane (FS_GCS_ENABL, FS_LONG_ACTN 0 continue, 1 RTL, 2 glide, 3 parachute, 4 AUTO,
///   5 AUTOLAND, FS_LONG_TIMEOUT; events.cpp `failsafe_long_on_event`): in AUTO, GUIDED or
///   AVOID_ADSB `continue` for 0, else the action; in RTL `continue` (it carries on home), but
///   `rtl` for AUTOLAND; landing or initialising, `continue`; in a VTOL mode its Q_OPTIONS
///   decide (FS_RTL or FS_QRTL: `rtl`; else QLAND, `land`); in any other mode `rtl` for 0 and 1.
///   The parachute is `land` (it comes down where it is), glide `glide`, AUTO `continue` (it
///   flies its mission), AUTOLAND `rtl` (home, then down).
/// - ArduCopter (FS_GCS_ENABLE 1 and 2 RTL, 3 SmartRTL or RTL, 4 SmartRTL or land, 5 land, 6
///   AUTO's landing sequence or RTL, 7 brake or land; FS_OPTIONS; FS_GCS_TIMEOUT; events.cpp
///   `failsafe_gcs_on_event`): landing with FS_OPTIONS bit 3 `land`; in AUTO with bit 1, or in
///   a mode a pilot flies with bit 4, `continue`; else the action (an unknown value RTL).
/// - ArduRover (FS_GCS_ENABLE, 2 continuing a mission in AUTO; FS_ACTION 0 none, 1 RTL, 2 hold,
///   3 SmartRTL or RTL, 4 SmartRTL or hold, 5 disarm, 6 loiter; FS_OPTIONS bit 0 acts in HOLD;
///   failsafe.cpp `failsafe_trigger`): in RTL, or in HOLD without bit 0, `continue`; its timer
///   is FS_GCS_TIMEOUT and then FS_TIMEOUT.
/// - ArduSub (FS_GCS_ENABLE 1 warn, 2 disarm, 3 depth hold, 4 surface; FS_GCS_TIMEOUT;
///   failsafe.cpp `failsafe_gcs_check`).
/// - Blimp: any FS_GCS_ENABLE but 0 disarms (4.8 has no `failsafe_gcs_on_event`: its
///   documented land does not exist).
/// - AntennaTracker: none.
///
/// A switch the firmware does not have means it has no such failsafe (`off`); an option
/// bitmask it does not have is clear; an action parameter it does not have leaves the plan
/// unknown (None).
pub fn lost_link_plan(
    autopilot: AutopilotKind,
    mode: Option<&str>,
    armed: bool,
    param: impl Fn(&str) -> Option<Option<f64>>,
) -> Option<(LostLinkAction, Option<f64>)> {
    use LostLinkAction::Off;
    let fw = Firmware::of_autopilot(autopilot);
    let mut values: BTreeMap<&str, Option<f64>> = BTreeMap::new();
    for name in fw.names() {
        values.insert(name, param(name)?);
    }
    let value = |n: &str| values.get(n).copied().flatten();
    let int = |n: &str| value(n).map(|v| v.round() as i64);
    let bits = |n: &str| int(n).unwrap_or(0);
    let mode = mode.unwrap_or("");
    let switch = match fw {
        Firmware::Plane => int("FS_GCS_ENABL"),
        Firmware::Tracker => None,
        _ => int("FS_GCS_ENABLE"),
    };
    let off = Some((Off, None));
    let Some(switch) = switch else { return off };
    if switch == 0 || !armed || autopilot == AutopilotKind::Drifter {
        return off;
    }
    let action = match fw {
        Firmware::Plane => plane_action(mode, int("FS_LONG_ACTN")?, bits("Q_OPTIONS")),
        Firmware::Copter => copter_action(mode, switch, bits("FS_OPTIONS")),
        Firmware::Rover => rover_action(mode, switch, int("FS_ACTION")?, bits("FS_OPTIONS")),
        Firmware::Sub => sub_action(switch),
        Firmware::Blimp => LostLinkAction::Disarm,
        Firmware::Tracker => Off,
    };
    if action == Off {
        return off;
    }
    let timer = match fw {
        Firmware::Plane => value("FS_LONG_TIMEOUT"),
        Firmware::Rover => value("FS_GCS_TIMEOUT").map(|t| t + value("FS_TIMEOUT").unwrap_or(0.0)),
        _ => value("FS_GCS_TIMEOUT"),
    };
    Some((action, timer))
}

/// ArduPlane 4.8's long failsafe (`failsafe_long_on_event`), the switch on.
fn plane_action(mode: &str, actn: i64, q_options: i64) -> LostLinkAction {
    use LostLinkAction::{Continue, Glide, Land, Rtl};
    // QuadPlane::Option FS_QRTL (bit 5) and FS_RTL (bit 20)
    const FS_QRTL: i64 = 1 << 5;
    const FS_RTL: i64 = 1 << 20;
    let act = |otherwise| match actn {
        2 => Glide,
        3 => Land,
        4 => Continue,
        1 | 5 => Rtl,
        _ => otherwise,
    };
    match mode {
        "AUTO" | "GUIDED" | "AVOID_ADSB" => act(Continue),
        "RTL" if actn == 5 => Rtl,
        "RTL" | "QLAND" | "QRTL" | "LOITERALTQLAND" | "INITIALISING" | "AUTOLAND" => Continue,
        "QSTABILIZE" | "QHOVER" | "QLOITER" | "QACRO" | "QAUTOTUNE" => {
            if q_options & (FS_RTL | FS_QRTL) != 0 {
                Rtl
            } else {
                Land
            }
        }
        _ => act(Rtl),
    }
}

/// ArduCopter 4.8's GCS failsafe (`failsafe_gcs_on_event`), armed, the switch on.
fn copter_action(mode: &str, switch: i64, options: i64) -> LostLinkAction {
    use LostLinkAction::{Brake, Continue, Land, Rtl, SmartRtl};
    // FailsafeOption GCS_CONTINUE_IF_AUTO (bit 1), CONTINUE_IF_LANDING (bit 3),
    // GCS_CONTINUE_IF_PILOT_CONTROL (bit 4)
    const IF_AUTO: i64 = 1 << 1;
    const IF_LANDING: i64 = 1 << 3;
    const IF_PILOT: i64 = 1 << 4;
    // the modes whose `is_autopilot()` is true (ArduCopter/mode.h)
    let autopilot = matches!(
        mode,
        "AUTO" | "AUTO_RTL" | "BRAKE" | "CIRCLE" | "GUIDED" | "GUIDED_NOGPS" | "LAND" | "RTL"
            | "SMART_RTL" | "AVOID_ADSB" | "FOLLOW" | "AUTOROTATE"
    );
    if mode == "LAND" && options & IF_LANDING != 0 {
        return Land;
    }
    if (mode == "AUTO" && options & IF_AUTO != 0) || (options & IF_PILOT != 0 && !autopilot) {
        return Continue;
    }
    match switch {
        3 | 4 => SmartRtl,
        5 => Land,
        7 => Brake,
        _ => Rtl,
    }
}

/// ArduRover 4.8's failsafe (`failsafe_trigger`), the switch on.
fn rover_action(mode: &str, switch: i64, action: i64, options: i64) -> LostLinkAction {
    use LostLinkAction::{Continue, Disarm, Hold, Loiter, Off, Rtl, SmartRtl};
    // Failsafe_Option_Active_In_Hold (bit 0)
    const IN_HOLD: i64 = 1;
    if mode == "RTL" || (mode == "HOLD" && options & IN_HOLD == 0) || (mode == "AUTO" && switch == 2) {
        return Continue;
    }
    match action {
        1 => Rtl,
        2 => Hold,
        3 | 4 => SmartRtl,
        5 => Disarm,
        6 => Loiter,
        _ => Off,
    }
}

/// ArduSub 4.8's GCS failsafe (`failsafe_gcs_check`), armed.
fn sub_action(switch: i64) -> LostLinkAction {
    match switch {
        2 => LostLinkAction::Disarm,
        3 => LostLinkAction::DepthHold,
        4 => LostLinkAction::Surface,
        _ => LostLinkAction::Off,
    }
}

/// Rules 30: the GCS failsafe parameters Murmur gives a firmware, as `lost_link_plan` reads
/// them: SITL's `sim/params/*.parm` and the stand-ins' own table
/// (`crates/murmur-standins/src/params.rs`), and the timer `sim/swarm.py` writes from the
/// vehicle's link profile (`lost_link_s`; without one, the firmware's 5 s). RTL for planes,
/// copters and rovers; an AUV (`acoustic`) surfaces after 120 s, a tethered ROV disarms
/// (ArduSub's default); the airship's is off (Blimp 4.8's would only disarm it); a tracker has
/// none. None for an autopilot the bridge does not know.
pub fn configured_params(kind: Kind, acoustic: bool, lost_link_s: Option<f64>) -> Option<BTreeMap<&'static str, Option<f64>>> {
    let fw = Firmware::of_kind(kind)?;
    let timer = |default: f64| Some(lost_link_s.unwrap_or(default));
    let values: Vec<(&'static str, Option<f64>)> = match fw {
        Firmware::Plane => vec![
            ("FS_GCS_ENABL", Some(1.0)),
            ("FS_LONG_ACTN", Some(1.0)),
            ("FS_LONG_TIMEOUT", timer(5.0)),
            // the QuadPlane's options (none set); a plane has no Q_ parameters
            ("Q_OPTIONS", (kind == Kind::Vtol).then_some(0.0)),
        ],
        Firmware::Copter => vec![
            ("FS_GCS_ENABLE", Some(1.0)),
            ("FS_OPTIONS", Some(16.0)),
            ("FS_GCS_TIMEOUT", timer(5.0)),
        ],
        Firmware::Rover => vec![
            ("FS_GCS_ENABLE", Some(1.0)),
            ("FS_ACTION", Some(1.0)),
            ("FS_OPTIONS", Some(0.0)),
            ("FS_GCS_TIMEOUT", timer(5.0)),
            ("FS_TIMEOUT", Some(1.5)),
        ],
        Firmware::Sub => vec![
            ("FS_GCS_ENABLE", Some(if acoustic { 4.0 } else { 2.0 })),
            ("FS_GCS_TIMEOUT", timer(if acoustic { 120.0 } else { 5.0 })),
        ],
        Firmware::Blimp => vec![("FS_GCS_ENABLE", Some(0.0)), ("FS_GCS_TIMEOUT", timer(5.0))],
        Firmware::Tracker => vec![("FS_GCS_ENABLE", None)],
    };
    Some(values.into_iter().collect())
}

/// Rules 30: what a vehicle's lost-link read stands at, for [`read_lost_link`].
struct ReadState {
    /// Its firmware's parameters not yet reported in this boot.
    wanted: Vec<&'static str>,
    /// Every one of them reported in some boot.
    read_once: bool,
    configured: bool,
    next_try: f64,
}

/// Rules 30 (decision D4): each vehicle's own lost-link failsafe, read from its parameters as a
/// GCS reads them: once its autopilot is known (its first heartbeat) and again after each
/// reboot the bridge sees (a report from before it no longer counts as this boot's); retried
/// with backoff while a read does not finish; never into a silent link; at most
/// [`LOST_LINK_READS_MAX`] at once. Where the link cannot carry a read (the telemetry plan's
/// `silent` tier: an AUV under water on its acoustic modem) and none has finished, the
/// parameters Murmur configures stand in ([`LostLinkParams::configured`]) until one does. The
/// decision loop calls it once a tick, under rules 30 only: a run under older rules asks its
/// vehicles for nothing it did not ask before. A vehicle read in this boot costs one look
/// under its lock.
pub(super) fn read_lost_link(world: &World) {
    let now = crate::clock::mono();
    let mut reading = 0usize;
    let mut due: Vec<(Arc<VehicleCell>, Vec<&'static str>)> = Vec::new();
    for cell in world.vehicles() {
        let state = cell.read(|v| {
            let fw = v.kind.and_then(Firmware::of_kind)?;
            let p = &v.lost_link;
            if p.reading {
                reading += 1;
                return None;
            }
            let wanted: Vec<&'static str> = fw
                .names()
                .iter()
                .filter(|n| p.reported.get(*n).is_none_or(|r| r.epoch != v.boot_epoch))
                .copied()
                .collect();
            (!wanted.is_empty()).then(|| ReadState {
                wanted,
                read_once: fw.names().iter().all(|n| p.reported.contains_key(n)),
                configured: p.configured.is_some(),
                next_try: p.next_try,
            })
        });
        let Some(state) = state else { continue };
        match cell.link_snapshot(now).telemetry.map(|p| p.tier) {
            // no plan chosen yet: the next tick knows what carries the link
            None => {}
            Some(crate::telemetry::Tier::Silent) => {
                if !state.read_once && !state.configured {
                    configure(world, &cell);
                }
            }
            Some(_) if now < state.next_try || crate::tasks::link_silent(&cell) => {}
            Some(_) => due.push((cell, state.wanted)),
        }
    }
    for (cell, names) in due.into_iter().take(LOST_LINK_READS_MAX.saturating_sub(reading)) {
        start_lost_link_read(cell, names);
    }
}

/// Rules 30: the parameters Murmur gives this vehicle's firmware and link profile, as its plan
/// where the link cannot carry a read (estimate.rs reads the same timer for its failsafe
/// branch).
fn configure(world: &World, cell: &VehicleCell) {
    let (kind, identity) = cell.read(|v| (v.kind, v.identity.clone()));
    let Some(kind) = kind else { return };
    let ident = identity.as_deref();
    let platform = ident.and_then(|i| i.str("platform"));
    let limits = crate::estimate::platform_limits(world, platform, Some(kind));
    let link_class = ident.and_then(|i| i.str("link")).or(limits.link.as_deref());
    let space_class = ident.and_then(|i| i.str("space_class")).or(limits.space_class.as_deref());
    let timer = crate::estimate::lost_link_timer(world, platform, link_class, space_class).map(|(_, t)| t);
    let Some(params) = configured_params(kind, link_class == Some("acoustic"), timer) else {
        return;
    };
    cell.write(|v| {
        if v.lost_link.configured.is_none() {
            v.lost_link.configured = Some((params, crate::clock::wall()));
        }
    });
}

/// Clears a vehicle's lost-link `reading` when its read ends, however it ends (a runtime that
/// stops drops it unfinished).
struct Reading(Arc<VehicleCell>);

impl Drop for Reading {
    fn drop(&mut self) {
        self.0.write(|v| v.lost_link.reading = false);
    }
}

/// Rules 30: read `names` on the vehicle's link, one after another, each [`ATTEMPTS`] times
/// over the link's timing (as the preload's reads), after any engagement job running on it. The
/// answers land in the vehicle's state as every report of them does (`Vehicle::apply`); this
/// says only whether every one was answered, a value or that it has no such parameter.
fn start_lost_link_read(cell: Arc<VehicleCell>, names: Vec<&'static str>) {
    cell.write(|v| v.lost_link.reading = true);
    let guard = Reading(cell.clone());
    let on = cell.clone();
    let job = async move {
        let _guard = guard;
        let cell = on;
        let mut finished = true;
        for name in names {
            while cell.param_job_running() {
                tokio::time::sleep(Duration::from_secs_f64(LOST_LINK_YIELD_S)).await;
            }
            match read(&cell, name).await {
                Ok(_) => {}
                Err(e) if e.starts_with(MISSING) => {}
                Err(_) => {
                    finished = false;
                    break;
                }
            }
        }
        cell.write(|v| read_ended(&mut v.lost_link, finished, crate::clock::mono()));
    };
    match tokio::runtime::Handle::try_current() {
        Ok(rt) => {
            rt.spawn(job);
        }
        Err(_) => {
            drop(job);
            cell.write(|v| read_ended(&mut v.lost_link, false, crate::clock::mono()));
        }
    }
}

/// A read ended: finished, or tried again after a backoff.
fn read_ended(p: &mut LostLinkParams, finished: bool, now: f64) {
    if finished {
        p.failures = 0;
        p.next_try = now;
    } else {
        p.failures += 1;
        let wait = LOST_LINK_RETRY_S * 2f64.powi(p.failures.saturating_sub(1).min(8) as i32);
        p.next_try = now + wait.min(LOST_LINK_RETRY_MAX_S);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LostLinkAction::*;

    /// `lost_link_plan` on these parameters (the rest of the firmware's: not reported).
    fn plan(ap: AutopilotKind, mode: &str, armed: bool, params: &[(&str, Option<f64>)]) -> Option<(LostLinkAction, Option<f64>)> {
        let p: BTreeMap<&str, Option<f64>> = params.iter().copied().collect();
        lost_link_plan(ap, Some(mode), armed, |n| p.get(n).copied())
    }

    const PLANE: &[(&str, Option<f64>)] = &[
        ("FS_GCS_ENABL", Some(1.0)),
        ("FS_LONG_ACTN", Some(1.0)),
        ("FS_LONG_TIMEOUT", Some(20.0)),
        ("Q_OPTIONS", None),
    ];

    fn with(base: &[(&'static str, Option<f64>)], name: &'static str, v: Option<f64>) -> Vec<(&'static str, Option<f64>)> {
        let mut out: Vec<_> = base.iter().copied().filter(|(n, _)| *n != name).collect();
        out.push((name, v));
        out
    }

    #[test]
    fn nothing_until_every_parameter_of_the_firmware_is_reported() {
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", true, &PLANE[..3]), None);
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", true, PLANE), Some((Rtl, Some(20.0))));
        // a switch the firmware does not have: no such failsafe
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", true, &with(PLANE, "FS_GCS_ENABL", None)), Some((Off, None)));
        // an action it does not have: not known
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", true, &with(PLANE, "FS_LONG_ACTN", None)), None);
        // a timer it does not have: the action, untimed
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", true, &with(PLANE, "FS_LONG_TIMEOUT", None)), Some((Rtl, None)));
    }

    #[test]
    fn off_switched_off_disarmed_or_drifting() {
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", true, &with(PLANE, "FS_GCS_ENABL", Some(0.0))), Some((Off, None)));
        assert_eq!(plan(AutopilotKind::Plane, "LOITER", false, PLANE), Some((Off, None)));
        let rover = [
            ("FS_GCS_ENABLE", Some(1.0)),
            ("FS_ACTION", Some(1.0)),
            ("FS_OPTIONS", Some(0.0)),
            ("FS_GCS_TIMEOUT", Some(120.0)),
            ("FS_TIMEOUT", Some(1.5)),
        ];
        assert_eq!(plan(AutopilotKind::Boat, "LOITER", true, &rover), Some((Rtl, Some(121.5))));
        assert_eq!(plan(AutopilotKind::Drifter, "LOITER", true, &rover), Some((Off, None)));
        assert_eq!(plan(AutopilotKind::Tracker, "STOP", true, &[("FS_GCS_ENABLE", None)]), Some((Off, None)));
    }

    /// ArduPlane's long failsafe: its action by mode group.
    #[test]
    fn plane_by_mode() {
        let actn = |a: f64, mode: &str| plan(AutopilotKind::Plane, mode, true, &with(PLANE, "FS_LONG_ACTN", Some(a))).map(|p| p.0);
        // continue: carries on in AUTO, RTLs anywhere else
        assert_eq!(actn(0.0, "AUTO"), Some(Continue));
        assert_eq!(actn(0.0, "GUIDED"), Some(Continue));
        assert_eq!(actn(0.0, "LOITER"), Some(Rtl));
        assert_eq!(actn(0.0, "FBWA"), Some(Rtl));
        assert_eq!(actn(1.0, "AUTO"), Some(Rtl));
        assert_eq!(actn(2.0, "AUTO"), Some(Glide));
        assert_eq!(actn(3.0, "LOITER"), Some(Land));
        assert_eq!(actn(4.0, "LOITER"), Some(Continue));
        assert_eq!(actn(5.0, "AUTO"), Some(Rtl));
        // in RTL it carries on home (AUTOLAND: home, then down); landing, carries on
        assert_eq!(actn(1.0, "RTL"), Some(Continue));
        assert_eq!(actn(5.0, "RTL"), Some(Rtl));
        assert_eq!(actn(1.0, "AUTOLAND"), Some(Continue));
        assert_eq!(actn(1.0, "QLAND"), Some(Continue));
        // a QuadPlane hovering: QLAND unless its options say RTL or QRTL
        assert_eq!(actn(1.0, "QHOVER"), Some(Land));
        let q = |bits: f64| plan(AutopilotKind::Vtol, "QLOITER", true, &with(PLANE, "Q_OPTIONS", Some(bits))).map(|p| p.0);
        assert_eq!(q(0.0), Some(Land));
        assert_eq!(q(32.0), Some(Rtl));
        assert_eq!(q(f64::from(1u32 << 20)), Some(Rtl));
    }

    /// ArduCopter's GCS failsafe: FS_GCS_ENABLE's action, and FS_OPTIONS' exceptions.
    #[test]
    fn copter_by_switch_and_options() {
        let copter = |switch: f64, options: f64, mode: &str| {
            plan(
                AutopilotKind::Copter,
                mode,
                true,
                &[("FS_GCS_ENABLE", Some(switch)), ("FS_OPTIONS", Some(options)), ("FS_GCS_TIMEOUT", Some(10.0))],
            )
        };
        assert_eq!(copter(1.0, 16.0, "BRAKE"), Some((Rtl, Some(10.0))));
        assert_eq!(copter(2.0, 0.0, "AUTO").map(|p| p.0), Some(Rtl));
        assert_eq!(copter(3.0, 0.0, "AUTO").map(|p| p.0), Some(SmartRtl));
        assert_eq!(copter(4.0, 0.0, "AUTO").map(|p| p.0), Some(SmartRtl));
        assert_eq!(copter(5.0, 0.0, "AUTO").map(|p| p.0), Some(Land));
        assert_eq!(copter(6.0, 0.0, "AUTO").map(|p| p.0), Some(Rtl));
        assert_eq!(copter(7.0, 0.0, "AUTO").map(|p| p.0), Some(Brake));
        assert_eq!(copter(9.0, 0.0, "AUTO").map(|p| p.0), Some(Rtl));
        // bit 1: a mission carries on; bit 4: a mode a pilot flies carries on, not one the
        // autopilot flies; bit 3: a landing carries on
        assert_eq!(copter(1.0, 2.0, "AUTO").map(|p| p.0), Some(Continue));
        assert_eq!(copter(1.0, 2.0, "GUIDED").map(|p| p.0), Some(Rtl));
        assert_eq!(copter(1.0, 16.0, "LOITER").map(|p| p.0), Some(Continue));
        assert_eq!(copter(1.0, 16.0, "GUIDED").map(|p| p.0), Some(Rtl));
        assert_eq!(copter(1.0, 8.0, "LAND").map(|p| p.0), Some(Land));
        assert_eq!(copter(1.0, 0.0, "LAND").map(|p| p.0), Some(Rtl));
        // disarmed it does nothing
        assert_eq!(
            plan(AutopilotKind::Copter, "AUTO", false, &[("FS_GCS_ENABLE", Some(1.0)), ("FS_OPTIONS", Some(0.0)), ("FS_GCS_TIMEOUT", Some(10.0))]),
            Some((Off, None))
        );
    }

    /// ArduRover's failsafe: FS_ACTION, the modes it leaves alone, its second timer.
    #[test]
    fn rover_by_action_and_mode() {
        let rover = |switch: f64, action: f64, options: f64, mode: &str| {
            plan(
                AutopilotKind::Ugv,
                mode,
                true,
                &[
                    ("FS_GCS_ENABLE", Some(switch)),
                    ("FS_ACTION", Some(action)),
                    ("FS_OPTIONS", Some(options)),
                    ("FS_GCS_TIMEOUT", Some(10.0)),
                    ("FS_TIMEOUT", Some(1.5)),
                ],
            )
        };
        assert_eq!(rover(1.0, 1.0, 0.0, "GUIDED"), Some((Rtl, Some(11.5))));
        let a = |action: f64| rover(1.0, action, 0.0, "AUTO").map(|p| p.0);
        assert_eq!(a(0.0), Some(Off));
        assert_eq!(a(2.0), Some(Hold));
        assert_eq!(a(3.0), Some(SmartRtl));
        assert_eq!(a(4.0), Some(SmartRtl));
        assert_eq!(a(5.0), Some(Disarm));
        assert_eq!(a(6.0), Some(Loiter));
        // RTL and HOLD are left alone (HOLD unless FS_OPTIONS bit 0); 2 carries a mission on
        assert_eq!(rover(1.0, 1.0, 0.0, "RTL").map(|p| p.0), Some(Continue));
        assert_eq!(rover(1.0, 1.0, 0.0, "HOLD").map(|p| p.0), Some(Continue));
        assert_eq!(rover(1.0, 2.0, 1.0, "HOLD").map(|p| p.0), Some(Hold));
        assert_eq!(rover(2.0, 1.0, 0.0, "AUTO").map(|p| p.0), Some(Continue));
        assert_eq!(rover(2.0, 1.0, 0.0, "LOITER").map(|p| p.0), Some(Rtl));
    }

    #[test]
    fn sub_and_blimp() {
        let sub = |switch: f64| plan(AutopilotKind::Sub, "POSHOLD", true, &[("FS_GCS_ENABLE", Some(switch)), ("FS_GCS_TIMEOUT", Some(120.0))]);
        assert_eq!(sub(0.0), Some((Off, None)));
        assert_eq!(sub(1.0), Some((Off, None)));
        assert_eq!(sub(2.0), Some((Disarm, Some(120.0))));
        assert_eq!(sub(3.0), Some((DepthHold, Some(120.0))));
        assert_eq!(sub(4.0), Some((Surface, Some(120.0))));
        let blimp = |switch: f64, armed: bool| plan(AutopilotKind::Blimp, "LOITER", armed, &[("FS_GCS_ENABLE", Some(switch)), ("FS_GCS_TIMEOUT", Some(5.0))]);
        assert_eq!(blimp(0.0, true), Some((Off, None)));
        // 4.8 only disarms, whatever the value says
        assert_eq!(blimp(5.0, true), Some((Disarm, Some(5.0))));
        assert_eq!(blimp(5.0, false), Some((Off, None)));
    }

    #[test]
    fn a_failed_read_backs_off_and_a_finished_one_resets() {
        let mut p = LostLinkParams::default();
        read_ended(&mut p, false, 100.0);
        assert_eq!((p.failures, p.next_try), (1, 100.0 + LOST_LINK_RETRY_S));
        read_ended(&mut p, false, 200.0);
        assert_eq!(p.next_try, 200.0 + 2.0 * LOST_LINK_RETRY_S);
        for _ in 0..10 {
            read_ended(&mut p, false, 300.0);
        }
        assert_eq!(p.next_try, 300.0 + LOST_LINK_RETRY_MAX_S);
        read_ended(&mut p, true, 400.0);
        assert_eq!((p.failures, p.next_try), (0, 400.0));
    }
}
