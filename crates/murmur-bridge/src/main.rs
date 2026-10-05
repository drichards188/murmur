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
//! op-1), MURMUR_ENGAGE_OPERATORS (those of them with engagement authority; one not in
//! MURMUR_OPERATORS stops the bridge at start; both are logged in run.started),
//! MURMUR_TIDEWATER_URL (tidewater serve, for the domain rule; unset: "not
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
mod estimate;
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

/// A fleet manifest older than this at start is probably a previous run's: said in the log.
const MANIFEST_FRESH_S: f64 = 60.0;

/// Which fleet manifest this bridge read and how old it was: every vehicle's identity and
/// the carry-over check's fleet come from it, so a stale one must show in the log.
fn report_manifest(world: &World) {
    let path = world.cfg.manifest.display();
    let vehicles = world.manifest().identities.len();
    match std::fs::metadata(&world.cfg.manifest).and_then(|m| m.modified()) {
        Ok(written) => {
            let age = written.elapsed().map_or(0.0, |d| d.as_secs_f64());
            let stale = if age > MANIFEST_FRESH_S {
                " (older than a minute: a previous run's?)"
            } else {
                ""
            };
            println!("fleet manifest {path}: {vehicles} vehicles, written {age:.1} s ago{stale}");
        }
        Err(e) => println!("fleet manifest {path}: {e}; vehicles get synthetic ids until it appears"),
    }
}

/// SIGTERM or SIGINT.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(e) => {
            eprintln!("no SIGTERM handler ({e}): ^C only");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
    let http_port = cfg.http_port;
    let settings = Settings::from_env(&cfg.root, &cfg.platforms);
    let world = World::new(cfg);
    // this run's fleet manifest, before the decision layer checks a previous run's open
    // orders against it (scripts/up.sh writes it before starting the bridge, in both link
    // modes); logged with its age, so a previous run's shows
    world.load_manifest();
    report_manifest(&world);
    // no decision layer, no bridge: nothing may reach a vehicle without it
    let decision =
        Decision::load(world.clone(), &settings).map_err(|e| format!("decision layer: {e}"))?;
    println!("audit log {}", decision.audit_path().display());
    match decision.anchor_path() {
        Some(p) => println!("audit anchors {} (MURMUR_ANCHOR_CMD posts them elsewhere)", p.display()),
        None => println!("audit anchors off (MURMUR_ANCHORS=0)"),
    }
    // a clean stop (TERM from make down, or ^C) anchors the log's head before exiting
    {
        let decision = decision.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            decision.shutdown_and_exit();
        });
    }
    if !decision.terrain_configured() {
        println!("no MURMUR_TIDEWATER_URL: the domain rule records \"not evaluated\"");
    }
    // tracks of contacts that are not ours (Phase C2): sensors from the catalog
    let tracks = tracks::init(&world.cfg.platforms).map_err(|e| format!("tracks: {e}"))?;
    if tracks.eval_truth_enabled() {
        println!("MURMUR_EVAL_TRUTH=1: GET /api/sim/truth serves simulation truth (evaluation only)");
    }
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
    // the current under silent surface vehicles, for their drift estimate (tidewater)
    tokio::spawn(estimate::water_sampler(world.clone()));
    cot::spawn(world.clone());
    let emu = emu::Emu::from_env();
    if emu.enabled() {
        tokio::spawn(emu.clone().run(world.clone()));
    }

    let listener = TcpListener::bind(("127.0.0.1", http_port)).await?;
    println!("murmur-bridge on http://127.0.0.1:{http_port}");
    let lod = push::LodSettings::from_env();
    println!(
        "console feed: level of detail above {} vehicles, clusters above {} in view (/ws?lod=1)",
        lod.min_fleet, lod.max_full
    );
    let api = api::router(api::App {
        world: world.clone(),
        frames,
        decision: decision.clone(),
        tracks,
        lod,
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
