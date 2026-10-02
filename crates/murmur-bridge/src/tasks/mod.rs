//! Group tasking and the per-vehicle command lifecycle.
//!
//! A Tasking is what the operator issued ("orbit here" to vehicles 1, 3, 7). It fans out into
//! one op per vehicle (see `op` for the lifecycle each walks through).
//!
//! The boundary: a request becomes a [`TaskOrder`] (parsed and checked against the world),
//! and [`TaskManager::dispatch`] is the only way an order reaches the vehicles. Phase C puts
//! intents, rules, the risk-tier policy and the audit log in front of `dispatch`; nothing
//! below it needs to change.

mod caps;
mod exec;
mod op;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

pub use caps::{caps, platform_kind};
pub use op::{OpCell, OpSnapshot, OpState};

use crate::clock;
use crate::mav::Kind;
use crate::vehicle::{Uid, VehicleCell};

/// The last this many taskings are kept (and shown).
const KEEP_TASKINGS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    Orbit,
    Patrol,
    Hold,
    Rtl,
    Track,
}

impl TaskKind {
    pub fn name(self) -> &'static str {
        match self {
            TaskKind::Orbit => "orbit",
            TaskKind::Patrol => "patrol",
            TaskKind::Hold => "hold",
            TaskKind::Rtl => "rtl",
            TaskKind::Track => "track",
        }
    }
}

impl FromStr for TaskKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "orbit" => TaskKind::Orbit,
            "patrol" => TaskKind::Patrol,
            "hold" => TaskKind::Hold,
            "rtl" => TaskKind::Rtl,
            "track" => TaskKind::Track,
            _ => return Err(format!("unknown task {s}")),
        })
    }
}

/// [lon, lat]
pub type LngLat = [f64; 2];

/// Task parameters as the operator gave them (echoed back in the tasking).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TaskParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<LngLat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<Vec<LngLat>>,
    /// uid of the vehicle to track
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<Uid>,
}

/// POST /api/tasks
#[derive(Debug, Clone, Deserialize)]
pub struct TaskRequest {
    pub task: String,
    pub uids: Vec<Uid>,
    #[serde(flatten)]
    pub params: TaskParams,
}

#[derive(Debug, PartialEq)]
pub enum OrderError {
    UnknownVehicles(Vec<Uid>),
    Invalid(String),
}

impl std::fmt::Display for OrderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderError::UnknownVehicles(uids) => {
                let list: Vec<String> = uids.iter().map(ToString::to_string).collect();
                write!(f, "unknown vehicles [{}]", list.join(", "))
            }
            OrderError::Invalid(s) => f.write_str(s),
        }
    }
}

/// A well-formed order: known vehicles, a known task, the parameters it needs. Whether each
/// vehicle can do it is decided per op at dispatch (an incapable one fails, the rest go).
pub struct TaskOrder {
    pub kind: TaskKind,
    pub params: TaskParams,
    pub vehicles: Vec<Arc<VehicleCell>>,
    /// for "track"
    pub target: Option<Arc<VehicleCell>>,
    /// every vehicle, for tasks that touch others (a tracker's relay)
    pub fleet: Vec<Arc<VehicleCell>>,
}

impl TaskOrder {
    pub fn new(
        req: TaskRequest,
        fleet: &BTreeMap<Uid, Arc<VehicleCell>>,
    ) -> Result<TaskOrder, OrderError> {
        let missing: Vec<Uid> = req
            .uids
            .iter()
            .copied()
            .filter(|u| !fleet.contains_key(u))
            .collect();
        if !missing.is_empty() {
            return Err(OrderError::UnknownVehicles(missing));
        }
        if req.uids.is_empty() {
            return Err(OrderError::Invalid("select at least one vehicle".into()));
        }
        let kind: TaskKind = req.task.parse().map_err(OrderError::Invalid)?;
        let p = &req.params;
        let invalid = |s: &str| Err(OrderError::Invalid(s.into()));
        match kind {
            TaskKind::Orbit if p.center.is_none() => {
                return invalid("orbit needs center [lon, lat]");
            }
            TaskKind::Patrol if p.route.as_ref().is_none_or(|r| r.len() < 2) => {
                return invalid("patrol needs a route of at least 2 points");
            }
            _ => {}
        }
        let target = p.target.and_then(|t| fleet.get(&t).cloned());
        if kind == TaskKind::Track && target.is_none() {
            return invalid("track needs a target vehicle");
        }
        Ok(TaskOrder {
            kind,
            vehicles: req
                .uids
                .iter()
                .filter_map(|u| fleet.get(u).cloned())
                .collect(),
            target,
            params: req.params,
            fleet: fleet.values().cloned().collect(),
        })
    }
}

pub struct Tasking {
    pub id: u64,
    pub kind: TaskKind,
    pub params: TaskParams,
    pub created: f64,
    pub ops: Vec<Arc<OpCell>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskingSnapshot {
    pub id: u64,
    pub task: TaskKind,
    pub params: TaskParams,
    pub created: f64,
    pub ops: Vec<OpSnapshot>,
}

impl Tasking {
    pub fn snapshot(&self) -> TaskingSnapshot {
        TaskingSnapshot {
            id: self.id,
            task: self.kind,
            params: self.params.clone(),
            created: self.created,
            ops: self.ops.iter().map(|o| o.snapshot()).collect(),
        }
    }
}

#[derive(Default)]
struct Inner {
    last_tasking: u64,
    last_op: u64,
    taskings: VecDeque<Arc<Tasking>>,
    /// uid -> the op that last took the vehicle
    active: HashMap<Uid, Arc<OpCell>>,
}

#[derive(Default)]
pub struct TaskManager {
    inner: Mutex<Inner>,
}

fn kind_name(k: Option<Kind>) -> &'static str {
    k.map_or("None", Kind::name)
}

impl TaskManager {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Send an order to its vehicles: one op each, run concurrently. A vehicle that can't do
    /// the task gets a failed op; one that was busy has its previous op superseded.
    pub fn dispatch(&self, order: TaskOrder) -> Arc<Tasking> {
        let mut inner = self.lock();
        inner.last_tasking += 1;
        let tasking_id = inner.last_tasking;
        let kinds: Vec<Option<Kind>> = order.vehicles.iter().map(|v| v.read(|s| s.kind)).collect();
        let mut counts: HashMap<Option<Kind>, usize> = HashMap::new();
        let mut ops = Vec::new();
        for (v, kind) in order.vehicles.iter().zip(&kinds) {
            let slot = counts.entry(*kind).or_default();
            inner.last_op += 1;
            ops.push(Arc::new(OpCell::new(
                inner.last_op,
                tasking_id,
                order.kind,
                v.uid,
                v.sysid,
                *slot,
            )));
            *slot += 1;
        }
        for ((op, v), kind) in ops.iter().zip(&order.vehicles).zip(&kinds) {
            if !caps::can(*kind, order.kind) {
                op.set(
                    OpState::Failed,
                    format!("a {} can't {}", kind_name(*kind), order.kind.name()),
                );
                continue;
            }
            if let Some(prev) = inner.active.get(&v.uid)
                && !prev.finished()
            {
                prev.supersede(format!("replaced by {} #{tasking_id}", order.kind.name()));
            }
            inner.active.insert(v.uid, op.clone());
            let ctx = exec::OpCtx {
                op: op.clone(),
                v: v.clone(),
                kind: order.kind,
                params: order.params.clone(),
                same_kind: counts.get(kind).copied().unwrap_or(1),
                target: order.target.clone(),
                fleet: order.fleet.clone(),
            };
            exec::spawn(ctx);
        }
        let tasking = Arc::new(Tasking {
            id: tasking_id,
            kind: order.kind,
            params: order.params,
            created: clock::wall(),
            ops,
        });
        inner.taskings.push_back(tasking.clone());
        while inner.taskings.len() > KEEP_TASKINGS {
            inner.taskings.pop_front();
        }
        tasking
    }

    /// Newest first.
    pub fn snapshot(&self) -> Vec<TaskingSnapshot> {
        let taskings: Vec<Arc<Tasking>> = self.lock().taskings.iter().rev().cloned().collect();
        taskings.iter().map(|t| t.snapshot()).collect()
    }
}
