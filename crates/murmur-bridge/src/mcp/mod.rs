//! The MCP server (Phase F): AI agents read the world model and propose intents over the
//! Model Context Protocol, served by the bridge at `/mcp` (streamable HTTP, through the
//! official Rust SDK, `rmcp`). Agents decide, they don't drive (docs/DECISIONS.md):
//!
//! - **No approve tool exists.** The tools are reads (fleet, platforms, areas, policy,
//!   intents, inbox, recent vehicle events, tracks of contacts that are not ours as our
//!   sensors see them, satellite passes and tasking options) and four
//!   about intents: `preview_intent`, `propose_intent`, `explain_intent` and
//!   `withdraw_intent` (own intents only). Every write goes through the engine's
//!   `AgentPort` (`crates/murmur-decide`, [`crate::decision::agent`]), which has no approve
//!   method; this module never holds an operator session and never builds a vehicle order.
//!   `tests::no_tool_implies_approval_or_control` enumerates the tool list and fails on any
//!   name or description that implies approval or direct control.
//! - **Staleness on every fact.** Every value an agent reads carries `as_of` (wall-clock
//!   seconds when it was observed) and `age_s`; vehicle facts also say whether they are
//!   past the freshness limit the rules apply (`stale`). The server instructions and the
//!   tool descriptions tell the model to treat stale facts as uncertain.
//! - **Agent identity** is the `X-Murmur-Agent` header on each request (or, within an MCP
//!   session, the `clientInfo.name` sent with `initialize`), checked against
//!   `MURMUR_AGENTS` (comma-separated; default `agent-planner,agent-watch`). A placeholder
//!   for real authentication, like the operator header: anyone who can reach the port can
//!   claim a listed id. Each id gets its own `AgentPort`: its proposals carry it as their
//!   `source`, the console's inbox shows it with the rationale, and the audit log records
//!   `agent.action` events for `session.connected`, `intent.preview`, `intent.propose` and
//!   `intent.withdraw`.
//! - **Through the decision engine.** A proposal is an agent-sourced intent: auto tiers
//!   dispatch at once, human tiers wait in the inbox for an operator (never
//!   `approval.at_submission`, which replay checks).
//!
//! Satellite reads go through the bridge's own `/api/space/*` handlers in process (the
//! answers the console gets), all of them read-only.

mod tools;
mod world;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, header};
use murmur_decide::AgentId;
use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, InitializeRequestParams,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::decision::{Decision, DecisionError};
use crate::world::World;

pub const AGENT_HEADER: &str = "x-murmur-agent";
const JSON: &str = "application/json";
/// The most an in-process API answer may be (a satellite pass list with scenes).
const API_BODY_LIMIT: usize = 16 << 20;

/// What the model is told at `initialize`.
pub const INSTRUCTIONS: &str = "Murmur's decision layer, for AI agents supervising a fleet \
of autonomous air, ground, surface and subsurface vehicles with a human operator. You can \
read the world and propose intents. You cannot approve, reject, dispatch or control \
anything: no such tool exists, and the engine's agent interface has no such method. Every \
proposal goes through fail-closed checks (capability, readiness, platform limits, land or \
water, no-go areas, endurance reserves) and a risk-tier policy: routine tasks inside the \
approved area run on their own; tasks on armed platforms, tasks leaving the approved area \
and tasks whose reserves cannot be verified wait in the operator's inbox, where the operator \
sees your agent id and your rationale. Every fact you read carries as_of (wall-clock seconds \
when it was observed) and age_s; vehicle facts also carry stale, true when the fact is older \
than the freshness limit the rules apply. A stale fact cannot support a decision: treat it \
as uncertain, say so in your rationale, and prefer a hold or a return to a new task. Use \
preview_intent before propose_intent to see the checks and the policy tier without creating \
an intent; use explain_intent to follow what you proposed. Everything you do is written to \
the audit log under your agent id. You may nominate an engagement (propose_intent with \
task engage) on a track a human operator declared hostile, with one armed platform and a \
rationale: the effect is an abstract event, and the nomination waits for two human \
operators with engagement authority, never its proposer. You cannot approve, stop or \
release an engagement: no such tool exists. get_nominations shows where each stands. While \
an engagement is in play on a vehicle, anything you propose for that vehicle waits for a \
human.";

/// Where the agent list comes from (see `main.rs`).
pub struct Settings {
    pub agents: Vec<AgentId>,
}

impl Settings {
    pub fn from_env() -> Settings {
        let list = std::env::var("MURMUR_AGENTS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "agent-planner,agent-watch".into());
        Settings {
            agents: list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(AgentId::new)
                .collect(),
        }
    }
}

/// What every MCP session shares.
pub struct Mcp {
    pub(crate) world: Arc<World>,
    pub(crate) decision: Arc<Decision>,
    /// The bridge's HTTP API without `/mcp`, for in-process read-only calls (satellite
    /// passes and tasking options come from the same handlers the console uses).
    api: Router,
    agents: Vec<AgentId>,
    /// Agents that have connected this run (`agent.action session.connected`, once each).
    seen: Mutex<BTreeSet<AgentId>>,
}

impl Mcp {
    pub fn new(
        settings: Settings,
        world: Arc<World>,
        decision: Arc<Decision>,
        api: Router,
    ) -> Arc<Mcp> {
        Arc::new(Mcp {
            world,
            decision,
            api,
            agents: settings.agents,
            seen: Mutex::new(BTreeSet::new()),
        })
    }

    /// The agent a request acts for: the `X-Murmur-Agent` header, else the session's
    /// `clientInfo.name`, and it must be listed in `MURMUR_AGENTS`. Logged once per agent
    /// and run as `agent.action session.connected`.
    pub(crate) fn agent(&self, ctx: &RequestContext<RoleServer>) -> Result<AgentId, ErrorData> {
        let header = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.headers.get(AGENT_HEADER))
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| (s.to_string(), "X-Murmur-Agent header"));
        let client = ctx.peer.peer_info();
        let claimed = header.or_else(|| {
            client
                .as_ref()
                .map(|c| (c.client_info.name.clone(), "MCP clientInfo.name"))
        });
        let (id, via) = claimed.ok_or_else(|| {
            ErrorData::invalid_request(
                "set the X-Murmur-Agent header (or initialize with clientInfo.name) to an \
                 agent id listed in MURMUR_AGENTS; a placeholder for real authentication",
                None,
            )
        })?;
        let agent = self.resolve(&id)?;
        self.connected(&agent, via, client.as_deref());
        Ok(agent)
    }

    /// A claimed id against the configured list.
    pub(crate) fn resolve(&self, id: &str) -> Result<AgentId, ErrorData> {
        self.agents
            .iter()
            .find(|a| a.as_str() == id)
            .cloned()
            .ok_or_else(|| {
                ErrorData::invalid_request(
                    format!("agent {id:?} is not in MURMUR_AGENTS (a placeholder for real authentication)"),
                    None,
                )
            })
    }

    fn connected(&self, agent: &AgentId, via: &str, client: Option<&InitializeRequestParams>) {
        let first = self
            .seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.clone());
        if !first {
            return;
        }
        let detail = match client {
            Some(c) => format!(
                "via {via}; client {} {}",
                c.client_info.name, c.client_info.version
            ),
            None => format!("via {via}; client unknown"),
        };
        if let Err(e) = self
            .decision
            .agent_action(agent, "session.connected", &detail)
        {
            eprintln!("mcp: agent.action: {}", e.detail);
        }
    }

    /// A read-only call to the bridge's own HTTP API, in process (no socket): the same
    /// handler and JSON the console gets. Non-2xx answers become a [`DecisionError`].
    pub(crate) async fn api(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, DecisionError> {
        let fail = |status: u16, detail: String| DecisionError {
            status,
            detail,
            intent: None,
        };
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, JSON)
            .body(match body {
                Some(v) => Body::from(v.to_string()),
                None => Body::empty(),
            })
            .map_err(|e| fail(500, e.to_string()))?;
        let resp = match self.api.clone().oneshot(req).await {
            Ok(r) => r,
            Err(never) => match never {},
        };
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), API_BODY_LIMIT)
            .await
            .map_err(|e| fail(500, e.to_string()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "detail": String::from_utf8_lossy(&bytes) }));
        if status.is_success() {
            Ok(value)
        } else {
            Err(fail(
                status.as_u16(),
                value["detail"]
                    .as_str()
                    .unwrap_or("the bridge refused the request")
                    .to_string(),
            ))
        }
    }
}

/// One MCP session's handler (the SDK makes one per session; all share [`Mcp`]).
#[derive(Clone)]
pub struct Handler(pub(crate) Arc<Mcp>);

/// The service to mount at `/mcp`. `api` is the bridge's router without `/mcp`.
pub fn service(
    settings: Settings,
    world: Arc<World>,
    decision: Arc<Decision>,
    api: Router,
) -> StreamableHttpService<Handler, LocalSessionManager> {
    let shared = Mcp::new(settings, world, decision, api);
    StreamableHttpService::new(
        move || Ok(Handler(shared.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    )
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        );
        info.server_info = Implementation::new("murmur-bridge", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(INSTRUCTIONS.into());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let agent = self.0.agent(&ctx)?;
        let args = request.arguments.unwrap_or_default();
        tools::call(&self.0, &agent, &request.name, args)
            .await
            .map(Into::into)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(resources()))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(templates()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let agent = self.0.agent(&ctx)?;
        let value = read(&self.0, &agent, &request.uri).await?;
        Ok(ReadResourceResult::new(vec![
            ResourceContents::text(value.to_string(), request.uri).with_mime_type(JSON),
        ])
        .into())
    }
}

/// The world as resources: the same JSON as the read tools, for clients that prefer
/// resources (a client can subscribe to what it reads here; the data is a snapshot).
pub fn resources() -> Vec<Resource> {
    let r = |uri: &str, name: &str, description: &str| {
        let mut r = Resource::new(uri, name).with_description(description);
        r.mime_type = Some(JSON.into());
        r
    };
    vec![
        r(
            "murmur://fleet",
            "fleet",
            "Every vehicle: identity, platform limits and capabilities, position, navigation, battery, home and link health, each fact with as_of, age_s and stale; the current task per vehicle.",
        ),
        r(
            "murmur://platforms",
            "platforms",
            "The platform catalog (sim/platforms.toml): limits, endurance, minimum turn radius, link class, armed flag, the tasks each kind can do, and which fleet vehicles are of each type.",
        ),
        r(
            "murmur://areas",
            "areas",
            "The operator's areas in force: approved operating areas (leaving one needs a human) and no-go areas (no route may enter), as polygons of [lon, lat].",
        ),
        r(
            "murmur://policy",
            "policy",
            "The risk-tier policy and rules configuration in force, as logged at the start of the run. Agent proposals in human tiers always wait for an operator.",
        ),
        r(
            "murmur://intents",
            "intents",
            "Every intent of this run, newest first, with state, checks, policy decision, approval and history, plus the audit log's head hash.",
        ),
        r(
            "murmur://inbox",
            "inbox",
            "What waits for the operator: pending approvals (with why each needs a human and when it expires) and current alerts (lost links, vehicles not moving, failed tasks).",
        ),
        r(
            "murmur://events",
            "events",
            "Recent vehicle events the audit log recorded (mode changes, failsafes, status text), newest first, with age.",
        ),
        r(
            "murmur://tracks",
            "tracks",
            "Tracks of contacts that are not ours, as our own sensors see them (perception only): estimated position and its uncertainty now, velocity, existence confidence, identity estimate with confidence and conflicts, affiliation estimate with confidence and reason (hostile only when an operator declared it), the sensors and vehicles behind each track, and last_seen as as_of with age_s and stale.",
        ),
        r(
            "murmur://space/passes",
            "satellite-passes",
            "Imaging satellite access windows over the scenario area for the next six hours (read-only; nothing is tasked).",
        ),
        r(
            "murmur://space/runs",
            "satellite-runs",
            "Every dispatched satellite collect and cover of this run, newest first, with its simulated progress (tasked, collected, product ready; watching, outages), and the scenario clock.",
        ),
    ]
}

pub fn templates() -> Vec<ResourceTemplate> {
    let t = |uri: &str, name: &str, description: &str| {
        let mut t = ResourceTemplate::new(uri, name);
        t.description = Some(description.into());
        t.mime_type = Some(JSON.into());
        t
    };
    vec![
        t(
            "murmur://fleet/{uid}",
            "vehicle",
            "One vehicle by fleet uid, with every fact's age and staleness.",
        ),
        t(
            "murmur://intents/{id}",
            "intent",
            "One intent by id: its checks, policy tier, approval, history and why it needs a human.",
        ),
        t(
            "murmur://tracks/{id}",
            "track",
            "One track by id (T-0001...), with its uncertainty, confidence, identity and affiliation estimates, sources and age.",
        ),
    ]
}

/// A resource read, by URI.
async fn read(mcp: &Mcp, agent: &AgentId, uri: &str) -> Result<Value, ErrorData> {
    let not_found = || ErrorData::resource_not_found(format!("no resource {uri:?}"), None);
    let api_err = |e: DecisionError| ErrorData::internal_error(e.detail, None);
    let path = uri.strip_prefix("murmur://").ok_or_else(not_found)?;
    Ok(match path {
        "fleet" => world::fleet(mcp, None),
        "platforms" => world::platforms(mcp).await.map_err(api_err)?,
        "areas" => world::areas(mcp),
        "policy" => world::policy(mcp),
        "intents" => world::intents(mcp, agent, None, false, usize::MAX),
        "inbox" => world::inbox(mcp),
        "events" => world::events(mcp, None, 100),
        "tracks" => world::tracks(mcp, None, None, true).await.map_err(api_err)?,
        "space/passes" => mcp
            .api(Method::GET, "/api/space/passes", None)
            .await
            .map_err(api_err)?,
        "space/runs" => mcp.decision.space_runs(),
        _ => {
            if let Some(uid) = path.strip_prefix("fleet/") {
                let uid: u64 = uid.parse().map_err(|_| not_found())?;
                world::vehicle(mcp, uid).ok_or_else(not_found)?
            } else if let Some(id) = path.strip_prefix("intents/") {
                let id: u64 = id.parse().map_err(|_| not_found())?;
                mcp.decision.explain_json(id).ok_or_else(not_found)?
            } else if let Some(id) = path.strip_prefix("tracks/") {
                let all = world::tracks(mcp, None, None, true).await.map_err(api_err)?;
                let t = all["t"].clone();
                let track = all["tracks"]
                    .as_array()
                    .and_then(|ts| ts.iter().find(|t| t["id"].as_str() == Some(id)).cloned())
                    .ok_or_else(not_found)?;
                json!({ "t": t, "note": all["note"], "track": track })
            } else {
                return Err(not_found());
            }
        }
    })
}
