//! Murmur bridge: one UDP MAVLink link per vehicle in, JSON over WebSocket out, tasking via
//! REST. The Rust port of bridge/murmur_bridge (same API), keyed by fleet-unique uid, with
//! the Phase C decision layer (`decision`) in front of every task.
//!
//! Env: MURMUR_BASE_PORT (14560), MURMUR_HTTP_PORT (8765), MURMUR_MAX_VEHICLES (32 ports
//! opened up front; the fleet manifest opens the rest), MURMUR_FLEET_MANIFEST
//! (sim/run/fleet.json), MURMUR_ROOT (the repo, for sim/ and logs/).
//! Decision layer: MURMUR_POLICY (sim/policy.toml), MURMUR_AREAS (sim/areas),
//! MURMUR_RUN_ID (audit log logs/audit/<run-id>.jsonl; default the UTC start time and pid),
//! MURMUR_OPERATORS (operator ids accepted in X-Murmur-Operator, comma-separated; default
//! op-1), MURMUR_TIDEWATER_URL (tidewater serve, for the domain rule; unset: "not
//! evaluated"), MURMUR_AT and MURMUR_RATE (scenario time for tidewater, as `make goldengate`).

mod api;
mod clock;
mod decision;
mod geo;
mod link;
mod mav;
mod platforms;
mod push;
mod tasks;
mod vehicle;
mod world;

use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::decision::{Decision, Settings};
use crate::world::{Config, World};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
    let http_port = cfg.http_port;
    let settings = Settings::from_env(&cfg.root, &cfg.platforms);
    let world = World::new(cfg);
    // no decision layer, no bridge: nothing may reach a vehicle without it
    let decision =
        Decision::load(world.clone(), &settings).map_err(|e| format!("decision layer: {e}"))?;
    println!("audit log {}", decision.audit_path().display());
    if !decision.terrain_configured() {
        println!("no MURMUR_TIDEWATER_URL: the domain rule records \"not evaluated\"");
    }
    world.load_manifest();
    world.ensure_ports().await;
    tokio::spawn(world.clone().housekeeping());
    tokio::spawn(decision.clone().run());

    let frames = Arc::new(watch::channel(None).0);
    tokio::spawn(push::pusher(world.clone(), frames.clone()));

    let listener = TcpListener::bind(("127.0.0.1", http_port)).await?;
    println!("murmur-bridge on http://127.0.0.1:{http_port}");
    axum::serve(
        listener,
        api::router(api::App {
            world,
            frames,
            decision,
        }),
    )
    .await?;
    Ok(())
}
