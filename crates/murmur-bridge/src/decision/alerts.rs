//! Inbox alerts, from what the bridge already detects: a lost link (no telemetry for more
//! than 3 s), NOT MOVING, a vehicle that left its task, a failed task, an ACK timeout.
//!
//! Derived from the current state on every read, so an alert clears itself when its cause
//! does (the link comes back, the vehicle gets a new task). Op alerts look only at each
//! vehicle's latest op: an old failure stops mattering once the vehicle has been re-tasked.
//! Acknowledge and snooze are console work (C.5).

use serde::Serialize;

use crate::tasks::{OpSnapshot, OpState};
use crate::vehicle::{Uid, VehicleCell};

pub const LOST_LINK_S: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Alert {
    /// Stable while the condition lasts: `link:<uid>` or `op:<op id>`.
    pub id: String,
    /// lost_link | not_moving | left_task | task_failed | ack_timeout
    pub kind: &'static str,
    pub severity: Severity,
    pub uid: Uid,
    pub callsign: Option<String>,
    pub detail: String,
    /// When the condition started (wall clock, seconds).
    pub since_t: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<u64>,
}

fn callsign(cell: &VehicleCell) -> Option<String> {
    cell.read(|v| {
        v.identity
            .as_ref()
            .and_then(|i| i.str("callsign"))
            .map(str::to_string)
    })
}

/// What an op's state says, if it needs the operator.
fn op_alert(op: &OpSnapshot) -> Option<(&'static str, Severity)> {
    match op.state {
        OpState::Failed if op.detail.starts_with("no ACK") => Some(("ack_timeout", Severity::High)),
        OpState::Failed if op.detail.starts_with("vehicle left the task") => {
            Some(("left_task", Severity::High))
        }
        OpState::Failed => Some(("task_failed", Severity::Medium)),
        s if !s.is_terminal() && op.detail.starts_with("NOT MOVING") => {
            Some(("not_moving", Severity::High))
        }
        _ => None,
    }
}

/// All current alerts, most severe first, then oldest first.
pub fn alerts(
    vehicles: &[std::sync::Arc<VehicleCell>],
    ops: &[OpSnapshot],
    now_wall: f64,
) -> Vec<Alert> {
    let mut out = Vec::new();
    for cell in vehicles {
        let link = cell.link_snapshot(crate::clock::mono());
        if let Some(last) = link.last_rx_t
            && now_wall - last > LOST_LINK_S
        {
            out.push(Alert {
                id: format!("link:{}", cell.uid),
                kind: "lost_link",
                severity: Severity::High,
                uid: cell.uid,
                callsign: callsign(cell),
                detail: format!("no telemetry since t={last:.1}"),
                since_t: last,
                op_id: None,
                intent_id: None,
            });
        }
    }
    for op in ops {
        let Some((kind, severity)) = op_alert(op) else {
            continue;
        };
        let since_t = op.history.last().map_or(0.0, |h| h.t);
        out.push(Alert {
            id: format!("op:{}", op.id),
            kind,
            severity,
            uid: op.uid,
            callsign: vehicles
                .iter()
                .find(|c| c.uid == op.uid)
                .and_then(|c| callsign(c)),
            detail: op.detail.clone(),
            since_t,
            op_id: Some(op.id),
            intent_id: (op.intent_id != 0).then_some(op.intent_id),
        });
    }
    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.since_t.total_cmp(&b.since_t))
            .then(a.id.cmp(&b.id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{HistoryEntry, TaskKind};

    fn op(id: u64, state: OpState, detail: &str) -> OpSnapshot {
        OpSnapshot {
            id,
            tasking: 1,
            intent_id: 7,
            task: TaskKind::Orbit,
            uid: 3,
            sysid: 3,
            state,
            detail: detail.into(),
            progress: None,
            history: vec![HistoryEntry { state, t: 100.0 }],
        }
    }

    #[test]
    fn op_states_become_alerts() {
        let ops = [
            op(1, OpState::Failed, "no ACK for DO_SET_MODE after 5 tries"),
            op(
                2,
                OpState::Failed,
                "vehicle left the task, now in RTL: Failsafe",
            ),
            op(3, OpState::Failed, "mission rejected"),
            op(
                4,
                OpState::Active,
                "NOT MOVING for 12s (aground or stuck?) · waypoint 1/2",
            ),
            op(5, OpState::Active, "executing"),
            op(6, OpState::Done, "on station"),
        ];
        let a = alerts(&[], &ops, 200.0);
        let kinds: Vec<_> = a.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            ["ack_timeout", "left_task", "not_moving", "task_failed"]
        );
        assert_eq!(a[0].intent_id, Some(7));
    }
}
