//! Synthetic load for the emulator: N fake vehicles and N fake bridge ports on loopback.
//!
//! Writes a fleet manifest, then drives MAVLink GLOBAL_POSITION_INT frames from each vehicle
//! at a fixed rate; the fake bridge echoes one packet in `--echo-every` back as downlink
//! traffic. Run the emulator against the manifest, then this, and read the emulator's
//! `/stats/summary` for CPU.

use murmur_linkemu::mavpos::position_frame;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;
use tokio::net::UdpSocket;

struct Args {
    vehicles: u32,
    rate: f64,
    duration: f64,
    vehicle_base: u16,
    bridge_base: u16,
    fleet_out: String,
    echo_every: u64,
    classes: Vec<String>,
}

fn parse() -> Result<Args, String> {
    let mut a = Args {
        vehicles: 1000,
        rate: 8.0,
        duration: 30.0,
        vehicle_base: 18560,
        bridge_base: 19560,
        fleet_out: "fleet-load.json".into(),
        echo_every: 8,
        classes: vec![
            "los".into(),
            "mesh".into(),
            "satcom".into(),
            "tether".into(),
        ],
    };
    let mut it = std::env::args().skip(1);
    while let Some(f) = it.next() {
        let v = it.next().ok_or(format!("{f} needs a value"))?;
        let bad = |_: &dyn std::fmt::Debug| format!("{f}: bad value {v:?}");
        match f.as_str() {
            "--vehicles" => a.vehicles = v.parse().map_err(|e| bad(&e))?,
            "--rate" => a.rate = v.parse().map_err(|e| bad(&e))?,
            "--duration" => a.duration = v.parse().map_err(|e| bad(&e))?,
            "--vehicle-base" => a.vehicle_base = v.parse().map_err(|e| bad(&e))?,
            "--bridge-base" => a.bridge_base = v.parse().map_err(|e| bad(&e))?,
            "--fleet-out" => a.fleet_out = v,
            "--echo-every" => a.echo_every = v.parse().map_err(|e| bad(&e))?,
            "--classes" => a.classes = v.split(',').map(str::to_string).collect(),
            _ => return Err(format!("unknown option {f}")),
        }
    }
    Ok(a)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "loadgen: {e}\nusage: loadgen [--vehicles N] [--rate HZ] [--duration S] [--vehicle-base P] [--bridge-base P] [--fleet-out FILE] [--echo-every N] [--classes a,b]"
            );
            std::process::exit(2);
        }
    };
    let vehicles: Vec<serde_json::Value> = (0..a.vehicles)
        .map(|i| {
            serde_json::json!({
                "uid": i + 1, "sysid": i % 250 + 1, "port": u32::from(a.vehicle_base) + i,
                "link": a.classes[i as usize % a.classes.len()], "domain": "air", "platform": "load",
            })
        })
        .collect();
    std::fs::write(
        &a.fleet_out,
        serde_json::json!({ "vehicles": vehicles }).to_string(),
    )
    .expect("write fleet");

    let sent = Arc::new(AtomicU64::new(0));
    let at_bridge = Arc::new(AtomicU64::new(0));
    let at_vehicle = Arc::new(AtomicU64::new(0));
    let mut tasks = Vec::new();
    for i in 0..a.vehicles {
        let bsock = UdpSocket::bind(("127.0.0.1", a.bridge_base + i as u16))
            .await
            .expect("bind bridge port");
        let (at_bridge, echo_every) = (at_bridge.clone(), a.echo_every);
        tasks.push(tokio::spawn(async move {
            let mut buf = [0u8; 512];
            let mut n = 0u64;
            while let Ok((len, from)) = bsock.recv_from(&mut buf).await {
                at_bridge.fetch_add(1, Relaxed);
                n += 1;
                if echo_every > 0 && n.is_multiple_of(echo_every) {
                    let _ = bsock.send_to(&buf[..len], from).await;
                }
            }
        }));
        let vsock = Arc::new(
            UdpSocket::bind(("127.0.0.1", 0))
                .await
                .expect("bind vehicle"),
        );
        vsock
            .connect(("127.0.0.1", a.vehicle_base + i as u16))
            .await
            .expect("connect");
        let rx = vsock.clone();
        let at_vehicle = at_vehicle.clone();
        tasks.push(tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while rx.recv(&mut buf).await.is_ok() {
                at_vehicle.fetch_add(1, Relaxed);
            }
        }));
        let (sent, period) = (sent.clone(), Duration::from_secs_f64(1.0 / a.rate));
        let offset = Duration::from_secs_f64(f64::from(i) / f64::from(a.vehicles) / a.rate);
        tasks.push(tokio::spawn(async move {
            tokio::time::sleep(offset).await;
            let mut tick = tokio::time::interval(period);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut seq = 0u8;
            loop {
                tick.tick().await;
                let frame = position_frame(seq, 37.8 + f64::from(i) * 1e-4, -122.4, 50.0);
                seq = seq.wrapping_add(1);
                if vsock.send(&frame).await.is_ok() {
                    sent.fetch_add(1, Relaxed);
                }
            }
        }));
    }
    eprintln!(
        "loadgen: {} vehicles at {} msg/s for {} s",
        a.vehicles, a.rate, a.duration
    );
    let t0 = std::time::Instant::now();
    tokio::time::sleep(Duration::from_secs_f64(a.duration)).await;
    let s = sent.load(Relaxed);
    println!(
        "loadgen: {:.0} s, sent {s} ({:.0}/s), at bridge {} ({:.1}% of sent), echoed to vehicles {}",
        t0.elapsed().as_secs_f64(),
        s as f64 / t0.elapsed().as_secs_f64(),
        at_bridge.load(Relaxed),
        100.0 * at_bridge.load(Relaxed) as f64 / s.max(1) as f64,
        at_vehicle.load(Relaxed)
    );
}
