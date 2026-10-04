//! The safe command queue in the bridge (rules 5, murmur-5p4): orders for vehicles the link
//! can't carry them to right now are held, not sent into a dead link, and survive a bridge
//! restart.
//!
//! - **Dispatch or queue.** An approved intent goes to [`murmur_decide::Engine::dispatch_or_queue`]
//!   with the world now: dispatched when every vehicle is reachable (heard within its link's
//!   lost-link time; a mission upload also needs a link that is not an AUV's acoustic modem),
//!   else queued with a deadline (`[queue]` per kind in `sim/policy.toml`). A newer order
//!   queued for a vehicle voids an older queued one (the newest order stands), as a dispatch
//!   already did.
//! - **Dequeue.** Each tick, every queued intent is offered to the engine with a fresh
//!   snapshot ([`murmur_decide::Engine::dequeue`]): still unreachable, nothing happens;
//!   reachable, its checks run on the world then and the engine dispatches it, sends it back
//!   to the inbox, or voids it. The executor gets the kind's queue wait as its patience for
//!   a link lost mid-exchange (`tasks::exec`).
//! - **Restart.** The bridge keeps a pointer to its log and the ids it leaves open
//!   (`logs/audit/.resume-<http port>.json`, rewritten when that set changes). A bridge that
//!   starts within `MURMUR_RESUME_WINDOW_S` (900 s) of the last write replays that log and
//!   carries every open vehicle intent over (`intent.resumed`): queued (and approved, or
//!   dispatched but never started) ones continue queued, re-checked before they go and, if
//!   they may have been delivered, checked against what the vehicle holds first; dispatched
//!   ones some vehicle had started are followed there (`Resume::Adopt`), never sent again.
//!   `MURMUR_RESUME=0` turns this off, `MURMUR_RESUME=<log.jsonl>` resumes from that log.
//! - **Reboots.** A vehicle whose time since boot went back is logged (`vehicle.rebooted`);
//!   its running op ends (`tasks::exec`), and its boot instant is a fact the dequeue
//!   re-check compares with the approval.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use murmur_decide::queue::{self as dq, Reach};
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str_with};
use murmur_decide::{
    Dequeued, DispatchOrder, Intent, IntentId, Order, ResumedFrom, Routed, State, TaskKind as DKind,
};
use serde_json::{Value, json};

use super::snapshot::{bridge_uid, duid};
use super::{AuthorizedOrder, Core, Decision, DecisionError, Running, engine_err, task_request};
use crate::clock;
use crate::tasks::{Resume, TaskOrder};
use crate::vehicle::Uid;

/// A bridge that restarts within this long of its previous run's last pointer write carries
/// that run's open intents over (`MURMUR_RESUME_WINDOW_S`).
pub const RESUME_WINDOW_S: f64 = 900.0;

/// Where the previous run's open intents come from, at start (`MURMUR_RESUME`).
#[derive(Debug, Clone, PartialEq)]
pub enum ResumeFrom {
    /// Not at all (`MURMUR_RESUME=0`).
    Off,
    /// The log the pointer file names, if it was written within the window.
    Pointer { window_s: f64 },
    /// This log (`MURMUR_RESUME=<path>`).
    Log(PathBuf),
}

/// The pointer file: the log this bridge writes and the intents it leaves open now.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pointer {
    pub log: PathBuf,
    pub open: Vec<u64>,
    /// Wall clock, s.
    pub t: f64,
}

/// A resumed intent some vehicle had started, waiting for its vehicles to connect before
/// the follow tasking starts.
pub(super) struct PendingFollow {
    /// Each vehicle's last op state in the previous run.
    ops: BTreeMap<murmur_decide::Uid, String>,
    /// When it was carried over (monotonic s): its vehicles have the kind's queue wait to
    /// connect.
    since: f64,
}

impl Decision {
    /// How long an order of `kind` may wait for its vehicles (in the queue, or for a link
    /// lost mid-exchange): `[queue]` per kind; zero under rules before 5.
    pub(super) fn patience(&self, kind: DKind) -> Duration {
        if self.rules.rules_version < 5 {
            return Duration::ZERO;
        }
        Duration::from_secs(u64::from(self.queue_cfg.expire_s(kind)))
    }

    /// A vehicle intent the engine approved: dispatched to its vehicles if the link can carry
    /// it to each, else queued (older queued orders on those vehicles are voided).
    pub(super) fn dispatch_vehicles(
        &self,
        core: &mut Core,
        id: IntentId,
    ) -> Result<(), DecisionError> {
        let world = self.snapshot();
        match core.engine.dispatch_or_queue(id, &world).map_err(engine_err)? {
            Routed::Dispatched(Order::Vehicles(released)) => {
                self.run_order(core, id, released, &HashMap::new());
                Ok(())
            }
            Routed::Dispatched(_) => Err(DecisionError::new(
                500,
                "a vehicle intent was dispatched as something else",
            )),
            Routed::Queued { .. } => {
                let uids = core
                    .engine
                    .intent(id)
                    .map(|i| i.spec().uids.clone())
                    .unwrap_or_default();
                self.supersede_queued(core, id, &uids);
                Ok(())
            }
        }
    }

    /// Hand a released order to the task layer (the one place it happens for vehicle
    /// intents): with the kind's patience for a lost link and, after a restart, how each
    /// vehicle's op delivers it. Older orders waiting for these vehicles are superseded.
    pub(super) fn run_order(
        &self,
        core: &mut Core,
        id: IntentId,
        released: DispatchOrder,
        resume: &HashMap<Uid, Resume>,
    ) {
        let fleet = self.fleet();
        let mut order = match task_request(&released.task, &released.uids)
            .and_then(|r| TaskOrder::new(r, &fleet).map_err(|e| e.to_string()))
        {
            Ok(o) => o,
            Err(e) => {
                if let Err(err) = core.engine.finish(
                    id,
                    murmur_decide::TaskOutcome::Failed {
                        detail: format!("the task layer refused the order: {e}"),
                    },
                ) {
                    eprintln!("decision: finish {id}: {err}");
                }
                return;
            }
        };
        order.patience = self.patience(released.task.kind());
        order.resume = resume.clone();
        let tasking = self.world.tasks.dispatch(AuthorizedOrder {
            intent: released.intent,
            order,
        });
        core.taskings.insert(id, tasking.clone());
        core.running.insert(
            id,
            Running {
                tasking,
                reported: HashMap::new(),
            },
        );
        self.supersede(core, id, &released.uids);
    }

    /// A newer order queued for these vehicles: older queued ones on them are voided (the
    /// newest order stands, as at a dispatch).
    fn supersede_queued(&self, core: &mut Core, id: IntentId, uids: &[murmur_decide::Uid]) {
        let stale: Vec<(IntentId, murmur_decide::Uid)> = core
            .engine
            .intents()
            .filter(|i| i.id() != id && i.id() < id && i.state() == State::Queued)
            .filter_map(|i| {
                let u = i.spec().uids.iter().find(|u| uids.contains(u))?;
                Some((i.id(), u.clone()))
            })
            .collect();
        for (other, uid) in stale {
            let why = format!("superseded: {id} was queued for vehicle {uid} (the newest order stands)");
            if let Err(e) = core.engine.void(other, &why) {
                eprintln!("decision: void {other}: {e}");
            }
        }
    }

    /// Each tick: offer every queued intent to the engine on the world now.
    pub(super) fn follow_queue(&self, core: &mut Core) {
        let queued: Vec<IntentId> = core
            .engine
            .intents()
            .filter(|i| i.state() == State::Queued)
            .map(Intent::id)
            .collect();
        if queued.is_empty() {
            return;
        }
        let world = self.snapshot();
        for id in queued {
            // an older one may have been superseded by a younger one's dispatch this tick
            if core.engine.intent(id).map(Intent::state) != Some(State::Queued) {
                continue;
            }
            // its vehicles must be in the fleet for the task layer to take it
            let Some(intent) = core.engine.intent(id) else {
                continue;
            };
            let resume = resume_modes(intent);
            match core.engine.dequeue(id, &world, &self.terrain.oracle()) {
                Ok(Dequeued::Dispatched(Order::Vehicles(o))) => {
                    self.run_order(core, id, o, &resume);
                }
                Ok(Dequeued::Dispatched(_)) => {
                    eprintln!("decision: queued intent {id} dispatched as something else");
                }
                Ok(_) => {}
                Err(e) => eprintln!("decision: dequeue {id}: {e}"),
            }
        }
    }

    /// The queued intents for the console: each with why it waits now (its vehicles' reach on
    /// the world now; display only, nothing logged) and its deadline.
    pub(super) fn queued_json(&self, core: &Core) -> Vec<Value> {
        let queued: Vec<&Intent> = core
            .engine
            .intents()
            .filter(|i| i.state() == State::Queued)
            .collect();
        if queued.is_empty() {
            return Vec::new();
        }
        let world = self.snapshot();
        queued
            .into_iter()
            .map(|i| {
                let reach: Vec<Reach> = dq::reach(i.spec(), &world, &self.queue_cfg, &self.rules);
                let mut v = super::intent_json(i, None, self.tol());
                v["reach_now"] = json!(reach);
                v
            })
            .collect()
    }

    /// The waypoints of queued intents, for the terrain cache (a dequeue re-check reads
    /// them from it inside the lock).
    pub(super) fn queued_paths(&self) -> Vec<(murmur_decide::Task, Vec<murmur_decide::Uid>)> {
        self.lock()
            .engine
            .intents()
            .filter(|i| i.state() == State::Queued)
            .map(|i| (i.spec().task.clone(), i.spec().uids.clone()))
            .collect()
    }

    /// Reboots the vehicles reported since the last tick, to the audit log.
    pub(super) fn log_reboots(&self, core: &mut Core) {
        for cell in self.world.vehicles() {
            let reboots: Vec<_> = cell.write(|v| v.reboots.drain(..).collect());
            for r in reboots {
                if let Err(e) = core.engine.record_reboot(
                    &duid(cell.uid),
                    r.boot_ms_before,
                    r.boot_ms,
                    super::snapshot::ms(r.booted_wall),
                ) {
                    eprintln!("decision: reboot: {e}");
                }
            }
        }
    }

    // ---- restart ----------------------------------------------------------------------

    /// Carry the previous run's open intents into this one: replay `text` (the log named
    /// `name`), and resume every vehicle intent it leaves open. Refused (nothing carried) if
    /// it does not replay, or was written under rules before 5.
    pub fn carry_over(&self, name: &str, text: &str) -> Result<Vec<IntentId>, String> {
        let report = replay_str_with(
            text,
            &ReplayOptions {
                rules: RulesAccepted::AtLeast(5),
                // its own carried-over intents were checked when it started
                trust_resumed: true,
                ..ReplayOptions::default()
            },
        )
        .map_err(|e| format!("{name}: {e}"))?;
        if !report.ok() {
            return Err(format!(
                "{name} does not replay ({} mismatch(es), first: {}): nothing carried over",
                report.mismatches.len(),
                report.mismatches.first().map_or("", |m| m.what.as_str())
            ));
        }
        let head = report.head_sha256.clone().unwrap_or_default();
        let mut carried = Vec::new();
        {
            let mut guard = self.lock();
            let core = &mut *guard;
            core.engine.reserve_ids(report.next_intent);
            for open in &report.open {
                let from = ResumedFrom {
                    log: name.to_string(),
                    head: head.clone(),
                    seq: report.last_seq,
                    was: open.state,
                };
                match core.engine.resume(open, from) {
                    Ok(id) => {
                        carried.push(id);
                        if open.resumes_as() == State::Dispatched {
                            core.follow.insert(
                                id,
                                PendingFollow {
                                    ops: open.ops.clone(),
                                    since: clock::mono(),
                                },
                            );
                        }
                    }
                    Err(e) => eprintln!("decision: resume {}: {e}", open.spec.id),
                }
            }
        }
        self.publish();
        Ok(carried)
    }

    /// Each tick: resumed intents a vehicle had started get their follow tasking once all
    /// their vehicles are connected; one whose vehicles don't connect within the kind's wait
    /// ends failed.
    pub(super) fn follow_resumed(&self, core: &mut Core) {
        if core.follow.is_empty() {
            return;
        }
        let fleet = self.fleet();
        let ids: Vec<IntentId> = core.follow.keys().copied().collect();
        for id in ids {
            let Some(intent) = core.engine.intent(id) else {
                core.follow.remove(&id);
                continue;
            };
            if intent.state() != State::Dispatched {
                core.follow.remove(&id);
                continue;
            }
            let spec = intent.spec().clone();
            let connected = spec
                .uids
                .iter()
                .all(|u| bridge_uid(u).is_some_and(|b| fleet.contains_key(&b)));
            let Some(pending) = core.follow.get(&id) else {
                continue;
            };
            if !connected {
                if clock::mono() - pending.since > self.patience(spec.kind()).as_secs_f64() {
                    core.follow.remove(&id);
                    let detail = "after the bridge restart its vehicles did not connect: not followed, not sent again".to_string();
                    if let Err(e) =
                        core.engine
                            .finish(id, murmur_decide::TaskOutcome::Failed { detail })
                    {
                        eprintln!("decision: finish {id}: {e}");
                    }
                }
                continue;
            }
            let resume: HashMap<Uid, Resume> = spec
                .uids
                .iter()
                .filter_map(|u| {
                    let mode = match pending.ops.get(u).map(String::as_str) {
                        Some(s) if dq::committed_op(s) => Resume::Adopt,
                        Some(s) if dq::ended_op(s) => Resume::Ended,
                        _ => Resume::Undelivered,
                    };
                    Some((bridge_uid(u)?, mode))
                })
                .collect();
            core.follow.remove(&id);
            let released = DispatchOrder {
                intent: id,
                task: spec.task.clone(),
                uids: spec.uids.clone(),
            };
            self.run_order(core, id, released, &resume);
        }
    }

    /// Keep the pointer file (`logs/audit/.resume-<port>.json`) on the intents this run leaves
    /// open, when that set changed.
    pub(super) fn write_pointer(&self, core: &mut Core) {
        let Some(path) = self.pointer.get() else {
            return;
        };
        let open: Vec<u64> = core
            .engine
            .intents()
            .filter(|i| {
                dq::queueable(i.spec().kind())
                    && matches!(
                        i.state(),
                        State::Queued | State::Approved | State::AutoApproved | State::Dispatched
                    )
            })
            .map(|i| i.id().0)
            .collect();
        // rewritten when the set changes, and now and then so its time says the run is alive
        let stale = core
            .pointer_written
            .as_ref()
            .is_none_or(|(ids, t)| *ids != open || clock::wall() - t > 60.0);
        if !stale {
            return;
        }
        let p = Pointer {
            log: self.audit_path.clone(),
            open: open.clone(),
            t: clock::wall(),
        };
        match write_atomic(path, &p) {
            Ok(()) => core.pointer_written = Some((open, p.t)),
            Err(e) => eprintln!("decision: {}: {e}", path.display()),
        }
    }
}

/// How a dequeued intent's ops deliver it: one that the previous run had dispatched (not
/// started: it resumed queued) may be on the vehicle already, so each op checks first.
fn resume_modes(intent: &Intent) -> HashMap<Uid, Resume> {
    match intent.resumed() {
        Some(r) if r.from.was == State::Dispatched => intent
            .spec()
            .uids
            .iter()
            .filter_map(|u| Some((bridge_uid(u)?, Resume::Verify)))
            .collect(),
        _ => HashMap::new(),
    }
}

fn write_atomic(path: &Path, p: &Pointer) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(p).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

/// At start: the previous run's log to carry open intents over from, if any (see
/// [`ResumeFrom`]). The pointer must name a log other than ours and have been written within
/// the window. Its list of open intents is not trusted to be current (it is rewritten once a
/// tick, and a crash can come between an order and that write): the log says what is open.
pub fn previous_log(from: &ResumeFrom, pointer: Option<&Path>, ours: &Path) -> Option<PathBuf> {
    match from {
        ResumeFrom::Off => None,
        ResumeFrom::Log(p) => Some(p.clone()),
        ResumeFrom::Pointer { window_s } => {
            let text = std::fs::read(pointer?).ok()?;
            let p: Pointer = serde_json::from_slice(&text).ok()?;
            let fresh = clock::wall() - p.t <= *window_s;
            (fresh && p.log != ours && p.log.exists()).then_some(p.log)
        }
    }
}
