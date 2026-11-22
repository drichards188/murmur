//! The tasking tools: what an agent (the watch officer) may do with the operator's own stores
//! ([`super::OperatorStores`]), offered only by a bridge that keeps them (the live one).
//!
//! - `get_groups` reads the operator's groups.
//! - `make_group` makes a group for one of the operator's tasking items in force, or changes one
//!   the agent made for it while no operator has changed it since (`crate::groups`, its agent
//!   path: every member in the fleet, at most `AGENT_GROUPS_MAX` a run). It never deletes one,
//!   and the operator's own routes stay closed to agents.
//! - `reply_tasking` answers one item: an outcome, a short text, the intents the agent proposed
//!   for it (each checked to be its own), and the groups it made for it (from the store).
//!
//! None of it reaches the engine: a group and a reply decide nothing, change no intent and are
//! never logged (the tests compare the audit log before and after). Nothing here can make,
//! retire or edit a tasking item.

use std::collections::HashSet;

use murmur_decide::AgentId;
use rmcp::model::{CallToolResult, JsonObject, Tool};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{Mcp, OperatorStores};
use crate::groups::{AGENT_GROUPS_MAX, AgentGroup, AgentGroupError, AgentPatch, NAME_MAX};
use crate::tasking::{Outcome, REPLIES_MAX, REPLY_INTENTS_MAX, REPLY_LINES_MAX, REPLY_MAX, Reply, ReplyError};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MakeGroupParams {
    /// The operator's tasking item the group is for (its id in the update's Operator tasking,
    /// such as tk-3): one still in force.
    pub tasking_id: String,
    /// To change a group you made for that item (its id, such as og-4) instead of making one.
    pub group_id: Option<String>,
    /// The group's name, at most 64 characters (required to make one).
    pub name: Option<String>,
    /// Its members as fleet uids, every one in the fleet (required to make one; changing a group,
    /// they replace its members).
    pub members: Option<Vec<u64>>,
    /// A CSS colour such as #7fb8ff (optional).
    pub color: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyParams {
    /// The operator's tasking item you answer (tk-3).
    pub tasking_id: String,
    /// done (every order it needs proposed, or the groups it asked for made), partial (some of
    /// it), blocked (none of it could go) or declined (not yours to do).
    pub status: String,
    /// Bottom line first: line 1 what you did and where it stands now, then up to 4 lines
    /// starting "- " (what was refused and why, what would let it go). At most 600 characters.
    pub text: String,
    /// The ids of the intents you proposed for this item (yours only).
    #[serde(default)]
    pub intents: Vec<u64>,
}

fn now_ms() -> u64 {
    (crate::clock::wall() * 1000.0) as u64
}

fn refused(error: impl Into<String>) -> CallToolResult {
    CallToolResult::structured_error(json!({ "error": error.into() }))
}

fn parse<T: DeserializeOwned>(args: &JsonObject) -> Result<T, Box<CallToolResult>> {
    serde_json::from_value(Value::Object(args.clone())).map_err(|e| Box::new(refused(format!("bad arguments: {e}"))))
}

/// The tasking tools ([`super::Mcp::tool_list`] adds them where the operator's stores are kept).
pub fn tools() -> Vec<Tool> {
    use super::tools::tool;
    vec![
        tool::<super::tools::NoParams>(
            "get_groups",
            "Read the operator's groups: named sets of our vehicles kept in the console, made by \
             the operator or, for one of the operator's tasking items, by an agent (make_group). \
             Each group's id, name, colour, members (fleet uids), who made it (by; agent true and \
             tasking for an agent's) and who changed it last (changed_by). Organisation only: a \
             group decides nothing and orders nothing; to task its members, name their uids in \
             preview_intent and propose_intent. Read-only, never logged."
                .into(),
            true,
        ),
        tool::<MakeGroupParams>(
            "make_group",
            format!(
                "Make a group of our vehicles for one of the operator's tasking items in force \
                 (tasking_id, from the update's Operator tasking), or change one you made for it \
                 (group_id): a name (at most {NAME_MAX} characters), its members (fleet uids, every \
                 one in the fleet; changing a group, they replace its members) and an optional \
                 colour. Use it where the operator asks for groups, or splits a set they will want \
                 to task again. A group is organisation only: it decides nothing, orders nothing and \
                 is never logged; the operator sees it in the console's Groups list marked as the \
                 watch officer's, for that item, and may change or delete it. You cannot change a \
                 group the operator made, another agent's, or one an operator has changed since \
                 you made it, and no tool deletes one. At most {AGENT_GROUPS_MAX} a run."
            ),
            false,
        ),
        tool::<ReplyParams>(
            "reply_tasking",
            format!(
                "Answer one of the operator's tasking items (tasking_id) once you have done what \
                 you can for it: status done, partial, blocked or declined; the ids of the intents \
                 you proposed for it (yours only; the groups you made for it are added from the \
                 store); and a short text, bottom line first: line 1 what you did and where it \
                 stands now (an order that waits for the operator says so), then up to {} lines \
                 starting \"- \" (what a check refused and why, what would let it go), at most \
                 {REPLY_MAX} characters. The console shows it under the item with its status and \
                 links to those intents. A reply is not an order and not a decision, and is not \
                 logged. At most {REPLIES_MAX} an item: answer again only when the outcome changes.",
                REPLY_LINES_MAX - 1
            ),
            false,
        ),
    ]
}

/// One call of a tasking tool, or None for another tool's name.
pub fn call(mcp: &Mcp, ops: &OperatorStores, agent: &AgentId, name: &str, args: &JsonObject) -> Option<CallToolResult> {
    Some(match name {
        "get_groups" => {
            let groups = ops.groups.read().unwrap_or_else(|e| e.into_inner()).list();
            CallToolResult::structured(json!({
                "t": crate::clock::wall(),
                "count": groups.len(),
                "note": "The operator's groups, oldest first: organisation only, never an order. A group an agent made says agent true and the tasking item it was made for; changed_by names whoever changed it last.",
                "groups": groups,
            }))
        }
        "make_group" => match parse::<MakeGroupParams>(args) {
            Ok(p) => make_group(mcp, ops, agent, p),
            Err(r) => *r,
        },
        "reply_tasking" => match parse::<ReplyParams>(args) {
            Ok(p) => reply(mcp, ops, agent, p),
            Err(r) => *r,
        },
        _ => return None,
    })
}

/// Why an item is not one an agent may make a group for now, if it is not.
fn not_in_force(ops: &OperatorStores, id: &str) -> Option<String> {
    let store = ops.tasking.read().unwrap_or_else(|e| e.into_inner());
    match store.get(id) {
        None => Some(format!("no tasking {id}: the update's Operator tasking lists the items in force")),
        Some(i) if !i.active => Some(format!("{id} was retired by the operator: it is no longer in force")),
        Some(_) => None,
    }
}

fn make_group(mcp: &Mcp, ops: &OperatorStores, agent: &AgentId, p: MakeGroupParams) -> CallToolResult {
    if let Some(why) = not_in_force(ops, &p.tasking_id) {
        return refused(why);
    }
    let known: HashSet<u64> = mcp.world.vehicles().iter().map(|v| v.uid).collect();
    let mut groups = ops.groups.write().unwrap_or_else(|e| e.into_inner());
    let done = match &p.group_id {
        None => {
            let (Some(name), Some(members)) = (p.name.as_deref(), p.members.as_deref()) else {
                return refused("making a group needs its name and its members (fleet uids)");
            };
            let asked = AgentGroup { name, members, color: p.color.as_deref() };
            groups.agent_make(agent.as_str(), &p.tasking_id, asked, &known, now_ms())
        }
        Some(id) => {
            if p.name.is_none() && p.members.is_none() && p.color.is_none() {
                return refused("changing a group needs a new name, members or colour");
            }
            let patch = AgentPatch { name: p.name, color: p.color.map(Some), members: p.members };
            groups.agent_change(id, agent.as_str(), &p.tasking_id, patch, &known, now_ms())
        }
    };
    match done {
        Ok(g) => CallToolResult::structured(json!({
            "group": g,
            "note": "Organisation only: the operator sees it in the console's Groups list as the watch officer's, and nothing is ordered. To task its members, name their uids in preview_intent and propose_intent.",
        })),
        Err(e) => {
            let status = match e {
                AgentGroupError::Invalid(_) => 400,
                AgentGroupError::NotYours(_) => 403,
                AgentGroupError::Unknown(_) => 404,
                AgentGroupError::Spent(_) => 409,
            };
            CallToolResult::structured_error(json!({ "error": e.to_string(), "status": status }))
        }
    }
}

fn reply(mcp: &Mcp, ops: &OperatorStores, agent: &AgentId, p: ReplyParams) -> CallToolResult {
    let Some(status) = Outcome::parse(&p.status) else {
        return refused(format!("status is done, partial, blocked or declined, not {:?}", p.status));
    };
    if p.intents.len() > REPLY_INTENTS_MAX {
        return refused(format!("a reply names at most {REPLY_INTENTS_MAX} intents"));
    }
    // each intent named must be one this agent proposed: a reply never speaks for anyone else's
    for &id in &p.intents {
        let source = mcp.decision.intent_json(id).map(|v| v["intent"]["source"].clone());
        match source {
            None => return refused(format!("no intent {id}")),
            Some(s) if s["kind"].as_str() != Some("agent") || s["id"].as_str() != Some(agent.as_str()) => {
                return refused(format!("intent {id} is not one you proposed: a reply names only your own"));
            }
            Some(_) => {}
        }
    }
    let groups: Vec<String> = ops
        .groups
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .made_for(agent.as_str(), &p.tasking_id)
        .into_iter()
        .map(|g| g.id.clone())
        .collect();
    let r = Reply { t: now_ms(), agent: agent.to_string(), status, text: p.text, intents: p.intents, groups };
    match ops.tasking.write().unwrap_or_else(|e| e.into_inner()).reply(&p.tasking_id, r) {
        Ok(item) => CallToolResult::structured(json!({
            "item": item,
            "note": "Shown under the item in the console's Tasking drawer. Answer again only when the outcome changes.",
        })),
        Err(ReplyError::Unknown) => refused(format!("no tasking {}", p.tasking_id)),
        Err(ReplyError::Invalid(why)) => CallToolResult::structured_error(json!({ "error": why, "status": 400 })),
        Err(ReplyError::Full(why)) => CallToolResult::structured_error(json!({ "error": why, "status": 409 })),
    }
}
