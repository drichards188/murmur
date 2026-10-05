//! Murmur bridge: one UDP MAVLink link per vehicle in, JSON over WebSocket out, tasking via
//! REST. The Rust port of bridge/murmur_bridge (same API), keyed by fleet-unique uid, with
//! the Phase C decision layer (`decision`) in front of every task.
//!
//! Env: MURMUR_BASE_PORT (14560), MURMUR_HTTP_PORT (8765), MURMUR_MAX_VEHICLES (32 ports
//! opened up front; the fleet manifest opens the rest), MURMUR_FLEET_MANIFEST
//! (sim/run/fleet.json), MURMUR_ROOT (the repo, for sim/ and logs/).
//! Decision layer: MURMUR_POLICY (sim/policy.toml), MURMUR_AREAS (sim/areas),
//! MURMUR_RUN_ID (audit log logs/audit/<run-id>.jsonl; default the UTC start time and pid),
//! MURMUR_TIDEWATER_URL (tidewater serve, for the domain rule; unset: "not
//! evaluated"), MURMUR_AT and MURMUR_RATE (scenario time for tidewater, as `make goldengate`).
//! Who acts (rules 7, `auth`): MURMUR_AUTH (`credentials`, the default: operators log in with
//! their own password and agents present their own key, from the credential file
//! MURMUR_CREDENTIALS, else ~/.config/murmur/credentials.json, managed with `murmur-auth`; the
//! file's operators are the run's roster. `dev`: ids asserted by header, said loudly at start
//! and logged as such: MURMUR_OPERATORS (ids accepted in X-Murmur-Operator, comma-separated;
//! default op-1), MURMUR_ENGAGE_OPERATORS (those with engagement authority; one not in
//! MURMUR_OPERATORS stops the bridge), MURMUR_AGENTS (X-Murmur-Agent or the MCP
//! clientInfo.name; default agent-planner,agent-watch)); MURMUR_SESSION_IDLE_S (1800),
//! MURMUR_SESSION_MAX_S (28800), MURMUR_COOKIE_SECURE=1, MURMUR_WEB_PORT (5173) and
//! MURMUR_ALLOWED_ORIGINS (who may send state-changing requests from a browser).
//! Link emulator: MURMUR_LINKEMU_URL (murmur-linkemu's stats endpoint, polled for
//! `/api/links` and each link's `emu` state; unset: no emulator view). Behind the emulator
//! MURMUR_BASE_PORT is the bridge-side base and MURMUR_FLEET_MANIFEST its rewritten manifest.

mod api;
mod auth;
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

/// Who may act in this run: the credential file (credentials run: its operators are the
/// roster), or the dev run's header lists, announced loudly. Refuses to start a credentials
/// run without a readable file holding at least one operator.
fn who_acts(
    a: &auth::Settings,
    settings: &mut Settings,
) -> Result<murmur_auth::CredentialFile, String> {
    settings.auth = a.mode;
    if a.mode == murmur_decide::AuthMode::Dev {
        let banner = "\
==========================================================================================
 MURMUR_AUTH=dev: operator and agent ids are ASSERTED by request headers, not authenticated.
 Anyone who can reach this port can act as any listed operator, sign as two of them, or
 claim any listed agent. The audit log records auth \"dev\": this run is not evidence about
 people and is excluded from any human-control claim. For scripted tests and the proxy.
==========================================================================================";
        println!("{banner}");
        eprintln!("{banner}");
        return Ok(murmur_auth::CredentialFile::default());
    }
    let path = a.file.clone().ok_or("no credential file path")?;
    let how = format!(
        "add an operator with `printf '%s\\n' \"$PASSWORD\" | target/release/murmur-auth add op-1 --engage` \
         (file {}; MURMUR_CREDENTIALS moves it), or run with MURMUR_AUTH=dev for header ids \
         (logged as dev, no human-control claim)",
        path.display()
    );
    let file = match murmur_auth::CredentialFile::load(&path) {
        Ok(f) => f,
        Err(murmur_auth::AuthError::Io(_, e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("no credential file: {how}"));
        }
        Err(e) => return Err(e.to_string()),
    };
    if file.operators.is_empty() {
        return Err(format!("the credential file holds no operator: {how}"));
    }
    if murmur_auth::CredentialFile::readable_by_others(&path) {
        eprintln!("auth: warning: {} is readable by other users (chmod 600 it)", path.display());
    }
    for k in ["MURMUR_OPERATORS", "MURMUR_ENGAGE_OPERATORS", "MURMUR_AGENTS"] {
        if std::env::var(k).is_ok_and(|v| !v.trim().is_empty()) {
            eprintln!("auth: {k} is ignored in a credentials run (the credential file says who may act)");
        }
    }
    let (ops, engage) = auth::roster_from(&file);
    println!(
        "auth: credentials from {}: operators {} (engagement authority: {}), {} agent key(s)",
        path.display(),
        ops.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),
        if engage.is_empty() {
            "none".to_string()
        } else {
            engage.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        },
        file.agents.len()
    );
    settings.operators = ops;
    settings.engage_operators = engage;
    Ok(file)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::from_env();
    let http_port = cfg.http_port;
    let mut settings = Settings::from_env(&cfg.root, &cfg.platforms);
    let auth_settings = auth::Settings::from_env(http_port).map_err(|e| format!("auth: {e}"))?;
    let credentials = who_acts(&auth_settings, &mut settings).map_err(|e| format!("auth: {e}"))?;
    let world = World::new(cfg);
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
    // who is acting: logins, step-ups and agent keys (or a dev run's headers)
    let origins = Arc::new(auth_settings.origins.clone());
    let auth = auth::Auth::new(auth_settings, decision.clone(), credentials);
    tokio::spawn(auth.clone().run());
    match auth.mode() {
        murmur_decide::AuthMode::Credentials => println!(
            "operators sign in at the console (POST /api/auth/login); sessions end after {} s idle, {} s at most; browser requests only from {}",
            auth.settings.idle.as_secs(),
            auth.settings.max.as_secs(),
            origins.join(", ")
        ),
        murmur_decide::AuthMode::Dev => println!(
            "DEV RUN: operators named by X-Murmur-Operator ({})",
            settings.operators.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
        ),
    }
    let api = api::router(api::App {
        world: world.clone(),
        frames,
        decision: decision.clone(),
        tracks,
        lod,
        auth: auth.clone(),
    })
    .merge(emu::router(emu));
    // the MCP server for agents: in-process AgentPorts, read-only reuse of the API above
    let mut agents = mcp::Settings::from_env();
    match auth.mode() {
        murmur_decide::AuthMode::Credentials => {
            agents.keys = Some(auth.clone());
            println!("mcp for agents at http://127.0.0.1:{http_port}/mcp (Authorization: Bearer <the agent's key>)");
        }
        murmur_decide::AuthMode::Dev => println!(
            "mcp for agents at http://127.0.0.1:{http_port}/mcp (DEV: X-Murmur-Agent: {})",
            agents
                .agents
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    let router = app_router(api, agents, world, decision, auth, origins);
    axum::serve(listener, router).await?;
    Ok(())
}

/// The whole HTTP surface: the API, `/mcp` behind the agents' keys (a credentials run), and
/// every browser's state-changing request (or WebSocket) only from the console's origin.
fn app_router(
    api: axum::Router,
    agents: mcp::Settings,
    world: Arc<world::World>,
    decision: Arc<Decision>,
    auth: Arc<auth::Auth>,
    origins: Arc<Vec<String>>,
) -> axum::Router {
    let mcp = axum::Router::new()
        .nest_service("/mcp", mcp::service(agents, world, decision, api.clone()))
        .layer(axum::middleware::from_fn_with_state(auth, auth::require_agent_key));
    api.merge(mcp)
        .layer(axum::middleware::from_fn_with_state(origins, auth::check_origin))
}
