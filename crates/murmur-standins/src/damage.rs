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
//! SITL vehicles act out the same file through `sim/sitl_damage.py` (docs/AUTONOMOUS-NODES.md
//! section 10); the headless simulator acts it out in process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

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

/// Where a reader of the file is (bd murmur-4gnm). The file outlives its run: nothing empties it
/// in a stack without a contacts process, and a contacts process empties it only as it starts,
/// after the stand-ins (scripts/up.sh starts it last, the evaluation's stack at its origin), by
/// when they have read it. So a file not written since this engine started holds a previous
/// run's records on this HTTP port, for vehicles that may have this run's uids: they are skipped,
/// as sim/sitl_damage.py skips them. A file that no longer begins with what was read (emptied by
/// the contacts process, then written again; or replaced) is read again from its start.
#[derive(Debug)]
pub struct Follow {
    started: SystemTime,
    /// What has been read of the file (none: not seen yet).
    read: Option<String>,
}

impl Follow {
    /// A reader for an engine that started at `started`.
    pub fn new(started: SystemTime) -> Follow {
        Follow { started, read: None }
    }

    /// Read the file's `text` into `board`; `modified`: when it was last written, as it stood
    /// before `text` was read. Returns how many bytes of a previous run's it skipped (only on its
    /// first look at the file).
    pub fn step(&mut self, board: &Board, text: &str, modified: Option<SystemTime>) -> usize {
        let at = match &self.read {
            None if modified.is_some_and(|m| m < self.started) => {
                self.read = Some(text.to_string());
                return text.len();
            }
            Some(read) if text.starts_with(read.as_str()) => read_into(board, text, read.len()),
            _ => read_into(board, text, 0),
        };
        self.read = Some(text[..at].to_string());
        0
    }
}

/// Follow `path` every second into `board` ([`Follow`]: a previous run's records are skipped).
/// `started`: when this engine started.
pub async fn watch(path: PathBuf, board: std::sync::Arc<Board>, started: SystemTime) {
    let mut follow = Follow::new(started);
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        every.tick().await;
        // when it was written, before reading it: a file emptied in between is skipped empty
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let skipped = follow.step(&board, &text, modified);
        if skipped > 0 {
            println!(
                "damage: {} predates this engine: its {skipped} bytes are a previous run's, skipped",
                path.display()
            );
        }
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

    /// bd murmur-4gnm: a file last written before this engine started is a previous run's on the
    /// same HTTP port (a plain `make goldengate` after one with contacts, or the stand-ins reading
    /// before the contacts process empties it): its records are skipped and hurt no vehicle of
    /// this run. Emptied and written again, it is read from its start, even when it has grown past
    /// the old length between two looks. A file written since the start is read whole.
    #[test]
    fn a_previous_runs_file_is_skipped_and_this_runs_is_read() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let before = Some(started - Duration::from_secs(600));
        let after = Some(started + Duration::from_secs(5));
        let old = "{\"uid\":3,\"state\":\"destroyed\"}\n{\"uid\":4,\"state\":\"mobility\"}\n";

        // the old file seen first, then emptied by the contacts process, then this run's record
        let (b, mut f) = (Board::default(), Follow::new(started));
        assert_eq!(f.step(&b, old, before), old.len());
        assert_eq!(f.step(&b, old, before), 0, "said once");
        assert!(b.of(3).is_empty() && b.of(4).is_empty() && b.generation() == 0);
        f.step(&b, "", after);
        let new = "{\"uid\":4,\"state\":\"firepower\"}\n";
        f.step(&b, new, after);
        assert_eq!((b.of(3), b.of(4)), (vec![], vec![State::Firepower]));
        // then appended to
        let more = format!("{new}{{\"uid\":5,\"state\":\"mobility\"}}\n");
        f.step(&b, &more, after);
        assert_eq!((b.of(4), b.of(5)), (vec![State::Firepower], vec![State::Mobility]));

        // emptied and rewritten past the old length between two looks
        let (b, mut f) = (Board::default(), Follow::new(started));
        f.step(&b, old, before);
        let longer = "{\"uid\":7,\"state\":\"mobility\",\"cause\":\"struck by ATGM-class missile (raider-1)\"}\n";
        assert!(longer.len() > old.len());
        f.step(&b, longer, after);
        assert_eq!((b.of(3), b.of(4), b.of(7)), (vec![], vec![], vec![State::Mobility]));

        // a file this run wrote before the first look (or one whose time can't be read) is read
        for when in [after, None] {
            let (b, mut f) = (Board::default(), Follow::new(started));
            assert_eq!(f.step(&b, old, when), 0);
            assert_eq!((b.of(3), b.of(4)), (vec![State::Destroyed], vec![State::Mobility]));
        }
    }
}
