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
//!   to the inbox, or voids it (also when a newer order for one of its vehicles stands: an
//!   older order never goes over a newer one).
//! - **Requeue.** A dispatched order whose link is lost before its vehicle started it ends
//!   its op `requeued` (`tasks::exec`); when every op of the order ended so, it goes back to
//!   the queue ([`murmur_decide::Engine::requeue`]), re-checked before it is sent again, and
//!   then delivered asking the vehicle what it holds first (`Resume::Verify`). An op that
//!   follows an order its vehicle had started waits for a lost link the kind's queue wait,
//!   120 s at most. A newer order queued for a vehicle stops an older order's op there that
//!   has not started (failed, "abandoned").
//! - **Restart.** The bridge keeps a pointer to its log and the ids it leaves open
//!   (`logs/audit/.resume-<http port>.json`, rewritten when that set changes, with the pid of
//!   the bridge that writes it); a clean shutdown deletes it. A bridge that starts within
//!   `MURMUR_RESUME_WINDOW_S` (900 s) of the last write of a pointer left behind (a crash),
//!   whose writer is no longer running, replays that log and carries every open vehicle intent
//!   over (`intent.resumed`): queued (and approved, or dispatched but never started) ones
//!   continue queued, re-checked before they go (a human's approval goes back to the inbox)
//!   and, if they may have been delivered, checked against what the vehicle holds first;
//!   dispatched ones some vehicle had started are followed there (`Resume::Adopt`), never
//!   sent again. The log must match its anchors (a crash leaves no shutdown anchor), each
//!   intent's vehicles must be the same platforms under the same uids in this run's fleet,
//!   and an approval must be by an operator of this run, or that intent is not carried.
//!   `MURMUR_RESUME=0` turns this off, `MURMUR_RESUME=<log.jsonl>` resumes from that log.
//! - **Reboots.** A vehicle whose time since boot went back is logged (`vehicle.rebooted`);
//!   its running op ends (`tasks::exec`), and its boot instant is a fact the dequeue
//!   re-check compares with the approval.
//! - **Rules 7** (`QUEUE_FOLLOWUPS_SINCE`). Each op logs its start before sending it
//!   ([`Decision::write_ahead_hook`]: `op.state` starting, `ahead`), so a crash right after a
//!   start still leaves the order may-have-started; a mission waits in the queue while its
//!   platform is owed its own lost-link values after an engagement (the engine's reach, shown
//!   in the queue's `reach_now`); and a queued order supersedes an engagement on its vehicles
//!   as a dispatched one does (no fallback).

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
    /// The log the pointer file names, if it was written within the window by a bridge that
    /// is no longer running (a clean shutdown deletes the pointer).
    Pointer { window_s: f64 },
    /// This log (`MURMUR_RESUME=<path>`).
    Log(PathBuf),
}

/// The pointer file: the log this bridge writes and the intents it leaves open now.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pointer {
    pub log: PathBuf,
    pub open: Vec<u64>,
    /// The wall clock when it was written (`clock::utc`), s: another run reads it.
    pub t: f64,
    /// The bridge process that writes it (0: unknown).
    #[serde(default)]
    pub pid: u32,
}

/// A vehicle as this run's fleet names it (the manifest; a connected vehicle's own identity
/// and home): what an order carried over a restart must find again under its uid. Written
/// beside the audit log (`<log>.fleet.json`) as the run goes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VehicleIdentity {
    pub platform: Option<String>,
    pub callsign: Option<String>,
    /// Its home, [lat, lon]: the manifest's spawn point, or where it reported its home.
    pub home: Option<[f64; 2]>,
}

/// The fleet a run had, by uid.
pub type FleetIdentity = BTreeMap<Uid, VehicleIdentity>;

/// Two homes this close are the same vehicle's (a respawn of the same fleet), m.
pub const HOME_TOLERANCE_M: f64 = 200.0;

/// What a carry-over is checked against besides the log itself: the previous log's anchor
/// file and its fleet identity. `None`: not checked (the caller decided: tests, or
/// `MURMUR_RESUME_UNVERIFIED=1`).
#[derive(Debug, Clone, Default)]
pub struct CarryCheck {
    pub anchors: Option<String>,
    pub fleet: Option<FleetIdentity>,
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
    /// How long an op following an order its vehicle had started (after a bridge restart)
    /// waits for a lost link, and how long a resumed order's vehicles have to connect: the
    /// kind's queue wait, 120 s at most; zero under rules before 5.
    pub(super) fn patience(&self, kind: DKind) -> Duration {
        if self.rules.rules_version < 5 {
            return Duration::ZERO;
        }
        Duration::from_secs(u64::from(self.queue_cfg.in_flight_patience_s(kind)))
    }

    /// Whether a link lost before the vehicle started an order of `kind` sends the order back
    /// to the queue (rules 5, a kind the policy queues), instead of failing it.
    pub(super) fn requeues(&self, kind: DKind) -> bool {
        self.rules.rules_version >= 5 && self.queue_cfg.queues(kind)
    }

    /// A vehicle intent the engine approved: dispatched to its vehicles if the link can carry
    /// it to each, else queued (older queued orders on those vehicles are voided). One sent
    /// before (a human approved it again after it came back to the inbox) is delivered
    /// asking each vehicle what it holds first, as from the queue.
    pub(super) fn dispatch_vehicles(
        &self,
        core: &mut Core,
        id: IntentId,
    ) -> Result<(), DecisionError> {
        let world = self.snapshot();
        let resume = core.engine.intent(id).map(resume_modes).unwrap_or_default();
        match core.engine.dispatch_or_queue(id, &world).map_err(engine_err)? {
            Routed::Dispatched(Order::Vehicles(released)) => {
                self.run_order(core, id, released, &resume);
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
            // rules 7: refused, saying why (a mission of a kind never queued, its platform
            // still owed its own lost-link values): nothing sent, but the newest order stands
            // over an engagement all the same, as a dispatch or a queueing would (stopped in
            // this step, superseded, no fallback), so the platform's own values can go back
            Routed::Voided { .. } => {
                let uids = core
                    .engine
                    .intent(id)
                    .map(|i| i.spec().uids.clone())
                    .unwrap_or_default();
                self.stop_superseded_engagements(core, id, &uids);
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
        order.requeue = self.requeues(released.task.kind());
        order.resume = resume.clone();
        // rules 21: an identify order is flown as its orbit, its camera looking for its track
        // first; never queued (its track moves on)
        let look = core
            .engine
            .intent(id)
            .and_then(|i| super::identify::track_of(i.spec()).map(str::to_string));
        if look.is_some() {
            order.requeue = false;
            order.params.look = look.clone();
        }
        // rules 7: each vehicle's start in the audit log before it goes out
        order.ahead = self.write_ahead_hook();
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
                look: look.is_some(),
            },
        );
        self.supersede(core, id, &released.uids);
    }

    /// A newer order queued for these vehicles: older queued ones on them are voided, and an
    /// older order's op there that its vehicle has not started (waiting for a fix or a link,
    /// mid-exchange) is stopped (the newest order stands, as at a dispatch). Never a newer one.
    /// Rules 7 (murmur-636): an engagement in play on them is stopped too, superseded by this
    /// order with no fallback commanded, as its dispatch would: before, the abandoned
    /// intercept stopped it as a system failure and its fallback flew over this order.
    fn supersede_queued(&self, core: &mut Core, id: IntentId, uids: &[murmur_decide::Uid]) {
        if self.rules.rules_version >= murmur_decide::QUEUE_FOLLOWUPS_SINCE {
            self.stop_superseded_engagements(core, id, uids);
        }
        let stale: Vec<(IntentId, murmur_decide::Uid)> = core
            .engine
            .intents()
            .filter(|i| i.id() < id && i.state() == State::Queued)
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
        for u in uids {
            let Some(b) = bridge_uid(u) else { continue };
            let detail = format!(
                "abandoned before the vehicle started it: {id} was queued for vehicle {u} (the newest order stands)"
            );
            self.world.tasks.abandon_unstarted(b, id.0, &detail);
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
                // as its dequeue would judge it (rules 7: a platform still owed its own
                // lost-link values holds a mission)
                let reach: Vec<Reach> = core
                    .engine
                    .reach_now(i.id(), &world)
                    .unwrap_or_else(|| dq::reach(i.spec(), &world, &self.queue_cfg, &self.rules));
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
                    r.booted_before_wall.map(super::snapshot::ms),
                ) {
                    eprintln!("decision: reboot: {e}");
                }
            }
        }
    }

    // ---- restart ----------------------------------------------------------------------

    /// Carry the previous run's open intents into this one: replay `text` (the log named
    /// `name`), and resume every vehicle intent it leaves open. Refused (nothing carried) if
    /// it does not replay, was written under rules before 5, or does not match its anchor
    /// file (`anchors`, when there is one; a missing shutdown anchor is a crash, the case this
    /// is for), and nothing from a run that authenticated its operators otherwise (dev or
    /// credentials; a log of rules before 7 counts as dev). An intent is not carried when
    /// one of its vehicles is not the same platform under its uid in this run's fleet, or
    /// when a human approved it who is not an operator of this run.
    pub fn carry_over(
        &self,
        name: &str,
        text: &str,
        check: &CarryCheck,
    ) -> Result<Vec<IntentId>, String> {
        if let Some(a) = &check.anchors
            && let Some(why) = anchors_refuse(text, a)
        {
            return Err(format!(
                "{name} does not match its anchors ({why}): nothing carried over"
            ));
        }
        let report = replay_str_with(
            text,
            &ReplayOptions {
                rules: RulesAccepted::AtLeast(5),
                // its own carried-over intents were checked when it started; the lost-link
                // values it carried (rules 6) never were, and are trusted here too (murmur-cq3)
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
        // rules 8: only between runs that authenticate operators alike (an approval made on
        // asserted ids never stands in a credentials run, nor a person's in a dev run; a log
        // of earlier rules asserted its ids)
        let was = murmur_decide::authority::effective_mode(report.auth);
        if was != self.auth {
            return Err(format!(
                "{name} was a {} run and this is a {} run: approvals are carried only between runs that authenticate operators alike; nothing carried over",
                was.as_str(),
                self.auth.as_str()
            ));
        }
        let head = report.head_sha256.clone().unwrap_or_default();
        let platforms = self.fleet_platforms();
        let fleet_now = self.fleet_identity();
        let mut carried = Vec::new();
        {
            let mut guard = self.lock();
            let core = &mut *guard;
            core.engine.reserve_ids(report.next_intent);
            for open in &report.open {
                let why = refuse_carry(open, &platforms, &self.operators).or_else(|| {
                    check
                        .fleet
                        .as_ref()
                        .and_then(|was| other_vehicle(&open.spec.uids, was, &fleet_now))
                });
                if let Some(why) = why {
                    eprintln!("decision: {} not carried over: {why}", open.spec.id);
                    continue;
                }
                let from = ResumedFrom {
                    log: name.to_string(),
                    head: head.clone(),
                    seq: report.last_seq,
                    was: open.state,
                    auth: report.auth,
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

    /// The platform under each uid in this run's fleet: the manifest file's, or the
    /// identity of a vehicle already connected (which wins).
    fn fleet_platforms(&self) -> BTreeMap<murmur_decide::Uid, String> {
        let mut out = BTreeMap::new();
        if let Some(data) = std::fs::read(&self.world.cfg.manifest)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        {
            let (m, _) = crate::world::Manifest::parse(&data);
            for (uid, identity) in &m.identities {
                if let Some(p) = identity.str("platform") {
                    out.insert(duid(*uid), p.to_string());
                }
            }
        }
        for cell in self.world.vehicles() {
            if let Some(p) = cell.read(|v| {
                v.identity
                    .as_ref()
                    .and_then(|i| i.str("platform").map(str::to_string))
            }) {
                out.insert(duid(cell.uid), p);
            }
        }
        out
    }

    /// This run's fleet as it stands: the manifest's entries (platform, callsign, spawn
    /// point), and a connected vehicle's own identity and reported home where the manifest
    /// has none.
    pub fn fleet_identity(&self) -> FleetIdentity {
        // the manifest file itself: its entries carry the spawn point the world's identities
        // leave out
        let mut out: FleetIdentity = std::fs::read(&self.world.cfg.manifest)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|data| manifest_identities(&data))
            .unwrap_or_default();
        for cell in self.world.vehicles() {
            let (identity, home) = cell.read(|v| (v.identity.clone(), v.home));
            let now = identity_of_opt(identity.as_deref(), home);
            let e = out.entry(cell.uid).or_insert_with(|| now.clone());
            e.platform = e.platform.clone().or(now.platform);
            e.callsign = e.callsign.clone().or(now.callsign);
            e.home = e.home.or(now.home);
        }
        out
    }

    /// Keep the fleet identity beside the audit log (`<log>.fleet.json`) as it changes: what
    /// a next run checks a carried-over order's vehicles against.
    pub(super) fn write_fleet(&self, core: &mut Core) {
        let Some(path) = self.fleet_file.get() else {
            return;
        };
        // looked at every few seconds: the manifest can be large
        let now = clock::mono();
        if core.fleet_looked.is_some_and(|t| now - t < FLEET_LOOK_S) {
            return;
        }
        core.fleet_looked = Some(now);
        let fleet = self.fleet_identity();
        if core.fleet_written.as_ref() == Some(&fleet) {
            return;
        }
        let json = serde_json::to_vec_pretty(&fleet).unwrap_or_default();
        let tmp = path.with_extension("tmp");
        match std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, path)) {
            Ok(()) => core.fleet_written = Some(fleet),
            Err(e) => eprintln!("decision: {}: {e}", path.display()),
        }
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
            // a newer order for one of its vehicles stands: the older one is not followed
            if let Some((newer, uid)) = core.engine.newer_order(id) {
                core.follow.remove(&id);
                let detail = format!(
                    "after the bridge restart: {newer} is a newer order for vehicle {uid}: not followed, not sent again"
                );
                if let Err(e) = core
                    .engine
                    .finish(id, murmur_decide::TaskOutcome::Failed { detail })
                {
                    eprintln!("decision: finish {id}: {e}");
                }
                continue;
            }
            let committed = intent.committed().clone();
            let maybe_started = intent.maybe_started().clone();
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
                    // started (in any run before: the set is sticky) and not ended: followed;
                    // its start went out unconfirmed: followed only if the vehicle runs it
                    let mode = match pending.ops.get(u).map(String::as_str) {
                        Some(s) if dq::committed_op(s) => Resume::Adopt,
                        _ if committed.contains(u) => Resume::Adopt,
                        _ if maybe_started.contains(u) => Resume::Confirm,
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
        // (the next run reads that time against its own wall clock: `utc`, not this run's
        // clock, which keeps the monotonic rate)
        let stale = core
            .pointer_written
            .as_ref()
            .is_none_or(|(ids, at)| *ids != open || clock::mono() - at > 60.0);
        if !stale {
            return;
        }
        let p = Pointer {
            log: self.audit_path.clone(),
            open: open.clone(),
            t: clock::utc(),
            pid: std::process::id(),
        };
        match write_atomic(path, &p) {
            Ok(()) => core.pointer_written = Some((open, clock::mono())),
            Err(e) => eprintln!("decision: {}: {e}", path.display()),
        }
    }
}

/// How an intent dispatched before (in this run, before its link was lost, or in a run
/// before a restart; and now out of the queue, or approved again in the inbox) is delivered:
/// each op asks its vehicle what it holds first (`Verify`); where its start went out and
/// nothing confirmed it, it is only followed if the vehicle runs it (`Confirm`: never
/// started again on its own). The engine's `confirming` set is the same.
fn resume_modes(intent: &Intent) -> HashMap<Uid, Resume> {
    if !intent.dispatched_before() {
        return HashMap::new();
    }
    intent
        .spec()
        .uids
        .iter()
        .filter_map(|u| {
            let mode = if intent.maybe_started().contains(u) {
                Resume::Confirm
            } else {
                Resume::Verify
            };
            Some((bridge_uid(u)?, mode))
        })
        .collect()
}

/// Why an open intent of the previous run is not carried into this one, if it isn't: a vehicle
/// that is another platform under its uid now (or not in this run's fleet), or an approval
/// by someone who is not an operator of this run.
fn refuse_carry(
    open: &dq::OpenIntent,
    platforms: &BTreeMap<murmur_decide::Uid, String>,
    operators: &[murmur_decide::OperatorId],
) -> Option<String> {
    for u in &open.spec.uids {
        let now = platforms.get(u);
        match (open.platforms.get(u), now) {
            (Some(was), Some(now)) if was == now => {}
            (Some(was), Some(now)) => {
                return Some(format!(
                    "vehicle {u} was a {was}, is a {now} in this run's fleet"
                ));
            }
            (Some(was), None) => {
                return Some(format!("vehicle {u} (a {was}) is not in this run's fleet"));
            }
            (None, _) => {
                return Some(format!(
                    "the previous log does not say which platform vehicle {u} was"
                ));
            }
        }
    }
    if let Some(a) = &open.approval
        && !operators.contains(&a.by)
    {
        return Some(format!(
            "approved by {}, who is not an operator of this run",
            a.by
        ));
    }
    None
}

/// A manifest identity as a [`VehicleIdentity`]; its home from the manifest's spawn point
/// (`home`: [lat, lon, alt, heading]), else `reported`.
fn identity_of(i: &crate::vehicle::Identity, reported: Option<(f64, f64)>) -> VehicleIdentity {
    let spawn = i.0.get("home").and_then(Value::as_array).and_then(|a| {
        Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
    });
    VehicleIdentity {
        platform: i.str("platform").map(str::to_string),
        callsign: i.str("callsign").map(str::to_string),
        home: spawn.or(reported.map(|(la, lo)| [la, lo])),
    }
}

/// How often the fleet identity beside the log is brought up to date, s.
const FLEET_LOOK_S: f64 = 5.0;

/// The fleet manifest's vehicles (`sim/run/fleet.json`: `vehicles`, each with its `uid`
/// (else its sysid), `platform`, `callsign` and spawn point `home` [lat, lon, alt, heading]).
pub fn manifest_identities(data: &Value) -> FleetIdentity {
    let mut out = FleetIdentity::new();
    for v in data
        .get("vehicles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(uid) = v
            .get("uid")
            .and_then(Value::as_u64)
            .or_else(|| v.get("sysid").and_then(Value::as_u64))
        else {
            continue;
        };
        let home = v.get("home").and_then(Value::as_array).and_then(|a| {
            Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
        });
        out.insert(
            uid,
            VehicleIdentity {
                platform: v.get("platform").and_then(Value::as_str).map(str::to_string),
                callsign: v.get("callsign").and_then(Value::as_str).map(str::to_string),
                home,
            },
        );
    }
    out
}

fn identity_of_opt(
    i: Option<&crate::vehicle::Identity>,
    reported: Option<(f64, f64)>,
) -> VehicleIdentity {
    match i {
        Some(i) => identity_of(i, reported),
        None => VehicleIdentity {
            platform: None,
            callsign: None,
            home: reported.map(|(la, lo)| [la, lo]),
        },
    }
}

/// Why the vehicles under an order's uids in this run are not the ones of the run it comes
/// from, if they are not: another platform or callsign, or a home farther than
/// [`HOME_TOLERANCE_M`] (another vehicle of the same type elsewhere); or one side does not
/// say (it can't be told the same).
fn other_vehicle(
    uids: &[murmur_decide::Uid],
    was: &FleetIdentity,
    now: &FleetIdentity,
) -> Option<String> {
    for u in uids {
        let Some(b) = bridge_uid(u) else {
            return Some(format!("vehicle {u} is not a fleet uid"));
        };
        let (Some(w), Some(n)) = (was.get(&b), now.get(&b)) else {
            return Some(format!(
                "vehicle {u} is not in both runs' fleets (the previous run's fleet identity, this run's manifest)"
            ));
        };
        if w.platform != n.platform || w.callsign != n.callsign {
            return Some(format!(
                "vehicle {u} was {} ({}), is {} ({}) in this run's fleet",
                w.callsign.as_deref().unwrap_or("?"),
                w.platform.as_deref().unwrap_or("?"),
                n.callsign.as_deref().unwrap_or("?"),
                n.platform.as_deref().unwrap_or("?"),
            ));
        }
        match (w.home, n.home) {
            (Some(a), Some(c)) => {
                let d = crate::geo::haversine_m(a[0], a[1], c[0], c[1]);
                if d > HOME_TOLERANCE_M {
                    return Some(format!(
                        "vehicle {u} ({}) has its home {d:.0} m from the previous run's: another vehicle",
                        n.callsign.as_deref().unwrap_or("?")
                    ));
                }
            }
            _ => {
                return Some(format!(
                    "vehicle {u}: no home position on one side, so it can't be told the same vehicle"
                ));
            }
        }
    }
    None
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
/// [`ResumeFrom`]). The pointer must name a log other than ours, have been written within
/// the window, and by a bridge that is no longer running (a clean shutdown deletes it, so a
/// pointer left behind means a crash). Its list of open intents is not trusted to be current
/// (it is rewritten once a tick, and a crash can come between an order and that write): the
/// log says what is open.
pub fn previous_log(from: &ResumeFrom, pointer: Option<&Path>, ours: &Path) -> Option<PathBuf> {
    match from {
        ResumeFrom::Off => None,
        ResumeFrom::Log(p) => Some(p.clone()),
        ResumeFrom::Pointer { window_s } => {
            let text = std::fs::read(pointer?).ok()?;
            let p: Pointer = serde_json::from_slice(&text).ok()?;
            let fresh = clock::utc() - p.t <= *window_s;
            let writer_alive = p.pid != 0
                && p.pid != std::process::id()
                && Path::new("/proc").join(p.pid.to_string()).exists();
            (fresh && !writer_alive && p.log != ours && p.log.exists()).then_some(p.log)
        }
    }
}

/// The fleet identity file beside an audit log: `<log>.fleet.json`.
pub fn fleet_file(log: &Path) -> PathBuf {
    log.with_extension("fleet.json")
}

/// Delete the pointer file if it names `ours` (a clean shutdown: nothing is left to carry
/// over, and the next run starts afresh).
pub fn drop_pointer(pointer: &Path, ours: &Path) {
    let names_ours = std::fs::read(pointer)
        .ok()
        .and_then(|b| serde_json::from_slice::<Pointer>(&b).ok())
        .is_some_and(|p| p.log == ours);
    if names_ours && let Err(e) = std::fs::remove_file(pointer) {
        eprintln!("decision: {}: {e}", pointer.display());
    }
}

/// Why a previous log's anchor file refuses it, if it does: every problem `verify` finds but
/// the missing shutdown anchor (a crashed run, the one a carry-over is for).
pub fn anchors_refuse(log: &str, anchors: &str) -> Option<String> {
    let r = murmur_decide::anchor::verify(log, anchors);
    r.problems
        .iter()
        .find(|p| !p.what.starts_with("no shutdown anchor"))
        .map(|p| format!("seq {}: {}", p.seq, p.what))
}
