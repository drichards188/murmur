//! The agent side of the decision layer (Phase F): what the MCP server (`crate::mcp`) can
//! do with the engine, and nothing more. Every write here goes through the engine's
//! [`murmur_decide::AgentPort`], which has no approve method; this module never opens an
//! operator session (`Decision::session` is not called here) and never makes an
//! [`super::AuthorizedOrder`] itself: an agent's intent reaches a vehicle only through the
//! same `after_decision` step a human's does, after the engine released it.
//!
//! Reads are the operator API's JSON plus what an agent needs to reason about freshness:
//! the rules' freshness limits and the recent vehicle events the audit log recorded.

use std::collections::VecDeque;

use murmur_decide::space::SpaceResult;
use murmur_decide::{
    AgentId, AreaSet, Outcome as CheckOutcome, Preview, Rationale, SpaceAnswer, Targets,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::*;
use crate::vehicle::VehicleEvent;

/// Vehicle events kept for agents: the last this many, fleet-wide.
const RECENT_EVENTS: usize = 500;

/// A vehicle event the audit log recorded (`vehicle.event`), kept for `get_recent_events`.
#[derive(Clone, Debug, Serialize)]
pub struct RecentEvent {
    /// Its number in the ring, from 1 (events logged together share `t`; this tells them
    /// apart, so a reader that read up to one of them misses none of the rest).
    #[serde(skip)]
    pub seq: u64,
    /// When the bridge logged it (the bridge's clock, s).
    pub t: f64,
    pub uid: Uid,
    pub callsign: Option<String>,
    /// mode, failsafe or statustext
    pub kind: String,
    pub text: String,
}

/// The ring the decision loop fills and agents read.
#[derive(Default)]
pub struct RecentEvents(Mutex<VecDeque<RecentEvent>>);

/// Rejection notes kept for agents: this many, the newest.
const REJECTION_NOTES: usize = 2000;
/// The most of one note kept, in characters.
pub const REJECTION_NOTE_CHARS: usize = 1000;

/// What an operator wrote when they rejected an intent (the audit log's `approval.rejected`
/// note; the engine keeps no note on the intent), for the agent that proposed it to read with
/// the intent: intent id to (operator, note). Display only: never an input to a decision,
/// written only after `approval.rejected` is logged, empty after a restart.
#[derive(Default)]
pub struct RejectionNotes(Mutex<BTreeMap<u64, (String, String)>>);

/// One intent as the agents' change feed compares it (`crate::mcp` `get_changes`): what can
/// change and what identifies it, taken briefly under the lock, not the whole intent with
/// its checks and policy decision.
#[derive(Clone, Debug, PartialEq)]
pub struct IntentBrief {
    pub id: u64,
    pub state: Value,
    /// `{"kind": "agent"|"human", "id": ...}`
    pub source: Value,
    /// The proposing agent, when an agent proposed it.
    pub agent: Option<String>,
    pub task: &'static str,
    pub uids: Value,
    /// An engagement's track.
    pub track: Option<String>,
    /// The last state change's reason.
    pub reason: Option<String>,
    /// A rejection's note and who wrote it.
    pub rejected_note: Option<(String, String)>,
}

/// One review flag as the change feed compares it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlagBrief {
    /// `F-<n>`
    pub id: String,
    pub kind: &'static str,
    pub status: &'static str,
    pub tracks: Vec<String>,
    pub evidence: String,
    pub raises_suspect: bool,
}

impl Decision {
    /// An agent's proposal: [`Decision::submit`]'s path through an `AgentPort`. The
    /// rationale is required (400 without one). Malformed requests (unknown task or
    /// vehicle, missing parameters) are refused before an intent exists, as for operators.
    /// An auto-tier intent is dispatched at once; a human-tier one waits in the inbox,
    /// whatever the policy says about submitters (an agent has no authority to lend).
    pub async fn agent_propose(
        &self,
        agent: &AgentId,
        req: IntentRequest,
    ) -> Result<Outcome, DecisionError> {
        let rationale = req
            .rationale
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Rationale::new)
            .transpose()
            .map_err(|e| DecisionError::new(400, e.to_string()))?
            .ok_or_else(|| {
                DecisionError::new(
                    400,
                    "an agent's proposal needs a rationale: why this task, on these vehicles, \
                     now (the operator reads it in the inbox)",
                )
            })?;
        let proposal = self.agent_proposal(&req)?;
        self.prefetch(&proposal.task, &proposal.uids).await;
        let out = {
            let mut guard = self.lock_with_space(&proposal.task).await;
            let core = &mut *guard;
            let world = self.snapshot();
            let id = core
                .engine
                .agent_port(agent.clone())
                .propose(proposal, rationale, &world, &self.terrain.oracle())
                .map_err(engine_err)?
                .id();
            self.after_decision(core, id)
        };
        self.publish();
        out
    }

    /// A dry run of an agent's proposal: the checks and the policy decision it would get
    /// right now, with no intent created (nothing in the inbox, nothing to withdraw). The
    /// world is read exactly as a real submission reads it.
    pub async fn agent_preview(
        &self,
        agent: &AgentId,
        req: IntentRequest,
    ) -> Result<Value, DecisionError> {
        let proposal = self.agent_proposal(&req)?;
        self.prefetch(&proposal.task, &proposal.uids).await;
        let preview = {
            let mut guard = self.lock_with_space(&proposal.task).await;
            guard
                .engine
                .agent_port(agent.clone())
                .preview(proposal, &self.snapshot(), &self.terrain.oracle())
                .map_err(engine_err)?
        };
        Ok(preview_json(&preview, self.tol()))
    }

    /// Withdraw one of the agent's own intents before dispatch. Another agent's or a
    /// human's intent is refused (403); so is one already dispatched or ended (409). With
    /// `if_state` (`withdraw_intent`'s), a state name (an unknown one is 400), the intent's
    /// owner and then its state are read under the same lock as the withdrawal: another's
    /// intent is 403 before its state is said, and a mismatch is 409, both before the engine is
    /// asked, so nothing is withdrawn or logged and the audit log and replay are as without the
    /// precondition (an agent that read pending_approval cannot void an approval that came
    /// meanwhile).
    pub fn agent_withdraw(
        &self,
        agent: &AgentId,
        id: u64,
        reason: &str,
        if_state: Option<&str>,
    ) -> Result<Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            if let Some(want) = if_state {
                let Ok(state) = serde_json::from_value::<State>(json!(want)) else {
                    return Err(DecisionError {
                        status: 400,
                        detail: format!("if_state {want:?} is not an intent state"),
                        intent: None,
                    });
                };
                let Some(i) = core.engine.intent(id) else {
                    return Err(DecisionError {
                        status: 404,
                        detail: format!("unknown intent {id}"),
                        intent: None,
                    });
                };
                // the owner first: another's intent is refused before its state is said
                let mine = matches!(
                    &i.spec().source,
                    murmur_decide::Source::Agent { id: a } if a == agent
                );
                if !mine {
                    return Err(engine_err(EngineError::NotOwner {
                        agent: agent.clone(),
                        id,
                    }));
                }
                if i.state() != state {
                    let now = serde_json::to_value(i.state())
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "?".into());
                    let e = DecisionError {
                        status: 409,
                        detail: format!("{id} is {now}, not {want}: not withdrawn (if_state)"),
                        intent: None,
                    };
                    return Err(self.with_intent(core, id, e));
                }
            }
            match core.engine.agent_port(agent.clone()).withdraw(id, reason) {
                Ok(_) => Ok(self.outcome(core, id)),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// Log something an agent did that isn't a proposal (`agent.action <action>`):
    /// `session.connected`, for one.
    pub fn agent_action(
        &self,
        agent: &AgentId,
        action: &str,
        detail: &str,
    ) -> Result<(), DecisionError> {
        self.lock()
            .engine
            .agent_port(agent.clone())
            .record_action(action, detail)
            .map_err(engine_err)
    }

    /// `explain_intent`: the intent with its checks, policy decision, approval and history,
    /// why it needs (or needed) a human, its tasking once dispatched, and a plain-language
    /// summary of where it stands and what would move it.
    pub fn explain_json(&self, id: u64) -> Option<Value> {
        let core = self.lock();
        let id = IntentId(id);
        let i = core.engine.intent(id)?;
        let tasking = core.taskings.get(&id).map(|t| t.snapshot());
        Some(json!({
            "intent": intent_json(i, core.taskings.get(&id).map(|t| t.id), self.tol()),
            "why_human": why_human(i),
            "summary": explain(i),
            "tasking": tasking,
            "space": core.space_runs.get(&id).map(space::SpaceRun::json),
        }))
    }

    /// `GET /api/space/runs` for agents: every dispatched collect and cover, newest first.
    pub fn space_runs(&self) -> Value {
        self.space_runs_json()
    }

    /// The policy in force (tiers, approval expiry, rules configuration), as logged in
    /// `run.started`.
    pub fn policy_json(&self) -> Value {
        serde_json::to_value(self.lock().engine.policy()).unwrap_or(Value::Null)
    }

    /// The areas the rules and policy read (approved, no-go), with their polygons.
    pub fn areas(&self) -> &AreaSet {
        &self.areas
    }

    pub fn rules(&self) -> &murmur_decide::RulesConfig {
        &self.rules
    }

    /// The world as the rules would read it right now: every vehicle's facts with the time
    /// each was observed, at engine time `t`.
    pub fn world_now(&self) -> murmur_decide::WorldSnapshot {
        self.snapshot()
    }

    /// The last events, newest first, for one vehicle or all.
    pub fn recent_events(&self, uid: Option<Uid>, limit: usize) -> Vec<RecentEvent> {
        let ring = self.recent.0.lock().unwrap_or_else(|e| e.into_inner());
        ring.iter()
            .rev()
            .filter(|e| uid.is_none_or(|u| e.uid == u))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Every intent of the run, lean (see [`IntentBrief`]), oldest first.
    pub fn intent_briefs(&self) -> Vec<IntentBrief> {
        // the core first, then the notes: the order `reject` takes them in
        let core = self.lock();
        let notes = self
            .rejection_notes
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        core.engine
            .intents()
            .map(|i| {
                let spec = i.spec();
                let id = spec.id.0;
                IntentBrief {
                    id,
                    state: serde_json::to_value(i.state()).unwrap_or(Value::Null),
                    source: serde_json::to_value(&spec.source).unwrap_or(Value::Null),
                    agent: match &spec.source {
                        murmur_decide::Source::Agent { id } => Some(id.as_str().to_string()),
                        murmur_decide::Source::Human { .. } => None,
                    },
                    task: spec.kind().as_str(),
                    uids: serde_json::to_value(&spec.uids).unwrap_or(Value::Null),
                    track: match &spec.task {
                        Task::Engage { track, .. } => Some(track.clone()),
                        _ => None,
                    },
                    reason: i.history().last().map(|h| h.reason.clone()),
                    rejected_note: notes.get(&id).cloned(),
                }
            })
            .collect()
    }

    /// Every review flag of the run (in force and resolved), lean (see [`FlagBrief`]).
    pub fn flag_briefs(&self) -> Vec<FlagBrief> {
        let core = self.lock();
        core.engine
            .identification()
            .flags()
            .map(|(f, s)| FlagBrief {
                id: format!("F-{}", f.id),
                kind: f.kind.as_str(),
                status: s.name(),
                tracks: f.tracks.clone(),
                evidence: f.evidence.clone(),
                raises_suspect: f.kind.raises_suspect(),
            })
            .collect()
    }

    /// Keep an operator's rejection note for agents (display only; the audit log has it).
    pub(super) fn remember_rejection(&self, id: u64, by: &OperatorId, note: Option<&str>) {
        let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) else {
            return;
        };
        // a note is a reason, not a document: cut at REJECTION_NOTE_CHARS
        let mut kept: String = note.chars().take(REJECTION_NOTE_CHARS).collect();
        if kept.len() < note.len() {
            kept.push_str("...");
        }
        let mut notes = self.rejection_notes.0.lock().unwrap_or_else(|e| e.into_inner());
        notes.insert(id, (by.as_str().to_string(), kept));
        while notes.len() > REJECTION_NOTES {
            notes.pop_first();
        }
    }

    /// Keep what the loop just wrote to the audit log as `vehicle.event`.
    pub(super) fn remember_events(&self, cell: &VehicleCell, events: &[VehicleEvent]) {
        if events.is_empty() {
            return;
        }
        let callsign = cell.read(|v| {
            v.identity
                .as_ref()
                .and_then(|i| i.str("callsign"))
                .map(str::to_string)
        });
        let t = clock::wall();
        let mut ring = self.recent.0.lock().unwrap_or_else(|e| e.into_inner());
        for e in events {
            // the ring never empties once filled, so the newest holds the last number
            let seq = ring.back().map_or(1, |l| l.seq + 1);
            if ring.len() == RECENT_EVENTS {
                ring.pop_front();
            }
            ring.push_back(RecentEvent {
                seq,
                t,
                uid: cell.uid,
                callsign: callsign.clone(),
                kind: e.kind.to_string(),
                text: e.text.clone(),
            });
        }
    }

    /// The engine proposal for an agent's request: a collect or cover through
    /// [`space::proposal`] (no vehicles), else a vehicle task, refused before any intent
    /// exists when the task layer couldn't take it (as [`Decision::submit`] does).
    fn agent_proposal(&self, req: &IntentRequest) -> Result<Proposal, DecisionError> {
        if req.task == "engage" {
            // a nomination: the engine routes it to operators with engagement authority;
            // accepting a lost vehicle is an operator's decision, never an agent's (rules 6)
            if !req.engage.accept_unknown.is_empty() {
                return Err(DecisionError::new(
                    403,
                    "an agent can't accept a lost vehicle's unknown position: only an operator can (engage.accepted)",
                ));
            }
            // rules 16: nor a lost contact
            if !req.engage.accept_lost.is_empty() {
                return Err(DecisionError::new(
                    403,
                    "an agent can't accept a lost contact: only an operator with engagement authority can (engage.accepted)",
                ));
            }
            self.check_vehicles(&req.uids)?;
            return engage::proposal(req, self.clock.now());
        }
        if matches!(req.task.as_str(), "collect" | "cover") {
            return space::proposal(
                &req.task,
                !req.uids.is_empty(),
                &req.space,
                req.rationale.as_deref(),
            );
        }
        let fleet = self.fleet();
        let order = TaskRequest {
            task: req.task.clone(),
            uids: req.uids.clone(),
            params: req.params.clone(),
        };
        match TaskOrder::new(order, &fleet) {
            Ok(_) => {}
            Err(e @ OrderError::UnknownVehicles(_)) => {
                return Err(DecisionError::new(404, e.to_string()));
            }
            Err(e) => return Err(DecisionError::new(400, e.to_string())),
        }
        proposal(req)
    }
}

fn state_name(state: State) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{state:?}"))
}

fn uids_text(uids: &[DUid]) -> String {
    let v: Vec<&str> = uids.iter().map(DUid::as_str).collect();
    v.join(", ")
}

/// `on [sky-1, sky-2]`, or what a satellite request is on.
fn targets_text(task: &Task, uids: &[DUid]) -> String {
    match task.targets() {
        Targets::Space => "(a satellite request, no fleet vehicles)".to_string(),
        Targets::Vehicles => format!("on [{}]", uids_text(uids)),
    }
}

/// The compact form of a space answer for agents (the full answer is in the audit log as
/// `space.query`): the first ranked options, the exclusion counts, a cover's summary.
fn compact_answer(a: &SpaceAnswer) -> Value {
    let mut v = json!({ "time_map": a.time_map, "segment": a.segment.short() });
    match &a.result {
        SpaceResult::Tasking(r) => {
            v["options"] = json!(r.options.iter().take(5).collect::<Vec<_>>());
            v["options_total"] = json!(r.options.len());
            v["exclusions_total"] = json!(r.exclusions.len());
            v["exclusion_summary"] = json!(r.exclusion_summary);
            v["classes_without_passes"] = json!(r.classes_without_passes);
            v["now"] = json!(r.request.now);
            v["note"] = json!(
                "the first 5 of options_total ranked options; propose a collect with chosen \
                 {norad_id, collect_time} from one of them"
            );
        }
        SpaceResult::Links(l) => v["links"] = json!(l),
        SpaceResult::Gnss(g) => v["gnss"] = json!(g),
        SpaceResult::CoverLinks(c) => super::space::cover_links_json(&mut v, c),
        SpaceResult::CoverGnss(c) => super::space::cover_gnss_json(&mut v, c),
    }
    v
}

/// One sentence per check outcome class: what failed, what a human must judge, what
/// was skipped.
fn checks_text(checks: &[murmur_decide::CheckResult]) -> String {
    let count = |o: CheckOutcome| checks.iter().filter(|c| c.outcome == o).count();
    let list = |o: CheckOutcome| -> Vec<String> {
        checks
            .iter()
            .filter(|c| c.outcome == o)
            .map(|c| match &c.uid {
                Some(u) => format!("{} on vehicle {u}: {}", c.rule.as_str(), c.detail),
                None => format!("{}: {}", c.rule.as_str(), c.detail),
            })
            .collect()
    };
    let mut parts = vec![format!(
        "{} checks: {} pass, {} fail, {} unverified, {} not evaluated",
        checks.len(),
        count(CheckOutcome::Pass),
        count(CheckOutcome::Fail),
        count(CheckOutcome::Unverified),
        count(CheckOutcome::NotEvaluated)
    )];
    let fails = list(CheckOutcome::Fail);
    if !fails.is_empty() {
        parts.push(format!("failed: {}", fails.join("; ")));
    }
    let unverified = list(CheckOutcome::Unverified);
    if !unverified.is_empty() {
        parts.push(format!(
            "a human must judge (unverified): {}",
            unverified.join("; ")
        ));
    }
    parts.join(". ")
}

/// Where an intent stands and what would move it, for a model to read.
fn explain(i: &Intent) -> String {
    let spec = i.spec();
    let who = match &spec.source {
        murmur_decide::Source::Human { id } => format!("human operator {id}"),
        murmur_decide::Source::Agent { id } => format!("agent {id}"),
    };
    let mut s = format!(
        "Intent {} is {}: {} {}, proposed by {}.",
        i.id(),
        state_name(i.state()),
        spec.kind().as_str(),
        targets_text(&spec.task, &spec.uids),
        who
    );
    if !i.checks().is_empty() {
        s.push(' ');
        s.push_str(&checks_text(i.checks()));
        s.push('.');
    }
    if let Some(p) = i.policy() {
        let tier = match p.tier {
            murmur_decide::Tier::Auto => "auto (policy approves, no human)",
            murmur_decide::Tier::Human => "human (a listed operator must decide)",
        };
        s.push_str(&format!(" Policy tier {tier}, rule {}: {}.", p.rule, p.reason));
        let why: Vec<&str> = p
            .facts
            .unverified
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(p.facts.area_detail.as_str()).filter(|_| p.facts.leaves_approved_area))
            .collect();
        if !why.is_empty() {
            s.push_str(&format!(" Why a human: {}.", why.join("; ")));
        }
    }
    match i.state() {
        State::PendingApproval => {
            s.push_str(
                " It waits in the operator's inbox; an agent cannot approve it (no such tool \
                 exists). An operator approves or rejects it in the console or over the \
                 operator API, and the checks run again at approval against the world then.",
            );
            if let Some(e) = i.expires_t() {
                s.push_str(&format!(
                    " It expires at t={:.0} if nobody decides.",
                    e.0 as f64 / 1000.0
                ));
            }
            if spec.source.is_agent() {
                s.push_str(" The proposing agent may withdraw it until then.");
            }
        }
        State::Queued => {
            s.push_str(
                concat!(
                    " It is approved but held: a vehicle it is for can't be reached now (a silent",
                    " link, or an AUV whose acoustic modem can't carry a mission). When the link",
                    " can carry it, its checks run again on the world then: it is sent if they",
                    " pass, goes back to the inbox if a human approved a picture that changed (or",
                    " was approved before a bridge restart, or long enough ago), and is voided if a",
                    " check fails or a newer order for its vehicle stands.",
                ),
            );
            if let Some(q) = i.queue() {
                s.push_str(&format!(
                    " It expires at t={:.0} if the vehicle can't be reached by then.",
                    q.expires_t.0 as f64 / 1000.0
                ));
            }
            if spec.source.is_agent() {
                s.push_str(" The proposing agent may withdraw it until then.");
            }
        }
        State::Rejected | State::Expired | State::Voided | State::Failed | State::Aborted => {
            if let Some(h) = i.history().last() {
                s.push_str(&format!(" Last change: {}.", h.reason));
            }
            if i.state() == State::Rejected
                && matches!(&spec.task, Task::Collect { chosen: None, .. })
            {
                s.push_str(
                    " A collect without a chosen option is a query, not an order: its space \
                     answer lists the ranked options; propose the collect again with chosen \
                     {norad_id, collect_time} from one of them (preview_intent shows the \
                     options without creating an intent).",
                );
            }
        }
        State::AutoApproved
        | State::Approved
        | State::Dispatched
        | State::Effected
        | State::Done => {
            match i.approval() {
                Some(a) if a.at_submission => s.push_str(&format!(
                    " Authorized at submission by {} (the submitter's own command).",
                    a.by
                )),
                Some(a) => s.push_str(&format!(
                    " Approved in the inbox by operator {}{}.",
                    a.by,
                    if a.self_approved {
                        " (who also proposed it)"
                    } else {
                        ""
                    }
                )),
                None => s.push_str(" Approved by policy (auto tier), no human decision."),
            }
            if let Some(h) = i.history().last()
                && i.state() != State::AutoApproved
            {
                s.push_str(&format!(" Last change: {}.", h.reason));
            }
        }
        State::Proposed | State::Checked => {}
    }
    s
}

/// A preview as the tool returns it: the engine's answer (a space answer in its compact
/// form) plus why a human would be needed and a summary sentence.
fn preview_json(p: &Preview, _tolerance_s: Option<u32>) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    if let Some(a) = &p.space {
        v["space"] = compact_answer(a);
    }
    let why: Vec<String> = p
        .policy
        .as_ref()
        .map(|d| {
            let mut why = vec![format!("policy {}: {}", d.rule, d.reason)];
            if !d.facts.armed.is_empty() {
                why.push(format!(
                    "armed platform: vehicle {}",
                    uids_text(&d.facts.armed)
                ));
            }
            if d.facts.leaves_approved_area {
                why.push(d.facts.area_detail.clone());
            }
            why.extend(d.facts.unverified.iter().cloned());
            why
        })
        .unwrap_or_default();
    let outcome = match p.would_reach {
        State::Rejected => "the checks would reject it".to_string(),
        State::AutoApproved => "policy would approve it and it would be dispatched at once (auto tier)".to_string(),
        _ => "it would wait in the operator's inbox for a human decision".to_string(),
    };
    let query_note = match (p.space.as_deref(), p.would_reach) {
        (Some(SpaceAnswer { result: SpaceResult::Tasking(r), .. }), State::Rejected)
            if !r.options.is_empty() =>
        {
            format!(
                " This collect has no chosen option, so it is a query: {} ranked options \
                 are in space.options; propose again with chosen {{norad_id, collect_time}} \
                 from one of them.",
                r.options.len()
            )
        }
        _ => String::new(),
    };
    v["summary"] = json!(format!(
        "Preview only, no intent was created. {} {}: {}. If proposed now, {}.{query_note}",
        p.kind.as_str(),
        targets_text_kind(p),
        checks_text(&p.checks),
        outcome
    ));
    if p.would_reach == State::PendingApproval {
        v["why_human"] = json!(why);
    }
    v
}

fn targets_text_kind(p: &Preview) -> String {
    match p.kind.targets() {
        Targets::Space => "(a satellite request, no fleet vehicles)".to_string(),
        Targets::Vehicles => format!("on [{}]", uids_text(&p.uids)),
    }
}
