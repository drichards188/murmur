//! Run-time anchors: the audit log's first line and its head, taken as the log grows and
//! written to a separate append-only file (and, optionally, handed to a command that can
//! post them elsewhere), so a rewritten log or a truncated tail shows against them
//! (`murmur-replay --anchors`).
//!
//! The hash chain alone is tamper-evident, not tamper-proof (see [`crate::audit`]): whoever
//! can write the log can recompute every hash after an edit, or drop the last events. An
//! anchor taken before the edit catches both, **but it is only as strong as the separation
//! between where the anchors are kept and whoever can write the log.** A local anchor file
//! in the same account catches an accident, a crash that loses the tail, or an edit of the
//! log alone; whoever can write both files can rewrite both. Real protection needs a
//! separate trust domain: `MURMUR_ANCHOR_CMD` handing each anchor to a machine, a service or
//! a timestamping authority that the log's writer can't change.
//!
//! An anchor is taken after the first event (`run.started`), every `every` events, after every
//! event a restart carries intents over by (`intent.state`, `intent.resumed`, `op.state`:
//! "decision"), and at a clean shutdown. Events after the last anchor are not covered: their
//! truncation can't be told from a run that ended there (the shutdown anchor tells a clean
//! end); after a crash, that is at most the vehicle events since the last decision.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};

use serde::{Deserialize, Serialize};

use crate::audit::AuditSink;
use crate::hash::sha256_hex;
use crate::replay::Mismatch;
use crate::time::{Clock, SystemClock};

/// One anchor: a line of the anchor file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// When it was taken: ms since the Unix epoch, on the writer's clock (the bridge's: the
    /// audit log's), never earlier than the anchor before it.
    pub t: i64,
    /// The audit log it anchors, as its writer named it.
    pub log: String,
    /// SHA-256 of the log's first line (its `run.started`).
    pub first: String,
    /// The last event written when the anchor was taken, and the SHA-256 of its line.
    pub seq: u64,
    pub head: String,
    /// "start", "every N events", "decision" or "shutdown".
    pub why: String,
    /// The cadence the writer was set to: an anchor every this many events. The verifier
    /// holds the file to it, so a removed anchor shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<u64>,
    /// The wall clock (ms since the Unix epoch) when it was taken, beside `t` on the writer's
    /// clock, when the writer keeps a clock of its own (the bridge's, murmur-dup): any time
    /// in the log maps to UTC through the nearest pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub utc: Option<i64>,
}

/// Where anchors go and how often.
#[derive(Clone, Debug)]
pub struct AnchorConfig {
    /// The anchor file (appended to; e.g. `logs/anchors/<run-id>.anchors`, outside
    /// `logs/audit`).
    pub path: PathBuf,
    /// An anchor every this many events (at least 1).
    pub every: u64,
    /// A shell command run (`sh -c`) for each anchor, the anchor's JSON on its stdin and in
    /// `MURMUR_ANCHOR`, e.g. to post it elsewhere. Run in order on a thread of its own: a
    /// slow or failing command never holds up the log.
    pub cmd: Option<String>,
}

enum HookJob {
    Run(String),
    Flush(mpsc::Sender<()>),
}

struct State {
    clock: Box<dyn Clock + Send>,
    /// The wall clock each anchor records beside `clock`'s time, when it is another clock.
    utc: Option<Box<dyn Clock + Send>>,
    /// The last anchor's time: the next is never earlier (the verifier holds the file to it).
    last_t: i64,
    file: File,
    path: PathBuf,
    log: String,
    every: u64,
    hook: Option<mpsc::Sender<HookJob>>,
    first: Option<String>,
    seq: u64,
    head: Option<String>,
    since: u64,
    /// The shutdown anchor is written: nothing more may be appended.
    closed: bool,
}

/// The anchoring of one audit log; clones share it (the sink takes one, the bridge keeps
/// one for the shutdown anchor).
#[derive(Clone)]
pub struct Anchors(Arc<Mutex<State>>);

impl Anchors {
    /// Open (append to) the anchor file for the log at `log`, its anchors timed by the wall
    /// clock.
    pub fn open(cfg: &AnchorConfig, log: &Path) -> std::io::Result<Anchors> {
        Anchors::open_with(cfg, log, Box::new(SystemClock))
    }

    /// [`Anchors::open`], its anchors timed by `clock` (the bridge's: the one its log is on).
    pub fn open_with(cfg: &AnchorConfig, log: &Path, clock: Box<dyn Clock + Send>) -> std::io::Result<Anchors> {
        if let Some(dir) = cfg.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&cfg.path)?;
        let hook = cfg.cmd.clone().filter(|c| !c.trim().is_empty()).map(spawn_hook);
        Ok(Anchors(Arc::new(Mutex::new(State {
            clock,
            utc: None,
            last_t: i64::MIN,
            file,
            path: cfg.path.clone(),
            log: log.display().to_string(),
            every: cfg.every.max(1),
            hook,
            first: None,
            seq: 0,
            head: None,
            since: 0,
            closed: false,
        }))))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record `utc` (the wall clock) beside each anchor's time, so a log written on a clock
    /// of its own maps to UTC.
    pub fn with_utc(self, utc: Box<dyn Clock + Send>) -> Anchors {
        self.lock().utc = Some(utc);
        self
    }

    /// The anchor file.
    pub fn path(&self) -> PathBuf {
        self.lock().path.clone()
    }

    /// Wrap the log's sink: every line it appends is counted and anchored.
    pub fn sink<S: AuditSink>(&self, inner: S) -> AnchoredSink<S> {
        AnchoredSink {
            inner,
            anchors: self.clone(),
        }
    }

    /// Anchor the head now (a clean shutdown). None before the first event.
    pub fn anchor_now(&self, why: &str) -> std::io::Result<Option<Anchor>> {
        let mut st = self.lock();
        if st.head.is_none() {
            return Ok(None);
        }
        st.write(why).map(Some)
    }

    /// The shutdown anchor, and the log closed behind it: every later append fails (the
    /// engine then doesn't act on what it couldn't log), so nothing follows the anchor.
    pub fn close(&self) -> std::io::Result<Option<Anchor>> {
        let mut st = self.lock();
        st.closed = true;
        if st.head.is_none() {
            return Ok(None);
        }
        st.write("shutdown").map(Some)
    }

    fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Wait until the command has run for every anchor taken so far.
    pub fn flush_hook(&self) {
        let hook = self.lock().hook.clone();
        if let Some(h) = hook {
            let (tx, rx) = mpsc::channel();
            if h.send(HookJob::Flush(tx)).is_ok() {
                let _ = rx.recv();
            }
        }
    }

    fn appended(&self, line: &str) {
        let mut st = self.lock();
        st.seq = line_seq(line).unwrap_or(st.seq + 1);
        let head = sha256_hex(line.as_bytes());
        st.head = Some(head.clone());
        let why = if st.first.is_none() {
            st.first = Some(head);
            st.since = 0;
            Some("start".to_string())
        } else {
            st.since += 1;
            if st.since >= st.every {
                Some(format!("every {} events", st.every))
            } else if decision_line(line) {
                Some("decision".to_string())
            } else {
                None
            }
        };
        if let Some(why) = why
            && let Err(e) = st.write(&why)
        {
            // the event is logged: anchoring failing must not fail it (the engine would
            // then believe it unwritten and write its seq again); the next anchor retries
            eprintln!("anchor {}: {e}", st.path.display());
        }
    }
}

impl State {
    fn write(&mut self, why: &str) -> std::io::Result<Anchor> {
        // never earlier than the anchor before (a wall clock steps back: this host's about
        // 0.65 s every 32 s), or a genuine file fails its order check
        let t = self.clock.now().0.max(self.last_t);
        let a = Anchor {
            t,
            log: self.log.clone(),
            first: self.first.clone().unwrap_or_default(),
            seq: self.seq,
            head: self.head.clone().unwrap_or_default(),
            why: why.to_string(),
            every: Some(self.every),
            utc: self.utc.as_ref().map(|c| c.now().0),
        };
        let json = serde_json::to_string(&a).map_err(std::io::Error::other)?;
        let mut buf = Vec::with_capacity(json.len() + 1);
        buf.extend_from_slice(json.as_bytes());
        buf.push(b'\n');
        self.file.write_all(&buf)?;
        self.file.flush()?;
        self.file.sync_data()?;
        self.since = 0;
        self.last_t = t;
        if let Some(h) = &self.hook {
            let _ = h.send(HookJob::Run(json));
        }
        Ok(a)
    }
}

fn spawn_hook(cmd: String) -> mpsc::Sender<HookJob> {
    let (tx, rx) = mpsc::channel::<HookJob>();
    std::thread::spawn(move || {
        for job in rx {
            match job {
                HookJob::Run(json) => run_hook(&cmd, &json),
                HookJob::Flush(done) => {
                    let _ = done.send(());
                }
            }
        }
    });
    tx
}

fn run_hook(cmd: &str, json: &str) {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("MURMUR_ANCHOR", json)
        .stdin(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return eprintln!("MURMUR_ANCHOR_CMD: {e}"),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(json.as_bytes());
        let _ = stdin.write_all(b"\n");
    }
    match child.wait() {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("MURMUR_ANCHOR_CMD: {s}"),
        Err(e) => eprintln!("MURMUR_ANCHOR_CMD: {e}"),
    }
}

/// An audit sink whose lines are anchored ([`Anchors::sink`]).
pub struct AnchoredSink<S> {
    inner: S,
    anchors: Anchors,
}

impl<S: AuditSink> AuditSink for AnchoredSink<S> {
    fn append(&mut self, line: &str) -> std::io::Result<()> {
        if self.anchors.is_closed() {
            return Err(std::io::Error::other("audit log closed at shutdown"));
        }
        self.inner.append(line)?;
        self.anchors.appended(line);
        Ok(())
    }
}

/// An event a restart carries intents over by: an intent's state change, a carried-over
/// intent, an op's state (whether a vehicle started an order).
fn decision_line(line: &str) -> bool {
    ["\"type\":\"intent.state\"", "\"type\":\"intent.resumed\"", "\"type\":\"op.state\""]
        .iter()
        .any(|t| line.contains(t))
}

/// An event line's `seq` (lines start `{"seq":N,`).
fn line_seq(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("{\"seq\":")?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

/// What an anchor file says about a log ([`verify`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AnchorReport {
    pub anchors: usize,
    /// The highest seq an anchor covers.
    pub last_anchored: Option<u64>,
    /// Events after it: not covered (their truncation can't be told from a run that ended).
    pub after_last: u64,
    /// The anchors end with a clean shutdown.
    pub shutdown: bool,
    pub problems: Vec<Mismatch>,
}

impl AnchorReport {
    pub fn ok(&self) -> bool {
        self.problems.is_empty() && self.anchors > 0
    }
}

/// The cadence an anchor file declares: its anchors' `every`, or "every N events".
fn cadence(parsed: &[Anchor]) -> Option<u64> {
    parsed.iter().find_map(|a| {
        a.every.or_else(|| {
            a.why
                .strip_prefix("every ")
                .and_then(|r| r.strip_suffix(" events"))
                .and_then(|n| n.parse().ok())
        })
    })
}

/// Check a log against its anchor file: its first line and every anchored event's line must
/// hash as anchored, and the log must reach the last anchor (no truncation). The file must
/// be whole: a start anchor on the first event, one every N events as it declares, and a
/// shutdown anchor last, with nothing after it in the log. A run that crashed has no
/// shutdown anchor and fails too: its tail can't be told from a cut one.
pub fn verify(log: &str, anchors: &str) -> AnchorReport {
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let seqs: Vec<Option<u64>> = lines.iter().map(|l| line_seq(l)).collect();
    let last_seq = seqs.iter().flatten().max().copied().unwrap_or(0);
    let mut r = AnchorReport::default();
    let mut problem = |seq: u64, what: String, logged: String, anchored: String| {
        r.problems.push(Mismatch {
            seq,
            intent: None,
            what,
            logged,
            recomputed: anchored,
        });
    };
    let first = lines.first().map(|l| sha256_hex(l.as_bytes()));
    let mut parsed = Vec::new();
    for (n, l) in anchors.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        match serde_json::from_str::<Anchor>(l) {
            Ok(a) => parsed.push(a),
            Err(e) => problem(0, format!("anchor line {} unreadable: {e}", n + 1), String::new(), String::new()),
        }
    }
    let mut first_reported = false;
    let mut prev: Option<&Anchor> = None;
    for a in &parsed {
        if !first_reported && first.as_deref() != Some(a.first.as_str()) {
            first_reported = true;
            problem(
                1,
                "the log's first line (run.started) is not the one anchored: another run, or rewritten".into(),
                first.clone().unwrap_or_default(),
                a.first.clone(),
            );
        }
        if let Some(p) = prev
            && (a.seq < p.seq || a.t < p.t)
        {
            problem(a.seq, "anchors out of order".into(), format!("{} at {}", a.seq, a.t), format!(">= {} at {}", p.seq, p.t));
        }
        prev = Some(a);
        match seqs.iter().position(|s| *s == Some(a.seq)) {
            Some(i) => {
                let h = sha256_hex(lines[i].as_bytes());
                if h != a.head {
                    problem(a.seq, "event differs from its anchor: the log was rewritten".into(), h, a.head.clone());
                }
            }
            None if a.seq > last_seq => problem(
                a.seq,
                format!("log truncated: anchored up to seq {}, the log ends at {last_seq}", a.seq),
                last_seq.to_string(),
                a.seq.to_string(),
            ),
            None => problem(a.seq, "no event with the anchored seq".into(), String::new(), a.seq.to_string()),
        }
    }
    // the file is whole: the start, the declared cadence, the shutdown last
    let first_seq = seqs.iter().flatten().min().copied().unwrap_or(1);
    match parsed.first() {
        Some(a) if a.why == "start" && a.seq == first_seq => {}
        Some(a) => problem(
            a.seq,
            "the first anchor is not the start anchor on run.started: anchors were removed".into(),
            format!("{} at seq {}", a.why, a.seq),
            format!("start at seq {first_seq}"),
        ),
        None => {}
    }
    let every = cadence(&parsed);
    if let Some(n) = every {
        for w in parsed.windows(2) {
            let gap = w[1].seq.saturating_sub(w[0].seq);
            // every anchor restarts the count: a cadence anchor comes exactly N events after
            // the one before, a decision or shutdown anchor at most N
            let ok = match w[1].why.as_str() {
                "shutdown" | "decision" => gap <= n,
                _ => gap == n,
            };
            if !ok {
                problem(
                    w[1].seq,
                    format!("anchor missing: one every {n} events is declared, these are {gap} apart"),
                    format!("seq {} then {}", w[0].seq, w[1].seq),
                    format!("every {n}"),
                );
            }
        }
    } else if parsed.len() > 2 {
        problem(0, "anchors declare no cadence".into(), String::new(), String::new());
    }
    match parsed.iter().position(|a| a.why == "shutdown") {
        Some(i) if i + 1 != parsed.len() => problem(
            parsed[i + 1].seq,
            "anchors after the shutdown anchor".into(),
            parsed[i + 1].why.clone(),
            "shutdown last".into(),
        ),
        Some(_) => {}
        None if !parsed.is_empty() => problem(
            last_seq,
            "no shutdown anchor: the run did not end cleanly, or its last anchors were removed".into(),
            parsed.last().map_or(String::new(), |a| format!("last anchor: {} at seq {}", a.why, a.seq)),
            "shutdown".into(),
        ),
        None => {}
    }
    r.anchors = parsed.len();
    r.last_anchored = parsed.iter().map(|a| a.seq).max();
    r.after_last = r.last_anchored.map_or(last_seq, |s| last_seq.saturating_sub(s));
    if let Some(s) = parsed.iter().filter(|a| a.why == "shutdown").map(|a| a.seq).max() {
        r.shutdown = true;
        if last_seq > s {
            r.problems.push(Mismatch {
                seq: s + 1,
                intent: None,
                what: "events after the shutdown anchor: appended after the run ended".into(),
                logged: last_seq.to_string(),
                recomputed: s.to_string(),
            });
        }
    }
    if parsed.is_empty() {
        r.problems.push(Mismatch {
            seq: 0,
            intent: None,
            what: "no anchors".into(),
            logged: String::new(),
            recomputed: String::new(),
        });
    }
    r
}
