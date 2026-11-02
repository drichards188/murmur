//! Damage to our own vehicles (bd murmur-lbw8.2): the truth side's damage model decides that a
//! strike hurt one of ours (crates/murmur-contacts `damage`), and the vehicle simulation acts it
//! out. Never on the MAVLink wire, as the truth log is not: the contacts process appends one
//! JSON line per hurt vehicle to a file ([`path`]), and the stand-in engine reads it
//! ([`watch`] into a [`Board`]) and puts each state on its vehicle:
//!
//! - **mobility** (its propulsion lost): the vehicle steps unpowered, as with a flat battery: a
//!   boat drifts with the water, a ground vehicle rolls to a stop, an aircraft comes down;
//! - **firepower** (its weapons out of action): nothing moves differently;
//! - **destroyed**: the vehicle goes silent, sending nothing and answering nothing.
//!
//! A damaged vehicle that can still talk says so itself, as an autopilot reports a fault: a
//! STATUSTEXT `DAMAGE mobility: ...` or `DAMAGE firepower: ...`, at once and every
//! [`REPORT_EVERY_S`] while it lasts (so a bridge that restarts learns it again). That report,
//! over the vehicle's own link, is all the C2 learns. Damage never heals in a run.
//!
//! ```json
//! {"uid": 3, "state": "mobility", "t": 1781463600.0, "cause": "struck by ATGM-class missile (act-2)"}
//! ```
//!
//! SITL vehicles read none of it (docs/OPPONENT-WEAPONS.md, "Damage"): only stand-ins and the
//! headless simulator act damage out.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How often a damaged vehicle repeats its report, simulated seconds.
pub const REPORT_EVERY_S: f64 = 30.0;

/// What a strike did to one of ours (the damage model's states that change anything).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Mobility,
    Firepower,
    Destroyed,
}

impl State {
    /// The vehicle's own report of it (none: a destroyed vehicle says nothing).
    pub fn report(self) -> Option<&'static str> {
        match self {
            State::Mobility => Some("DAMAGE mobility: propulsion lost"),
            State::Firepower => Some("DAMAGE firepower: weapons out of action"),
            State::Destroyed => None,
        }
    }

    /// From the damage model's name; none for `undamaged` and anything else.
    pub fn parse(s: &str) -> Option<State> {
        match s {
            "mobility" => Some(State::Mobility),
            "firepower" => Some(State::Firepower),
            "destroyed" => Some(State::Destroyed),
            _ => None,
        }
    }
}

/// One line of the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    pub uid: u64,
    pub state: State,
    /// Scenario time it happened (unix s).
    #[serde(default)]
    pub t: f64,
    #[serde(default)]
    pub cause: String,
}

/// The file the contacts process writes and the stand-in engine reads: `MURMUR_DAMAGE_FILE`, else
/// `$MURMUR_ROOT/sim/run/damage-<MURMUR_HTTP_PORT, 8765>.jsonl` (one per stack: each stack has its
/// own HTTP port).
pub fn path() -> PathBuf {
    if let Some(p) = std::env::var("MURMUR_DAMAGE_FILE").ok().filter(|p| !p.trim().is_empty()) {
        return PathBuf::from(p);
    }
    let root = std::env::var("MURMUR_ROOT").ok().filter(|p| !p.trim().is_empty()).map(PathBuf::from).unwrap_or_default();
    let port = std::env::var("MURMUR_HTTP_PORT").ok().filter(|p| !p.trim().is_empty()).unwrap_or_else(|| "8765".into());
    root.join("sim/run").join(format!("damage-{port}.jsonl"))
}

/// The damage every vehicle has taken, by uid, as read so far; `generation` changes whenever
/// anything is added, so a vehicle looks only then.
#[derive(Debug, Default)]
pub struct Board {
    generation: AtomicU64,
    states: Mutex<HashMap<u64, Vec<State>>>,
}

impl Board {
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Record `state` on `uid` (once).
    pub fn put(&self, uid: u64, state: State) {
        let mut m = self.states.lock().unwrap_or_else(|e| e.into_inner());
        let v = m.entry(uid).or_default();
        if !v.contains(&state) {
            v.push(state);
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// What `uid` has taken.
    pub fn of(&self, uid: u64) -> Vec<State> {
        self.states.lock().unwrap_or_else(|e| e.into_inner()).get(&uid).cloned().unwrap_or_default()
    }
}

/// Read the lines of `text` from byte `from` on into `board`; returns where the next read starts
/// (a partial last line is left for it).
pub fn read_into(board: &Board, text: &str, from: usize) -> usize {
    let mut at = from;
    let rest = text.get(from..).unwrap_or("");
    for line in rest.split_inclusive('\n') {
        if !line.ends_with('\n') {
            break;
        }
        at += line.len();
        if let Ok(o) = serde_json::from_str::<Order>(line.trim()) {
            board.put(o.uid, o.state);
        }
    }
    at
}

/// Follow `path` every second into `board`, from its start (a file shorter than what was read
/// was started again: read from its start).
pub async fn watch(path: PathBuf, board: std::sync::Arc<Board>) {
    let mut at = 0usize;
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        every.tick().await;
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.len() < at {
            at = 0;
        }
        at = read_into(&board, &text, at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_are_read_once_and_partial_lines_wait() {
        let b = Board::default();
        let text = "{\"uid\":3,\"state\":\"mobility\",\"t\":1.0}\n{\"uid\":4,\"state\":\"destroyed\"}\n{\"uid\":3,";
        let at = read_into(&b, text, 0);
        assert_eq!(b.of(3), vec![State::Mobility]);
        assert_eq!(b.of(4), vec![State::Destroyed]);
        assert_eq!(b.generation(), 2);
        let text = format!("{}\"state\":\"firepower\"}}\n", text);
        read_into(&b, &text, at);
        assert_eq!(b.of(3), vec![State::Mobility, State::Firepower]);
        // the same again changes nothing
        b.put(3, State::Mobility);
        assert_eq!(b.generation(), 3);
        assert_eq!(State::Destroyed.report(), None);
    }
}
