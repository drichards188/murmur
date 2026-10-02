//! The tool table and dispatch. Every description is written for the model: what the tool
//! reads or proposes, and that nothing here approves or drives. The tool list is what
//! `tests::no_tool_implies_approval_or_control` checks.

use axum::http::Method;
use murmur_decide::{AgentId, Constraints};
use rmcp::ErrorData;
use rmcp::handler::server::tool::schema_for_type;
use rmcp::model::{CallToolResult, JsonObject, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{Mcp, world};
use crate::decision::{DecisionError, IntentRequest};
use crate::tasks::TaskParams;

/// Said on every read: how to treat the ages on each fact.
pub const STALE_NOTE: &str = "Every fact carries as_of (wall-clock seconds when it was \
observed) and age_s; vehicle facts also carry stale, true when the fact is older than the \
freshness limit the rules apply to it (freshness_limit_s). A stale fact cannot support a \
decision: treat it as uncertain and say so in your rationale.";

#[derive(Deserialize, JsonSchema)]
pub struct NoParams {}

#[derive(Deserialize, JsonSchema)]
pub struct FleetParams {
    /// Only these fleet uids (default: every vehicle).
    pub uids: Option<Vec<u64>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct VehicleParams {
    /// The vehicle's fleet uid (from get_fleet).
    pub uid: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct IntentsParams {
    /// Only intents in this state: proposed, checked, auto_approved, pending_approval,
    /// approved, rejected, expired, voided, dispatched, done or failed.
    pub state: Option<String>,
    /// Only intents this agent proposed.
    pub mine: Option<bool>,
    /// At most this many, newest first (default 50).
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EventsParams {
    /// Only events of this vehicle (fleet uid).
    pub uid: Option<u64>,
    /// At most this many, newest first (default 50).
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PassesParams {
    /// A named area from the scenario (see the catalog's areas), a box as "w,s,e,n" in
    /// degrees, or omitted for the scenario area.
    pub area: Option<String>,
    /// Window start, RFC 3339 in scenario time (default: now).
    pub from: Option<String>,
    /// Window end, RFC 3339 (default: six hours after the start; at most three days).
    pub to: Option<String>,
    /// Only these satellite classes (ids from the space catalog).
    pub classes: Option<Vec<String>>,
    /// Keep optical windows in darkness (default true; they are flagged daylight_ok false).
    pub include_night: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TaskingParams {
    /// Collection capabilities wanted: "optical", "sar" (default both).
    pub capabilities: Option<Vec<String>>,
    /// A named area, a box as "w,s,e,n", an object {"bbox": [w, s, e, n]}, or omitted for
    /// the scenario area.
    pub area: Option<Value>,
    /// Window start, RFC 3339 in scenario time (default: now).
    pub from: Option<String>,
    /// Window end, RFC 3339 (default: the deadline, else six hours after the start).
    pub to: Option<String>,
    /// When the product (the image) must be ready, RFC 3339.
    pub deadline: Option<String>,
    /// Coarsest acceptable ground resolution, metres.
    pub max_resolution_m: Option<f64>,
    /// Ranking: "soonest" or "best" (resolution and geometry); the default is the module's.
    pub rank: Option<String>,
    /// Only these satellite classes.
    pub classes: Option<Vec<String>>,
    /// How many exclusions to list in full (default 200; the summary covers all).
    pub exclusions: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
pub struct IntentParams {
    /// The task kind: orbit, patrol, hold, rtl or track.
    pub task: String,
    /// Fleet uids of the vehicles to task (from get_fleet). At least one.
    pub uids: Vec<u64>,
    /// orbit: the centre as [lon, lat].
    pub center: Option<[f64; 2]>,
    /// orbit: the radius in metres (default 200). The rules require at least 1.1 times the
    /// platform's minimum turn radius and suggest a radius when yours is too tight.
    pub radius_m: Option<f64>,
    /// patrol: the waypoints as [[lon, lat], ...], flown as a loop until retasked.
    pub route: Option<Vec<[f64; 2]>>,
    /// track: the fleet uid of the vehicle to follow (antenna trackers only).
    pub target: Option<u64>,
    /// Constraint: the endurance to keep in reserve after the task, percent of full. Can
    /// only tighten the policy's reserve, never loosen it.
    pub reserve_pct: Option<f64>,
    /// Constraint: the time on task the reserves estimate assumes, seconds (can only
    /// lengthen the policy's assumption).
    pub duration_s: Option<u32>,
    /// Why this task, on these vehicles, now. Required for propose_intent (the operator
    /// reads it in the inbox and it goes to the audit log); optional for preview_intent.
    /// Name the facts you relied on and any that were stale.
    pub rationale: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct IdParams {
    /// The intent id (from propose_intent, get_intents or get_inbox).
    pub id: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct WithdrawParams {
    /// The id of an intent this agent proposed.
    pub id: u64,
    /// Why it is withdrawn (goes to the audit log).
    pub reason: String,
}

impl IntentParams {
    fn request(self) -> IntentRequest {
        IntentRequest {
            task: self.task,
            uids: self.uids,
            params: TaskParams {
                center: self.center,
                radius: self.radius_m,
                route: self.route,
                target: self.target,
            },
            constraints: Constraints {
                reserve_pct: self.reserve_pct,
                duration_s: self.duration_s,
            },
            rationale: self.rationale,
        }
    }
}

fn tool<T: JsonSchema + 'static>(name: &'static str, description: String, read_only: bool) -> Tool {
    let mut t = Tool::new(name, description, schema_for_type::<T>());
    let mut a = ToolAnnotations::new();
    a.read_only_hint = Some(read_only);
    a.destructive_hint = Some(false);
    a.open_world_hint = Some(false);
    t.annotations = Some(a);
    t
}

/// Every tool the server offers, in the order a model should read them.
pub fn tools() -> Vec<Tool> {
    let read = |name, description: &str| tool::<NoParams>(name, format!("{description} {STALE_NOTE}"), true);
    vec![
        tool::<FleetParams>(
            "get_fleet",
            format!(
                "Read every vehicle in the fleet: identity (uid, callsign, platform), the \
                 platform's limits and the tasks its kind can do, position, navigation \
                 readiness and source, battery, home, link health and the task it is on. \
                 {STALE_NOTE} A vehicle whose link is lost or whose position is stale is not \
                 taskable: the readiness check will refuse it."
            ),
            true,
        ),
        tool::<VehicleParams>(
            "get_vehicle",
            format!("Read one vehicle by fleet uid, with the same facts as get_fleet. {STALE_NOTE}"),
            true,
        ),
        read(
            "get_platforms",
            "Read the platform catalog: for each platform type its limits (cruise speed, \
             endurance, minimum turn radius, cruise altitude), link class, whether it is \
             armed (every task on an armed platform needs a human), the tasks its autopilot \
             kind can do, and which fleet vehicles are of that type.",
        ),
        read(
            "get_areas",
            "Read the operator's areas in force: approved operating areas (a route that \
             leaves one needs a human) and no-go areas (the geofence check refuses any \
             route that enters one), as polygons of [lon, lat].",
        ),
        read(
            "get_policy",
            "Read the risk-tier policy and rules configuration in force (first matching tier \
             wins; hold and rtl are auto; armed platforms, leaving the approved area and \
             unverifiable reserves need a human). An agent's proposal in a human tier always \
             waits for an operator, whatever the tier says about submitters.",
        ),
        tool::<IntentsParams>(
            "get_intents",
            format!(
                "Read the intents of this run, newest first: state, source (human or agent \
                 and its id), task, vehicles, checks, policy decision, approval and history. \
                 Filter by state or to your own. {STALE_NOTE}"
            ),
            true,
        ),
        read(
            "get_inbox",
            "Read what waits for the operator: pending approvals (each with why it needs a \
             human and when it expires) and the current alerts (lost links, vehicles not \
             moving, vehicles that left their task, failed tasks, ACK timeouts). Pending \
             items can be decided only by a human operator.",
        ),
        tool::<EventsParams>(
            "get_recent_events",
            format!(
                "Read recent vehicle events the audit log recorded: mode changes, failsafes \
                 and status text from the autopilots, newest first. {STALE_NOTE}"
            ),
            true,
        ),
        tool::<PassesParams>(
            "get_satellite_passes",
            "Read imaging satellite access windows over an area in a time window (read-only: \
             nothing is tasked). Each window has its satellite, class, times, elevation, sun \
             and daylight, and the scene it would cover. The answer says whether orbits are \
             TIME-SHIFTED (propagated whole days after the scenario date, so pass times are \
             representative, not actual)."
                .into(),
            true,
        ),
        tool::<TaskingParams>(
            "get_satellite_tasking_options",
            "Read ranked options for a satellite collection (imagery or SAR) over an area \
             before a deadline, with the reasons every other pass was excluded (read-only: \
             nothing is requested; any collection needs a human decision through the \
             decision pipeline)."
                .into(),
            true,
        ),
        tool::<IntentParams>(
            "preview_intent",
            "Dry run of a proposal: run the fail-closed checks and the risk-tier policy on \
             the current world without creating an intent. Returns each check's outcome and \
             detail (with a suggested fix when there is one), the policy tier and rule, why \
             a human would be needed, and the state the intent would reach: rejected, \
             auto_approved (policy lets it run) or pending_approval (an operator decides). \
             Nothing is created and nothing moves; the preview is logged under your agent id. \
             Use it before propose_intent."
                .into(),
            true,
        ),
        tool::<IntentParams>(
            "propose_intent",
            "Propose an intent: a task on one or more vehicles, with constraints and a \
             required rationale. The engine checks it (fail closed) and the policy routes \
             it: an auto tier runs it at once (the response says dispatched true); a human \
             tier puts it in the operator's inbox with your id and rationale, where only a \
             listed human operator can decide it, and it expires if nobody does. The \
             response is the intent with its checks, policy decision and state. You cannot \
             move it further yourself; follow it with explain_intent and withdraw it with \
             withdraw_intent while it waits."
                .into(),
            false,
        ),
        tool::<IdParams>(
            "explain_intent",
            "Explain one intent: every check with its outcome and detail, the policy tier \
             and rule it matched, why it needs a human (armed platform, leaving the approved \
             area, an unverifiable check), who decided it and how (policy, an operator in \
             the inbox, or the operator's own submission), its history, its tasking once \
             dispatched, and a plain summary of where it stands."
                .into(),
            true,
        ),
        tool::<WithdrawParams>(
            "withdraw_intent",
            "Withdraw an intent you proposed, while it waits (pending or approved but not \
             yet dispatched). Another agent's or a human's intent is refused, and so is one \
             already dispatched or ended. The withdrawal and your reason go to the audit log."
                .into(),
            false,
        ),
    ]
}

/// Arguments as the tool's parameter type; a mismatch is a result the model can read.
fn parse<T: DeserializeOwned>(args: JsonObject) -> Result<T, Box<CallToolResult>> {
    serde_json::from_value(Value::Object(args)).map_err(|e| {
        Box::new(CallToolResult::structured_error(
            json!({ "error": format!("bad arguments: {e}") }),
        ))
    })
}

/// A decision-layer refusal, as a tool error the model can read (status, detail, and the
/// intent when one exists).
fn refusal(e: DecisionError) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": e.detail,
        "status": e.status,
        "intent": e.intent,
    }))
}

fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b',' | b':' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn query(pairs: &[(&str, Option<String>)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={}", enc(v))))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// Dispatch one call. Unknown tools are a protocol error; everything else answers with a
/// result the model can read, refusals included.
pub async fn call(
    mcp: &Mcp,
    agent: &AgentId,
    name: &str,
    args: JsonObject,
) -> Result<CallToolResult, ErrorData> {
    macro_rules! params {
        ($t:ty) => {
            match parse::<$t>(args) {
                Ok(p) => p,
                Err(r) => return Ok(*r),
            }
        };
    }
    let out: Result<Value, DecisionError> = match name {
        "get_fleet" => {
            let p = params!(FleetParams);
            Ok(world::fleet(mcp, p.uids.as_deref()))
        }
        "get_vehicle" => {
            let p = params!(VehicleParams);
            world::vehicle(mcp, p.uid).ok_or_else(|| DecisionError {
                status: 404,
                detail: format!("no vehicle with uid {}", p.uid),
                intent: None,
            })
        }
        "get_platforms" => world::platforms(mcp).await,
        "get_areas" => Ok(world::areas(mcp)),
        "get_policy" => Ok(world::policy(mcp)),
        "get_intents" => {
            let p = params!(IntentsParams);
            Ok(world::intents(
                mcp,
                agent,
                p.state.as_deref(),
                p.mine.unwrap_or(false),
                p.limit.unwrap_or(50),
            ))
        }
        "get_inbox" => Ok(world::inbox(mcp)),
        "get_recent_events" => {
            let p = params!(EventsParams);
            Ok(world::events(mcp, p.uid, p.limit.unwrap_or(50)))
        }
        "get_satellite_passes" => {
            let p = params!(PassesParams);
            let q = query(&[
                ("area", p.area),
                ("from", p.from),
                ("to", p.to),
                ("classes", p.classes.map(|c| c.join(","))),
                (
                    "include_night",
                    p.include_night.map(|b| if b { "1" } else { "0" }.to_string()),
                ),
            ]);
            mcp.api(Method::GET, &format!("/api/space/passes{q}"), None)
                .await
        }
        "get_satellite_tasking_options" => {
            let p = params!(TaskingParams);
            let mut body = serde_json::Map::new();
            let mut put = |k: &str, v: Option<Value>| {
                if let Some(v) = v {
                    body.insert(k.into(), v);
                }
            };
            put("capabilities", p.capabilities.map(|c| json!(c)));
            put("area", p.area);
            put("from", p.from.map(Value::String));
            put("to", p.to.map(Value::String));
            put("deadline", p.deadline.map(Value::String));
            put("max_resolution_m", p.max_resolution_m.map(|v| json!(v)));
            put("rank", p.rank.map(Value::String));
            put("classes", p.classes.map(|c| json!(c)));
            put("exclusions", p.exclusions.map(|v| json!(v)));
            mcp.api(Method::POST, "/api/space/task", Some(Value::Object(body)))
                .await
        }
        "preview_intent" => {
            let p = params!(IntentParams);
            mcp.decision.agent_preview(agent, p.request()).await
        }
        "propose_intent" => {
            let p = params!(IntentParams);
            mcp.decision
                .agent_propose(agent, p.request())
                .await
                .map(|o| {
                    let dispatched = o.tasking.is_some();
                    let state = serde_json::to_value(o.state).unwrap_or(Value::Null);
                    json!({
                        "id": o.id.0,
                        "state": state,
                        "dispatched": dispatched,
                        "intent": o.intent,
                        "tasking": o.tasking,
                        "summary": mcp.decision.explain_json(o.id.0)
                            .and_then(|v| v["summary"].as_str().map(str::to_string)),
                    })
                })
        }
        "explain_intent" => {
            let p = params!(IdParams);
            mcp.decision.explain_json(p.id).ok_or_else(|| DecisionError {
                status: 404,
                detail: format!("unknown intent {}", p.id),
                intent: None,
            })
        }
        "withdraw_intent" => {
            let p = params!(WithdrawParams);
            mcp.decision
                .agent_withdraw(agent, p.id, &p.reason)
                .map(|o| {
                    json!({
                        "id": o.id.0,
                        "state": serde_json::to_value(o.state).unwrap_or(Value::Null),
                        "intent": o.intent,
                    })
                })
        }
        _ => {
            let names: Vec<_> = tools().into_iter().map(|t| t.name).collect();
            return Err(ErrorData::invalid_params(
                format!("unknown tool {name:?}; the tools are {}", names.join(", ")),
                None,
            ));
        }
    };
    Ok(match out {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => refusal(e),
    })
}
