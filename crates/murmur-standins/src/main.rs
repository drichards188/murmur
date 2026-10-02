//! murmur-standins [standins.json]: run every stand-in in the file, one UDP link each.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use murmur_standins::conditions::{self, Slot};
use murmur_standins::config::Fleet;
use murmur_standins::link;
use murmur_standins::vehicle::{self, Vehicle};

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sim/run/standins.json".into());
    // One worker by default: measured at 3,000 vehicles, 1 thread used 8% of a core, the
    // default of one per core 17% (cross-thread wakeups). MURMUR_STANDINS_THREADS to change.
    let threads = std::env::var("MURMUR_STANDINS_THREADS")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(1);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("murmur-standins: runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(run(&path)) {
        eprintln!("murmur-standins: {e}");
        std::process::exit(1);
    }
}

async fn run(path: &str) -> Result<(), String> {
    let fleet = Fleet::load(path)?;
    let tidewater = conditions::Config::from_env();
    let host = fleet.bridge_host.clone();

    let mut vehicles = Vec::with_capacity(fleet.vehicles.len());
    let mut slots = Vec::new();
    for cfg in fleet.vehicles {
        let sock = link::connect(&host, cfg.port)
            .map_err(|e| format!("{}: udp {}: {e}", cfg.callsign, cfg.port))?;
        let line = format!(
            "{:12} sysid {:3} {} -> udp {}",
            cfg.callsign, cfg.sysid, cfg.spec.name, cfg.port
        );
        let mut v = Vehicle::new(cfg, sock, None)?;
        if tidewater.is_some() && v.behavior.marine() {
            let slot = Arc::new(Slot::default());
            slot.report(v.st.lat, v.st.lon, v.st.depth());
            slots.push(slot.clone());
            v.water = Some(slot);
        }
        println!("{line}");
        vehicles.push(v);
    }
    println!("{} stand-ins", vehicles.len());
    if let Some(cfg) = tidewater
        && !slots.is_empty()
    {
        conditions::start(cfg, slots).await;
    }
    let stats_s: u64 = std::env::var("MURMUR_STANDINS_STATS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    if stats_s > 0 {
        tokio::spawn(stats(vehicles.len(), Duration::from_secs(stats_s)));
    }
    let n = vehicles.len().max(1) as u32;
    let tasks: Vec<_> = (0u32..)
        .zip(vehicles)
        .map(|(i, v)| tokio::spawn(vehicle::run(v, vehicle::TICK * i / n)))
        .collect();
    for t in tasks {
        t.await.map_err(|e| format!("a vehicle task died: {e}"))?;
    }
    Ok(())
}

/// A line every `period` (MURMUR_STANDINS_STATS seconds, 0 = off): what actually went out.
async fn stats(n: usize, period: Duration) {
    let mut every = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    let (mut sent0, mut err0) = (0, 0);
    loop {
        every.tick().await;
        let sent = link::SENT.load(Ordering::Relaxed);
        let err = link::SEND_ERRORS.load(Ordering::Relaxed);
        let secs = period.as_secs_f64();
        println!(
            "{n} stand-ins: {:.0} msg/s sent, {:.0} send errors/s",
            (sent - sent0) as f64 / secs,
            (err - err0) as f64 / secs
        );
        (sent0, err0) = (sent, err);
    }
}
