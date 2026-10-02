//! The truth log: where vehicles really are versus what they report, for the evaluation.
//! Never on the MAVLink wire. Set MURMUR_STANDINS_TRUTH to a file path to write JSON lines
//! every `PERIOD_S` for each vehicle that has navigated without GPS or been spoofed:
//!
//! {"t": unix_s, "uid", "sysid", "callsign", "truth": [lat, lon, alt],
//!  "reported": [lat, lon, alt], "error_m", "dead_reckoning_s", "gps",
//!  "ekf": gps | glitch | coasting | dead_reckoning | no_position | const_pos,
//!  "fix": receiver 3D fix, "sats": tracked, "js_db": jammer-to-signal or null,
//!  "spoof_m": the spoofer's offset in the receiver's position}
//!
//! The engine's stats line also gives the worst current error.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::vehicle::Vehicle;

pub const PERIOD_S: f64 = 5.0;

static LOG: OnceLock<Option<Mutex<BufWriter<File>>>> = OnceLock::new();
/// Largest navigation error recorded since the stats line last read it, centimetres.
pub static WORST_CM: AtomicU64 = AtomicU64::new(0);

fn log() -> Option<&'static Mutex<BufWriter<File>>> {
    LOG.get_or_init(|| {
        let path = std::env::var("MURMUR_STANDINS_TRUTH").ok()?;
        match File::create(&path) {
            Ok(f) => Some(Mutex::new(BufWriter::new(f))),
            Err(e) => {
                eprintln!("MURMUR_STANDINS_TRUTH {path}: {e}");
                None
            }
        }
    })
    .as_ref()
}

pub fn record(v: &Vehicle) {
    let err = v.nav.error_m();
    WORST_CM.fetch_max((err * 100.0) as u64, Ordering::Relaxed);
    let Some(log) = log() else { return };
    let (tlat, tlon, talt) = v.truth();
    let line = serde_json::json!({
        "t": crate::messages::unix_usec() as f64 / 1e6,
        "uid": v.cfg.uid(),
        "sysid": v.sysid(),
        "callsign": v.cfg.callsign,
        "truth": [tlat, tlon, talt],
        "reported": [v.st.lat, v.st.lon, v.st.alt],
        "error_m": (err * 100.0).round() / 100.0,
        "dead_reckoning_s": v.nav.dead_reckoning_s.round(),
        "gps": v.nav.gps,
        "ekf": v.nav.ekf.name(),
        "fix": v.nav.fix,
        "sats": v.rx.tracked,
        "js_db": v.rx.js_db.map(|x| (x * 10.0).round() / 10.0),
        "spoof_m": (v.nav.spoof_m() * 10.0).round() / 10.0,
    });
    if let Ok(mut w) = log.lock() {
        let _ = writeln!(w, "{line}");
        let _ = w.flush();
    }
}
