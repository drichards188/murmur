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

/// Rules 23: an identify order's informational alerts while it looks, one for each other vehicle
/// that identified its track meanwhile (its latest kind), on the order's vehicle: "HUNTER 1
/// identified T-0001 as cargo_ship; PUMA 1 continues for its own look". `name` gives a vehicle's
/// callsign by uid. Each says when it was noted, and keeps its id (`identify-meanwhile:<intent>:
/// <uid>`) so an acknowledgement holds until the order ends.
pub(super) fn meanwhile_alerts(
    op: &crate::tasks::OpSnapshot,
    track: &str,
    noted: &[murmur_decide::identify::Noted],
    name: &dyn Fn(&str) -> String,
    now_wall: f64,
) -> Vec<alerts::Alert> {
    let sent = name(&op.uid.to_string());
    let mut latest: std::collections::BTreeMap<&str, &murmur_decide::identify::Noted> = std::collections::BTreeMap::new();
    for n in noted {
        latest.insert(n.by.as_str(), n);
    }
    latest
        .into_values()
        .map(|n| alerts::Alert {
            id: format!("identify-meanwhile:{}:{}", op.intent_id, n.by),
            kind: "identify_meanwhile",
            severity: alerts::Severity::Info,
            uid: op.uid,
            callsign: Some(sent.clone()),
            detail: format!(
                "{} identified {track}{}; {sent} continues for its own look",
                name(&n.by),
                n.kind.as_deref().map(|k| format!(" as {k}")).unwrap_or_default()
            ),
            since_t: (n.t.0 as f64 / 1000.0).min(now_wall),
            op_id: Some(op.id),
            intent_id: Some(op.intent_id),
        })
        .collect()
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
    /// Rules 23: while the order looks, an informational alert for each other vehicle that
    /// identified its track meanwhile ([`meanwhile_alerts`]).
    pub(super) fn identify_alerts(&self, core: &Core, now_wall: f64) -> Vec<alerts::Alert> {
        if !self.rules.identify() {
            return Vec::new();
        }
        let fleet = self.fleet();
        let name = |uid: &str| {
            uid.parse::<crate::vehicle::Uid>()
                .ok()
                .and_then(|u| fleet.get(&u))
                .and_then(|c| alerts::callsign(c))
                .unwrap_or_else(|| format!("vehicle {uid}"))
        };
        self.world
            .tasks
            .current_ops()
            .into_iter()
            .filter(|op| !matches!(op.state, OpState::Failed | OpState::Superseded))
            .flat_map(|op| {
                let Some(i) = core.engine.intent(IntentId(op.intent_id)) else { return Vec::new() };
                let Some(track) = track_of(i.spec()) else { return Vec::new() };
                let Some(run) = i.identify_run() else { return Vec::new() };
                // rules 23: while it looks, what other vehicles identified meanwhile
                let mut out = if run.ended.is_none() { meanwhile_alerts(&op, track, &run.meanwhile, &name, now_wall) } else { Vec::new() };
                let (kind, severity, detail) = match (&run.ended, &run.held) {
                    (Some((murmur_decide::identify::EndKind::Identified, d)), _) if d.contains("; holding") => {
                        ("identify_holding", alerts::Severity::Medium, format!("identify {track}: {d}"))
                    }
                    (Some((murmur_decide::identify::EndKind::Identified, _)), _) => return out,
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
                    (None, None) => return out,
                };
                out.push(alerts::Alert {
                    id: format!("identify:{}", op.intent_id),
                    kind,
                    severity,
                    uid: op.uid,
                    callsign: fleet.get(&op.uid).and_then(|c| alerts::callsign(c)),
                    detail,
                    since_t: op.history.first().map_or(now_wall, |h| h.t),
                    op_id: Some(op.id),
                    intent_id: Some(op.intent_id),
                });
                out
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

#[cfg(test)]
mod tests {
    use murmur_decide::Time;
    use murmur_decide::identify::Noted;

    use super::meanwhile_alerts;
    use crate::decision::alerts::Severity;
    use crate::tasks::{HistoryEntry, OpSnapshot, OpState, TaskKind};

    /// Rules 23: one informational alert for each other vehicle that identified the order's track
    /// meanwhile (its latest kind), on the order's vehicle, saying when it was noted.
    #[test]
    fn another_vehicles_identification_is_one_informational_alert_per_vehicle() {
        let op = OpSnapshot {
            id: 12,
            tasking: 4,
            intent_id: 4,
            task: TaskKind::Orbit,
            uid: 3,
            sysid: 3,
            state: OpState::Active,
            detail: "orbiting".into(),
            progress: None,
            history: vec![HistoryEntry { state: OpState::Active, t: 100.0 }],
        };
        let name = |u: &str| match u {
            "1" => "HUNTER 1".to_string(),
            "3" => "PUMA 1".to_string(),
            u => format!("vehicle {u}"),
        };
        let noted = [
            Noted { by: "1".into(), kind: Some("fishing_vessel".into()), t: Time(150_000) },
            Noted { by: "2".into(), kind: None, t: Time(155_000) },
            Noted { by: "1".into(), kind: Some("cargo_ship".into()), t: Time(160_000) },
        ];
        let a = meanwhile_alerts(&op, "T-0001", &noted, &name, 200.0);
        assert_eq!(a.len(), 2, "{a:?}");
        assert_eq!((a[0].kind, a[0].severity, a[0].uid), ("identify_meanwhile", Severity::Info, 3));
        assert_eq!(a[0].detail, "HUNTER 1 identified T-0001 as cargo_ship; PUMA 1 continues for its own look");
        assert_eq!((a[0].id.as_str(), a[0].since_t, a[0].intent_id), ("identify-meanwhile:4:1", 160.0, Some(4)));
        assert_eq!(a[1].detail, "vehicle 2 identified T-0001; PUMA 1 continues for its own look");
    }
}
