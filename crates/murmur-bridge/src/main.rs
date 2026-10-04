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
//! Link emulator: MURMUR_LINKEMU_URL (murmur-linkemu's stats endpoint, polled for
//! `/api/links` and each link's `emu` state; unset: no emulator view). Behind the emulator
//! MURMUR_BASE_PORT is the bridge-side base and MURMUR_FLEET_MANIFEST its rewritten manifest.
//! Agents (Phase F, the MCP server at /mcp): MURMUR_AGENTS (agent ids accepted in
//! X-Murmur-Agent or as the MCP clientInfo.name, comma-separated; default
//! agent-planner,agent-watch).

mod api;
mod clock;
mod cot;
mod decision;
mod emu;
mod geo;
mod heartbeat;
mod link;
mod mav;
mod mcp;
mod platforms;
mod push;
mod space;
mod tasks;
mod telemetry;
mod tracks;
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
    // tracks of contacts that are not ours (Phase C2): sensors from the catalog
    let tracks = tracks::init(&world.cfg.platforms).map_err(|e| format!("tracks: {e}"))?;
    if tracks.eval_truth_enabled() {
        println!("MURMUR_EVAL_TRUTH=1: GET /api/sim/truth serves simulation truth (evaluation only)");
    }
    world.load_manifest();
    world.ensure_ports().await;
    // the GCS heartbeat on its own thread, apart from everything on the runtime
    let beats = heartbeat::spawn(world.clone())?;
    println!(
        "gcs heartbeat: own thread, every {} s",
        heartbeat::PERIOD.as_secs_f64()
    );
    tokio::spawn(heartbeat::report(beats));
    tokio::spawn(world.clone().housekeeping());
    tokio::spawn(decision.clone().run());
    tokio::spawn(tracks.clone().run());

    let frames = Arc::new(watch::channel(None).0);
    tokio::spawn(push::pusher(world.clone(), frames.clone()));
    cot::spawn(world.clone());
    let emu = emu::Emu::from_env();
    if emu.enabled() {
        tokio::spawn(emu.clone().run(world.clone()));
    }

    let listener = TcpListener::bind(("127.0.0.1", http_port)).await?;
    println!("murmur-bridge on http://127.0.0.1:{http_port}");
    let api = api::router(api::App {
        world: world.clone(),
        frames,
        decision: decision.clone(),
        tracks,
    })
    .merge(emu::router(emu));
    // the MCP server for agents: in-process AgentPorts, read-only reuse of the API above
    let agents = mcp::Settings::from_env();
    println!(
        "mcp for agents at http://127.0.0.1:{http_port}/mcp (X-Murmur-Agent: {})",
        agents
            .agents
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let router = api
        .clone()
        .nest_service("/mcp", mcp::service(agents, world, decision, api));
    axum::serve(listener, router).await?;
    Ok(())
}
