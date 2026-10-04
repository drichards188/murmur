//! Engagements in the bridge (Phase E): the request shape, the platform's intercept and
//! fallback through the task layer, the loop that holds the picture, the abstract effect on
//! the simulation channel, and what the console sees (the live picture of a pending
//! nomination, who may still sign it).
//!
//! The decision itself is the engine's (`murmur_decide::engage`): this module only feeds it
//! the world each second and carries out what it releases.
//!
//! - **Intercept.** An approved engagement's order ([`murmur_decide::EngageOrder`]) becomes
//!   an orbit at the predicted effect point (a loiter there: the platform waits at the point,
//!   it does not home on anything), at the platform's minimum orbit radius.
//! - **Fallback.** When the engine stops an engagement after dispatch (an operator's abort,
//!   a void, link loss, the window), the recorded fallback becomes a hold (`abort`), an RTL
//!   (`return`) or an orbit around the platform's current position (`loiter`). Both are
//!   [`super::AuthorizedOrder`]s made here only from what the engine released.
//! - **Effect.** The engine's [`murmur_decide::EffectOrder`] goes to the contacts process on
//!   the simulation channel (`crate::tracks::Tracks::queue_effect`): a point, a radius, a
//!   time. Nothing here models a weapon.
//! - **Operators.** `MURMUR_ENGAGE_OPERATORS` lists the operators whose sessions are opened
//!   with engagement authority (a placeholder for real authentication, like
//!   `MURMUR_OPERATORS`); only they can sign an engagement, never its proposer, and the
//!   engagement tier asks for two of them.

use std::collections::HashMap;
use std::sync::Arc;

use murmur_decide::{
    Clock,
    CheckResult, EffectOrder, EngageOrder, EngageWindow, Fallback, FallbackOrder, Intent,
    IntentId, Outcome as CheckOutcome, Proposal, Rationale, State, Task, Time, TrackFacts,
    engage,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::snapshot::{bridge_uid, duid};
use super::{Core, Decision, DecisionError, IntentRequest, engine_err};
use crate::tasks::{OpState, TaskOrder, TaskParams, TaskRequest, Tasking};

/// `POST /api/intents` (and the MCP's `propose_intent`) for `task = "engage"`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EngageParams {
    /// The perception track id (from `/api/tracks`), which an operator must have declared
    /// hostile.
    #[serde(default)]
    pub track: Option<String>,
    /// The effect radius, metres.
    #[serde(default)]
    pub effect_radius_m: Option<f64>,
    /// The window closes this many seconds after the nomination (default:
    /// `[rules.engage] default_window_s`).
    #[serde(default)]
    pub window_s: Option<u32>,
    /// What the platform does if the engagement stops before the effect: `abort` (hold
    /// where it is), `return` (RTL) or `loiter` (orbit where it is).
    #[serde(default)]
    pub fallback: Option<String>,
}

/// How long after an engagement ended its run is kept (its platform's orders followed).
const FORGET_AFTER_S: i64 = 300;

/// An engagement the loop follows: its platform's intercept and, once stopped, the
/// fallback.
pub(super) struct EngageRun {
    intercept: Option<Arc<Tasking>>,
    fallback: Option<Arc<Tasking>>,
    /// op id -> state last written to the log
    reported: HashMap<u64, OpState>,
}

/// The engine proposal for an engage request.
pub(super) fn proposal(req: &IntentRequest, now: Time) -> Result<Proposal, DecisionError> {
    let bad = |m: String| DecisionError::new(400, m);
    let p = &req.engage;
    let track = p
        .track
        .clone()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| bad("engage needs track: a track id from /api/tracks".into()))?;
    let radius = p
        .effect_radius_m
        .ok_or_else(|| bad("engage needs effect_radius_m".into()))?;
    let fallback: Fallback = match p.fallback.as_deref() {
        None => return Err(bad("engage needs fallback: abort, return or loiter".into())),
        Some(f) => serde_json::from_value(json!(f))
            .map_err(|_| bad(format!("fallback {f:?}: abort, return or loiter")))?,
    };
    if req.uids.len() != 1 {
        return Err(bad("engage names exactly one armed platform in uids".into()));
    }
    let window = p.window_s.map(|s| EngageWindow {
        from: None,
        until: now.plus_ms(i64::from(s) * 1000),
    });
    let mut proposal = Proposal::new(
        Task::Engage {
            track,
            effect_radius_m: radius,
            window,
            fallback,
        },
        req.uids.iter().map(|u| duid(*u)).collect(),
    );
    proposal.constraints = req.constraints.clone();
    proposal.rationale = req
        .rationale
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(Rationale::new)
        .transpose()
        .map_err(|e| bad(e.to_string()))?;
    proposal.validate().map_err(|e| bad(e.to_string()))?;
    Ok(proposal)
}

impl Decision {
    /// The orbit radius the platform loiters at over the effect point: its catalog minimum
    /// orbit (the rules' margin over its minimum turn radius, or the fixed-wing default),
    /// at least 30 m.
    fn loiter_radius(&self, uid: &murmur_decide::Uid) -> f64 {
        let platform = bridge_uid(uid)
            .and_then(|u| self.fleet().get(&u).cloned())
            .and_then(|c| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("platform"))
                        .map(str::to_string)
                })
            })
            .and_then(|p| self.catalog.get(&p).cloned());
        let min = platform.as_ref().and_then(|p| {
            p.min_turn_radius_m.or_else(|| {
                p.autopilot
                    .filter(|a| a.is_fixed_wing())
                    .map(|_| self.rules.fixed_wing_default_min_turn_radius_m)
            })
        });
        min.map_or(30.0, |m| (m * self.rules.orbit_radius_margin).ceil())
            .max(30.0)
    }

    /// Dispatch an approved engagement: the engine releases its order (`intent.dispatched`
    /// with the effect point), the platform is sent to loiter at the point.
    pub(super) fn dispatch_engage(&self, core: &mut Core, id: IntentId) -> Result<(), DecisionError> {
        let spec = core
            .engine
            .intent(id)
            .ok_or_else(|| DecisionError::new(404, format!("unknown intent {id}")))?
            .spec()
            .clone();
        let fleet = self.fleet();
        let Some(uid) = spec.uids.first().and_then(bridge_uid) else {
            core.engine
                .void(id, "the platform has no bridge uid")
                .map_err(engine_err)?;
            return Ok(());
        };
        let preview_point = core
            .engine
            .intent(id)
            .and_then(Intent::engagement)
            .and_then(|e| e.snapshot.as_ref())
            .map(|s| s.preview.effect_point);
        let Some(point) = preview_point else {
            core.engine
                .void(id, "no approval snapshot")
                .map_err(engine_err)?;
            return Ok(());
        };
        let radius = self.loiter_radius(&spec.uids[0]);
        let request = |p: [f64; 2]| TaskRequest {
            task: "orbit".into(),
            uids: vec![uid],
            params: TaskParams {
                center: Some(p),
                radius: Some(radius),
                ..TaskParams::default()
            },
        };
        // the task layer must be able to take it before the engine marks it dispatched
        if let Err(e) = TaskOrder::new(request([point.lon, point.lat]), &fleet) {
            core.engine
                .void(id, &format!("the task layer can't take it: {e}"))
                .map_err(engine_err)?;
            return Ok(());
        }
        let o: EngageOrder = match core.engine.dispatch_order(id).map_err(engine_err)? {
            murmur_decide::Order::Engage(o) => o,
            _ => return Err(DecisionError::new(500, "an engagement released another order")),
        };
        let order = match TaskOrder::new(request([o.effect_point.lon, o.effect_point.lat]), &fleet) {
            Ok(order) => order,
            Err(e) => {
                let why = format!("the task layer refused the intercept: {e}");
                if let Some(f) = core
                    .engine
                    .stop_engagement_system(id, &why)
                    .map_err(engine_err)?
                {
                    self.command_fallback(core, f);
                }
                return Ok(());
            }
        };
        let tasking = self.world.tasks.dispatch(super::AuthorizedOrder {
            intent: id,
            order,
        });
        core.taskings.insert(id, tasking.clone());
        core.engagements.insert(
            id,
            EngageRun {
                intercept: Some(tasking),
                fallback: None,
                reported: HashMap::new(),
            },
        );
        self.supersede(core, id, &[spec.uids[0].clone()]);
        Ok(())
    }

    /// Command an engagement's recorded fallback (the engine released it with the stop).
    pub(super) fn command_fallback(&self, core: &mut Core, f: FallbackOrder) {
        let fleet = self.fleet();
        let Some(uid) = bridge_uid(&f.uid) else {
            return;
        };
        let here = fleet
            .get(&uid)
            .and_then(|c| c.read(|v| v.pos))
            .map(|(lat, lon)| [lon, lat]);
        let (task, params) = match f.fallback {
            Fallback::Abort => ("hold", TaskParams::default()),
            Fallback::Return => ("rtl", TaskParams::default()),
            Fallback::Loiter => (
                "orbit",
                TaskParams {
                    center: here,
                    radius: Some(self.loiter_radius(&f.uid).max(150.0)),
                    ..TaskParams::default()
                },
            ),
        };
        let detail = format!("{} commanded ({task}): {}", f.fallback.as_str(), f.reason);
        let req = TaskRequest {
            task: task.into(),
            uids: vec![uid],
            params,
        };
        match TaskOrder::new(req, &fleet) {
            Ok(order) => {
                let tasking = self.world.tasks.dispatch(super::AuthorizedOrder {
                    intent: f.intent,
                    order,
                });
                let run = core.engagements.entry(f.intent).or_insert(EngageRun {
                    intercept: None,
                    fallback: None,
                    reported: HashMap::new(),
                });
                run.fallback = Some(tasking);
                if let Err(e) = core.engine.record_op_state(f.intent, &f.uid, "fallback", &detail) {
                    eprintln!("decision: fallback op state: {e}");
                }
            }
            Err(e) => {
                let detail = format!("{} could not be commanded: {e}", f.fallback.as_str());
                if let Err(e) = core.engine.record_op_state(f.intent, &f.uid, "fallback", &detail) {
                    eprintln!("decision: fallback op state: {e}");
                }
            }
        }
    }

    /// Engagements in play (pending, approved, under way) on vehicles another order (`id`)
    /// just took: stopped, cause superseded by `id` and its source (the new order stands, so
    /// no fallback is commanded). Only an operator's order gets here on its own: an agent's
    /// order on a vehicle with an engagement in play always waits for a human.
    pub(super) fn stop_superseded_engagements(
        &self,
        core: &mut Core,
        id: IntentId,
        took: &[murmur_decide::Uid],
    ) {
        let hit: Vec<IntentId> = core
            .engine
            .intents()
            .filter(|i| {
                i.id() != id
                    && matches!(
                        i.state(),
                        State::PendingApproval | State::Approved | State::Dispatched
                    )
                    && matches!(i.spec().task, Task::Engage { .. })
                    && i.spec().uids.iter().any(|u| took.contains(u))
            })
            .map(Intent::id)
            .collect();
        for other in hit {
            if let Err(e) = core.engine.supersede_engagement(other, id) {
                eprintln!("decision: supersede {other}: {e}");
            }
            core.engagements.remove(&other);
        }
    }

    /// Once a second: follow the intercepts and fallbacks (op states to the log; a failed
    /// intercept stops the engagement), then let the engine hold the picture, release
    /// effects and assess, and carry out what it says.
    pub(super) fn step_engagements(&self, core: &mut Core) {
        let mut failed: Vec<(IntentId, String)> = Vec::new();
        {
            let Core {
                engine,
                engagements,
                ..
            } = core;
            for (id, run) in engagements.iter_mut() {
                let dispatched = engine.intent(*id).map(Intent::state) == Some(State::Dispatched);
                for (tasking, is_intercept) in [(&run.intercept, true), (&run.fallback, false)] {
                    let Some(t) = tasking else { continue };
                    for op in t.snapshot().ops {
                        if run.reported.get(&op.id) == Some(&op.state) {
                            continue;
                        }
                        let state = serde_json::to_value(op.state)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default();
                        let what = if is_intercept {
                            format!("intercept {state}")
                        } else {
                            format!("fallback {state}")
                        };
                        if let Err(e) = engine.record_op_state(*id, &duid(op.uid), &what, &op.detail) {
                            eprintln!("decision: op state: {e}");
                            continue;
                        }
                        run.reported.insert(op.id, op.state);
                        if is_intercept
                            && dispatched
                            && matches!(op.state, OpState::Failed | OpState::Superseded)
                        {
                            failed.push((*id, format!("the platform's intercept {state}: {}", op.detail)));
                        }
                    }
                }
            }
        }
        for (id, why) in failed {
            match core.engine.stop_engagement_system(id, &why) {
                Ok(Some(f)) => self.command_fallback(core, f),
                Ok(None) => {}
                Err(e) => eprintln!("decision: stop {id}: {e}"),
            }
        }
        let active = core.engine.intents().any(|i| {
            matches!(i.spec().task, Task::Engage { .. })
                && match i.state() {
                    // signatures waiting for the rest are held on their plan too
                    State::PendingApproval => i.engagement().is_some_and(|e| e.plan.is_some()),
                    State::Approved | State::Dispatched | State::Effected => true,
                    _ => false,
                }
        });
        // the world only when an engagement is in play (a snapshot of a large fleet is not free)
        if active {
            let world = self.snapshot();
            let out = core.engine.step_engagements(&world, &self.terrain.oracle());
            // each engagement on its own: one that failed (an audit write) is reported, the
            // others' orders are still carried out
            for (id, e) in &out.errors {
                eprintln!("decision: engagement {id}: {e}");
            }
            for ev in out.events {
                match ev {
                    murmur_decide::EngagementEvent::Stopped(f) => {
                        self.command_fallback(core, f);
                    }
                    murmur_decide::EngagementEvent::Effect(e) => {
                        self.send_effect(&e, &world);
                    }
                    murmur_decide::EngagementEvent::Voided { .. }
                    | murmur_decide::EngagementEvent::Assessed { .. } => {}
                }
            }
        }
        self.forget_finished_engagements(core);
    }

    /// Forget the runs of engagements that ended: once their platform's last order (the
    /// intercept, or the fallback) finished, or [`FORGET_AFTER_S`] after the end (an orbit
    /// at the effect point never finishes on its own). Op states are logged until then.
    fn forget_finished_engagements(&self, core: &mut Core) {
        let now = self.clock.now();
        let done: Vec<IntentId> = core
            .engagements
            .iter()
            .filter(|(id, run)| {
                let Some(i) = core.engine.intent(**id) else {
                    return true;
                };
                if !i.state().is_terminal() {
                    return false;
                }
                let ended = i.history().last().map_or(now, |h| h.t);
                let stale = now.ms_since(ended) > FORGET_AFTER_S * 1000;
                let finished = [&run.intercept, &run.fallback]
                    .into_iter()
                    .flatten()
                    .all(|t| t.snapshot().ops.iter().all(|o| o.state.is_terminal()));
                finished || stale
            })
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            core.engagements.remove(&id);
        }
    }

    /// The abstract effect to the contacts process, on the simulation channel: the point,
    /// the radius, the time, the engaging platform, and (for the evaluation truth log only)
    /// where our other vehicles reported themselves (none for one that never has: the truth
    /// log lists it as unknown, not as clear).
    fn send_effect(&self, e: &EffectOrder, world: &murmur_decide::WorldSnapshot) {
        let ours: Vec<Value> = world
            .vehicles
            .iter()
            .filter(|(uid, _)| **uid != e.uid)
            .map(|(uid, v)| {
                let p = v.position.as_ref().map(|p| p.value);
                json!({ "uid": uid, "lon": p.map(|p| p.lon), "lat": p.map(|p| p.lat) })
            })
            .collect();
        if let Some(t) = self.tracks() {
            t.queue_effect(e, ours);
        }
    }

    /// `POST /api/intents/{id}/abort`: any operator stops an approved or dispatched
    /// engagement before its effect; after dispatch the recorded fallback is commanded.
    pub fn abort(
        &self,
        by: &murmur_decide::OperatorId,
        id: u64,
        note: Option<String>,
    ) -> Result<super::Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, by, &self.engage_operators)?;
            let note = note.unwrap_or_else(|| "operator abort".into());
            match core.engine.abort_engagement(id, session.authority(), &note) {
                Ok(f) => {
                    if let Some(f) = f {
                        self.command_fallback(core, f);
                    }
                    Ok(self.outcome(core, id))
                }
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// For the console: a pending nomination's checks and preview on the world now, and
    /// what changed since it was nominated ("picture changed" warnings), plus who may still
    /// sign it.
    pub(super) fn engage_view(&self, core: &Core, i: &Intent, world: &murmur_decide::WorldSnapshot) -> Option<Value> {
        let Task::Engage { track, .. } = &i.spec().task else {
            return None;
        };
        let p = i.policy();
        let proposer = match &i.spec().source {
            murmur_decide::Source::Human { id } => Some(id.clone()),
            murmur_decide::Source::Agent { .. } => None,
        };
        let signed: Vec<String> = i
            .concurrences()
            .iter()
            .map(|a| a.by.to_string())
            .chain(i.approval().map(|a| a.by.to_string()))
            .collect();
        let record = i.engagement();
        // each current signature, when, and on what effect point
        let signatures: Vec<Value> = record
            .map(|e| {
                e.signed
                    .iter()
                    .map(|s| {
                        json!({
                            "by": s.by,
                            "t": s.t,
                            "effect_point": s.snapshot.preview.effect_point,
                            "effect_t": s.snapshot.preview.effect_t,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let reset = record.and_then(|e| e.reset.as_ref()).map(|r| {
            json!({ "t": r.t, "dropped": r.dropped, "reason": r.reason })
        });
        let eligible: Vec<String> = self
            .engage_operators
            .iter()
            .filter(|o| Some(*o) != proposer.as_ref() && !signed.contains(&o.to_string()))
            .map(ToString::to_string)
            .collect();
        let mut v = json!({
            "track": track,
            "approvers_needed": p.map_or(1, |p| p.approvers),
            "signed": signed,
            "proposer": proposer,
            "proposer_excluded": p.is_some_and(|p| p.proposer_excluded),
            "eligible": eligible,
            "engage_operators": self.engage_operators,
            "signatures": signatures,
            "reset": reset,
            "token_ttl_s": self.rules.engage_rules().token_ttl(),
            "max_signing_s": self.rules.engage_rules().max_signing_s,
            // the plan the first signature froze: every later signer signs it exactly
            "plan": record.and_then(|e| e.plan.as_ref()).map(|p| json!({
                "by": p.by,
                "t": p.t,
                "effect_point": p.preview.effect_point,
                "effect_t": p.preview.effect_t,
                "effect_radius_m": p.preview.effect_radius_m,
                "track": p.track,
            })),
        });
        let now_track: Option<&TrackFacts> = world.tracks.get(track);
        if let Some(t) = now_track {
            let age = world.t.ms_since(t.last_seen_t) as f64 / 1000.0;
            v["track_now"] = json!({
                "position": engage::track_at(t, world.t),
                "pos_sigma_m": t.pos_sigma_m,
                "age_s": age,
                "affiliation": t.affiliation.value,
                "declared_by": t.declared_by,
                "identity": t.identity,
                "identity_confidence": t.identity_confidence,
            });
        } else {
            v["track_now"] = Value::Null;
        }
        if i.state() == State::PendingApproval
            && let Some((checks, pv)) = core.engine.engage_live(i.id(), world, &self.terrain.oracle())
        {
            let changed = changes(i.checks(), &checks);
            // how far the track is now from where the nomination's preview put it now
            let drift = match (i.engagement().and_then(|e| e.preview.as_ref()), now_track) {
                (Some(nom), Some(t)) => {
                    let then = nom.track_path.first().copied();
                    then.map(|p0| {
                        let dt = world.t.ms_since(nom.t) as f64 / 1000.0;
                        let [e, n] = nom
                            .track_path
                            .get(1)
                            .map_or([0.0, 0.0], |p1| {
                                let (de, dn) = engage::enu(p0, *p1);
                                let span = nom.effect_t.ms_since(nom.t) as f64 / 1000.0;
                                if span > 0.0 { [de / span, dn / span] } else { [0.0, 0.0] }
                            });
                        let expected = engage::offset(p0, e * dt, n * dt);
                        murmur_decide::geo::haversine_m(expected, engage::track_at(t, world.t))
                    })
                }
                _ => None,
            };
            v["live"] = json!({
                "t": world.t,
                "passed": murmur_decide::rules::passed(&checks, murmur_decide::TaskKind::Engage),
                "checks": checks,
                "preview": pv,
                "changed": changed,
                "track_drift_m": drift,
            });
        }
        Some(v)
    }
}

/// `POST /api/engage/options`: which armed platforms could take a nomination now.
#[derive(Debug, Clone, Deserialize)]
pub struct OptionsRequest {
    pub track: String,
    pub effect_radius_m: f64,
    #[serde(default)]
    pub window_s: Option<u32>,
    #[serde(default)]
    pub fallback: Option<String>,
    /// Only these vehicles (default: every armed vehicle in the fleet).
    #[serde(default)]
    pub uids: Option<Vec<crate::vehicle::Uid>>,
}

impl Decision {
    /// A dry run for the nomination form (read-only, not a decision, not logged): for each
    /// armed vehicle, the checks a nomination with it would get on the world now, and its
    /// preview. Listed by uid, with no ranking and no recommendation: the operator picks.
    pub async fn engage_options(&self, req: OptionsRequest) -> Result<Value, DecisionError> {
        let fleet = self.fleet();
        let armed: Vec<crate::vehicle::Uid> = fleet
            .iter()
            .filter(|(u, _)| req.uids.as_ref().is_none_or(|l| l.contains(u)))
            .filter(|(_, c)| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("platform"))
                        .and_then(|p| self.catalog.get(p))
                        .is_some_and(|p| p.armed)
                })
            })
            .map(|(u, _)| *u)
            .collect();
        let fallback: Fallback = req
            .fallback
            .as_deref()
            .and_then(|f| serde_json::from_value(json!(f)).ok())
            .unwrap_or(Fallback::Return);
        let now = self.clock.now();
        let window = EngageWindow {
            from: None,
            until: now.plus_ms(
                i64::from(
                    req.window_s
                        .unwrap_or(self.rules.engage_rules().default_window_s),
                ) * 1000,
            ),
        };
        let mut out = Vec::new();
        for uid in armed {
            let spec = murmur_decide::IntentSpec {
                id: IntentId(0),
                created_t: now,
                source: murmur_decide::Source::Human {
                    id: murmur_decide::OperatorId::new("dry-run"),
                },
                task: Task::Engage {
                    track: req.track.clone(),
                    effect_radius_m: req.effect_radius_m,
                    window: Some(window),
                    fallback,
                },
                uids: vec![duid(uid)],
                constraints: murmur_decide::Constraints::default(),
                rationale: None,
            };
            self.prefetch(&spec.task, &spec.uids).await;
            let world = self.snapshot();
            let inputs = world.for_intent_with(&spec, &self.terrain.oracle(), &self.rules);
            let checks = murmur_decide::rules::evaluate(&spec, &inputs, &self.rules);
            let passed = murmur_decide::rules::passed(&checks, murmur_decide::TaskKind::Engage);
            let preview = engage::preview(&spec, &inputs, &self.rules).ok();
            let callsign = fleet.get(&uid).and_then(|c| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("callsign"))
                        .map(str::to_string)
                })
            });
            out.push(json!({
                "uid": uid,
                "callsign": callsign,
                "passed": passed,
                "checks": checks,
                "preview": preview,
            }));
        }
        // by uid (the fleet's order): no "best" first
        Ok(json!({
            "t": now,
            "track": req.track,
            "options": out,
            "note": "listed by uid; not a ranking or a recommendation",
        }))
    }

    /// Every engagement intent, newest first, as the console and agents see it (the intent
    /// with its engagement record, and the signatures and live picture).
    pub fn nominations_json(&self) -> Vec<Value> {
        let world = self.snapshot();
        let core = self.lock();
        let all: Vec<&Intent> = core
            .engine
            .intents()
            .filter(|i| matches!(i.spec().task, Task::Engage { .. }))
            .collect();
        all.into_iter()
            .rev()
            .map(|i| {
                let mut v = super::intent_json(i, core.taskings.get(&i.id()).map(|t| t.id), None);
                if let Some(e) = self.engage_view(&core, i, &world) {
                    v["engage"] = e;
                }
                v
            })
            .collect()
    }
}

/// The checks whose outcome is not what it was at nomination.
fn changes(then: &[CheckResult], now: &[CheckResult]) -> Vec<Value> {
    now.iter()
        .filter_map(|c| {
            let before = then
                .iter()
                .find(|b| b.rule == c.rule && b.uid == c.uid)
                .map(|b| b.outcome);
            (before != Some(c.outcome)).then(|| {
                json!({
                    "rule": c.rule.as_str(),
                    "was": before,
                    "now": c.outcome,
                    "detail": c.detail,
                    "worse": c.outcome != CheckOutcome::Pass,
                })
            })
        })
        .collect()
}
