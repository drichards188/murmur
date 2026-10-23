//! The faults a situation's timeline puts on the world (world v3, H4; eval/headless/script.py
//! sends them at their instant), outside the bridge, each the stand-ins' or ArduPilot's own
//! behaviour, so what the C2 sees of one is what it would see live:
//!
//! - **`gnss`**, a jammer or a spoofer: an event of the stand-ins' own GNSS model
//!   (crates/murmur-standins/src/gnss.rs, the `gnss` table of an events file, as
//!   sim/events/golden-gate-gnss.toml gives the live stand-in engine), from now for its duration.
//!   Jamming raises the receiver's noise floor by J/S from free-space loss at L1; below four
//!   tracked satellites the fix is lost, the navigation filter dead-reckons and then has no
//!   position (nav.rs), and each firmware's EKF failsafe acts as its ekf_check.cpp does
//!   (failsafe.rs: Copter LAND, Rover HOLD, a fixed wing flies on). The C2 sees it in
//!   GPS_RAW_INT's fix and EKF_STATUS_REPORT's flags (the bridge's `nav` facts, `gps_glitch`).
//!   Spoofing captures the receiver inside its circle and walks its position off the truth at
//!   `walk_mps` on `bearing_deg`, the fix and the flags still good: the vehicle believes it, and
//!   so does every report the C2 reads (nav.rs: what happens when the capture ends). GNSS
//!   events in the situation's events files reach the stand-ins too, at start (`from_events`),
//!   as `MURMUR_EVENTS` gives them to the live engine.
//! - **`battery`**: `drop_pct` of the pack's capacity gone at once (a damaged cell; the
//!   stand-in's monitor counts used mAh, so the remaining charge it reports drops, and its
//!   BATT_LOW_MAH and BATT_CRT_MAH failsafes act as AP_BattMonitor's check_failsafes would on
//!   that count; a real pack also shows it as a voltage sag, which the stand-in does not model),
//!   or the draw times `rate` from now (a fouled propeller, a failing motor: the same load, more
//!   current, battery.rs's drain scaled and the current the autopilot reads with it).
//! - **`reboot`**: the autopilot restarts (fleet.rs `reboot`): silent for `down_s` (a boot,
//!   default 3 s, an estimate of ArduPilot's on a flight controller), its time since boot from
//!   0, stream rates as its parameters set them (the bridge's telemetry plan is re-sent, as
//!   world.rs does for a rebooted vehicle), failsafes and navigation filter new; its parameters,
//!   stored mission and pack kept (the pack's used mAh too: ArduPilot's monitor would count from
//!   0 again and report a full pack, which is not modelled). On the ground or the water it boots
//!   disarmed in its boot mode with home where it is; in the air it comes back armed in its mode
//!   with its home and mission place, as ArduPilot restores a watchdog reset in flight
//!   (`was_watchdog_armed`, persistent_data). The bridge sees the time since boot go back and
//!   logs `vehicle.rebooted` (vehicle.rs).
//! - **`current`**: a uniform current (east, north) in place of the world's field from now for
//!   its duration, for the contacts and our vehicles that take conditions (water.rs).

use std::sync::Arc;
use std::time::Duration;

use murmur_standins::gnss;
use serde_json::{Value, json};
use tokio::time::Instant;

use super::Episode;
use super::spec::Spec;

/// A reboot's silence when the step gives none (an estimate: ArduPilot's boot to its first
/// heartbeat on a flight controller).
pub const REBOOT_DOWN_S: f64 = 3.0;

fn num(f: &Value, k: &str) -> Option<f64> {
    f[k].as_f64()
}

fn uid_index(ep: &Episode, f: &Value, what: &str) -> Result<usize, String> {
    let uid = f["uid"].as_u64().ok_or_else(|| format!("{what}: uid"))?;
    ep.index_of(uid)
}

pub fn apply(ep: &mut Episode, kind: &str, f: &Value) -> Result<Value, String> {
    let _g = ep.rt.enter();
    match kind {
        "gnss" => gnss_fault(ep, f),
        "battery" => {
            let i = uid_index(ep, f, "battery")?;
            let (drop, rate) = (num(f, "drop_pct"), num(f, "rate"));
            if drop.is_none() && rate.is_none() {
                return Err("battery: drop_pct or rate".into());
            }
            let pct = ep.fleet.write(i, |v| {
                let cap = v.capacity_mah();
                if let Some(p) = drop {
                    v.battery.consumed_mah = (v.battery.consumed_mah + cap * p.clamp(0.0, 100.0) / 100.0).min(cap.max(0.0));
                }
                (cap > 0.0).then(|| 100.0 * (1.0 - v.battery.consumed_mah / cap))
            });
            if let Some(r) = rate {
                if r.is_nan() || r < 0.0 {
                    return Err(format!("battery: rate {r} must be 0 or more"));
                }
                ep.fleet.set_drain(i, r, num(f, "duration_s"));
            }
            Ok(json!({ "uid": f["uid"], "drop_pct": drop, "rate": rate, "battery_pct": pct }))
        }
        "reboot" => {
            let i = uid_index(ep, f, "reboot")?;
            let down = num(f, "down_s").unwrap_or(REBOOT_DOWN_S).max(0.0);
            let until = Instant::now() + Duration::from_secs_f64(down);
            let mut out = ep.fleet.reboot(i, until)?;
            out["down_s"] = json!(down);
            Ok(out)
        }
        "current" => {
            let (e, n) = (num(f, "east_mps").ok_or("current: east_mps")?, num(f, "north_mps").ok_or("current: north_mps")?);
            let from = ep.spec.epoch + ep.now_s();
            ep.water.set_uniform(e, n, from, num(f, "duration_s"));
            Ok(json!({ "east_mps": e, "north_mps": n, "duration_s": f["duration_s"] }))
        }
        other => Err(format!("unknown fault kind {other:?}")),
    }
}

/// A jammer or a spoofer from now (module doc), added to the GNSS events the vehicles share.
fn gnss_fault(ep: &mut Episode, f: &Value) -> Result<Value, String> {
    let effect = f["effect"].as_str().ok_or("gnss: effect (jam or spoof)")?;
    // the vehicles' own clock (seconds since the stand-ins' start), as an events file's at_s
    let engine_now = (ep.now_s() - ep.spec.standins_start_s).max(0.0);
    let end = num(f, "duration_s").map_or(f64::INFINITY, |d| engine_now + d);
    let circle: Option<gnss::Circle> = match &f["circle"] {
        Value::Null => None,
        c => Some(serde_json::from_value(c.clone()).map_err(|e| format!("gnss: circle: {e}"))?),
    };
    let kind = match effect {
        "jam" => {
            let c = circle.ok_or("gnss: a jammer needs a circle")?;
            gnss::Kind::Jam(gnss::Jammer {
                lat: c.lat,
                lon: c.lon,
                radius_m: c.radius_m,
                power_w: num(f, "power_w"),
                height_m: num(f, "height_m").unwrap_or(gnss::JAMMER_HEIGHT_M),
            })
        }
        "spoof" => gnss::Kind::Spoof(
            gnss::Spoof {
                walk_mps: num(f, "walk_mps").unwrap_or(1.0).max(0.0),
                bearing_deg: num(f, "bearing_deg").unwrap_or(90.0),
                max_offset_m: num(f, "max_offset_m").unwrap_or(f64::INFINITY),
            },
            circle,
        ),
        other => return Err(format!("gnss: effect {other:?} (jam or spoof)")),
    };
    let uids: Option<Vec<u32>> = f["uids"].as_array().map(|a| a.iter().filter_map(|u| u.as_u64()).map(|u| u as u32).collect());
    let name = f["name"].as_str().unwrap_or("headless gnss").to_string();
    let ev = gnss::Event { name: name.clone(), window: gnss::Window::Engine(engine_now, end), kind, class: None, uids };
    let mut events = ep.fleet.gnss().map(|e| e.events.clone()).unwrap_or_default();
    events.push(ev);
    let env = Arc::new(gnss::Env { t0_unix: ep.spec.epoch, rate: 1.0, sky: None, events });
    ep.fleet.set_gnss(&env);
    Ok(json!({ "name": name, "effect": effect, "from_engine_s": engine_now, "events": env.events.len() }))
}

/// The GNSS events of the situation's events files (their `gnss` tables), as the stand-in
/// engine reads `MURMUR_EVENTS`; None when there are none (open sky, as before world v3).
pub fn from_events(spec: &Spec) -> Result<Option<Arc<gnss::Env>>, String> {
    if !spec.events.iter().any(|e| e.get("gnss").is_some()) {
        return Ok(None);
    }
    let text = toml::to_string(&json!({ "event": spec.events })).map_err(|e| format!("events: {e}"))?;
    let events = gnss::parse_events(&text)?;
    Ok(Some(Arc::new(gnss::Env { t0_unix: spec.epoch, rate: 1.0, sky: None, events })))
}
