//! Rules 34 (docs/SWARMS.md section 13, stage 5; bd murmur-4nik): group orders on the bridge.
//!
//! An order of an ordinary kind (orbit, patrol, hold, rtl) to more than one vehicle is a group
//! order: the engine lays its members out, checks each share and routes the group once
//! (`murmur_decide::group`). Approved, it goes out here as its member orders
//! ([`Decision::dispatch_group`]): each a one-vehicle intent dispatched or queued on its own, its
//! share handed to the task layer as logged (`tasks::MemberShare`: the altitude, where it enters
//! the ring or the route), the task layer's own slots off. Each tick the engine steps every group
//! in force ([`Decision::step_groups`]: drops, re-forms inside the approved envelope and tier,
//! suspensions below the minimum), and the member orders a re-form makes go out the same way.
//! Beside the tick, the shares a re-form due would move members to are laid out outside the
//! decision lock and their terrain fetched ([`Decision::prefetch_groups`]), so the tick's step
//! judges them from the cache.

use murmur_decide::{Clock, IntentId, Order, Routed, Task, Uid as DUid, WorldSnapshot};

use super::{Core, Decision, DecisionError, engine_err};

impl Decision {
    /// Rules 34: an approved group order out as its member orders (the engine creates each under
    /// the group's approval and dispatches or queues it on its own), each then run as any
    /// one-vehicle order.
    pub(super) fn dispatch_group(&self, core: &mut Core, id: IntentId) -> Result<(), DecisionError> {
        let world = self.snapshot();
        let routed = core.engine.dispatch_group(id, &world).map_err(engine_err)?;
        self.run_members(core, routed);
        Ok(())
    }

    /// Member orders the engine dispatched or queued: each dispatched one to the task layer with
    /// its share; a queued one supersedes older queued orders on its vehicle, as any queued order
    /// does. A member an engagement holds was left out by its group's check, so none stops one.
    fn run_members(&self, core: &mut Core, routed: Vec<(IntentId, Routed)>) {
        for (mid, r) in routed {
            match r {
                Routed::Dispatched(Order::Vehicles(o)) => {
                    let resume = core.engine.intent(mid).map(super::queue::resume_modes).unwrap_or_default();
                    self.run_order(core, mid, o, &resume);
                }
                Routed::Dispatched(_) => eprintln!("decision: member order {mid} dispatched as something else"),
                Routed::Queued { .. } => {
                    let uids = core.engine.intent(mid).map(|i| i.spec().uids.clone()).unwrap_or_default();
                    self.supersede_queued(core, mid, &uids);
                }
                Routed::Voided { reason } => eprintln!("decision: member order {mid} voided: {reason}"),
            }
        }
    }

    /// Rules 34: the groups in force stepped on the world now (the engine's
    /// [`murmur_decide::Engine::step_groups`]): each re-form's member orders run.
    pub(super) fn step_groups(&self, core: &mut Core) {
        if core.engine.forces().is_empty() {
            return;
        }
        let world = self.snapshot();
        match core.engine.step_groups(&world, &self.terrain.oracle()) {
            Ok(step) => {
                for (gid, routed) in step.reformed {
                    eprintln!("decision: group order {gid} re-formed: {} member(s) moved or back", routed.len());
                    self.run_members(core, routed);
                }
                for (gid, why) in step.refused {
                    eprintln!("decision: group order {gid}: re-form refused: {why}");
                }
                for gid in step.suspended {
                    eprintln!("decision: group order {gid} suspended below its minimum");
                }
            }
            Err(e) => eprintln!("decision: step_groups: {e}"),
        }
    }

    /// Rules 34: a group order's member orders as its check will lay them out on `world` (each
    /// member's own task and share), for the terrain fetched before its decision; None for
    /// anything but a group order under these rules.
    pub(super) fn group_member_specs(&self, task: &Task, uids: &[DUid], world: &WorldSnapshot) -> Option<Vec<murmur_decide::IntentSpec>> {
        if !self.rules.group_orders() || uids.len() < 2 {
            return None;
        }
        murmur_decide::group::Spread::default_for(task.kind())?;
        let mut spec = Self::prefetch_spec(self.clock.now(), task, uids);
        spec.grouping.group = Some(murmur_decide::group::GroupSpec::default().normalized(task.kind(), uids.len(), murmur_decide::group::MIN_SHARE));
        Some(murmur_decide::group::member_specs(&spec, world, &self.rules))
    }

    /// Rules 34: beside the tick, each re-form due laid out outside the decision lock (the lock
    /// held only to read which are due), and the terrain along the shares it would move members to
    /// fetched, so the tick's step, which lays it out again and decides, reads it from the cache.
    pub(super) async fn prefetch_groups(&self) {
        if !self.terrain.fetches() || self.terrain.backing_off() {
            return;
        }
        let world = self.snapshot();
        let due = {
            let core = self.lock();
            if core.engine.forces().is_empty() {
                return;
            }
            core.engine.reforms_due(&world)
        };
        for (spec, force, on_share, cause) in due {
            let Some(gs) = spec.group().cloned() else { continue };
            let back: std::collections::BTreeSet<DUid> = cause.back.keys().cloned().collect();
            let members = murmur_decide::group::reform_members(&on_share, &back, &world);
            let pinned = murmur_decide::group::pinned(&spec, &on_share, &world, &self.queue_cfg, &self.rules);
            let rf = murmur_decide::group::reform(&spec.task, &gs, &force.layout, &members, &pinned);
            if rf.layout.refused.is_some() {
                continue;
            }
            for uid in &rf.moved {
                if let Some(task) = murmur_decide::group::member_task(&spec.task, &gs, &rf.layout, uid) {
                    self.prefetch_in(&world, &task, std::slice::from_ref(uid)).await;
                }
            }
        }
    }
}
