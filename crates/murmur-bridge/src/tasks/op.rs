//! One vehicle's part of a tasking, and the lifecycle it walks through:
//!
//! ```text
//! queued -> sent -> acked -> active -> done
//!    \________\_______\________\______> failed | superseded
//! ```
//!
//! queued: waiting for a navigation fix. sent: the first MAVLink message is on the wire.
//! acked: the vehicle accepted everything (mission, arm, mode). active: telemetry shows it
//! executing. done: telemetry shows the goal reached. An ACK is not done: "active" and
//! "done" are judged from telemetry, never from acknowledgements.

use std::sync::{Mutex, MutexGuard};

use serde::Serialize;
use tokio::task::AbortHandle;

use super::TaskKind;
use crate::clock;
use crate::vehicle::Uid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpState {
    Queued,
    Sent,
    Acked,
    Active,
    Done,
    Failed,
    Superseded,
}

impl OpState {
    pub fn is_terminal(self) -> bool {
        matches!(self, OpState::Done | OpState::Failed | OpState::Superseded)
    }

    fn rank(self) -> u8 {
        match self {
            OpState::Queued => 0,
            OpState::Sent => 1,
            OpState::Acked => 2,
            OpState::Active => 3,
            OpState::Done | OpState::Failed | OpState::Superseded => 4,
        }
    }

    /// Forward along the lifecycle (some steps may be skipped: a hold goes acked -> done),
    /// or out to failed/superseded from anywhere. Nothing leaves a terminal state, so a
    /// superseded op's executor can't write over its successor's story.
    pub fn can_become(self, next: OpState) -> bool {
        !self.is_terminal() && next.rank() >= self.rank()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub state: OpState,
    pub t: f64,
}

#[derive(Debug, Clone)]
pub struct OpRecord {
    pub state: OpState,
    pub detail: String,
    pub progress: Option<String>,
    pub history: Vec<HistoryEntry>,
}

impl OpRecord {
    fn new() -> OpRecord {
        OpRecord {
            state: OpState::Queued,
            detail: "waiting for navigation (GPS + EKF)".into(),
            progress: None,
            history: vec![HistoryEntry {
                state: OpState::Queued,
                t: clock::wall(),
            }],
        }
    }

    /// Move to `state` (or stay, updating the detail). Returns false if the transition isn't
    /// allowed; the record is then unchanged.
    pub fn set(&mut self, state: OpState, detail: Option<String>) -> bool {
        if state != self.state {
            if !self.state.can_become(state) {
                return false;
            }
            self.state = state;
            self.history.push(HistoryEntry {
                state,
                t: clock::wall(),
            });
        } else if matches!(state, OpState::Failed | OpState::Superseded) {
            return false;
        }
        if let Some(d) = detail {
            self.detail = d;
        }
        true
    }
}

pub struct OpCell {
    pub id: u64,
    pub tasking: u64,
    /// the decision engine's intent this op carries out (0 only in task-layer tests)
    pub intent_id: u64,
    pub task: TaskKind,
    pub uid: Uid,
    pub sysid: u8,
    /// index among same-kind vehicles in this tasking, for altitude/phase spreading
    pub slot: usize,
    record: Mutex<OpRecord>,
    pub(super) abort: Mutex<Option<AbortHandle>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpSnapshot {
    pub id: u64,
    pub tasking: u64,
    /// the decision engine's intent this op carries out (0 only in task-layer tests)
    pub intent_id: u64,
    pub task: TaskKind,
    pub uid: Uid,
    pub sysid: u8,
    pub state: OpState,
    pub detail: String,
    pub progress: Option<String>,
    pub history: Vec<HistoryEntry>,
}

impl OpCell {
    pub fn new(
        id: u64,
        tasking: u64,
        intent_id: u64,
        task: TaskKind,
        uid: Uid,
        sysid: u8,
        slot: usize,
    ) -> OpCell {
        OpCell {
            id,
            tasking,
            intent_id,
            task,
            uid,
            sysid,
            slot,
            record: Mutex::new(OpRecord::new()),
            abort: Mutex::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, OpRecord> {
        self.record.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> OpState {
        self.lock().state
    }

    pub fn set(&self, state: OpState, detail: impl Into<String>) -> bool {
        self.lock().set(state, Some(detail.into()))
    }

    /// Same state, new detail (progress narration while waiting).
    pub fn narrate(&self, detail: impl Into<String>) {
        let mut r = self.lock();
        let state = r.state;
        r.set(state, Some(detail.into()));
    }

    /// Progress outlives "done" (an orbit keeps orbiting), but not failure or replacement.
    pub fn set_progress(&self, progress: Option<String>) {
        let mut r = self.lock();
        if !matches!(r.state, OpState::Failed | OpState::Superseded) {
            r.progress = progress;
        }
    }

    pub fn finished(&self) -> bool {
        self.state().is_terminal()
    }

    /// Stop the executor and mark the op replaced.
    pub fn supersede(&self, detail: String) {
        if let Some(h) = self.abort.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.abort();
        }
        self.set(OpState::Superseded, detail);
    }

    pub fn snapshot(&self) -> OpSnapshot {
        let r = self.lock();
        OpSnapshot {
            id: self.id,
            tasking: self.tasking,
            intent_id: self.intent_id,
            task: self.task,
            uid: self.uid,
            sysid: self.sysid,
            state: r.state,
            detail: r.detail.clone(),
            progress: r.progress.clone(),
            history: r.history.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_moves_forward_only() {
        let mut r = OpRecord::new();
        assert!(r.set(OpState::Sent, Some("uploading".into())));
        assert!(r.set(OpState::Sent, Some("starting".into()))); // same state: detail only
        assert_eq!(r.history.len(), 2);
        assert!(r.set(OpState::Acked, None));
        assert!(!r.set(OpState::Sent, Some("late".into())));
        assert_eq!((r.state, r.detail.as_str()), (OpState::Acked, "starting"));
        assert!(r.set(OpState::Done, Some("holding".into()))); // a hold skips active
        let states: Vec<_> = r.history.iter().map(|h| h.state).collect();
        assert_eq!(
            states,
            [
                OpState::Queued,
                OpState::Sent,
                OpState::Acked,
                OpState::Done
            ]
        );
    }

    #[test]
    fn terminal_states_are_final() {
        let mut r = OpRecord::new();
        assert!(r.set(OpState::Superseded, Some("replaced by hold #2".into())));
        assert!(!r.set(OpState::Active, Some("executing".into())));
        assert!(!r.set(OpState::Failed, Some("no ACK".into())));
        assert!(!r.set(OpState::Superseded, Some("again".into())));
        assert_eq!(r.detail, "replaced by hold #2");
        // done may still narrate (it stays done), failure can't follow done
        let mut d = OpRecord::new();
        d.set(OpState::Done, Some("on station".into()));
        assert!(!d.set(OpState::Failed, Some("x".into())));
        assert!(d.set(OpState::Done, Some("on station".into())));
    }
}
