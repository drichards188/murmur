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
//! Who acts (rules 8, `auth`): MURMUR_AUTH (`dev`, the default: ids asserted by header, said
//! loudly at start and logged as such, never evidence about people, no credential file needed:
//! MURMUR_OPERATORS (ids accepted in X-Murmur-Operator, comma-separated; default op-1),
//! MURMUR_ENGAGE_OPERATORS (those with engagement authority; one not in MURMUR_OPERATORS stops
//! the bridge), MURMUR_AGENTS (X-Murmur-Agent or the MCP clientInfo.name; default
//! agent-planner,agent-watch). `credentials`: operators log in with their own password and
//! agents present their own key, from the credential file MURMUR_CREDENTIALS, else
//! ~/.config/murmur/credentials.json, managed with `murmur-auth`; the file's operators are the
//! run's roster); MURMUR_SESSION_IDLE_S (1800),
//! MURMUR_SESSION_MAX_S (28800), MURMUR_COOKIE_SECURE=1, MURMUR_WEB_PORT (5173) and
//! MURMUR_ALLOWED_ORIGINS (who may send state-changing requests from a browser). Passkeys
//! (rules 10, a credentials run): MURMUR_PASSKEY_RP_ID (the WebAuthn relying party, `localhost`;
//! a domain, never an IP address: passkeys work at the console's localhost origin, not at
//! 127.0.0.1); MURMUR_STUDY=<participant> (a study run: the participant's passkey at every
//! step-up; the bridge refuses to start without one) and MURMUR_STUDY_PASSWORD_STEP_UP=<why>
//! (the explicit override that allows a password, logged; never evidence).
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
 DEV RUN (MURMUR_AUTH unset or dev): operator and agent ids are ASSERTED by request headers,
 not authenticated. Anyone who can reach this port can act as any listed operator, sign as
 two of them, or claim any listed agent. The audit log records auth \"dev\": this run is not
 evidence about people and is excluded from any human-control claim. MURMUR_AUTH=credentials
 makes operators log in with their own password (murmur-auth; see agents/README.md).
==========================================================================================";
        println!("{banner}");
        eprintln!("{banner}");
        return Ok(murmur_auth::CredentialFile::default());
    }
    let path = a.file.clone().ok_or("no credential file path")?;
    let how = format!(
        "add an operator with `printf '%s\\n' \"$PASSWORD\" | target/release/murmur-auth add op-1 --engage` \
         (file {}; MURMUR_CREDENTIALS moves it), or leave MURMUR_AUTH unset for a dev run with \
         header ids (logged as dev, no human-control claim)",
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
    // rules 10: the relying party and every person's passkeys (public keys and counters), when
    // a console origin is on the relying party's host (an IP address can't be one)
    if a.passkey_origins.is_empty() {
        println!(
            "auth: passkeys off: no console origin on {} (MURMUR_ALLOWED_ORIGINS; WebAuthn takes no IP address)",
            a.passkey_rp
        );
    } else {
        let keys: Vec<murmur_decide::PasskeyKey> = file
            .operators
            .iter()
            .filter(|(_, e)| !e.scripted)
            .flat_map(|(id, e)| {
                e.passkeys.iter().map(move |p| murmur_decide::PasskeyKey {
                    operator: murmur_decide::OperatorId::new(id),
                    id: p.id.clone(),
                    public_key: p.public_key.clone(),
                    counter: p.counter,
                })
            })
            .collect();
        println!(
            "auth: passkeys for {} from {}: {} registered{}",
            a.passkey_rp,
            a.passkey_origins.join(", "),
            keys.len(),
            if keys.is_empty() {
                String::new()
            } else {
                format!(
                    " ({})",
                    file.operators
                        .iter()
                        .filter(|(_, e)| !e.passkeys.is_empty() && !e.scripted)
                        .map(|(id, e)| format!("{id} {}", e.passkeys.len()))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
        settings.passkeys = Some(murmur_decide::PasskeyRoster {
            rp_id: a.passkey_rp.clone(),
            origins: a.passkey_origins.clone(),
            keys,
        });
    }
    // rules 10: a study's participant is a person on the file, with a passkey unless the
    // explicit override allows password step-ups
    if let Some(st) = &a.study {
        let p = st.participant.as_str();
        let Some(e) = file.operators.get(p) else {
            return Err(format!("MURMUR_STUDY={p}: not an operator in the credential file"));
        };
        if e.scripted {
            return Err(format!("MURMUR_STUDY={p}: a study's participant is a person, and this credential is marked scripted (a harness's)"));
        }
        if st.password_step_up.is_none() && (settings.passkeys.is_none() || e.passkeys.is_empty()) {
            return Err(format!(
                "study: the participant {p} has no passkey this run can use: register one before the run (`murmur-auth passkey-invite {p}`, then open the link), or allow password step-ups explicitly with MURMUR_STUDY_PASSWORD_STEP_UP=<why> (logged; the run is then not evidence about people)"
            ));
        }
        println!("study: participant {p}");
        settings.study = Some(st.clone());
    }
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
