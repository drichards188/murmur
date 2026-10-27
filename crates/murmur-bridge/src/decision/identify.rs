//! Rules 21 (bd murmur-bia): identify orders on the bridge.
//!
//! The engine dispatches an identify order as an orbit around its track (`params.look` names
//! the track, so the physical model's payload manager cues it first). Each tick
//! [`Decision::step_identify`] asks the engine to step every identify order still looking:
//! an orbit moved with the track goes out as a follow of the order, and once the look ended the
//! order the engine sends afterwards (the replaced order again, or a hold) goes out under the
//! identify's own id. Each replaces the order's last tasking, whose ops are retired (superseded,
//! done or not), so the map draws only the orbit in force. While it looks, an op `done` (an
//! orbit is done once established) never ends the order: only the engine's step does.
//!
//! `POST /api/identify/options` ranks the vehicles that could take one: each vehicle with an
//! EO/IR, the checks an identify order for it would get now, its standoff and time to get
//! there, and its predicted link along the way (read-only, never logged).

use std::collections::HashMap;

use murmur_decide::{Clock, DispatchOrder, IdentifyEvent, IntentId, IntentSpec, Task, TaskKind};
use serde::Deserialize;
use serde_json::{Value, json};

use super::snapshot::duid;
use super::{AuthorizedOrder, Core, Decision, DecisionError, Running, alerts, task_request};
use crate::clock;
use crate::tasks::{OpState, TaskOrder};

/// `POST /api/identify/options`.
#[derive(Debug, Clone, Deserialize)]
pub struct OptionsRequest {
    /// The perception track id to identify.
    pub track: String,
    /// Only these vehicles, each answered whatever it carries (default: every vehicle with an
    /// EO/IR).
    #[serde(default)]
    pub uids: Option<Vec<crate::vehicle::Uid>>,
    /// An operator's standoff, metres (default: the engine's, from each camera's reach).
    #[serde(default)]
    pub standoff_m: Option<f64>,
}

/// The track an identify intent names.
pub(super) fn track_of(spec: &IntentSpec) -> Option<&str> {
    match &spec.task {
        Task::Identify { track, .. } => Some(track),
        _ => None,
    }
}

impl Decision {
    /// Rules 21: step the identify orders still looking (the engine logs each step before
    /// anything goes out) and send what each step calls for.
    pub(super) fn step_identify(&self, core: &mut Core) {
        if !self.rules.identify() {
            return;
        }
        let looking = core.engine.intents().any(|i| {
            i.spec().kind() == TaskKind::Identify
                && i.state() == murmur_decide::State::Dispatched
                && i.identify_run().is_some_and(|r| r.ended.is_none())
        });
        // the world only when an identify order looks (a snapshot of a large fleet is not free)
        if !looking {
            return;
        }
        let world = self.snapshot();
        let events = match core.engine.step_identify(&world, &self.terrain.oracle()) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("decision: identify step: {e}");
                return;
            }
        };
        for ev in events {
            match ev {
                IdentifyEvent::Recentre(o) => self.follow_identify(core, o, true, "its orbit moved with its track"),
                IdentifyEvent::Return(o) => self.follow_identify(core, o, false, "its look ended"),
                // rules 23: another vehicle identified its track; the order goes on (logged)
                IdentifyEvent::Meanwhile { .. } => {}
            }
        }
    }

    /// Send `o` (an identify order's moved orbit, `look`, or the order after its look) as the
    /// order's new tasking, retiring the one it replaces.
    fn follow_identify(&self, core: &mut Core, o: DispatchOrder, look: bool, why: &str) {
        let id = o.intent;
        let fleet = self.fleet();
        let track = core.engine.intent(id).and_then(|i| track_of(i.spec()).map(str::to_string));
        let mut order = match task_request(&o.task, &o.uids).and_then(|r| TaskOrder::new(r, &fleet).map_err(|e| e.to_string())) {
            Ok(order) => order,
            Err(e) => {
                let detail = format!("the task layer refused its follow: {e}");
                if let Err(err) = core.engine.finish(id, murmur_decide::TaskOutcome::Failed { detail }) {
                    eprintln!("decision: finish {id}: {err}");
                }
                core.running.remove(&id);
                return;
            }
        };
        if look {
            order.params.look = track;
        }
        // rules 22: the way there as the engine routed it
        order.params.via = super::queue::bridge_via(&o.via);
        order.patience = self.patience(o.task.kind());
        order.requeue = false;
        order.ahead = self.write_ahead_hook();
        let old = core.running.get(&id).map(|r| r.tasking.clone());
        let tasking = self.world.tasks.dispatch(AuthorizedOrder { intent: id, order });
        if let Some(old) = old {
            self.world
                .tasks
                .retire(&old, &format!("followed by {} #{}: {why}", o.task.kind().as_str(), tasking.id));
        }
        core.taskings.insert(id, tasking.clone());
        core.running.insert(
            id,
            Running {
                tasking,
                reported: HashMap::new(),
                look,
            },
        );
    }

    /// Rules 21: while a vehicle flies what an identify order sent it after a look that did not
    /// identify its track, or holds for want of an order to go back to, or while its orbit could
    /// not follow its track, an inbox alert says so. It clears when another order replaces it.
    pub(super) fn identify_alerts(&self, core: &Core, now_wall: f64) -> Vec<alerts::Alert> {
        if !self.rules.identify() {
            return Vec::new();
        }
        let fleet = self.fleet();
        self.world
            .tasks
            .current_ops()
            .into_iter()
            .filter(|op| !matches!(op.state, OpState::Failed | OpState::Superseded))
            .filter_map(|op| {
                let i = core.engine.intent(IntentId(op.intent_id))?;
                let track = track_of(i.spec())?;
                let run = i.identify_run()?;
                let (kind, severity, detail) = match (&run.ended, &run.held) {
                    (Some((murmur_decide::identify::EndKind::Identified, d)), _) if d.contains("; holding") => {
                        ("identify_holding", alerts::Severity::Medium, format!("identify {track}: {d}"))
                    }
                    (Some((murmur_decide::identify::EndKind::Identified, _)), _) => return None,
                    (Some((k, d)), _) => (
                        if *k == murmur_decide::identify::EndKind::Lost { "identify_lost" } else { "identify_timed_out" },
                        alerts::Severity::High,
                        format!("identify {track}: {d}"),
                    ),
                    (None, Some(reason)) => (
                        "identify_held",
                        alerts::Severity::Medium,
                        format!("identify {track}: its orbit can't follow the track ({reason})"),
                    ),
                    (None, None) => return None,
                };
                Some(alerts::Alert {
                    id: format!("identify:{}", op.intent_id),
                    kind,
                    severity,
                    uid: op.uid,
                    callsign: fleet.get(&op.uid).and_then(|c| alerts::callsign(c)),
                    detail,
                    since_t: op.history.first().map_or(now_wall, |h| h.t),
                    op_id: Some(op.id),
                    intent_id: Some(op.intent_id),
                })
            })
            .collect()
    }

    /// `POST /api/identify/options`: each vehicle that could be sent to identify `req.track`
    /// (one with an EO/IR whose autopilot takes the order; every vehicle `req.uids` names, so one
    /// without a camera hears why it can't go), with the checks an identify order
    /// for it would get now (`passed`, the failing ones in words), its plan (`standoff_m`, the
    /// camera's `reach_m`, `transit_s`) and, for one that passes, its predicted link along the
    /// way (`link`: `holds`, `p_failsafe`, its sentence). Passing first, soonest there first.
    /// Read-only, never logged.
    pub async fn identify_options(&self, req: OptionsRequest) -> Result<Value, DecisionError> {
        if !self.rules.identify() {
            return Err(DecisionError::new(422, "these decision rules take no identify order"));
        }
        let fleet = self.fleet();
        let now = self.clock.now();
        let candidates: Vec<crate::vehicle::Uid> = fleet
            .iter()
            .filter(|(u, _)| req.uids.as_ref().is_none_or(|l| l.contains(u)))
            .filter(|(_, c)| {
                req.uids.is_some()
                    || c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("platform"))
                        .and_then(|p| self.catalog.get(p))
                        .is_some_and(|p| {
                            p.sensors.iter().any(|s| s.kind == "eo_ir") && p.autopilot.is_some_and(|a| a.supports(TaskKind::Identify))
                        })
                })
            })
            .map(|(u, _)| *u)
            .collect();
        let mut out = Vec::new();
        for uid in candidates {
            let spec = IntentSpec {
                id: IntentId(0),
                created_t: now,
                source: murmur_decide::Source::Human {
                    id: murmur_decide::OperatorId::new("dry-run"),
                },
                task: Task::Identify {
                    track: req.track.clone(),
                    standoff_m: req.standoff_m,
                },
                uids: vec![duid(uid)],
                constraints: murmur_decide::Constraints::default(),
                rationale: None,
            };
            self.prefetch(&spec.task, &spec.uids).await;
            let inputs = {
                let w = self.snapshot();
                self.lock().engine.dry_run_inputs(&spec, &w, &self.terrain.oracle())
            };
            let checks = murmur_decide::rules::evaluate(&spec, &inputs, &self.rules);
            let passed = murmur_decide::rules::passed(&checks, TaskKind::Identify);
            let plan = murmur_decide::identify::plan(&spec, &inputs, &self.rules).ok();
            let callsign = fleet.get(&uid).and_then(|c| alerts::callsign(c));
            let platform = fleet
                .get(&uid)
                .and_then(|c| c.read(|v| v.identity.as_ref().and_then(|i| i.str("platform")).map(str::to_string)));
            let link = if passed {
                let v = self.link_coverage(&spec.task, &spec.uids).await;
                let order = v.get("vehicles").and_then(|vs| vs.get(0)).cloned().unwrap_or(v);
                Some(json!({
                    "holds": order.get("holds"),
                    "p_failsafe": order.get("p_failsafe"),
                    "sentence": order.get("sentence"),
                }))
            } else {
                None
            };
            out.push(json!({
                "uid": uid,
                "callsign": callsign,
                "platform": platform,
                "passed": passed,
                "failing": checks
                    .iter()
                    .filter(|c| c.outcome == murmur_decide::Outcome::Fail)
                    .map(|c| format!("{}: {}", c.rule.as_str(), c.detail))
                    .collect::<Vec<_>>(),
                "standoff_m": plan.as_ref().map(|p| p.radius_m),
                "reach_m": plan.as_ref().map(|p| p.reach_m),
                "transit_s": plan.as_ref().map(|p| p.transit_s),
                "center": plan.as_ref().map(|p| [p.center.lon, p.center.lat]),
                "link": link,
            }));
        }
        out.sort_by(|a, b| {
            let key = |v: &Value| (!v["passed"].as_bool().unwrap_or(false), v["transit_s"].as_f64().unwrap_or(f64::INFINITY));
            let (ka, kb) = (key(a), key(b));
            ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
        });
        Ok(json!({
            "track": req.track,
            "t": clock::wall(),
            "vehicles": out,
            "note": "read-only: nothing was created or logged; passing first, soonest there first",
        }))
    }
}
