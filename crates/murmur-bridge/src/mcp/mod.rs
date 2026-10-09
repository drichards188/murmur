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
//! - **Agent identity** (rules 8) is the agent's own key, `Authorization: Bearer <key>` on
//!   every request, from the credential file (`murmur-auth agent-add`; `crate::auth` checks it
//!   before any MCP session exists). In a dev run (`MURMUR_AUTH=dev`) it is the
//!   `X-Murmur-Agent` header (or, within an MCP session, the `clientInfo.name` sent with
//!   `initialize`), checked against `MURMUR_AGENTS` (comma-separated; default
//!   `agent-planner,agent-watch,agent-playbook`): anyone who can reach the port can claim a listed id, and
//!   the run is logged as dev. Each id gets its own `AgentPort`: its proposals carry it as their
//!   `source`, the console's inbox shows it with the rationale, and the audit log records
//!   `agent.action` events for `session.connected`, `intent.preview`, `intent.propose` and
//!   `intent.withdraw`.
//! - **Through the decision engine.** A proposal is an agent-sourced intent: auto tiers
//!   dispatch at once, human tiers wait in the inbox for an operator (never
//!   `approval.at_submission`, which replay checks).
//!
//! Satellite reads go through the bridge's own `/api/space/*` handlers in process (the
//! answers the console gets), all of them read-only. `get_changes` ([`changes`]) answers
//! what changed since a cursor, for agents that poll, from the same read-only views.

mod changes;
pub(crate) mod tools;
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
    CacheScope, CallToolRequestParams, CallToolResponse, Implementation, InitializeRequestParams,
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
/// How long a client may treat a tool, resource or template list as fresh. Protocol
/// 2026-07-28 (SEP-2549) requires `ttlMs` and `cacheScope` on list results, and clients
/// that speak it refuse a list without them (Claude Code 2.1.294 saw no tools at all).
/// The lists never change while the bridge runs; private: per client, as a precaution.
const LIST_TTL_MS: u64 = 300_000;
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
task engage) on a track declared hostile (by a human operator, or by the engine's \
hostile-act rule on an observed act against ours: get_tracks shows which, on what evidence, \
and whether an operator confirmed it), with one armed platform and a rationale: the effect \
is an abstract event, and the nomination waits for two human operators with engagement \
authority, never its proposer. You cannot approve, stop or release an engagement, and you \
cannot declare a track, confirm or revoke a declaration, or resolve a review flag: no such \
tool exists. A track only near or moving with a declared hostile is weak evidence (it may be \
a neutral): it is flagged for review, never declared. get_nominations shows where each \
stands; get_nomination_options shows, before a nomination, which armed platforms pass every \
check now and, where a keep-out blocks, which track or vehicle of ours blocks it, why and \
what would clear it (read-only: no keep-out is relaxed). A contact the tracker dropped still \
counts as a lost contact (get_tracks lists them under lost): you cannot accept one, only an \
operator with engagement authority can, for one engagement. While \
an engagement is in play on a vehicle, anything you propose for that vehicle waits for a \
human. get_changes answers what changed since a cursor (new tracks, affiliations, \
declarations, lost links, navigation and GPS glitch flags, alerts, intents' states): read \
the full picture with the other tools before you act on it.";

/// Who may connect (see `main.rs`): a credentials run checks each agent's key (`keys`); a
/// dev run believes the ids `MURMUR_AGENTS` lists.
pub struct Settings {
    /// Dev run: the ids an `X-Murmur-Agent` header (or `clientInfo.name`) may claim.
    pub agents: Vec<AgentId>,
    /// Credentials run: the credential file's agent keys (`Authorization: Bearer <key>`).
    pub keys: Option<Arc<crate::auth::Auth>>,
    /// How long an MCP session may be idle before it is dropped ([`IDLE_LIMIT`];
    /// `MURMUR_MCP_IDLE_S`). Only clients on a protocol version before 2026-07-28 hold a
    /// session: from that version requests are served statelessly.
    pub idle: std::time::Duration,
}

/// An MCP session idle this long is dropped (a client whose connection vanished leaves no
/// session behind). Fifteen minutes: only clients on a protocol before 2026-07-28 hold a
/// session (Claude Code 2.1.294 holds none); the watch officer's own reads come every 2 s and
/// the situation storyline connects once per proposal, so no known client is quiet longer
/// than the watch's heartbeat ceiling (5 minutes) between calls. Three times that leaves
/// room, where rmcp's own five minutes dropped an agent that connected once and waited, and
/// a leaked session lives a quarter of an hour, not an hour.
pub const IDLE_LIMIT: std::time::Duration = std::time::Duration::from_secs(900);

/// The sessions one agent id may hold, counted from each one's initialize; past this the one
/// it used least recently with no request in flight is closed (all busy: the cap waits).
/// Eight: the watch officer holds one (its reads) plus one per Codex turn in flight, the
/// situation storyline one per proposal, closed after; eight leaves room for a few clients
/// sharing an id while a client that opens sessions and never closes them holds eight at
/// most, not one per retry for the idle limit.
pub const SESSIONS_PER_AGENT: usize = 8;

impl Settings {
    pub fn from_env() -> Settings {
        let list = std::env::var("MURMUR_AGENTS")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "agent-planner,agent-watch,agent-playbook".into());
        let idle = std::env::var("MURMUR_MCP_IDLE_S")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .map_or(IDLE_LIMIT, std::time::Duration::from_secs);
        Settings {
            agents: list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(AgentId::new)
                .collect(),
            keys: None,
            idle,
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
    /// Credentials run: agents' keys (the dev list `agents` is then unused).
    keys: Option<Arc<crate::auth::Auth>>,
    /// Agents that have connected this run (`agent.action session.connected`, once each).
    seen: Mutex<BTreeSet<AgentId>>,
    /// When each agent last read the nomination dry run (`get_nomination_options` runs every
    /// armed platform's checks and keep-out explanation): at most one a second each.
    dry_runs: Mutex<std::collections::BTreeMap<AgentId, tokio::time::Instant>>,
    /// `get_changes`: what changed between reads, numbered for a cursor (read-only).
    pub(crate) changes: changes::Feed,
    /// Each agent's MCP sessions: when each was last used and its requests in flight
    /// ([`SESSIONS_PER_AGENT`]).
    sessions: Mutex<std::collections::BTreeMap<AgentId, Vec<Sess>>>,
    /// The session manager, to close a session past the cap (set by [`service`]).
    manager: std::sync::OnceLock<Arc<LocalSessionManager>>,
}

/// One MCP session of an agent, for the cap.
struct Sess {
    id: String,
    /// When a request on it last began or ended.
    used: tokio::time::Instant,
    /// Its requests in flight: the cap never closes a session while one runs (a proposal
    /// whose answer is lost would be made again by a client that retries).
    busy: usize,
}

/// A request in flight on a session (see [`Sess::busy`]); ends when dropped.
pub(crate) struct Busy {
    mcp: Arc<Mcp>,
    agent: AgentId,
    id: String,
}

impl Drop for Busy {
    fn drop(&mut self) {
        let mut all = self.mcp.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = all
            .get_mut(&self.agent)
            .and_then(|v| v.iter_mut().find(|s| s.id == self.id))
        {
            s.busy = s.busy.saturating_sub(1);
            s.used = tokio::time::Instant::now();
        }
    }
}

/// The session a request names (`Mcp-Session-Id`; none from a client on protocol 2026-07-28
/// or later: it holds no session).
fn session_of(ctx: &RequestContext<RoleServer>) -> Option<String> {
    ctx.extensions
        .get::<axum::http::request::Parts>()
        .and_then(|p| p.headers.get("mcp-session-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// The least time between two of one agent's nomination dry runs.
pub const DRY_RUN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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
            keys: settings.keys,
            seen: Mutex::new(BTreeSet::new()),
            dry_runs: Mutex::new(std::collections::BTreeMap::new()),
            changes: changes::Feed::default(),
            sessions: Mutex::new(std::collections::BTreeMap::new()),
            manager: std::sync::OnceLock::new(),
        })
    }

    /// Whether `agent` may run a nomination dry run now ([`DRY_RUN_INTERVAL`] since its last),
    /// recording it if so; else how long to wait.
    pub(crate) fn dry_run_slot(&self, agent: &AgentId) -> Result<(), std::time::Duration> {
        let now = tokio::time::Instant::now();
        let mut last = self.dry_runs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(t) = last.get(agent)
            && now.duration_since(*t) < DRY_RUN_INTERVAL
        {
            return Err(DRY_RUN_INTERVAL - now.duration_since(*t));
        }
        last.insert(agent.clone(), now);
        Ok(())
    }

    /// The agent a request acts for. Credentials run: the agent whose key the request's
    /// `Authorization: Bearer` header carries (an `X-Murmur-Agent` header naming another is
    /// refused). Dev run: the `X-Murmur-Agent` header, else the session's `clientInfo.name`,
    /// listed in `MURMUR_AGENTS`. Logged once per agent and run as `agent.action
    /// session.connected` (with the key's id, never the key).
    pub(crate) fn agent(&self, ctx: &RequestContext<RoleServer>) -> Result<AgentId, ErrorData> {
        let parts = ctx.extensions.get::<axum::http::request::Parts>();
        if let Some(keys) = &self.keys {
            let (agent, key_id) = parts
                .and_then(|p| crate::auth::bearer(&p.headers))
                .and_then(|k| keys.agent_for_key(k))
                .ok_or_else(|| {
                    ErrorData::invalid_request(
                        "an agent connects with its own key: Authorization: Bearer <key> (murmur-auth agent-add)",
                        None,
                    )
                })?;
            let named = parts
                .and_then(|p| p.headers.get(AGENT_HEADER))
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if let Some(n) = named
                && n != agent.as_str()
            {
                return Err(ErrorData::invalid_request(
                    format!("the key is {agent}'s, the request names {n}"),
                    None,
                ));
            }
            let via = format!("API key {key_id}");
            self.connected(&agent, &via, ctx.peer.peer_info().as_deref());
            self.note_session(&agent, parts);
            return Ok(agent);
        }
        let header = parts
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
                 agent id listed in MURMUR_AGENTS (a dev run: the id is asserted, not authenticated)",
                None,
            )
        })?;
        let agent = self.resolve(&id)?;
        self.connected(&agent, via, client.as_deref());
        self.note_session(&agent, parts);
        Ok(agent)
    }

    /// Record that `agent` used the session the request names (`Mcp-Session-Id`; a client on
    /// protocol 2026-07-28 or later names none: it holds no session).
    fn note_session(&self, agent: &AgentId, parts: Option<&axum::http::request::Parts>) {
        if let Some(id) = parts
            .and_then(|p| p.headers.get("mcp-session-id"))
            .and_then(|v| v.to_str().ok())
        {
            self.note_session_id(agent, id);
        }
    }

    /// Record that `agent` holds or used session `id` (at its initialize, and at each call).
    /// Past [`SESSIONS_PER_AGENT`], the agent's session used least recently with no request
    /// in flight is closed (none idle: the cap waits for one).
    fn note_session_id(&self, agent: &AgentId, id: &str) {
        let id = id.to_string();
        let now = tokio::time::Instant::now();
        let evict = {
            let mut all = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            let mine = all.entry(agent.clone()).or_default();
            // a session the manager no longer holds (closed by its client, which DELETE does
            // without reaching this handler, or idle past the limit) does not count
            if let Some(manager) = self.manager.get()
                && let Ok(live) = manager.sessions.try_read()
            {
                mine.retain(|s| s.id == id || s.busy > 0 || live.contains_key(s.id.as_str()));
            }
            match mine.iter_mut().find(|s| s.id == id) {
                Some(s) => s.used = now,
                None => mine.push(Sess {
                    id: id.clone(),
                    used: now,
                    busy: 0,
                }),
            }
            let mut out = Vec::new();
            while mine.len() > SESSIONS_PER_AGENT {
                let oldest = mine
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.id != id && s.busy == 0)
                    .min_by_key(|(_, s)| s.used)
                    .map(|(i, _)| i);
                match oldest {
                    Some(i) => out.push(mine.remove(i).id),
                    None => break,
                }
            }
            out
        };
        if evict.is_empty() {
            return;
        }
        if let Some(manager) = self.manager.get().cloned() {
            tokio::spawn(async move {
                for id in evict {
                    let id: rmcp::transport::common::server_side_http::SessionId = id.into();
                    let _ = rmcp::transport::streamable_http_server::session::SessionManager::close_session(
                        manager.as_ref(),
                        &id,
                    )
                    .await;
                }
            });
        }
    }

    /// A request in flight on `agent`'s session `id` (the guard ends it); none without one.
    pub(crate) fn busy(self: &Arc<Self>, agent: &AgentId, id: Option<String>) -> Option<Busy> {
        let id = id?;
        let mut all = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let s = all.get_mut(agent)?.iter_mut().find(|s| s.id == id)?;
        s.busy += 1;
        Some(Busy {
            mcp: self.clone(),
            agent: agent.clone(),
            id,
        })
    }

    /// Each session `agent` holds, oldest use first, with its requests in flight (tests).
    #[cfg(test)]
    pub(crate) fn sessions_of(&self, agent: &AgentId) -> Vec<(String, usize)> {
        let all = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<&Sess> = all.get(agent).map(|v| v.iter().collect()).unwrap_or_default();
        v.sort_by_key(|s| s.used);
        v.into_iter().map(|s| (s.id.clone(), s.busy)).collect()
    }

    /// The agent a session's opening request names, resolved as [`Mcp::agent`] resolves it:
    /// the key's agent in a credentials run; else the header, or the initialize request's
    /// `clientInfo.name`, listed in `MURMUR_AGENTS`.
    fn claimant(&self, headers: &axum::http::HeaderMap, body: &[u8]) -> Option<AgentId> {
        if let Some(keys) = &self.keys {
            return crate::auth::bearer(headers)
                .and_then(|k| keys.agent_for_key(k))
                .map(|(a, _)| a);
        }
        let named = headers
            .get(AGENT_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v["params"]["clientInfo"]["name"].as_str().map(str::to_string))
            })?;
        self.resolve(&named).ok()
    }

    /// A claimed id against the configured list.
    pub(crate) fn resolve(&self, id: &str) -> Result<AgentId, ErrorData> {
        self.agents
            .iter()
            .find(|a| a.as_str() == id)
            .cloned()
            .ok_or_else(|| {
                ErrorData::invalid_request(
                    format!("agent {id:?} is not in MURMUR_AGENTS (a dev run: ids are asserted, not authenticated)"),
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

type Inner = StreamableHttpService<Handler, LocalSessionManager>;
type InnerResponse = <Inner as tower::Service<Request<Body>>>::Response;

/// The MCP service at `/mcp`: rmcp's, with each new session recorded for its agent as it is
/// made (a session that initializes and never calls a tool counts against the cap too). The
/// handler cannot do it: rmcp makes the session id after the handler is built, and the
/// initialize request names none.
#[derive(Clone)]
pub struct McpService {
    inner: Inner,
    shared: Arc<Mcp>,
}

impl tower::Service<Request<Body>> for McpService {
    type Response = InnerResponse;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<InnerResponse, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let mut inner = self.inner.clone();
        let shared = self.shared.clone();
        Box::pin(async move {
            // a POST that names no session opens one (initialize); any other request passes
            if req.method() != Method::POST || req.headers().contains_key("mcp-session-id") {
                return inner.call(req).await;
            }
            let (parts, body) = req.into_parts();
            let bytes = match axum::body::to_bytes(body, OPENING_BODY_LIMIT).await {
                Ok(b) => b,
                Err(e) => {
                    let too_large = e
                        .into_inner()
                        .downcast_ref::<http_body_util::LengthLimitError>()
                        .is_some();
                    return Ok(if too_large {
                        plain(
                            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                            "an opening MCP request is at most 1 MB",
                        )
                    } else {
                        plain(axum::http::StatusCode::BAD_REQUEST, "the request body could not be read")
                    });
                }
            };
            // an unnamed or unlisted claimant's sessions share one bucket under the cap (its
            // calls are refused anyway; its sessions must not pile up uncounted)
            let agent = shared
                .claimant(&parts.headers, &bytes)
                .unwrap_or_else(|| AgentId::new(UNLISTED));
            let resp = inner.call(Request::from_parts(parts, Body::from(bytes))).await?;
            if let Some(id) = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) {
                shared.note_session_id(&agent, id);
            }
            Ok(resp)
        })
    }
}

/// The largest opening request read whole (an initialize is a few hundred bytes).
const OPENING_BODY_LIMIT: usize = 1 << 20;

/// The cap's bucket for sessions opened by a claimant that names no listed agent (or, in a
/// credentials run, no valid key): not an agent id anyone can be given.
const UNLISTED: &str = "(unnamed or unlisted)";

/// A plain-text answer of the inner service's type.
fn plain(status: axum::http::StatusCode, text: &'static str) -> InnerResponse {
    use http_body_util::BodyExt;
    let mut resp = axum::http::Response::new(http_body_util::Full::new(axum::body::Bytes::from(text)).boxed());
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, axum::http::HeaderValue::from_static("text/plain"));
    resp
}

/// The service to mount at `/mcp`. `api` is the bridge's router without `/mcp`.
pub fn service(settings: Settings, world: Arc<World>, decision: Arc<Decision>, api: Router) -> McpService {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(settings.idle);
    let sessions = Arc::new(sessions);
    let shared = Mcp::new(settings, world, decision, api);
    let _ = shared.manager.set(sessions.clone());
    let handler = shared.clone();
    McpService {
        inner: StreamableHttpService::new(
            move || Ok(Handler(handler.clone())),
            sessions,
            StreamableHttpServerConfig::default(),
        ),
        shared,
    }
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
        Ok(ListToolsResult::with_all_items(tools::tools())
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let agent = self.0.agent(&ctx)?;
        let _busy = self.0.busy(&agent, session_of(&ctx));
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
        Ok(ListResourcesResult::with_all_items(resources())
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(ListResourceTemplatesResult::with_all_items(templates())
            .with_ttl_ms(LIST_TTL_MS)
            .with_cache_scope(CacheScope::Private))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let agent = self.0.agent(&ctx)?;
        let _busy = self.0.busy(&agent, session_of(&ctx));
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
            "Tracks of contacts that are not ours, as our own sensors see them (perception only): estimated position and its uncertainty now, velocity, existence confidence, identity estimate with confidence and conflicts, affiliation estimate with confidence and reason (hostile only when declared, by an operator or by the engine's hostile-act rule on an observed act, with the basis in declared and the review flags in force in review), the sensors and vehicles behind each track, and last_seen as as_of with age_s and stale.",
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
        "fleet" => world::fleet(mcp, None, false),
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
