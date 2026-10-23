//! One episode: the bridge assembled as its `main.rs` assembles it, minus every socket, thread
//! and HTTP client, with the stand-ins, the link emulator and the contacts beside it, all on one
//! current-thread tokio runtime whose clock is paused. Time moves only when the driver asks
//! ([`Episode::step_to`]); everything else (a tool call, an operator's request, a read of
//! truth) happens at the instant the clock stands at, and is checked not to move it.
//!
//! Started like the bridge (`main.rs`), in the same order: the fleet manifest, the estimate's
//! configuration, the decision layer (`Decision::load`, the production constructor: its audit
//! log and anchors in the episode's directory), the track store (`tracks::init`), the
//! housekeeping, the decision loop, the tracker loop, the GCS heartbeat round
//! (`heartbeat::send_round`, once a simulated second, as the thread sends it), the link
//! emulator's view for the console (`emu`, every 2 s, from the in-memory emulator's `/stats`),
//! the router (`api::router`, with `emu` and `situation` merged as `main.rs` merges them), the
//! operators' authentication (dev) and the MCP server's shared state (`mcp::Mcp`). Not started:
//! the console push, the CoT export, the tidewater water sampler, the HTTP listener.

pub mod contacts;
pub mod emulator;
pub mod fleet;
pub mod perf;
pub mod sense;
pub mod server;
pub mod space_cache;
pub mod spec;
pub mod terrain;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, header};
use serde_json::{Value, json};
use tokio::time::Instant;
use tower::ServiceExt;

use crate::decision::Decision;
use crate::link::Link;
use crate::mcp::Mcp;
use crate::world::{Config, World};

use self::contacts::Contacts;
use self::emulator::Emulator;
use self::fleet::Fleet;
use self::spec::Spec;

/// The settle after a step: the clock has reached the target, and every task woken at that
/// instant runs before the driver's next request (a yield puts the stepping task behind them).
const SETTLE_YIELDS: usize = 64;

pub struct Episode {
    pub rt: tokio::runtime::Runtime,
    zero: Instant,
    pub spec: Spec,
    decision: Arc<Decision>,
    tracks: Arc<crate::tracks::Tracks>,
    api: Router,
    mcp: Arc<Mcp>,
    emu: Arc<Emulator>,
    fleet: Arc<Fleet>,
    contacts: Option<Arc<Contacts>>,
    /// Agents whose `session.connected` is logged (once each, as the MCP server logs it).
    connected: BTreeSet<String>,
    /// The bridge's own `/mcp` endpoint on the same `Mcp` (the view-identity test), and each
    /// agent's session on it.
    mcp_http: Option<Router>,
    http_sessions: std::collections::BTreeMap<String, String>,
    pub audit_path: PathBuf,
}

fn err<E: std::fmt::Display>(what: &str) -> impl Fn(E) -> String + '_ {
    move |e| format!("{what}: {e}")
}

impl Episode {
    pub fn start(spec: Spec) -> Result<Episode, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .map_err(err("runtime"))?;
        let parts = rt.block_on(assemble(&spec))?;
        let (zero, _world, decision, tracks, api, mcp, emu, fleet, contacts, audit_path) = parts;
        let mut ep = Episode {
            rt,
            zero,
            spec,
            decision,
            tracks,
            api,
            mcp,
            emu,
            fleet,
            contacts,
            connected: BTreeSet::new(),
            mcp_http: None,
            http_sessions: std::collections::BTreeMap::new(),
            audit_path,
        };
        ep.settle();
        Ok(ep)
    }

    /// Situation seconds now.
    pub fn now_s(&self) -> f64 {
        let _g = self.rt.enter();
        Instant::now().saturating_duration_since(self.zero).as_secs_f64()
    }

    fn settle(&mut self) {
        self.rt.block_on(async {
            for _ in 0..SETTLE_YIELDS {
                tokio::task::yield_now().await;
            }
        });
    }

    /// Run everything up to situation second `t` (never backwards).
    pub fn step_to(&mut self, t: f64) -> f64 {
        let target = self.zero + Duration::from_secs_f64(t.max(0.0));
        self.rt.block_on(async move { tokio::time::sleep_until(target).await });
        self.settle();
        self.now_s()
    }

    /// Run a request at the current instant; an error if it moved the clock.
    fn at_once<F: std::future::Future>(&mut self, what: &str, f: F) -> Result<F::Output, String> {
        let before = {
            let _g = self.rt.enter();
            Instant::now()
        };
        let out = self.rt.block_on(f);
        let after = {
            let _g = self.rt.enter();
            Instant::now()
        };
        if after != before {
            return Err(format!(
                "{what} moved the simulated clock by {:?}: a request must answer at the instant it is asked",
                after - before
            ));
        }
        Ok(out)
    }

    /// An MCP tool call as `agent` (its client's name and version, as an MCP session's
    /// `initialize` gives them): the bridge's own `mcp::tools::call`, answered as the MCP
    /// server answers it (`structuredContent`, `isError`).
    pub fn mcp_call(&mut self, agent: &str, client: (&str, &str), tool: &str, args: Value) -> Result<Value, String> {
        let id = self.mcp.resolve(agent).map_err(|e| e.message.to_string())?;
        if self.connected.insert(agent.to_string()) {
            // `Mcp::connected`: once per agent and run, as the header names it
            let detail = format!("via X-Murmur-Agent header; client {} {}", client.0, client.1);
            let _g = self.rt.enter();
            if let Err(e) = self.decision.agent_action(&id, "session.connected", &detail) {
                eprintln!("mcp: agent.action: {}", e.detail);
            }
        }
        let args = match args {
            Value::Object(m) => m,
            Value::Null => serde_json::Map::new(),
            other => return Err(format!("{tool}: arguments must be an object, not {other}")),
        };
        let mcp = self.mcp.clone();
        let name = tool.to_string();
        let out = self.at_once(tool, async move { crate::mcp::tools::call(&mcp, &id, &name, args).await })?;
        match out {
            Ok(result) => {
                let v = serde_json::to_value(&result).map_err(err("result"))?;
                let structured = tool_result_json(&v);
                Ok(json!({
                    "is_error": v.get("isError").and_then(Value::as_bool).unwrap_or(false),
                    "structured": structured,
                }))
            }
            // a protocol error (an unknown tool, bad parameters): what an MCP client raises
            Err(e) => Ok(json!({ "protocol_error": e.message })),
        }
    }

    /// The same tool call through the bridge's own `/mcp` endpoint (rmcp's streamable HTTP
    /// server around the same `Mcp`, driven in process with JSON-RPC as `mcp/tests.rs` drives
    /// it): what an MCP client over HTTP gets. For the view-identity test only.
    pub fn mcp_over_http(&mut self, agent: &str, client: (&str, &str), tool: &str, args: Value) -> Result<Value, String> {
        use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
        use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
        if self.mcp_http.is_none() {
            let _g = self.rt.enter();
            let mcp = self.mcp.clone();
            let svc = StreamableHttpService::new(
                move || Ok(crate::mcp::Handler(mcp.clone())),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default(),
            );
            self.mcp_http = Some(Router::new().nest_service("/mcp", svc));
        }
        let router = self.mcp_http.clone().ok_or("no endpoint")?;
        let session = match self.http_sessions.get(agent).cloned() {
            Some(s) => s,
            None => {
                let init = json!({
                    "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": client.0, "version": client.1}},
                });
                let (r, a) = (router.clone(), agent.to_string());
                let (sess, _) = self.at_once("initialize", async move { rpc(&r, None, &a, init).await })??;
                let s = sess.ok_or("initialize: no session id")?;
                let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
                let (r, a, s2) = (router.clone(), agent.to_string(), s.clone());
                self.at_once("initialized", async move { rpc(&r, Some(&s2), &a, note).await })??;
                self.http_sessions.insert(agent.to_string(), s.clone());
                s
            }
        };
        let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                           "params": { "name": tool, "arguments": args } });
        let (r, a) = (router, agent.to_string());
        let (_, v) = self.at_once(tool, async move { rpc(&r, Some(&session), &a, call).await })??;
        if let Some(e) = v.get("error") {
            return Ok(json!({ "protocol_error": e["message"] }));
        }
        let res = &v["result"];
        Ok(json!({
            "is_error": res.get("isError").and_then(Value::as_bool).unwrap_or(false),
            "structured": tool_result_json(res),
        }))
    }

    /// An operator's request to the bridge's own REST API (a dev run: the header names the
    /// operator), answered with its status and JSON.
    pub fn api_call(&mut self, method: &str, path: &str, body: Option<Value>, operator: Option<&str>) -> Result<Value, String> {
        let method = Method::from_bytes(method.as_bytes()).map_err(err("method"))?;
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(op) = operator {
            req = req.header("x-murmur-operator", op);
        }
        let req = req
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .map_err(err("request"))?;
        let api = self.api.clone();
        let (status, bytes) = self.at_once(path, async move {
            let resp = match api.oneshot(req).await {
                Ok(r) => r,
                Err(never) => match never {},
            };
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20).await;
            (status, bytes)
        })?;
        let bytes = bytes.map_err(err("body"))?;
        let body: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "detail": String::from_utf8_lossy(&bytes) }));
        Ok(json!({ "status": status, "body": body }))
    }

    /// Truth now, for labels (evaluation only; nothing here is reachable from the bridge's API
    /// or the MCP tools): the contacts, our vehicles as the stand-ins really are, each link's
    /// blocking cause, and the tracker's own association of tracks to contacts; with
    /// `log_from`, the contacts' truth log from that line on (one line per contacts step, as
    /// the contacts process writes contacts-truth.jsonl).
    pub fn truth(&mut self, association: bool, log_from: Option<usize>) -> Result<Value, String> {
        let _g = self.rt.enter();
        let now = crate::clock::wall();
        let ours: Vec<Value> = (0..self.fleet.len())
            .map(|i| {
                let blocked = self.emu.blocked(i);
                let mut o = self.fleet.read(i, |v| {
                    let (lat, lon, alt) = v.truth();
                    json!({
                        "uid": v.cfg.uid(),
                        "callsign": v.cfg.callsign,
                        "lat": lat, "lon": lon, "alt_m": alt,
                        "mode": v.st.mode,
                        "armed": v.st.armed,
                        "speed_mps": v.st.speed,
                        "battery_consumed_mah": v.battery.consumed_mah,
                        "battery_capacity_mah": v.capacity_mah(),
                        "link_blocked": blocked,
                    })
                });
                // spent by its own effect (`Spec::effects`): its last state, and that it is gone
                if self.fleet.is_gone(i) {
                    o["gone"] = json!(true);
                }
                o
            })
            .collect();
        let eval = if association { self.tracks.eval_json(now) } else { json!({ "association": null }) };
        let effects = self.contacts.as_ref().map(|c| c.effects()).filter(|e| !e.is_empty());
        let mut out = json!({
            "t": self.now_s(),
            "wall": now,
            "contacts": self.contacts.as_ref().map(|c| c.truth()),
            "ours": ours,
            "association": eval["association"],
            "contacts_start_s": self.spec.contacts.as_ref().map(|c| c.start_s),
            "bridge_start_s": self.spec.bridge_start_s,
            "log": log_from.and_then(|i| self.contacts.as_ref().map(|c| c.log_from(i))),
        });
        // the engagement effects applied with the episode's `effects`, when there were any
        if let Some(e) = effects {
            out["effects"] = json!(e);
        }
        Ok(out)
    }

    /// The picture and the truth at this instant, for scoring v2's sampler
    /// (eval/headless/v2/record.py; evaluation only, never reachable from the bridge's API or
    /// the MCP tools): every track as the bridge serves it (`Track::json`, the same function
    /// `GET /api/tracks` answers with), the tracker's own association of tracks to contacts
    /// (sorted: the store keeps it in a hash map), the inbox's alerts as the bridge derives
    /// them, and our vehicles as the stand-ins really are (position, altitude, speed, mode,
    /// armed, battery, home, the link's blocking cause). With `homes`, also each vehicle's
    /// platform and sensors (the bridge's sensor catalog) and the sensing model's seed and
    /// misclassification rate as the tracker draws them (`MURMUR_SENSE_SEED` and
    /// `MURMUR_SENSE_MISCLASS` over the scenario's). Reads only: an error if the clock moved.
    pub fn picture(&self, homes: bool) -> Result<Value, String> {
        let _g = self.rt.enter();
        let before = Instant::now();
        let now = crate::clock::wall();
        let ev = self.tracks.eval_json(now);
        let mut association: Vec<(String, String)> = ev["association"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|a| Some((a["track_id"].as_str()?.to_string(), a["contact_id"].as_str()?.to_string())))
            .collect();
        association.sort();
        let ours: Vec<Value> = (0..self.fleet.len())
            .map(|i| {
                let blocked = self.emu.blocked(i);
                self.fleet.read(i, |v| {
                    let (lat, lon, alt) = v.truth();
                    let cap = v.capacity_mah();
                    json!({
                        "uid": v.cfg.uid(),
                        "lon": lon, "lat": lat, "alt_m": alt,
                        "speed_mps": v.st.speed,
                        "mode": v.st.mode,
                        "armed": v.st.armed,
                        "battery_pct": (cap > 0.0).then(|| 100.0 * (1.0 - v.battery.consumed_mah / cap)),
                        "home": [v.st.home.1, v.st.home.0],
                        "link_blocked": blocked,
                    })
                })
            })
            .collect();
        let alerts = serde_json::to_value(self.decision.current_alerts()).map_err(err("alerts"))?;
        let mut out = json!({
            "t": self.now_s(),
            "wall": now,
            "tracks": ev["tracks"],
            "association": association,
            "alerts": alerts,
            "ours": ours,
        });
        if homes {
            let catalog = crate::tracks::sensing::SensorCatalog::load(&self.spec.root.join("sim/platforms.toml")).ok();
            let fleet: Vec<Value> = self
                .spec
                .fleet
                .iter()
                .map(|v| {
                    let platform = v["platform"].as_str().unwrap_or_default();
                    let sensors: Vec<Value> = catalog
                        .as_ref()
                        .and_then(|c| c.get(platform))
                        .map(|p| {
                            p.sensors
                                .iter()
                                .map(|s| json!({ "type": s.kind.as_str(), "range_m": s.range_m, "height": s.height }))
                                .collect()
                        })
                        .unwrap_or_default();
                    json!({ "uid": v["uid"], "callsign": v["callsign"], "platform": platform, "sensors": sensors })
                })
                .collect();
            let st = crate::tracks::Settings::from_env();
            let scenario = self
                .contacts
                .as_ref()
                .map(|c| (c.seed, c.scenario.sensing.misclassification_rate));
            out["fleet"] = json!(fleet);
            out["sensing"] = json!({
                "seed": st.seed.or(scenario.map(|s| s.0)),
                "misclassification_rate": st.misclassification_rate.or(scenario.map(|s| s.1)),
                "seed_from_env": st.seed.is_some(),
                "rate_from_env": st.misclassification_rate.is_some(),
            });
        }
        if Instant::now() != before {
            return Err("picture moved the simulated clock: a read must answer at the instant it is asked".into());
        }
        Ok(out)
    }

    /// A scripted fault, outside the bridge: a link event (the emulator's own `[[event]]`
    /// schema), or a stand-in that stops making way (its cruise and top speed set to zero, as a
    /// fouled propeller would; nothing goes through the bridge).
    pub fn fault(&mut self, f: &Value) -> Result<Value, String> {
        let _g = self.rt.enter();
        match f["kind"].as_str() {
            Some("link_event") => {
                let e: murmur_linkemu::config::EventCfg =
                    serde_json::from_value(f["event"].clone()).map_err(err("link event"))?;
                let n = self.emu.add_event(e)?;
                Ok(json!({ "events": n }))
            }
            Some("stop") => {
                let uid = f["uid"].as_u64().ok_or("stop: uid")?;
                let i = self.index_of(uid)?;
                self.fleet.write(i, |v| {
                    v.cfg.spec.cruise_mps = 0.0;
                    v.cfg.spec.max_mps = 0.0;
                });
                Ok(json!({ "stopped": uid }))
            }
            other => Err(format!("unknown fault kind {other:?}")),
        }
    }

    fn index_of(&self, uid: u64) -> Result<usize, String> {
        self.emu
            .uids
            .iter()
            .position(|u| u64::from(*u) == uid)
            .ok_or_else(|| format!("no vehicle with uid {uid}"))
    }

    /// The tool names the MCP server lists.
    pub fn tools(&self) -> Vec<String> {
        crate::mcp::tools::tools()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }

    /// The tools' definitions as the MCP server lists them (`tools/list`: name, description,
    /// `inputSchema`, annotations), for an agent that hands them to a model as they are
    /// (eval/headless/officer.py).
    pub fn tool_defs(&self) -> Result<Value, String> {
        serde_json::to_value(crate::mcp::tools::tools()).map_err(err("tools"))
    }

    /// The end: anchor the audit log's head and close it, as the bridge's clean stop does
    /// (`Decision::shutdown_and_exit`), which ends the process.
    pub fn finish(self) -> ! {
        let _g = self.rt.enter();
        self.emu.save_space_cache();
        self.decision.shutdown_and_exit()
    }
}

/// The sensing model's per-(contact, sensor type) misclassification draws
/// (`tracks::sensing::misclassify_draw`) for a seed and rate: the uniform each draw compares
/// with the rate, and the draw. Evaluation only: scoring v2's labels port the function to
/// Python (eval/headless/v2/labels.py) and its test compares the two.
pub fn misclassify(req: &Value) -> Result<Value, String> {
    use crate::tracks::sensing::{SensorType, misclassify_draw, uniform};
    let seed = req["seed"].as_u64().ok_or("misclassify: seed (an unsigned integer)")?;
    let rate = req["rate"].as_f64().unwrap_or(0.0);
    let contacts = req["contacts"].as_array().ok_or("misclassify: contacts (a list of ids)")?;
    let mut draws = Vec::new();
    for c in contacts {
        let id = c.as_str().ok_or("misclassify: a contact id is a string")?;
        for s in [SensorType::Radar, SensorType::EoIr, SensorType::Ais, SensorType::Sonar, SensorType::Esm] {
            draws.push(json!({
                "contact": id,
                "sensor": s.as_str(),
                "u": uniform(seed ^ 0xC1A5, -1, 0, id, s as u64),
                "wrong": misclassify_draw(seed, id, s, rate),
            }));
        }
    }
    Ok(json!({ "draws": draws }))
}

type Parts = (
    Instant,
    Arc<World>,
    Arc<Decision>,
    Arc<crate::tracks::Tracks>,
    Router,
    Arc<Mcp>,
    Arc<Emulator>,
    Arc<Fleet>,
    Option<Arc<Contacts>>,
    PathBuf,
);

async fn assemble(spec: &Spec) -> Result<Parts, String> {
    crate::clock::sim_clock::set_epoch(spec.epoch);
    let zero = Instant::now();
    std::fs::create_dir_all(&spec.workdir).map_err(err("workdir"))?;
    let platforms = spec.root.join("sim/platforms.toml");

    // the bridge's fleet manifest: the emulator's copy of swarm.py's, ports on the bridge side
    let mut bridge_fleet = Vec::new();
    for v in &spec.fleet {
        let mut v = v.clone();
        let port = v["port"].as_u64().ok_or("fleet: a vehicle without a port")?;
        let bport = u64::from(spec.bridge_base) + port - u64::from(spec.vehicle_base);
        v["vehicle_port"] = json!(port);
        v["port"] = json!(bport);
        bridge_fleet.push(v);
    }
    let manifest = spec.workdir.join("bridge-fleet.json");
    std::fs::write(&manifest, json!({ "vehicles": bridge_fleet }).to_string()).map_err(err("manifest"))?;

    // as main.rs: settings, the world, the manifest, the estimate's configuration
    let mut settings = crate::decision::Settings::from_env(&spec.root, &platforms);
    let run = std::env::var("MURMUR_RUN_ID").unwrap_or_else(|_| "headless".into());
    settings.audit = spec.workdir.join(format!("{run}.jsonl"));
    if let Some(a) = settings.anchors.as_mut() {
        a.path = spec.workdir.join(format!("{run}.anchors"));
        a.cmd = None;
    }
    settings.last_run = Some(spec.workdir.join(format!(".last-run-{}", spec.http_port)));
    settings.tidewater = None;
    let world = World::new(Config {
        base_port: spec.bridge_base,
        http_port: spec.http_port,
        default_ports: 0,
        manifest,
        platforms: platforms.clone(),
        root: spec.root.clone(),
    });
    world.load_manifest();
    crate::estimate::init(&world);
    let decision = Decision::load(world.clone(), &settings).map_err(err("decision layer"))?;
    let tracks = crate::tracks::init(&platforms).map_err(err("tracks"))?;

    // the link emulator, then the bridge's in-memory links, then the stand-ins
    let vspecs: Vec<murmur_linkemu::config::VehicleSpec> = spec
        .fleet
        .iter()
        .map(|v| serde_json::from_value(v.clone()))
        .collect::<Result<_, _>>()
        .map_err(err("fleet"))?;
    let events: Vec<murmur_linkemu::config::EventCfg> = spec
        .events
        .iter()
        .map(|e| serde_json::from_value(e.clone()))
        .collect::<Result<_, _>>()
        .map_err(err("link events"))?;
    let grid = match &spec.terrain_grid {
        Some(p) => Some(Arc::new(terrain::Grid::load(p)?)),
        None => None,
    };
    let emu = Emulator::new(
        emulator::Setup {
            links_file: &spec.links,
            events,
            fleet: &vspecs,
            seed: spec.linkemu_seed.unwrap_or(spec.seed),
            epoch: spec.epoch,
            grid: grid.clone(),
            space: spec.space,
            space_outlook: spec.space_outlook,
            space_cache: spec.space_cache.clone(),
            space_root: spec.space_root.as_deref(),
        },
        zero,
    )?;
    let mut bridge_links = Vec::new();
    for (i, v) in vspecs.iter().enumerate() {
        let bport = spec.bridge_base + (v.port - spec.vehicle_base);
        let e = emu.clone();
        let link = Link::in_memory(bport, Box::new(move |f: &[u8]| e.downlink(i, f))).map_err(err("link"))?;
        world.attach_link(link.clone());
        bridge_links.push(link);
    }
    let mut cfgs: Vec<murmur_standins::config::VehicleConfig> = spec
        .standins
        .iter()
        .map(|v| serde_json::from_value(v.clone()))
        .collect::<Result<_, _>>()
        .map_err(err("stand-ins"))?;
    // a stand-in that starts in the air has its home on the surface below it: the baked grid's
    // height there, as the stand-in engine samples tidewater's live
    if let Some(g) = &grid {
        for c in &mut cfgs {
            c.surface_m = c.surface_m.or_else(|| g.height(c.home[0], c.home[1]));
        }
    }
    if cfgs.len() != vspecs.len() {
        return Err(format!("{} stand-ins for {} vehicles: every vehicle must be a stand-in", cfgs.len(), vspecs.len()));
    }
    let fleet = Arc::new(Fleet::new(cfgs, spec.seed, &spec.battery_pct)?);
    // the bridge is up at `bridge_start_s` (the full stack's first anchor): before, a frame from
    // the emulator or a contacts message reaches no one
    let bridge_up = zero + Duration::from_secs_f64(spec.bridge_start_s.max(0.0));
    emu.connect(bridge_links, world.clone(), fleet.clone(), bridge_up);

    // the loops, in main.rs's order, from the bridge's start
    let emu_view = crate::emu::Emu::from_env();
    {
        let (world, decision, tracks, emu_view, e) =
            (world.clone(), decision.clone(), tracks.clone(), emu_view.clone(), emu.clone());
        tokio::spawn(async move {
            tokio::time::sleep_until(bridge_up).await;
            tokio::spawn(perf::timed("bridge.housekeeping", world.clone().housekeeping()));
            tokio::spawn(perf::timed("bridge.decision", decision.run()));
            tokio::spawn(perf::timed("bridge.tracks", tracks.run()));
            {
                let world = world.clone();
                tokio::spawn(perf::timed("bridge.heartbeat", async move {
                    let msg = crate::heartbeat::gcs_heartbeat();
                    let mut every = tokio::time::interval(crate::heartbeat::PERIOD);
                    let mut k: u64 = 0;
                    loop {
                        every.tick().await;
                        crate::heartbeat::send_round(&world, k, &msg);
                        k += 1;
                    }
                }));
            }
            tokio::spawn(perf::timed("bridge.emu", async move {
                // emu.rs's poll: every 2 s, the /stats answer to the vehicles' links
                let mut every = tokio::time::interval(Duration::from_secs(2));
                loop {
                    every.tick().await;
                    let briefs = emu_view.ingest(&e.stats_json(), crate::clock::mono());
                    for cell in world.vehicles() {
                        let (b, f) = briefs.get(&cell.uid).cloned().unzip();
                        cell.set_emu(b, f);
                    }
                }
            }));
        });
    }
    tokio::spawn(perf::timed("emulator", emu.clone().run()));
    tokio::spawn(perf::timed("emulator.terrain", emu.clone().run_terrain()));
    tokio::spawn(perf::timed("emulator.space", emu.clone().run_space()));
    let s0 = fleet::start_at(zero, spec.standins_start_s);
    for i in 0..fleet.len() {
        tokio::spawn(perf::timed("standins", fleet::run(fleet.clone(), emu.clone(), i, s0)));
    }

    // the router and the agents' server, as main.rs builds them
    let auth_settings = crate::auth::Settings::from_env(spec.http_port).map_err(err("auth"))?;
    let auth = crate::auth::Auth::new(auth_settings, decision.clone(), murmur_auth::CredentialFile::default())
        .map_err(err("auth"))?;
    tokio::spawn(auth.clone().run());
    let api = crate::api::router(crate::api::App {
        world: world.clone(),
        frames: Arc::new(tokio::sync::watch::channel(None).0),
        decision: decision.clone(),
        tracks: tracks.clone(),
        lod: crate::push::LodSettings::from_env(),
        auth: auth.clone(),
    })
    .merge(crate::emu::router(emu_view))
    .merge(crate::situation::router(crate::situation::Situation::from_env()));
    let mcp = Mcp::new(crate::mcp::Settings::from_env(), world.clone(), decision.clone(), api.clone());

    let contacts = match &spec.contacts {
        Some(c) => {
            let path = if c.scenario.is_absolute() { c.scenario.clone() } else { spec.root.join(&c.scenario) };
            let ct = Arc::new(Contacts::load(&path, c.seed, spec.epoch)?);
            let token = std::env::var("MURMUR_SIM_TOKEN").ok().filter(|t| !t.trim().is_empty());
            let fx = effect_sim(spec, &platforms, &emu.uids, &fleet)?;
            tokio::spawn(perf::timed("contacts", contacts::run(
                ct.clone(),
                api.clone(),
                token,
                zero + Duration::from_secs_f64(c.start_s.max(0.0)),
                bridge_up,
                fx,
            )));
            Some(ct)
        }
        None => None,
    };
    Ok((zero, world, decision, tracks, api, mcp, emu, fleet, contacts, settings.audit.clone()))
}

/// The episode's engagement effects (`Spec::effects`): its draw's seed and which of the fleet's
/// platforms the catalog says are spent by their own effect. None without `effects`.
fn effect_sim(spec: &Spec, platforms: &std::path::Path, uids: &[u32], fleet: &Arc<Fleet>) -> Result<Option<contacts::EffectSim>, String> {
    let Some(cfg) = spec.effects.clone() else {
        return Ok(None);
    };
    if !(0.0..=1.0).contains(&cfg.p_hit) {
        return Err(format!("effects: p_hit {} is not a probability", cfg.p_hit));
    }
    let catalog = murmur_decide::Catalog::load(platforms).map_err(err("effects: the platform catalog"))?;
    let mut expended = std::collections::BTreeMap::new();
    for (i, v) in spec.fleet.iter().enumerate() {
        let platform = v["platform"].as_str().unwrap_or_default();
        if catalog.get(platform).is_some_and(|p| p.expended_by_effect)
            && let Some(uid) = uids.get(i)
        {
            expended.insert(uid.to_string(), i);
        }
    }
    Ok(Some(contacts::EffectSim { cfg, seed: spec.seed, expended, fleet: fleet.clone() }))
}

/// A tool result's JSON as the live session reads it (agents/scripted_agent.py `content`): the
/// structured content, else the first text block parsed, or `{"text": ...}` when it is not JSON,
/// else `{}`.
pub fn tool_result_json(v: &Value) -> Value {
    if let Some(s) = v.get("structuredContent").filter(|s| !s.is_null()) {
        return s.clone();
    }
    for block in v["content"].as_array().map(Vec::as_slice).unwrap_or_default() {
        if block["type"].as_str() == Some("text") {
            let text = block["text"].as_str().unwrap_or_default();
            return serde_json::from_str(text).unwrap_or_else(|_| json!({ "text": text }));
        }
    }
    json!({})
}

/// One JSON-RPC exchange with an `/mcp` endpoint: (the session id it named, the answer); the
/// answer may come as JSON or as an SSE stream (its last `data:` event).
async fn rpc(router: &Router, session: Option<&str>, agent: &str, body: Value) -> Result<(Option<String>, Value), String> {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::HOST, "127.0.0.1")
        .header(crate::mcp::AGENT_HEADER, agent);
    if let Some(s) = session {
        req = req.header("mcp-session-id", s);
    }
    let req = req.body(Body::from(body.to_string())).map_err(err("request"))?;
    let resp = match router.clone().oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let session = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let sse = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20).await.map_err(err("body"))?;
    let text = String::from_utf8_lossy(&bytes);
    let json = if sse {
        text.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .and_then(|l| serde_json::from_str(l).ok())
            .unwrap_or(Value::Null)
    } else {
        serde_json::from_str(&text).unwrap_or(Value::Null)
    };
    Ok((session, json))
}
