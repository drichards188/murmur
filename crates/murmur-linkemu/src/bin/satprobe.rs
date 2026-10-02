//! Live check for SATCOM links: a few fake vehicles at chosen positions and a fake bridge on
//! loopback, with the emulator's `/stats/<uid>` read back alongside what actually crossed.
//!
//! Writes a fleet manifest (a platform's `space_class` in sim/platforms.toml, read from the working directory, picks the profile through `[space_class_profile]`),
//! sends a GLOBAL_POSITION_INT frame from each vehicle at `--rate` Hz, measures the one-way
//! delay and loss of those frames at the fake bridge, and every `--report` seconds prints one
//! line per vehicle with the emulator's own view (up/down, best satellite, latency, handovers,
//! next outage or pass). Start the emulator against the manifest first, with the scenario
//! clock you want (`MURMUR_AT`, `MURMUR_RATE`).

use murmur_linkemu::mavpos::position_frame;
use murmur_linkemu::terrain::http_request;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

struct Veh {
    uid: u32,
    platform: String,
    link: String,
    lat: f64,
    lon: f64,
    alt_m: f64,
}

struct Counters {
    sent: AtomicU64,
    recv: AtomicU64,
    lat_sum_us: AtomicU64,
}

const USAGE: &str = "satprobe: fake SATCOM vehicles against a running emulator

USAGE: satprobe --vehicle PLATFORM:LAT:LON:ALT_M[:LINK] ... [options]

  --vehicle SPEC     repeatable; LINK defaults to satcom (the platforms.toml link class)
  --fleet-out FILE   manifest to write (default fleet-satprobe.json)
  --vehicle-base P   (default 18560)    --bridge-base P (default 19560)
  --http ADDR        emulator stats (default 127.0.0.1:18766)
  --duration S       wall seconds to run (default 60)
  --rate HZ          probe frames per vehicle per second (default 10)
  --report S         wall seconds between lines (default 5)
  --write-only       write the manifest and exit
";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let mut vehicles: Vec<Veh> = Vec::new();
    let (mut fleet_out, mut vbase, mut bbase) =
        ("fleet-satprobe.json".to_string(), 18560u16, 19560u16);
    let (mut http, mut duration, mut rate, mut report) =
        ("127.0.0.1:18766".to_string(), 60.0f64, 10.0f64, 5.0f64);
    let mut write_only = false;
    let mut it = std::env::args().skip(1);
    while let Some(f) = it.next() {
        if f == "--write-only" {
            write_only = true;
            continue;
        }
        let Some(v) = it.next() else {
            eprint!("{USAGE}");
            std::process::exit(2);
        };
        fn die(f: &str, v: &str) -> ! {
            eprintln!("satprobe: bad value {v:?} for {f}\n{USAGE}");
            std::process::exit(2)
        }
        match f.as_str() {
            "--vehicle" => {
                let p: Vec<&str> = v.split(':').collect();
                if p.len() < 4 {
                    die(&f, &v);
                }
                let n = |i: usize| p[i].parse::<f64>().unwrap_or_else(|_| die(&f, &v));
                vehicles.push(Veh {
                    uid: vehicles.len() as u32 + 1,
                    platform: p[0].into(),
                    link: p.get(4).copied().unwrap_or("satcom").into(),
                    lat: n(1),
                    lon: n(2),
                    alt_m: n(3),
                });
            }
            "--fleet-out" => fleet_out = v,
            "--vehicle-base" => vbase = v.parse().unwrap_or_else(|_| die(&f, &v)),
            "--bridge-base" => bbase = v.parse().unwrap_or_else(|_| die(&f, &v)),
            "--http" => http = v,
            "--duration" => duration = v.parse().unwrap_or_else(|_| die(&f, &v)),
            "--rate" => rate = v.parse().unwrap_or_else(|_| die(&f, &v)),
            "--report" => report = v.parse().unwrap_or_else(|_| die(&f, &v)),
            _ => {
                eprint!("{USAGE}");
                std::process::exit(2);
            }
        }
    }
    if vehicles.is_empty() {
        eprint!("{USAGE}");
        std::process::exit(2);
    }
    // The SATCOM class comes from the platform catalog, as in a real fleet manifest.
    let catalog: toml::Table = std::fs::read_to_string("sim/platforms.toml")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or_default();
    let manifest: Vec<Value> = vehicles
        .iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::json!({
                "uid": v.uid, "sysid": v.uid, "port": u32::from(vbase) + i as u32,
                "link": v.link, "domain": "surface", "platform": v.platform,
                "space_class": catalog.get(&v.platform).and_then(|p| p.get("space_class")).and_then(toml::Value::as_str),
            })
        })
        .collect();
    std::fs::write(
        &fleet_out,
        serde_json::json!({ "vehicles": manifest }).to_string(),
    )
    .expect("write fleet");
    if write_only {
        return;
    }

    let t0 = Instant::now();
    let mut counters = Vec::new();
    for (i, v) in vehicles.iter().enumerate() {
        let c = Arc::new(Counters {
            sent: AtomicU64::new(0),
            recv: AtomicU64::new(0),
            lat_sum_us: AtomicU64::new(0),
        });
        counters.push(c.clone());
        let sent_at: Arc<Mutex<[Option<Instant>; 256]>> = Arc::new(Mutex::new([None; 256]));
        let bsock = UdpSocket::bind(("127.0.0.1", bbase + i as u16))
            .await
            .expect("bind bridge port");
        let (c2, s2) = (c.clone(), sent_at.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok(n) = bsock.recv(&mut buf).await {
                // MAVLink v2 header: STX, len, incompat, compat, seq.
                if n > 4
                    && buf[0] == 0xFD
                    && let Some(t) = s2.lock().map(|g| g[buf[4] as usize]).unwrap_or(None)
                {
                    c2.recv.fetch_add(1, Relaxed);
                    c2.lat_sum_us
                        .fetch_add(t.elapsed().as_micros() as u64, Relaxed);
                }
            }
        });
        let vsock = UdpSocket::bind(("127.0.0.1", 0))
            .await
            .expect("bind vehicle");
        vsock
            .connect(("127.0.0.1", vbase + i as u16))
            .await
            .expect("connect");
        let (lat, lon, alt) = (v.lat, v.lon, v.alt_m);
        let period = Duration::from_secs_f64(1.0 / rate);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            let mut seq = 0u8;
            loop {
                tick.tick().await;
                if let Ok(mut g) = sent_at.lock() {
                    g[seq as usize] = Some(Instant::now());
                }
                if vsock
                    .send(&position_frame(seq, lat, lon, alt))
                    .await
                    .is_ok()
                {
                    c.sent.fetch_add(1, Relaxed);
                }
                seq = seq.wrapping_add(1);
            }
        });
    }

    println!(
        "{:>5} {:>3} {:<10} {:<10} {:<5} {:>3} {:<28} {:>8} {:>8} {:>5} {:>6} {:>9} {:>9}",
        "wall",
        "uid",
        "platform",
        "class",
        "state",
        "vis",
        "best satellite",
        "stat_ms",
        "meas_ms",
        "loss%",
        "hands",
        "nxt_out_s",
        "nxt_up_s"
    );
    let mut prev: Vec<(u64, u64, u64)> = vec![(0, 0, 0); vehicles.len()];
    let mut next = report;
    while t0.elapsed().as_secs_f64() < duration {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if t0.elapsed().as_secs_f64() < next {
            continue;
        }
        next += report;
        for (i, v) in vehicles.iter().enumerate() {
            let st = http_request(
                &http,
                "GET",
                &format!("/stats/{}", v.uid),
                None,
                Duration::from_secs(3),
            )
            .await
            .ok()
            .and_then(|(_, b)| serde_json::from_slice::<Value>(&b).ok());
            let c = &counters[i];
            let (s, r, l) = (
                c.sent.load(Relaxed),
                c.recv.load(Relaxed),
                c.lat_sum_us.load(Relaxed),
            );
            let (ds, dr, dl) = (s - prev[i].0, r - prev[i].1, l - prev[i].2);
            prev[i] = (s, r, l);
            let sat = st.as_ref().map_or(&Value::Null, |s| &s["satcom"]);
            let f = |k: &str| {
                sat[k]
                    .as_f64()
                    .map_or("-".to_string(), |x| format!("{x:.0}"))
            };
            let state = match (sat["mode"].as_str(), sat["up"].as_bool()) {
                (Some("live"), Some(true)) => "up".to_string(),
                (Some("live"), Some(false)) => "DOWN".to_string(),
                (Some(m), _) => m.to_string(),
                _ => "?".to_string(),
            };
            println!(
                "{:>5.0} {:>3} {:<10} {:<10} {:<5} {:>3} {:<28} {:>8} {:>8} {:>5.0} {:>6} {:>9} {:>9}",
                t0.elapsed().as_secs_f64(),
                v.uid,
                v.platform,
                sat["class"].as_str().unwrap_or("-"),
                state,
                sat["visible"]
                    .as_u64()
                    .map_or("-".to_string(), |x| x.to_string()),
                sat["best_name"].as_str().unwrap_or("-"),
                sat["latency_ms"]
                    .as_f64()
                    .map_or("-".to_string(), |x| format!("{x:.1}")),
                if dr > 0 {
                    format!("{:.1}", dl as f64 / dr as f64 / 1000.0)
                } else {
                    "-".into()
                },
                if ds > 0 {
                    100.0 * (1.0 - dr as f64 / ds as f64).max(0.0)
                } else {
                    0.0
                },
                sat["handovers"].as_u64().unwrap_or(0),
                f("next_outage_in_s"),
                f("next_pass_in_s"),
            );
        }
    }
}
