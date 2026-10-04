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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use murmur_decide::LostLinkPreload;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::{Instant, timeout_at};

use crate::mav::{self, MavMessage, apm};
use crate::vehicle::VehicleCell;

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
    Job::spawn(async move { read_own(&cell, &preload, &known).await })
}

/// Write the preload (`before`: the platform's own values the write-ahead logged) and read it
/// back. Called only once the write-ahead is in the log.
pub fn start_write(cell: Arc<VehicleCell>, preload: LostLinkPreload, before: BTreeMap<String, f64>) -> Job {
    Job::spawn(async move { write_preload(&cell, &preload, before).await })
}

/// Read the preload's parameters again (near the release: is it still in force?).
pub fn start_verify(cell: Arc<VehicleCell>, preload: LostLinkPreload) -> Job {
    Job::spawn(async move {
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
    Job::spawn(async move {
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
