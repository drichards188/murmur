//! Murmur bridge: one UDP MAVLink link per vehicle in, JSON over WebSocket out, tasking via
//! REST. The Rust port of bridge/murmur_bridge (same API), keyed by fleet-unique uid.
//!
//! Env: MURMUR_BASE_PORT (14560), MURMUR_HTTP_PORT (8765), MURMUR_MAX_VEHICLES (32 ports
//! opened up front; the fleet manifest opens the rest), MURMUR_FLEET_MANIFEST
//! (sim/run/fleet.json), MURMUR_ROOT (the repo, for sim/).

mod api;
mod clock;
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

use crate::world::{Config, World};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
    let http_port = cfg.http_port;
    let world = World::new(cfg);
    world.load_manifest();
    world.ensure_ports().await;
    tokio::spawn(world.clone().housekeeping());

    let frames = Arc::new(watch::channel(None).0);
    tokio::spawn(push::pusher(world.clone(), frames.clone()));

    let listener = TcpListener::bind(("127.0.0.1", http_port)).await?;
    println!("murmur-bridge on http://127.0.0.1:{http_port}");
    axum::serve(listener, api::router(api::App { world, frames })).await?;
    Ok(())
}
