//! Engagements in the bridge (Phase E): the request shape, the platform's intercept and
//! fallback through the task layer, the loop that holds the picture, the abstract effect on
//! the simulation channel, and what the console sees (the live picture of a pending
//! nomination, who may still sign it).
//!
//! The decision itself is the engine's (`murmur_decide::engage`): this module only feeds it
//! the world each second and carries out what it releases.
//!
//! - **Intercept.** An approved engagement's order ([`murmur_decide::EngageOrder`]) becomes
//!   an orbit at the predicted effect point (a loiter there: the platform waits at the point,
//!   it does not home on anything), at the platform's minimum orbit radius.
//! - **Fallback.** When the engine stops an engagement after dispatch (an operator's abort,
//!   a void, link loss, the window), the recorded fallback becomes a hold (`abort`), an RTL
//!   (`return`) or an orbit around the platform's current position (`loiter`). Both are
//!   [`super::AuthorizedOrder`]s made here only from what the engine released.
//! - **Effect.** The engine's [`murmur_decide::EffectOrder`] goes to the contacts process on
//!   the simulation channel (`crate::tracks::Tracks::queue_effect`): a point, a radius, a
//!   time. Nothing here models a weapon.
//! - **Operators.** Sessions carry engagement authority for the operators whose credential
//!   holds it (a credentials run, `crate::auth`; a dev run: `MURMUR_ENGAGE_OPERATORS`, which
//!   `MURMUR_OPERATORS` must list too); only they can sign an engagement, never its proposer,
//!   the engagement tier asks for two of them, and in a credentials run each signature
//!   re-enters the signer's password (a step-up, rules 8).
//! - **Lost-link preload** (rules 6). The order carries the fallback as the platform's own
//!   lost-link action ([`murmur_decide::LostLinkPreload`]); at dispatch the bridge reads the
//!   platform's own values, sets the preload and reads it back (`super::preload`), reports it
//!   (`engage.preload`), and sends the intercept only once it confirms. One that does not
//!   confirm stops the engagement with its fallback; nothing is released without it. From
//!   30 s before the release window opens it reads the preload again every 5 s (`verify`:
//!   the release needs a read-back at most 10 s old; one that changed stops it). One
//!   parameter job at a time runs on a platform's link ([`ParamLane`]). The platform's own
//!   values are owed back from the set on: a newer engagement on it takes them over (its set
//!   does not read them off the preloaded vehicle), and they go back only when no engagement
//!   holds it, retried with backoff, every attempt logged, an inbox alert while they stay
//!   owed past [`RESTORE_ALERT_S`]. Values still owed when the bridge stops are carried over
//!   from its log at the next start (`platform.restore`, [`Decision::carry_restores`]). From
//!   rules 7 no restore goes into a dead link and one goes the moment the platform is heard
//!   again (the backoff an outage built up does not hold it), since a mission for the
//!   platform waits in the queue until it confirms; the alert lists those missions.
//! - **Lost friendlies** (rules 6). A nomination may carry `accept_unknown`: our vehicles
//!   whose position the nominating operator accepts as unknown since a named report
//!   (`POST /api/intents/{id}/accept_unknown` does it while the nomination waits with no
//!   signature; `.../revoke_unknown` withdraws it). Only an operator with engagement
//!   authority accepts (403 otherwise); an agent's request with it is refused. The card
//!   shows the acceptances to every signer, and their picture tokens carry them.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use murmur_decide::{
    Catalog, Clock,
    CheckResult, EffectOrder, EngageOrder, EngageWindow, Fallback, FallbackOrder, Intent,
    IntentId, Outcome as CheckOutcome, Proposal, Rationale, State, Task, Time, TrackFacts,
    engage,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::snapshot::{bridge_uid, duid};
use super::{Core, Decision, DecisionError, IntentRequest, engine_err};
use crate::tasks::{OpState, TaskOrder, TaskParams, TaskRequest, Tasking};

/// `POST /api/intents` (and the MCP's `propose_intent`) for `task = "engage"`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EngageParams {
    /// The perception track id (from `/api/tracks`), which an operator must have declared
    /// hostile.
    #[serde(default)]
    pub track: Option<String>,
    /// The effect radius, metres.
    #[serde(default)]
    pub effect_radius_m: Option<f64>,
    /// The window closes this many seconds after the nomination (default:
    /// `[rules.engage] default_window_s`).
    #[serde(default)]
    pub window_s: Option<u32>,
    /// What the platform does if the engagement stops before the effect: `abort` (hold
    /// where it is), `return` (RTL) or `loiter` (orbit where it is).
    #[serde(default)]
    pub fallback: Option<String>,
    /// Rules 6: our vehicles whose position the nominating operator accepts as unknown, each
    /// since the report the check names (`since`, engine ms; null: it never reported one).
    /// An operator's only.
    #[serde(default)]
    pub accept_unknown: Vec<AcceptUnknown>,
}

/// Rules 6: "vehicle `uid`, position unknown since `since`: accept".
#[derive(Debug, Clone, Deserialize)]
pub struct AcceptUnknown {
    pub uid: crate::vehicle::Uid,
    #[serde(default)]
    pub since: Option<i64>,
}

impl AcceptUnknown {
    pub(super) fn engine(&self) -> (murmur_decide::Uid, Option<Time>) {
        (duid(self.uid), self.since.map(Time))
    }
}

/// How long after an engagement ended its run is kept (its platform's orders followed).
const FORGET_AFTER_S: i64 = 300;

/// An engagement the loop follows: its platform's intercept and, once stopped, the
/// fallback.
pub(super) struct EngageRun {
    intercept: Option<Arc<Tasking>>,
    fallback: Option<Arc<Tasking>>,
    /// op id -> state last written to the log
    reported: HashMap<u64, OpState>,
    /// rules 6: the platform, the intercept waiting for its lost-link preload to confirm,
    /// whether the set still waits for the platform's link, and whether it was reported
    /// (or abandoned before it started)
    platform: Option<crate::vehicle::Uid>,
    pending: Option<TaskOrder>,
    want_set: bool,
    preload_reported: bool,
}

impl EngageRun {
    fn new(intercept: Option<Arc<Tasking>>) -> EngageRun {
        EngageRun {
            intercept,
            fallback: None,
            reported: HashMap::new(),
            platform: None,
            pending: None,
            want_set: false,
            preload_reported: false,
        }
    }
}

/// Rules 6: one platform's lost-link parameters as the bridge handles them: the one job on
/// its link, and its own values while a preload has replaced them.
#[derive(Default)]
pub(super) struct ParamLane {
    job: Option<(Purpose, super::preload::Job)>,
    owed: Option<Owed>,
}

/// What a lane's job is for: an engagement's preload (reading the platform's own values,
/// then, once they are logged as its write-ahead, writing it), a re-read near the release, a
/// restore.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Purpose {
    Read(IntentId),
    Write(IntentId),
    Verify(IntentId),
    Restore,
}

/// Rules 6: a platform's own lost-link values, owed back to it.
pub(super) struct Owed {
    values: BTreeMap<String, f64>,
    /// the vehicle they were preloaded on (a carried restore goes only to a match)
    vehicle: Option<murmur_decide::VehicleIdentity>,
    /// the engagement whose restore it is (the last one that preloaded the platform); none:
    /// carried over from an earlier run's log
    owner: Option<IntentId>,
    /// when the restore became due (no engagement holds the platform) and the next attempt
    /// (monotonic s), the attempts so far and the last one's error
    due_since: Option<f64>,
    next_try: f64,
    attempts: u32,
    last_error: Option<String>,
    /// rules 7: the vehicle was silent since the last attempt (or that attempt failed while it
    /// was): the restore goes the moment it is heard again, whatever the backoff says; and
    /// whether it is silent now (no attempt is sent into a dead link)
    waiting_link: bool,
    silent: bool,
}

impl Owed {
    pub(super) fn carried(values: BTreeMap<String, f64>, vehicle: Option<murmur_decide::VehicleIdentity>) -> Owed {
        Owed {
            values,
            vehicle,
            owner: None,
            due_since: None,
            next_try: 0.0,
            attempts: 0,
            last_error: None,
            waiting_link: false,
            silent: false,
        }
    }
}

impl ParamLane {
    /// The platform's own values owed back to it, if any.
    #[cfg(test)]
    pub(super) fn owed_values(&self) -> Option<&BTreeMap<String, f64>> {
        self.owed.as_ref().map(|o| &o.values)
    }

    pub(super) fn carrying(values: BTreeMap<String, f64>, vehicle: Option<murmur_decide::VehicleIdentity>) -> ParamLane {
        ParamLane {
            job: None,
            owed: Some(Owed::carried(values, vehicle)),
        }
    }
}

/// Rules 6: seconds before the first retry of a restore that did not confirm; each later one
/// waits twice as long, up to [`RESTORE_RETRY_MAX_S`].
const RESTORE_RETRY_S: f64 = 15.0;
const RESTORE_RETRY_MAX_S: f64 = 300.0;
/// Rules 6: how long a due restore may stay unconfirmed before the inbox says so, s.
pub(super) const RESTORE_ALERT_S: f64 = 30.0;

/// Rules 6: a fleet vehicle as a preload's write-ahead names it: its platform and callsign
/// (the manifest), its system id, and its home (from its telemetry, else the manifest's).
/// Rules 12: the same identity of every vehicle goes into each world snapshot
/// (`WorldSnapshot::identities`), so values carried over from an earlier run hold a mission
/// only for the vehicle they were preloaded on.
pub(super) fn identity_of(cell: &crate::vehicle::VehicleCell) -> murmur_decide::VehicleIdentity {
    cell.read(|v| {
        let ident = v.identity.as_ref();
        let manifest_home = ident
            .and_then(|i| i.0.get("home"))
            .and_then(|h| h.as_array())
            .and_then(|h| Some((h.first()?.as_f64()?, h.get(1)?.as_f64()?)));
        murmur_decide::VehicleIdentity {
            platform: ident.and_then(|i| i.str("platform")).map(str::to_string),
            callsign: ident.and_then(|i| i.str("callsign")).map(str::to_string),
            sysid: Some(cell.sysid),
            home: v.home.or(manifest_home).map(|(lat, lon)| murmur_decide::LonLat::new(lon, lat)),
        }
    })
}

/// Rules 15: what a start's carry reads besides the pointer: the catalog (which platforms are
/// armed), where the station's anchors are (a log it falls back on must check against its own),
/// and whether the rules record what rules 15 adds (the spent platforms, a failed carry's fleet
/// file, a fallback from a pointer that can't be read).
pub(super) struct CarryReading<'a> {
    pub catalog: &'a Catalog,
    pub anchors_dir: Option<&'a std::path::Path>,
    pub rules15: bool,
}

/// Rules 12 (murmur-cq3), before the engine starts: what earlier runs' logs still owe, as
/// `run.started` records it (`carried_from`), in that one line: the log `pointer`
/// (`.last-run-<port>`) names, then each log it still owed for because its carry had failed
/// (named pending, looked for beside it), tried again, and theirs in turn. A log that can't be
/// read is recorded with why, and with the armed platforms its fleet named (`may_owe`, from the
/// fleet identity beside it when that reads; rules 15: with the file's hash and the armed
/// platforms, `fleet_read`, or, for a pending log whose fleet file is gone too, the list the log
/// that named it pending recorded): the log that records it owes for it (pending) until a later
/// start reads it or an operator writes it off, so the chain never drops it. Rules 15: a log read
/// also carries the platforms it had spent (`spent`). A pointer that can't be read, is empty or
/// names no log (a crash mid-write, a stray edit) is a failed carry too, named by the pointer's
/// own file name (the log it named is unknown: it stays owed until written off); rules 15: unless
/// this station has a log ([`fallback_log`]): the newest by its carry chain is carried from in the
/// pointer's place when its chain and anchors check and no newer log could be the one the pointer
/// named (`pointer_failed` says why the pointer failed), else it is a failed carry under its own
/// name, its armed platforms held (fail closed) and pending, so the next start reads it and what
/// it owed for. A pointer of two identical lines names that log. Empty: no pointer (a first
/// start). Never an error: a start is never stopped by what the last one left behind.
pub(super) fn read_carry(pointer: &std::path::Path, r: &CarryReading<'_>) -> Vec<murmur_decide::replay::CarriedFrom> {
    let pointer_name = pointer
        .file_name()
        .map_or_else(|| ".last-run".to_string(), |f| f.to_string_lossy().into_owned());
    let unknown = |why: String| {
        // rules 15: this station's newest log in the pointer's place
        let why = if r.rules15 {
            let dir = pointer.parent().unwrap_or_else(|| std::path::Path::new("."));
            match fallback_log(dir, &pointer_name, r.anchors_dir) {
                PointerFallback::Carry(path) => return carry_chain(path, Some(why), r),
                PointerFallback::Refused { log, why: refused } => {
                    let (may_owe, fleet_read) = armed_of(&log, r);
                    return vec![murmur_decide::replay::CarriedFrom {
                        log: log.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                        error: Some(refused),
                        pointer_failed: Some(why),
                        may_owe,
                        fleet_read,
                        ..Default::default()
                    }];
                }
                PointerFallback::None(none) => format!("{why}; {none}"),
            }
        } else {
            why
        };
        vec![murmur_decide::replay::CarriedFrom {
            log: pointer_name.clone(),
            error: Some(why),
            ..Default::default()
        }]
    };
    let named = match std::fs::read(pointer) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return unknown(format!("the pointer can't be read ({e}): the log it named is unknown")),
        Ok(bytes) => match pointer_target(&bytes) {
            Ok(p) => p,
            Err(why) => return unknown(why),
        },
    };
    carry_chain(named, None, r)
}

/// Rules 12: the carry from `named` (the log the pointer named, or rules 15 the one found in its
/// place, `fallback` saying why) and every log it still owed for, as [`read_carry`] records them.
fn carry_chain(
    named: std::path::PathBuf,
    fallback: Option<String>,
    r: &CarryReading<'_>,
) -> Vec<murmur_decide::replay::CarriedFrom> {
    let dir = named.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let mut out: Vec<murmur_decide::replay::CarriedFrom> = Vec::new();
    // rules 15: what each log read recorded its pending logs may owe (a pending log whose fleet
    // file is gone too keeps that list)
    let mut recorded: BTreeMap<String, Vec<murmur_decide::replay::OwedRestore>> = BTreeMap::new();
    let mut todo = std::collections::VecDeque::from([named]);
    while let Some(path) = todo.pop_front() {
        let log = path
            .file_name()
            .map_or_else(|| path.display().to_string(), |f| f.to_string_lossy().into_owned());
        if out.iter().any(|c| c.log == log) {
            continue;
        }
        let mut entry = murmur_decide::replay::CarriedFrom {
            log: log.clone(),
            pointer_failed: if out.is_empty() { fallback.clone() } else { None },
            ..Default::default()
        };
        // a pointer recorded as a failed carry before: the log it named is unknown, so there
        // is nothing to read again (it stays owed until written off)
        if murmur_decide::replay::is_pointer_name(&log) {
            entry.error = Some("a .last-run pointer that could not be read, recorded so at an earlier start: the log it named is unknown".into());
            out.push(entry);
            continue;
        }
        match std::fs::read_to_string(&path) {
            Err(e) => {
                entry.error = Some(e.to_string());
                let (may_owe, fleet_read) = armed_of(&path, r);
                entry.may_owe = may_owe;
                entry.fleet_read = fleet_read;
                // rules 15: its fleet file gone too: the vehicles the log that named it pending
                // recorded it may owe stay held
                if r.rules15
                    && entry.fleet_read.is_none()
                    && entry.may_owe.is_empty()
                    && let Some(m) = recorded.get(&log)
                {
                    entry.may_owe = m.clone();
                }
            }
            Ok(text) => {
                if let Some((head, seq)) = murmur_decide::replay::log_head(&text) {
                    entry.head = Some(head);
                    entry.last_seq = Some(seq);
                }
                let debts = murmur_decide::replay::owed_debts(&text);
                for p in debts.pending.iter().filter(|p| murmur_decide::replay::is_log_file_name(p)) {
                    todo.push_back(dir.join(p));
                }
                for (l, m) in debts.pending_may_owe {
                    recorded.entry(l).or_insert(m);
                }
                entry.owed = debts.owed;
                entry.pending = debts.pending;
                // rules 15: the platforms it had spent stay spent
                if r.rules15 {
                    entry.spent = debts.spent;
                }
            }
        }
        out.push(entry);
    }
    out
}

/// Rules 15: what a start whose pointer can't be read finds in its place ([`fallback_log`]).
enum PointerFallback {
    /// This station's newest log, which checks: carried from in the pointer's place.
    Carry(std::path::PathBuf),
    /// This station's newest log, which the start won't carry from (`why`): a failed carry
    /// under its own name, its armed platforms held and pending (fail closed).
    Refused { log: std::path::PathBuf, why: String },
    /// No log of this station: the pointer is a failed carry, as before (why).
    None(String),
}

/// Rules 15: how far apart two starts may be and still not be told apart by their times (this
/// host's wall clock steps by up to a second; the bridge clock is steered within about 2 s of it).
const FALLBACK_CLOCK_MARGIN_MS: i64 = 10_000;

/// One `.jsonl` in the pointer's directory as the fallback reads it: its first line's
/// `run.started` (its time, the pointer it names, the logs it carried from), or none when that
/// line does not parse (then its modification time).
struct LogHead {
    name: String,
    path: std::path::PathBuf,
    started: Option<(murmur_decide::Time, Option<String>, Vec<String>)>,
    modified_ms: Option<i64>,
}

/// Rules 15 (murmur-zq3): what a start carries from when its pointer can't be read. The logs of
/// this station are those whose `run.started` names this pointer and, by the carry chain, every
/// log that carried from one of them (a start carries only from the log its own pointer names,
/// so an older build's log that names no pointer is this station's when it carried from one).
/// The newest is the one no other log carried from (ordered by the chain, not by wall-clock
/// times, which can step back here); two such, the later start, unless they started within
/// [`FALLBACK_CLOCK_MARGIN_MS`] of each other (ambiguous: refused). It is carried from only if it
/// names this pointer, its hash chain holds and it checks against its own anchors (every
/// problem but the shutdown anchor a crashed run never wrote), and no other log in the
/// directory that names no pointer, or whose first line does not parse, could be newer (started,
/// or for one that does not parse last written, after it less the margin): such a log may be the
/// one the pointer named. Otherwise it is refused (a failed carry under its own name: fail
/// closed). Logs of other stations sharing the directory (naming another pointer) are never
/// looked at further.
fn fallback_log(dir: &std::path::Path, pointer_name: &str, anchors_dir: Option<&std::path::Path>) -> PointerFallback {
    use std::io::BufRead as _;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return PointerFallback::None(format!("no log to fall back on: {} can't be listed", dir.display()));
    };
    let mut logs: Vec<LogHead> = Vec::new();
    for ent in entries.flatten() {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") || !murmur_decide::replay::is_log_file_name(&name) {
            continue;
        }
        let modified_ms = ent
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        let mut first = String::new();
        let read = std::fs::File::open(&path).map(std::io::BufReader::new).and_then(|mut f| f.read_line(&mut first));
        let started = read.ok().and_then(|_| serde_json::from_str::<murmur_decide::Event>(first.trim_end()).ok()).and_then(|ev| match ev.body {
            murmur_decide::EventBody::RunStarted { pointer, carried_from, .. } => {
                Some((ev.t, pointer, carried_from.into_iter().map(|c| c.log).collect()))
            }
            _ => None,
        });
        logs.push(LogHead {
            name,
            path,
            started,
            modified_ms,
        });
    }
    // this station's logs: those naming its pointer, and those that carried from one of them
    let mut station: std::collections::BTreeSet<String> = logs
        .iter()
        .filter(|l| l.started.as_ref().is_some_and(|(_, p, _)| p.as_deref() == Some(pointer_name)))
        .map(|l| l.name.clone())
        .collect();
    loop {
        let more: Vec<String> = logs
            .iter()
            .filter(|l| !station.contains(&l.name))
            .filter(|l| l.started.as_ref().is_some_and(|(_, _, from)| from.iter().any(|f| station.contains(f))))
            .map(|l| l.name.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        station.extend(more);
    }
    if station.is_empty() {
        return PointerFallback::None("no log of this station to fall back on".into());
    }
    // the newest by the chain: carried from by no log of the station
    let carried: std::collections::BTreeSet<&str> = logs
        .iter()
        .filter(|l| station.contains(&l.name))
        .flat_map(|l| l.started.iter().flat_map(|(_, _, from)| from.iter().map(String::as_str)))
        .collect();
    let mut tips: Vec<&LogHead> = logs
        .iter()
        .filter(|l| station.contains(&l.name) && !carried.contains(l.name.as_str()))
        .collect();
    tips.sort_by_key(|l| std::cmp::Reverse(l.started.as_ref().map(|(t, _, _)| *t)));
    let Some(newest) = tips.first() else {
        return PointerFallback::None("no log of this station that no later one carried from".into());
    };
    let t = newest.started.as_ref().map_or(murmur_decide::Time(0), |(t, _, _)| *t);
    let refuse = |why: String| PointerFallback::Refused {
        log: newest.path.clone(),
        why: format!("found in place of the pointer, not carried from: {why}"),
    };
    if let Some(next) = tips.get(1)
        && next.started.as_ref().is_some_and(|(u, _, _)| t.ms_since(*u) <= FALLBACK_CLOCK_MARGIN_MS)
    {
        return refuse(format!(
            "{} and {} both end this station's carry chain and started too close together to tell which is newer",
            newest.name, next.name
        ));
    }
    if newest.started.as_ref().is_none_or(|(_, p, _)| p.as_deref() != Some(pointer_name)) {
        return refuse(format!(
            "{} is this station's newest log by its carry chain but names no pointer (an older build): its anchors are not checked here",
            newest.name
        ));
    }
    // a log that may be newer and can't be placed: the pointer may have named it
    let since = t.0 - FALLBACK_CLOCK_MARGIN_MS;
    if let Some(other) = logs.iter().find(|l| {
        !station.contains(&l.name)
            && match &l.started {
                Some((u, None, _)) => u.0 >= since,
                Some((_, Some(_), _)) => false,
                None => l.modified_ms.is_none_or(|m| m >= since),
            }
    }) {
        let what = if other.started.is_some() { "names no pointer" } else { "does not start with a run.started that parses" };
        return refuse(format!(
            "{} is as new as {} or newer and {what}: it may be the log the pointer named",
            other.name, newest.name
        ));
    }
    let Some(anchors_dir) = anchors_dir else {
        return refuse("this run keeps no anchors to check it by".into());
    };
    let text = match std::fs::read_to_string(&newest.path) {
        Ok(t) => t,
        Err(e) => return refuse(format!("{} can't be read ({e})", newest.name)),
    };
    // up to its last complete event (a crash may have torn a line after it)
    let text = murmur_decide::replay::up_to_seq(&text, murmur_decide::replay::log_head(&text).map(|(_, s)| s));
    if let Some(why) = murmur_decide::replay::chain_problem(&text) {
        return refuse(format!("{} does not check: its hash chain ({why})", newest.name));
    }
    let stem = newest.name.strip_suffix(".jsonl").unwrap_or(&newest.name);
    let anchors = match std::fs::read_to_string(anchors_dir.join(format!("{stem}.anchors"))) {
        Ok(a) => a,
        Err(e) => return refuse(format!("{} has no anchors to check it by ({e})", newest.name)),
    };
    if let Some(why) = super::queue::anchors_refuse(&text, &anchors) {
        return refuse(format!("{} does not check against its anchors ({why})", newest.name));
    }
    PointerFallback::Carry(newest.path.clone())
}

/// Rules 12: a pending carry as the inbox names it: an earlier log, or a `.last-run` pointer
/// that could not be read (the log it named unknown).
fn carry_name(log: &str) -> String {
    if murmur_decide::replay::is_pointer_name(log) {
        format!("the pointer {log} (the log it named is unknown)")
    } else {
        format!("the earlier log {log}")
    }
}

/// Rules 12: what a pending carry's alert says of the vehicles it may owe values to.
fn may_owe_text(may_owe: &[murmur_decide::Uid]) -> String {
    if may_owe.is_empty() {
        return "which vehicles it may owe is unknown (its fleet can't be read either), so no mission is held for it".into();
    }
    let uids: Vec<&str> = may_owe.iter().map(murmur_decide::Uid::as_str).collect();
    format!(
        "it may owe them to vehicle(s) {} (the armed platforms its fleet named): their missions wait in the Queue until it is read or written off",
        uids.join(", ")
    )
}

/// Rules 12 (review M2): `pointer` names `log`, written whole or not at all: to a temporary
/// file beside it, synced, then renamed over it, so a crash mid-write leaves the old pointer
/// or the new one, never a torn one. Rules 15 (murmur-zq3): then the directory is synced too, so
/// the rename itself survives a power loss (without it the directory entry may still name the
/// old pointer, or none). `Ok(Some(e))`: written and renamed, the directory not synced (`e`), a
/// warning; Err: not written.
pub(super) fn write_pointer(pointer: &std::path::Path, log: &std::path::Path) -> std::io::Result<Option<std::io::Error>> {
    use std::io::Write as _;
    let mut name = pointer.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let tmp = pointer.with_file_name(name);
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(log.display().to_string().as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, pointer).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    // the rename is done: the pointer names this log; a directory that can't be synced only
    // leaves the rename to the system's own flush (a warning, not a failed write)
    let dir = match pointer.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => std::path::Path::new("."),
    };
    Ok(std::fs::File::open(dir).and_then(|d| d.sync_all()).err())
}

/// Rules 15: a pointer written whose directory could not be synced after the rename (`e`), said
/// as a warning: the pointer names this run's log now; only a power loss before the system
/// flushes the directory could bring back the old one.
fn pointer_unsynced(pointer: &std::path::Path, e: &std::io::Error) {
    eprintln!(
        "decision: warning: {} names this run's log, but its directory could not be synced after the rename ({e}): a power loss before the system flushes it could leave the previous pointer",
        pointer.display()
    );
}

/// Rules 12: the log a `.last-run` pointer's content names: one line, a path whose file name
/// is a log's (`*.jsonl`, a bare file name). Err says why not (empty, torn, garbled).
fn pointer_target(bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err("the pointer is garbled (not text): the log it named is unknown".into());
    };
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let Some(line) = lines.next() else {
        return Err("the pointer is empty (a write cut short?): the log it named is unknown".into());
    };
    // rules 15 (murmur-zq3): the same line twice (an editor's or a tool's doing) names that log
    // all the same; two different ones name none
    let line = if lines.all(|l| l == line) { line } else { text.trim() };
    let path = std::path::PathBuf::from(line);
    let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    if line.contains('\n') || !name.ends_with(".jsonl") || !murmur_decide::replay::is_log_file_name(&name) {
        let shown: String = line.chars().take(80).collect();
        return Err(format!("the pointer names no log ({shown:?}): the log it named is unknown"));
    }
    Ok(path)
}

/// Rules 12: the armed platforms the fleet beside an unreadable log named (its run's
/// `<log stem>.fleet.json`), as the vehicles they were: values it may owe them are unknown. Only an
/// armed platform is ever preloaded (engagements are on armed platforms). Empty when the fleet
/// can't be read either. Rules 15: with the file as read (its SHA-256 and the catalog's armed
/// platforms it was judged by, `fleet_read`), so replay can compare it when it is at hand.
fn armed_of(
    log: &std::path::Path,
    r: &CarryReading<'_>,
) -> (Vec<murmur_decide::replay::OwedRestore>, Option<murmur_decide::replay::FleetRead>) {
    let Ok(bytes) = std::fs::read(super::queue::fleet_file(log)) else {
        return (Vec::new(), None);
    };
    let armed: Vec<String> = r
        .catalog
        .platforms
        .iter()
        .filter(|(_, p)| p.armed)
        .map(|(id, _)| id.clone())
        .collect();
    let Some(may_owe) = murmur_decide::replay::fleet_may_owe(&bytes, &armed) else {
        return (Vec::new(), None);
    };
    let read = r.rules15.then(|| murmur_decide::replay::FleetRead {
        sha256: murmur_decide::hash::sha256_hex(&bytes),
        armed,
    });
    (may_owe, read)
}

/// The engine proposal for an engage request.
pub(super) fn proposal(req: &IntentRequest, now: Time) -> Result<Proposal, DecisionError> {
    let bad = |m: String| DecisionError::new(400, m);
    let p = &req.engage;
    let track = p
        .track
        .clone()
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| bad("engage needs track: a track id from /api/tracks".into()))?;
    let radius = p
        .effect_radius_m
        .ok_or_else(|| bad("engage needs effect_radius_m".into()))?;
    let fallback: Fallback = match p.fallback.as_deref() {
        None => return Err(bad("engage needs fallback: abort, return or loiter".into())),
        Some(f) => serde_json::from_value(json!(f))
            .map_err(|_| bad(format!("fallback {f:?}: abort, return or loiter")))?,
    };
    if req.uids.len() != 1 {
        return Err(bad("engage names exactly one armed platform in uids".into()));
    }
    let window = p.window_s.map(|s| EngageWindow {
        from: None,
        until: now.plus_ms(i64::from(s) * 1000),
    });
    let mut proposal = Proposal::new(
        Task::Engage {
            track,
            effect_radius_m: radius,
            window,
            fallback,
        },
        req.uids.iter().map(|u| duid(*u)).collect(),
    );
    proposal.constraints = req.constraints.clone();
    proposal.rationale = req
        .rationale
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(Rationale::new)
        .transpose()
        .map_err(|e| bad(e.to_string()))?;
    proposal.validate().map_err(|e| bad(e.to_string()))?;
    Ok(proposal)
}

impl Decision {
    /// The orbit radius the platform loiters at over the effect point: its catalog minimum
    /// orbit (the rules' margin over its minimum turn radius, or the fixed-wing default),
    /// at least 30 m.
    fn loiter_radius(&self, uid: &murmur_decide::Uid) -> f64 {
        let platform = bridge_uid(uid)
            .and_then(|u| self.fleet().get(&u).cloned())
            .and_then(|c| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("platform"))
                        .map(str::to_string)
                })
            })
            .and_then(|p| self.catalog.get(&p).cloned());
        let min = platform.as_ref().and_then(|p| {
            p.min_turn_radius_m.or_else(|| {
                p.autopilot
                    .filter(|a| a.is_fixed_wing())
                    .map(|_| self.rules.fixed_wing_default_min_turn_radius_m)
            })
        });
        min.map_or(30.0, |m| (m * self.rules.orbit_radius_margin).ceil())
            .max(30.0)
    }

    /// Dispatch an approved engagement: the engine releases its order (`intent.dispatched`
    /// with the effect point), the platform is sent to loiter at the point.
    pub(super) fn dispatch_engage(&self, core: &mut Core, id: IntentId) -> Result<(), DecisionError> {
        let spec = core
            .engine
            .intent(id)
            .ok_or_else(|| DecisionError::new(404, format!("unknown intent {id}")))?
            .spec()
            .clone();
        let fleet = self.fleet();
        let Some(uid) = spec.uids.first().and_then(bridge_uid) else {
            core.engine
                .void(id, "the platform has no bridge uid")
                .map_err(engine_err)?;
            return Ok(());
        };
        let preview_point = core
            .engine
            .intent(id)
            .and_then(Intent::engagement)
            .and_then(|e| e.snapshot.as_ref())
            .map(|s| s.preview.effect_point);
        let Some(point) = preview_point else {
            core.engine
                .void(id, "no approval snapshot")
                .map_err(engine_err)?;
            return Ok(());
        };
        let radius = self.loiter_radius(&spec.uids[0]);
        let request = |p: [f64; 2]| TaskRequest {
            task: "orbit".into(),
            uids: vec![uid],
            params: TaskParams {
                center: Some(p),
                radius: Some(radius),
                ..TaskParams::default()
            },
        };
        // the task layer must be able to take it before the engine marks it dispatched
        if let Err(e) = TaskOrder::new(request([point.lon, point.lat]), &fleet) {
            core.engine
                .void(id, &format!("the task layer can't take it: {e}"))
                .map_err(engine_err)?;
            return Ok(());
        }
        let o: EngageOrder = match core.engine.dispatch_order(id).map_err(engine_err)? {
            murmur_decide::Order::Engage(o) => o,
            _ => return Err(DecisionError::new(500, "an engagement released another order")),
        };
        // the loiter radius the order carries (rules 4 on), the one replay binds
        let radius = o.loiter_radius_m.unwrap_or(radius);
        let request = |p: [f64; 2]| TaskRequest {
            task: "orbit".into(),
            uids: vec![uid],
            params: TaskParams {
                center: Some(p),
                radius: Some(radius),
                ..TaskParams::default()
            },
        };
        let order = match TaskOrder::new(request([o.effect_point.lon, o.effect_point.lat]), &fleet) {
            Ok(order) => order,
            Err(e) => {
                let why = format!("the task layer refused the intercept: {e}");
                if let Some(f) = core
                    .engine
                    .stop_engagement_system(id, &why)
                    .map_err(engine_err)?
                {
                    self.command_fallback(core, f);
                }
                return Ok(());
            }
        };
        // rules 6: the fallback as the platform's own lost-link action first (on the
        // platform's lane, once any job there has finished); the intercept goes out once its
        // read-back confirms
        if o.preload.is_some() {
            // the older orders on the platform first, in the step of this dispatch (rules 7:
            // nothing logged between)
            self.supersede(core, id, &[spec.uids[0].clone()]);
            let mut run = EngageRun::new(None);
            run.platform = Some(uid);
            if fleet.contains_key(&uid) {
                run.want_set = true;
                run.pending = Some(order);
                core.engagements.insert(id, run);
            } else {
                // gone from the fleet: report it as a preload that could not be set
                let report = murmur_decide::PreloadReport {
                    uid: spec.uids[0].clone(),
                    phase: murmur_decide::PreloadPhase::Set,
                    before: BTreeMap::new(),
                    readback: BTreeMap::new(),
                    error: Some("the platform is not in the fleet".into()),
                    vehicle: None,
                };
                run.preload_reported = true;
                core.engagements.insert(id, run);
                if let Some(f) = core.engine.record_preload(id, report).map_err(engine_err)? {
                    self.command_fallback(core, f);
                }
            }
            self.step_params(core);
            return Ok(());
        }
        let tasking = self.world.tasks.dispatch(super::AuthorizedOrder {
            intent: id,
            order,
        });
        core.taskings.insert(id, tasking.clone());
        core.engagements.insert(id, EngageRun::new(Some(tasking)));
        self.supersede(core, id, &[spec.uids[0].clone()]);
        Ok(())
    }

    /// Rules 6: the platforms' lost-link parameter jobs, one at a time per platform. First the
    /// jobs that finished: a set reported to the engine (`engage.preload`; its own values are
    /// owed back from then on, the engagement owning their restore), then the intercept sent
    /// if it confirmed and the engagement is still under way, else (the engine stopped it)
    /// the fallback commanded; a re-read near the release reported (one that changed stops
    /// it); a restore reported (`engage.preload` restore by its owner, else, or when the
    /// engine refuses it, `platform.restore`: never dropped), retried with backoff until it
    /// confirms. Then, on each free lane: a set waiting, else a re-read due, else the
    /// restore once no engagement holds the platform.
    pub(super) fn step_params(&self, core: &mut Core) {
        self.drop_closed_restores(core);
        let mut stops: Vec<FallbackOrder> = Vec::new();
        let mut sends: Vec<(IntentId, TaskOrder)> = Vec::new();
        let now_mono = crate::clock::mono();
        let fleet = self.fleet();
        let finished: Vec<(crate::vehicle::Uid, Purpose, super::preload::JobResult)> = core
            .params
            .iter_mut()
            .filter_map(|(uid, lane)| {
                let (purpose, job) = lane.job.as_ref()?;
                let res = job.take()?;
                let purpose = *purpose;
                lane.job = None;
                Some((*uid, purpose, res))
            })
            .collect();
        for (uid, purpose, res) in finished {
            match purpose {
                Purpose::Read(id) => {
                    let under_way = core.engine.intent(id).map(Intent::state) == Some(State::Dispatched);
                    let preload = core.engine.intent(id).and_then(Intent::engagement).and_then(|e| e.preload.clone());
                    // ended meanwhile: nothing was written, nothing to report or write
                    if !under_way {
                        if let Some(run) = core.engagements.get_mut(&id) {
                            run.preload_reported = true;
                            run.pending = None;
                        }
                        continue;
                    }
                    // a read that failed: nothing written; the set fails with it
                    if res.error.is_some() || preload.is_none() {
                        if let Some(run) = core.engagements.get_mut(&id) {
                            run.preload_reported = true;
                        }
                        let report = murmur_decide::PreloadReport {
                            uid: duid(uid),
                            phase: murmur_decide::PreloadPhase::Set,
                            before: BTreeMap::new(),
                            readback: BTreeMap::new(),
                            error: res.error.or_else(|| Some("no preload in the order".into())),
                            vehicle: None,
                        };
                        match core.engine.record_preload(id, report) {
                            Ok(Some(f)) => stops.push(f),
                            Ok(None) => {}
                            Err(e) => eprintln!("decision: preload {id}: {e}"),
                        }
                        continue;
                    }
                    // the write-ahead: its own values in the log before anything is written
                    let vehicle = fleet.get(&uid).map(|c| identity_of(c));
                    let report = murmur_decide::PreloadReport {
                        uid: duid(uid),
                        phase: murmur_decide::PreloadPhase::Intent,
                        before: res.before.clone(),
                        readback: BTreeMap::new(),
                        error: None,
                        vehicle: vehicle.clone(),
                    };
                    if let Err(e) = core.engine.record_preload(id, report) {
                        eprintln!("decision: preload write-ahead {id}: {e}");
                        if let Some(run) = core.engagements.get_mut(&id) {
                            run.preload_reported = true;
                        }
                        continue;
                    }
                    // owed back from now on, this engagement owning their restore (it took
                    // over any earlier owner's)
                    let lane = core.params.entry(uid).or_default();
                    let owed = lane.owed.get_or_insert_with(|| Owed::carried(BTreeMap::new(), None));
                    for (n, v) in &res.before {
                        owed.values.entry(n.clone()).or_insert(*v);
                    }
                    owed.owner = Some(id);
                    owed.vehicle = vehicle;
                    owed.due_since = None;
                    // then the write, at once (the lane is this engagement's)
                    if let (Some(cell), Some(p)) = (fleet.get(&uid), preload) {
                        lane.job = Some((Purpose::Write(id), super::preload::start_write(cell.clone(), p, res.before)));
                    }
                }
                Purpose::Write(id) => {
                    if let Some(run) = core.engagements.get_mut(&id) {
                        run.preload_reported = true;
                    }
                    let report = murmur_decide::PreloadReport {
                        uid: duid(uid),
                        phase: murmur_decide::PreloadPhase::Set,
                        before: res.before,
                        readback: res.readback,
                        error: res.error,
                        vehicle: None,
                    };
                    match core.engine.record_preload(id, report) {
                        Ok(Some(f)) => stops.push(f),
                        Ok(None) => {
                            let i = core.engine.intent(id);
                            let confirmed = i
                                .and_then(Intent::engagement)
                                .and_then(|e| e.preloaded.as_ref())
                                .is_some_and(|r| r.confirmed);
                            let under_way = i.map(Intent::state) == Some(State::Dispatched);
                            if confirmed
                                && under_way
                                && let Some(order) = core.engagements.get_mut(&id).and_then(|r| r.pending.take())
                            {
                                sends.push((id, order));
                            }
                        }
                        Err(e) => eprintln!("decision: preload {id}: {e}"),
                    }
                }
                Purpose::Verify(id) => {
                    let report = murmur_decide::PreloadReport {
                        uid: duid(uid),
                        phase: murmur_decide::PreloadPhase::Verify,
                        before: BTreeMap::new(),
                        readback: res.readback,
                        error: res.error,
                        vehicle: None,
                    };
                    match core.engine.record_preload(id, report) {
                        Ok(Some(f)) => stops.push(f),
                        Ok(None) => {}
                        // ended meanwhile: the re-read changes nothing
                        Err(e) => eprintln!("decision: preload re-read {id}: {e}"),
                    }
                }
                Purpose::Restore => {
                    let Some(owed) = core.params.get_mut(&uid).and_then(|l| l.owed.as_mut()) else {
                        continue;
                    };
                    let confirmed = res.error.is_none() && engage::restored(&owed.values, &res.readback).is_ok();
                    // by its owner's engagement when the engine takes it; else, or when it
                    // refuses it, as the platform's own (logged either way)
                    let by_owner = owed.owner.is_some_and(|id| {
                        core.engine
                            .record_preload(
                                id,
                                murmur_decide::PreloadReport {
                                    uid: duid(uid),
                                    phase: murmur_decide::PreloadPhase::Restore,
                                    before: BTreeMap::new(),
                                    readback: res.readback.clone(),
                                    error: res.error.clone(),
                                    vehicle: None,
                                },
                            )
                            .is_ok()
                    });
                    if !by_owner
                        && let Err(e) = core.engine.record_platform_restore(murmur_decide::PlatformRestoreReport {
                            uid: duid(uid),
                            values: owed.values.clone(),
                            carried: false,
                            from: None,
                            readback: res.readback.clone(),
                            error: res.error.clone(),
                            vehicle: None,
                        })
                    {
                        eprintln!("decision: restore of {uid}: {e}");
                    }
                    if confirmed {
                        if let Some(lane) = core.params.get_mut(&uid) {
                            lane.owed = None;
                        }
                    } else {
                        owed.attempts += 1;
                        owed.last_error = Some(res.error.unwrap_or_else(|| {
                            engage::restored(&owed.values, &res.readback).err().unwrap_or_default()
                        }));
                        let wait = (RESTORE_RETRY_S * 2f64.powi(owed.attempts.saturating_sub(1).min(8) as i32))
                            .min(RESTORE_RETRY_MAX_S);
                        owed.next_try = now_mono + wait;
                        // rules 7: it failed because the link went: again as soon as it is back
                        if self.rules.rules_version >= murmur_decide::QUEUE_FOLLOWUPS_SINCE
                            && fleet.get(&uid).is_some_and(|c| crate::tasks::link_silent(c))
                        {
                            owed.waiting_link = true;
                        }
                    }
                }
            }
        }
        for (id, order) in sends {
            let tasking = self.world.tasks.dispatch(super::AuthorizedOrder {
                intent: id,
                order,
            });
            core.taskings.insert(id, tasking.clone());
            if let Some(run) = core.engagements.get_mut(&id) {
                run.intercept = Some(tasking);
            }
        }
        for f in stops {
            self.command_fallback(core, f);
        }
        self.start_param_jobs(core, now_mono);
    }

    /// Rules 6: on each platform whose lane is free, the next job: a set waiting (its
    /// engagement still under way; one that ended first never touches the vehicle), else a
    /// re-read due near the release, else the restore of its own values once no engagement
    /// holds it (none under way or waiting for its set: a restore never undoes a newer
    /// engagement's preload).
    fn start_param_jobs(&self, core: &mut Core, now_mono: f64) {
        let fleet = self.fleet();
        let now = self.clock.now();
        // engagements that ended before their set started: nothing to set or report
        for (id, run) in core.engagements.iter_mut() {
            let under_way = core.engine.intent(*id).map(Intent::state) == Some(State::Dispatched);
            if run.want_set && !under_way {
                run.want_set = false;
                run.preload_reported = true;
                run.pending = None;
            }
            if !under_way {
                run.pending = None;
            }
        }
        let mut platforms: Vec<crate::vehicle::Uid> = core.params.keys().copied().collect();
        platforms.extend(core.engagements.values().filter_map(|r| r.platform));
        platforms.sort_unstable();
        platforms.dedup();
        for uid in platforms {
            if core.params.get(&uid).is_some_and(|l| l.job.is_some()) {
                continue;
            }
            let cell = fleet.get(&uid).cloned();
            let holding: Vec<IntentId> = core
                .engagements
                .iter()
                .filter(|(id, r)| {
                    r.platform == Some(uid)
                        && (r.want_set || core.engine.intent(**id).map(Intent::state) == Some(State::Dispatched))
                })
                .map(|(id, _)| *id)
                .collect();
            // a set waiting
            if let Some(cell) = &cell
                && let Some(&id) = holding.iter().find(|id| core.engagements.get(id).is_some_and(|r| r.want_set))
                && let Some(p) = core.engine.intent(id).and_then(Intent::engagement).and_then(|e| e.preload.clone())
            {
                let known = core
                    .params
                    .get(&uid)
                    .and_then(|l| l.owed.as_ref())
                    .map(|o| o.values.clone())
                    .unwrap_or_default();
                if let Some(run) = core.engagements.get_mut(&id) {
                    run.want_set = false;
                }
                core.params.entry(uid).or_default().job =
                    Some((Purpose::Read(id), super::preload::start_read(cell.clone(), p, known)));
                continue;
            }
            // a re-read due
            let rules = &self.rules;
            if let Some(cell) = &cell
                && let Some((id, p)) = holding.iter().find_map(|id| {
                let e = core.engine.intent(*id).and_then(Intent::engagement)?;
                (core.engagements.get(id).is_some_and(|r| r.preload_reported)
                    && engage::preload_verify_due(e, now, rules))
                .then(|| (*id, e.preload.clone()))
                .and_then(|(id, p)| Some((id, p?)))
            }) {
                core.params.entry(uid).or_default().job =
                    Some((Purpose::Verify(id), super::preload::start_verify(cell.clone(), p)));
                continue;
            }
            // the restore, once nothing holds the platform
            if !holding.is_empty() {
                continue;
            }
            let Some(lane) = core.params.get_mut(&uid) else { continue };
            let Some(owed) = lane.owed.as_mut() else { continue };
            owed.due_since.get_or_insert(now_mono);
            // values carried over from an earlier run go back only to the vehicle they were
            // preloaded on (the uid may name another one now); rules 12: another vehicle is
            // one whose platform, callsign or system id differs, never its home alone (set
            // again at every arming), as the engine's hold judges it
            let mismatch = match (&cell, owed.owner, &owed.vehicle) {
                (Some(c), None, Some(was)) if self.rules.followups() => was.differs(&identity_of(c)),
                (Some(c), None, Some(was)) => was.matches(&identity_of(c)).err(),
                _ => None,
            };
            // rules 7 (murmur-85l): never into a dead link, and at once when the vehicle is
            // heard again after silence (a mission for it waits in the queue for this restore:
            // the backoff of attempts lost to the outage must not hold it for minutes)
            let rules7 = self.rules.rules_version >= murmur_decide::QUEUE_FOLLOWUPS_SINCE;
            let silent = rules7 && cell.as_ref().is_some_and(|c| crate::tasks::link_silent(c));
            owed.silent = silent;
            if silent {
                owed.waiting_link = true;
            } else if owed.waiting_link && cell.is_some() {
                owed.waiting_link = false;
                owed.next_try = now_mono;
            }
            match cell {
                // gone from the fleet: still owed, and the inbox says so
                None => owed.last_error = Some("the vehicle is not in the fleet".into()),
                Some(_) if mismatch.is_some() => {
                    owed.last_error = Some(format!(
                        "not written: the vehicle with this uid may not be the one preloaded ({})",
                        mismatch.unwrap_or_default()
                    ));
                }
                Some(_) if silent => {}
                Some(cell) if now_mono >= owed.next_try => {
                    lane.job = Some((Purpose::Restore, super::preload::start_restore(cell, owed.values.clone())));
                }
                Some(_) => {}
            }
        }
    }

    /// Rules 12: a platform the ledger no longer owes anything (spent by its own effect, or
    /// written off by an operator: `restore.closed`) is not restored: its lane forgets it.
    pub(super) fn drop_closed_restores(&self, core: &mut Core) {
        if !self.rules.followups() {
            return;
        }
        let Core { engine, params, .. } = core;
        for (uid, lane) in params.iter_mut() {
            if lane.owed.is_some() && !engine.restore_owed_to(&duid(*uid)) {
                lane.owed = None;
            }
        }
    }

    /// Rules 12, right after the engine started with what earlier logs still owed (in
    /// `run.started`): the values carried over are owed back on their platforms' lanes (put
    /// back once a vehicle that matches the one they were preloaded on answers), a log that
    /// could not be read (or a pointer that could not) is said in the inbox, and `pointer`
    /// names this run's log from now on, always (this log records the whole carry, failed
    /// ones included, so nothing drops out of the chain), written whole or not at all
    /// ([`write_pointer`]).
    pub(super) fn seed_carried(&self, pointer: Option<&std::path::Path>) {
        {
            let mut guard = self.lock();
            let core = &mut *guard;
            for o in core.engine.carried_owed() {
                let Some(uid) = bridge_uid(&o.uid) else { continue };
                println!("lost-link values of vehicle {uid} owed back from an earlier run (run.started): restoring");
                core.params.insert(uid, ParamLane::carrying(o.values, o.vehicle));
            }
            // rules 15: platforms an earlier run spent stay spent (the same vehicle only)
            for sp in core.engine.spent_platforms().into_iter().filter(|s| s.from.is_some()) {
                println!(
                    "vehicle {} was spent by its own effect in {} (run.started): it takes no order or nomination until an operator with engagement authority un-spends it",
                    sp.uid,
                    sp.from.as_deref().unwrap_or("an earlier run")
                );
            }
            for (log, why, may_owe) in core.engine.pending_carries() {
                eprintln!(
                    "decision: {} can't be carried over ({why}): what it still owed is unknown and stays owed until a start reads it or an operator writes it off; {}",
                    carry_name(&log),
                    may_owe_text(&may_owe)
                );
            }
        }
        if let Some(pointer) = pointer {
            match write_pointer(pointer, &self.audit_path) {
                Ok(None) => {}
                // rules 15: renamed, its directory not synced: a warning, not a failed write
                Ok(Some(e)) => pointer_unsynced(pointer, &e),
                Err(e) => {
                    let why = format!(
                        "{} can't be written ({e}): the next run won't find this one's log to carry over from",
                        pointer.display()
                    );
                    eprintln!("decision: {why}");
                    self.lock().carry_problem = Some((why, crate::clock::wall()));
                }
            }
        }
    }

    /// Rules 12 (murmur-q05, murmur-cq3): `POST /api/restores/write_off`: an operator writes
    /// off, under their own session and saying why, the lost-link values owed back to a
    /// vehicle (`uid`: lost, spent, another vehicle under the uid) or an earlier log whose
    /// carry failed (`log`: gone for good). Logged as `restore.closed`; the vehicle's restore
    /// stops, its missions no longer wait for it, nothing is carried to the next run.
    pub fn write_off(
        &self,
        by: &super::Actor,
        uid: Option<crate::vehicle::Uid>,
        log: Option<String>,
        note: &str,
    ) -> Result<Value, DecisionError> {
        let what = match (uid, log) {
            (Some(u), None) => murmur_decide::WriteOff::Platform(duid(u)),
            (None, Some(l)) => murmur_decide::WriteOff::Log(l),
            _ => return Err(DecisionError::new(422, "write off one vehicle's values (uid) or one earlier log (log)")),
        };
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            core.engine
                .write_off_restore(session.authority(), what.clone(), note)
                .map_err(engine_err)?;
            self.drop_closed_restores(core);
            json!({ "written_off": match &what {
                murmur_decide::WriteOff::Platform(u) => json!({ "uid": u }),
                murmur_decide::WriteOff::Log(l) => json!({ "log": l }),
            } })
        };
        self.publish();
        Ok(out)
    }

    /// Rules 15: whether `by` may un-spend a platform at all (engagement authority): the API
    /// answers 403 before it asks for a credential.
    pub fn may_unspend(&self, by: &super::Actor) -> Result<(), DecisionError> {
        let mut guard = self.lock();
        let core = &mut *guard;
        let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
        if session.engagement_authority().is_none() {
            return Err(DecisionError::new(
                403,
                format!("{by} has no engagement authority: only an operator with it un-spends a platform"),
            ));
        }
        Ok(())
    }

    /// Rules 15: what a passkey un-spend of `uid` is for (`/api/auth/step_up {"act": "unspend"}`).
    pub fn unspend_purpose(&self, uid: crate::vehicle::Uid) -> Result<murmur_decide::StepUpFor, DecisionError> {
        self.lock().engine.unspend_purpose(&duid(uid)).map_err(engine_err)
    }

    /// Rules 15 (murmur-imu): `POST /api/restores/unspend`: an operator with engagement authority
    /// un-spends a platform spent by its own effect that they believe survived (a miss), under
    /// their own session, saying why; only once the engagement that spent it ended, the platform
    /// was heard since and is not silent, and the vehicle under the uid is the one spent. In a
    /// credentials run with the step-up `crate::auth` checked (`step`). Logged as
    /// `platform.unspent`; it takes orders and nominations again, and its own lost-link values
    /// are owed back again (the engagement's fallback is still its lost-link action): its lane
    /// puts them back when it is heard, to the vehicle they were preloaded on, and a mission
    /// waits for that restore. 401 without the step-up, 403 without engagement authority, 409
    /// for a platform not spent or one the rules refuse now. Never an agent's (agents have no
    /// route here, and the MCP no such tool).
    pub fn unspend(
        &self,
        by: &super::Actor,
        uid: crate::vehicle::Uid,
        note: &str,
        step: Option<crate::auth::StepUpChecked>,
    ) -> Result<Value, DecisionError> {
        let world = self.snapshot();
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let Some(auth) = session.engagement_authority() else {
                return Err(DecisionError::new(
                    403,
                    format!("{by} has no engagement authority: only an operator with it un-spends a platform"),
                ));
            };
            // refused before a step-up is logged for it when the rules refuse it anyway
            if let Some(why) = core.engine.unspend_refusal(
                &duid(uid),
                world.vehicles.get(&duid(uid)),
                world.identities.get(&duid(uid)),
                world.t,
            ) {
                return Err(DecisionError::new(409, format!("un-spend: vehicle {uid}: {why}")));
            }
            let step = Self::mint_step(&mut core.engine, session, by, step)?;
            core.engine
                .unspend_platform(auth, duid(uid), note, &world, step)
                .map_err(engine_err)?;
            // its own values owed back again: put back when it answers, to the vehicle they
            // were preloaded on
            let owed = core.engine.owed_to(&duid(uid));
            if let Some(o) = &owed {
                println!("vehicle {uid} un-spent by {by}: its own lost-link values are owed back again: restoring");
                let lane = core.params.entry(uid).or_default();
                lane.owed = Some(Owed::carried(o.values.clone(), o.vehicle.clone()));
            }
            json!({ "unspent": { "uid": uid, "owed": owed.map(|o| o.values) } })
        };
        self.publish();
        Ok(out)
    }

    /// Rules 12 and 15: an alert for each platform spent by its own effect that an un-spend would
    /// be taken for now ([`murmur_decide::Engine::unspend_refusal`], as the engine judges it: the
    /// engagement that spent it ended, the platform heard since and not silent, the vehicle the
    /// one spent): it takes no order and no nomination while spent, and still carries the
    /// engagement's lost-link action. If it survived (a miss), an operator with engagement
    /// authority un-spends it, saying why.
    pub(super) fn spent_alerts(&self, core: &Core, now_wall: f64) -> Vec<super::alerts::Alert> {
        if !self.rules.carry_followups() {
            return Vec::new();
        }
        let spent = core.engine.spent_platforms();
        if spent.is_empty() {
            return Vec::new();
        }
        let fleet = self.fleet();
        let now = crate::clock::mono();
        let t = self.clock.now();
        spent
            .into_iter()
            .filter_map(|sp| {
                let uid = bridge_uid(&sp.uid)?;
                let cell = fleet.get(&uid)?;
                let interval = cell.link_snapshot(now).expected_interval_ms;
                let facts = cell.read(|v| super::snapshot::vehicle_facts(v, &self.catalog, now, interval, None));
                if core.engine.unspend_refusal(&sp.uid, Some(&facts), Some(&identity_of(cell)), t).is_some() {
                    return None;
                }
                let heard = facts.heard_t()?;
                let carried = sp.from.as_deref().map(|f| format!(" of {f}, carried over")).unwrap_or_default();
                Some(super::alerts::Alert {
                    id: format!("spent:{uid}"),
                    kind: "spent",
                    severity: super::alerts::Severity::Medium,
                    uid,
                    callsign: super::alerts::callsign(cell),
                    detail: format!(
                        "spent by its own effect (engagement {}{carried}) but still heard since that engagement ended: it takes no order and no nomination, and still carries that engagement's lost-link action. If it survived (a miss), an operator with engagement authority un-spends it, saying why: its own lost-link values are then owed back (POST /api/restores/unspend {{\"uid\": {uid}, \"note\": ...}})",
                        sp.by
                    ),
                    since_t: (heard.0 as f64 / 1000.0).min(now_wall),
                    op_id: None,
                    intent_id: sp.from.is_none().then_some(sp.by.0),
                })
            })
            .collect()
    }

    /// Rules 6, at start: the platforms' own lost-link values this station's last run still
    /// owed back when it ended (derived from its audit log, which `pointer` names), logged as
    /// carried (`platform.restore`) and put back like any other once the platform answers.
    /// Then `pointer` names this run's log. A missing pointer or log is a first start. Rules
    /// before 12 only: from rules 12 the carry is in `run.started` ([`read_carry`]).
    pub(super) fn carry_restores(&self, pointer: &std::path::Path) {
        let fail = |why: String| {
            eprintln!("decision: {why}");
            self.lock().carry_problem = Some((why, crate::clock::wall()));
        };
        match std::fs::read_to_string(pointer) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("no earlier run named by {}: nothing carried over", pointer.display());
            }
            Err(e) => fail(format!(
                "{} can't be read ({e}): lost-link values the last run on this port still owed can't be carried over",
                pointer.display()
            )),
            Ok(named) => {
                let prev = std::path::PathBuf::from(named.trim());
                match std::fs::read_to_string(&prev) {
                    Err(e) => fail(format!(
                        "the last run's log {} can't be read ({e}): lost-link values it still owed can't be carried over",
                        prev.display()
                    )),
                    Ok(text) => {
                        // a reused run id: the same file, this run appended after its own
                        // run.started; carry from what came before it
                        let text = if prev == self.audit_path {
                            let lines: Vec<&str> = text.lines().collect();
                            let start = lines
                                .iter()
                                .rposition(|l| l.contains("\"type\":\"run.started\""))
                                .unwrap_or(lines.len());
                            lines[..start].join("\n")
                        } else {
                            text
                        };
                        let file = prev
                            .file_name()
                            .map_or_else(|| prev.display().to_string(), |f| f.to_string_lossy().into_owned());
                        println!("carrying over from {file} (named by {})", pointer.display());
                        self.carry_from(&file, &text);
                    }
                }
            }
        }
        match write_pointer(pointer, &self.audit_path) {
            Ok(None) => {}
            Ok(Some(e)) => pointer_unsynced(pointer, &e),
            Err(e) => fail(format!(
                "{} can't be written ({e}): the next run won't find this one's log to carry over from",
                pointer.display()
            )),
        }
    }

    /// The values `text` (the log of run `file`) still owed, logged as carried and owed back.
    fn carry_from(&self, file: &str, text: &str) {
        let mut guard = self.lock();
        let core = &mut *guard;
        for o in murmur_decide::replay::owed_restores(text) {
            let Some(uid) = bridge_uid(&o.uid) else { continue };
            let by: Vec<String> = o.by.iter().map(ToString::to_string).collect();
            let from = if by.is_empty() {
                format!("{file} (carried over into it)")
            } else {
                format!("{file} (preloaded by {})", by.join(", "))
            };
            let report = murmur_decide::PlatformRestoreReport {
                uid: o.uid.clone(),
                values: o.values.clone(),
                carried: true,
                from: Some(from.clone()),
                readback: BTreeMap::new(),
                error: None,
                vehicle: o.vehicle.clone(),
            };
            match core.engine.record_platform_restore(report) {
                Ok(_) => {
                    println!("lost-link values of vehicle {uid} owed back from {from}: restoring");
                    core.params.insert(uid, ParamLane::carrying(o.values, o.vehicle));
                }
                Err(e) => eprintln!("decision: carry restore of {uid}: {e}"),
            }
        }
    }

    /// Rules 6: an alert of the bridge's start (the last run's log could not be read, so what
    /// it owed could not be carried over; from rules 12, that this run's pointer could not be
    /// written).
    pub(super) fn carry_alert(&self, core: &Core, now_wall: f64) -> Option<super::alerts::Alert> {
        core.carry_problem.as_ref().map(|(why, t)| super::alerts::Alert {
            id: "restore:carry".into(),
            kind: "restore_pending",
            severity: super::alerts::Severity::High,
            uid: 0,
            callsign: None,
            detail: why.clone(),
            since_t: now_wall.min(*t),
            op_id: None,
            intent_id: None,
        })
    }

    /// Rules 12: an alert for each earlier log whose carry failed and nobody wrote off: what it
    /// still owed is unknown; it is tried again at every start until read, or written off. It
    /// names the vehicles it may owe values to (the armed platforms of its fleet, whose
    /// missions wait meanwhile), or says no mission is held when they are unknown.
    pub(super) fn pending_carry_alerts(&self, core: &Core, now_wall: f64) -> Vec<super::alerts::Alert> {
        core.engine
            .pending_carries()
            .into_iter()
            .map(|(log, why, may_owe)| super::alerts::Alert {
                id: format!("restore:carry:{log}"),
                kind: "restore_pending",
                severity: super::alerts::Severity::High,
                uid: 0,
                callsign: None,
                detail: format!(
                    "{} can't be carried over ({why}): the lost-link values it still owed are unknown; {}. It is tried again at every start until read, or an operator writes it off (POST /api/restores/write_off {{\"log\": \"{log}\", \"note\": ...}})",
                    carry_name(&log),
                    may_owe_text(&may_owe)
                ),
                since_t: now_wall,
                op_id: None,
                intent_id: None,
            })
            .collect()
    }

    /// Rules 6: inbox alerts for platforms whose own lost-link values have been due back for
    /// longer than [`RESTORE_ALERT_S`] without a confirmed restore.
    pub(super) fn restore_alerts(&self, core: &Core, now_wall: f64) -> Vec<super::alerts::Alert> {
        let fleet = self.fleet();
        let now_mono = crate::clock::mono();
        core.params
            .iter()
            .filter_map(|(uid, lane)| {
                let owed = lane.owed.as_ref()?;
                let due = owed.due_since?;
                let since = now_wall - (now_mono - due);
                (now_mono - due > RESTORE_ALERT_S).then(|| {
                    let values: Vec<String> = owed.values.iter().map(|(n, v)| format!("{n} {v}")).collect();
                    let mut last = owed
                        .last_error
                        .as_deref()
                        .map(|e| format!("; last attempt: {e}"))
                        .unwrap_or_default();
                    if owed.silent {
                        last.push_str("; its link is silent: put back as soon as it is heard again");
                    }
                    // rules 7: still flying the intercept of an engagement a queued order
                    // superseded (its newest order waits for this restore): on the intercept
                    // path, loitering at the effect point, though nothing can be released
                    if let Some(op) = self.world.tasks.current_ops().into_iter().find(|o| o.uid == *uid)
                        && op.task == crate::tasks::TaskKind::Orbit
                        && !matches!(op.state, OpState::Failed | OpState::Superseded | OpState::Requeued | OpState::Unconfirmed)
                        && let Some(e) = core.engine.intent(IntentId(op.intent_id))
                        && matches!(e.spec().task, Task::Engage { .. })
                        && e.state().is_terminal()
                    {
                        last.push_str(&format!(
                            "; it is still on stopped engagement {}'s intercept path (loitering at its effect point; nothing can be released): a hold or rtl takes it off",
                            e.id()
                        ));
                    }
                    // rules 7: the missions waiting in the queue for it
                    let held: Vec<String> = core
                        .engine
                        .intents()
                        .filter(|i| {
                            i.state() == State::Queued
                                && !matches!(i.spec().task, Task::Hold {} | Task::Rtl {})
                                && i.spec().uids.contains(&duid(*uid))
                                && self.rules.rules_version >= murmur_decide::QUEUE_FOLLOWUPS_SINCE
                        })
                        .map(|i| format!("{} ({})", i.id(), i.spec().kind().as_str()))
                        .collect();
                    if !held.is_empty() {
                        last.push_str(&format!(
                            "; held in the queue until it is back (or their deadline): {}",
                            held.join(", ")
                        ));
                    }
                    super::alerts::Alert {
                        id: format!("restore:{uid}"),
                        kind: "restore_pending",
                        severity: super::alerts::Severity::High,
                        uid: *uid,
                        callsign: fleet.get(uid).and_then(|c| super::alerts::callsign(c)),
                        detail: format!(
                            "its own lost-link failsafe ({}) is not confirmed back: it still carries an ended engagement's lost-link action; {} attempt(s){last}{}",
                            values.join(", "),
                            owed.attempts,
                            if self.rules.followups() {
                                "; an operator writes them off if the vehicle is lost or spent, or another vehicle has its uid now"
                            } else {
                                ""
                            }
                        ),
                        since_t: since,
                        op_id: None,
                        intent_id: owed.owner.map(|i| i.0),
                    }
                })
            })
            .collect()
    }

    /// Command an engagement's recorded fallback (the engine released it with the stop).
    pub(super) fn command_fallback(&self, core: &mut Core, f: FallbackOrder) {
        let fleet = self.fleet();
        let Some(uid) = bridge_uid(&f.uid) else {
            return;
        };
        let here = fleet
            .get(&uid)
            .and_then(|c| c.read(|v| v.pos))
            .map(|(lat, lon)| [lon, lat]);
        // the task is the fallback's (Fallback::task: replay checks the report names it)
        let task = f.fallback.task();
        let params = match f.fallback {
            Fallback::Abort | Fallback::Return => TaskParams::default(),
            Fallback::Loiter => TaskParams {
                center: here,
                radius: Some(self.loiter_radius(&f.uid).max(150.0)),
                ..TaskParams::default()
            },
        };
        let detail = format!("{} commanded ({task}): {}", f.fallback.as_str(), f.reason);
        let req = TaskRequest {
            task: task.into(),
            uids: vec![uid],
            params,
        };
        match TaskOrder::new(req, &fleet) {
            Ok(order) => {
                let tasking = self.world.tasks.dispatch(super::AuthorizedOrder {
                    intent: f.intent,
                    order,
                });
                let run = core
                    .engagements
                    .entry(f.intent)
                    .or_insert_with(|| EngageRun::new(None));
                run.fallback = Some(tasking);
                if let Err(e) = core.engine.record_op_state(f.intent, &f.uid, "fallback", &detail) {
                    eprintln!("decision: fallback op state: {e}");
                }
            }
            Err(e) => {
                let detail = format!("{} could not be commanded: {e}", f.fallback.as_str());
                if let Err(e) = core.engine.record_op_state(f.intent, &f.uid, "fallback", &detail) {
                    eprintln!("decision: fallback op state: {e}");
                }
            }
        }
    }

    /// Engagements in play (pending, approved, under way) on vehicles another order (`id`)
    /// just took: stopped, cause superseded by `id` and its source (the new order stands, so
    /// no fallback is commanded). Only an operator's order gets here on its own: an agent's
    /// order on a vehicle with an engagement in play always waits for a human.
    pub(super) fn stop_superseded_engagements(
        &self,
        core: &mut Core,
        id: IntentId,
        took: &[murmur_decide::Uid],
    ) {
        let hit: Vec<IntentId> = core
            .engine
            .intents()
            .filter(|i| {
                i.id() != id
                    && matches!(
                        i.state(),
                        State::PendingApproval | State::Approved | State::Dispatched
                    )
                    && matches!(i.spec().task, Task::Engage { .. })
                    && i.spec().uids.iter().any(|u| took.contains(u))
            })
            .map(Intent::id)
            .collect();
        for other in hit {
            if let Err(e) = core.engine.supersede_engagement(other, id) {
                eprintln!("decision: supersede {other}: {e}");
            }
            // rules 6: no intercept for it; a set still on the platform's lane reports when it
            // finishes (the lane knows whose it is), and its own values go back once nothing
            // holds the platform
            core.engagements.remove(&other);
        }
    }

    /// Once a second: follow the intercepts and fallbacks (op states to the log; a failed
    /// intercept stops the engagement), then let the engine hold the picture, release
    /// effects and assess, and carry out what it says.
    pub(super) fn step_engagements(&self, core: &mut Core) {
        self.step_params(core);
        let mut failed: Vec<(IntentId, String)> = Vec::new();
        {
            let Core {
                engine,
                engagements,
                ..
            } = core;
            for (id, run) in engagements.iter_mut() {
                let dispatched = engine.intent(*id).map(Intent::state) == Some(State::Dispatched);
                for (tasking, is_intercept) in [(&run.intercept, true), (&run.fallback, false)] {
                    let Some(t) = tasking else { continue };
                    for op in t.snapshot().ops {
                        if run.reported.get(&op.id) == Some(&op.state) {
                            continue;
                        }
                        let state = serde_json::to_value(op.state)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_default();
                        let what = if is_intercept {
                            format!("intercept {state}")
                        } else {
                            format!("fallback {state}")
                        };
                        if let Err(e) = engine.record_op_state(*id, &duid(op.uid), &what, &op.detail) {
                            eprintln!("decision: op state: {e}");
                            continue;
                        }
                        run.reported.insert(op.id, op.state);
                        if is_intercept
                            && dispatched
                            && matches!(op.state, OpState::Failed | OpState::Superseded)
                        {
                            failed.push((*id, format!("the platform's intercept {state}: {}", op.detail)));
                        }
                    }
                }
            }
        }
        for (id, why) in failed {
            match core.engine.stop_engagement_system(id, &why) {
                Ok(Some(f)) => self.command_fallback(core, f),
                Ok(None) => {}
                Err(e) => eprintln!("decision: stop {id}: {e}"),
            }
        }
        let active = core.engine.intents().any(|i| {
            matches!(i.spec().task, Task::Engage { .. })
                && match i.state() {
                    // signatures waiting for the rest are held on their plan too
                    State::PendingApproval => i.engagement().is_some_and(|e| e.plan.is_some()),
                    State::Approved | State::Dispatched | State::Effected => true,
                    _ => false,
                }
        });
        // the world only when an engagement is in play (a snapshot of a large fleet is not free)
        if active {
            let world = self.snapshot();
            // every vehicle event that came in before this picture (the receive loop runs
            // beside the tick: a mode change can land after the tick's own drain) is handled
            // before anything is decided on it: a platform whose failsafe took it out of its
            // intercept is stopped first, never released on. The drain must stay after the
            // snapshot: the snapshot carries no mode, so an event that lands between a drain
            // placed before it and the snapshot would reopen the race (no test pins this order)
            self.log_vehicle_events(core);
            let out = core.engine.step_engagements(&world, &self.terrain.oracle());
            // each engagement on its own: one that failed (an audit write) is reported, the
            // others' orders are still carried out
            for (id, e) in &out.errors {
                eprintln!("decision: engagement {id}: {e}");
            }
            for ev in out.events {
                match ev {
                    murmur_decide::EngagementEvent::Stopped(f) => {
                        self.command_fallback(core, f);
                    }
                    murmur_decide::EngagementEvent::Effect(e) => {
                        self.send_effect(&e, &world);
                    }
                    murmur_decide::EngagementEvent::Voided { .. }
                    | murmur_decide::EngagementEvent::Assessed { .. } => {}
                }
            }
        }
        self.step_params(core);
        self.forget_finished_engagements(core);
    }

    /// Forget the runs of engagements that ended: once their platform's last order (the
    /// intercept, or the fallback) finished, or [`FORGET_AFTER_S`] after the end (an orbit
    /// at the effect point never finishes on its own). Op states are logged until then.
    fn forget_finished_engagements(&self, core: &mut Core) {
        let now = self.clock.now();
        let done: Vec<IntentId> = core
            .engagements
            .iter()
            .filter(|(id, run)| {
                let Some(i) = core.engine.intent(**id) else {
                    return true;
                };
                if !i.state().is_terminal() {
                    return false;
                }
                // rules 6: its preload reported first (or abandoned before it started)
                if run.platform.is_some() && !run.preload_reported {
                    return false;
                }
                let ended = i.history().last().map_or(now, |h| h.t);
                let stale = now.ms_since(ended) > FORGET_AFTER_S * 1000;
                let finished = [&run.intercept, &run.fallback]
                    .into_iter()
                    .flatten()
                    .all(|t| t.snapshot().ops.iter().all(|o| o.state.is_terminal()));
                finished || stale
            })
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            core.engagements.remove(&id);
        }
    }

    /// The abstract effect to the contacts process, on the simulation channel: the point,
    /// the radius, the time, the engaging platform, and (for the evaluation truth log only)
    /// where our other vehicles reported themselves (none for one that never has: the truth
    /// log lists it as unknown, not as clear).
    fn send_effect(&self, e: &EffectOrder, world: &murmur_decide::WorldSnapshot) {
        let ours: Vec<Value> = world
            .vehicles
            .iter()
            .filter(|(uid, _)| **uid != e.uid)
            .map(|(uid, v)| {
                let p = v.position.as_ref().map(|p| p.value);
                json!({ "uid": uid, "lon": p.map(|p| p.lon), "lat": p.map(|p| p.lat) })
            })
            .collect();
        if let Some(t) = self.tracks() {
            t.queue_effect(e, ours);
        }
    }

    /// `POST /api/intents/{id}/abort`: any operator stops an approved or dispatched
    /// engagement before its effect; after dispatch the recorded fallback is commanded.
    pub fn abort(
        &self,
        by: &super::Actor,
        id: u64,
        note: Option<String>,
    ) -> Result<super::Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let note = note.unwrap_or_else(|| "operator abort".into());
            match core.engine.abort_engagement(id, session.authority(), &note) {
                Ok(f) => {
                    if let Some(f) = f {
                        self.command_fallback(core, f);
                    }
                    Ok(self.outcome(core, id))
                }
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// `POST /api/intents/{id}/accept_unknown` (rules 6): an operator with engagement
    /// authority accepts, for a nomination waiting with no signature yet, that one of our
    /// vehicles is lost since the report the check names (`since`, engine ms; null: never
    /// reported). Logged as `engage.accepted` with the vehicle's facts; the next check reads
    /// it, and every signer's picture shows it. 403 without engagement authority. Never
    /// automatic, never an agent's (agents have no route here).
    pub fn accept_unknown(
        &self,
        by: &super::Actor,
        id: u64,
        accept: &AcceptUnknown,
        step: Option<crate::auth::StepUpChecked>,
    ) -> Result<super::Outcome, DecisionError> {
        let id = IntentId(id);
        let world = self.snapshot();
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let Some(auth) = session.engagement_authority() else {
                return Err(DecisionError::new(
                    403,
                    format!("{by} has no engagement authority: only an operator with it accepts a lost vehicle"),
                ));
            };
            let (uid, since) = accept.engine();
            // rules 8: the step-up, when the policy lists accept_unknown
            let step = Self::mint_step(&mut core.engine, session, by, step)?;
            let accepted = match step {
                Some(s) => core.engine.accept_unknown_friendly_stepped(id, auth, s, uid, since, &world),
                None => core.engine.accept_unknown_friendly(id, auth, uid, since, &world),
            };
            match accepted {
                Ok(_) => Ok(self.outcome(core, id)),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// `POST /api/intents/{id}/revoke_unknown` (rules 6): any operator withdraws an
    /// acceptance of a lost vehicle (`engage.acceptance_revoked`); the checks count it again,
    /// which stops an engagement it now blocks.
    pub fn revoke_unknown(
        &self,
        by: &super::Actor,
        id: u64,
        uid: crate::vehicle::Uid,
    ) -> Result<super::Outcome, DecisionError> {
        let id = IntentId(id);
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            match core.engine.revoke_unknown_friendly(id, session.authority(), duid(uid)) {
                Ok(_) => Ok(self.outcome(core, id)),
                Err(e) => Err(self.with_intent(core, id, engine_err(e))),
            }
        };
        self.publish();
        out
    }

    /// For the console: a pending nomination's checks and preview on the world now, and
    /// what changed since it was nominated ("picture changed" warnings), plus who may still
    /// sign it.
    pub(super) fn engage_view(&self, core: &Core, i: &Intent, world: &murmur_decide::WorldSnapshot) -> Option<Value> {
        let Task::Engage { track, .. } = &i.spec().task else {
            return None;
        };
        let p = i.policy();
        let proposer = match &i.spec().source {
            murmur_decide::Source::Human { id } => Some(id.clone()),
            murmur_decide::Source::Agent { .. } => None,
        };
        let signed: Vec<String> = i
            .concurrences()
            .iter()
            .map(|a| a.by.to_string())
            .chain(i.approval().map(|a| a.by.to_string()))
            .collect();
        let record = i.engagement();
        // each current signature, when, and on what effect point
        let signatures: Vec<Value> = record
            .map(|e| {
                e.signed
                    .iter()
                    .map(|s| {
                        // rules 6: the signer also accepted one of its lost vehicles
                        let self_accepted = i
                            .concurrences()
                            .iter()
                            .chain(i.approval())
                            .any(|a| a.by == s.by && a.self_accepted);
                        json!({
                            "by": s.by,
                            "t": s.t,
                            "effect_point": s.snapshot.preview.effect_point,
                            "effect_t": s.snapshot.preview.effect_t,
                            "self_accepted": self_accepted,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let reset = record.and_then(|e| e.reset.as_ref()).map(|r| {
            json!({ "t": r.t, "dropped": r.dropped, "reason": r.reason })
        });
        // rules 6: the lost vehicles accepted for it, shown to every signer
        let accepted: Vec<Value> = record
            .map(|e| {
                e.accepted
                    .iter()
                    .map(|(uid, a)| {
                        json!({
                            "uid": bridge_uid(uid),
                            "engine_uid": uid,
                            "since": a.since,
                            "by": a.by,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let eligible: Vec<String> = self
            .engage_operators
            .iter()
            .filter(|o| Some(*o) != proposer.as_ref() && !signed.contains(&o.to_string()))
            .map(ToString::to_string)
            .collect();
        let mut v = json!({
            "track": track,
            "approvers_needed": p.map_or(1, |p| p.approvers),
            "signed": signed,
            "proposer": proposer,
            "proposer_excluded": p.is_some_and(|p| p.proposer_excluded),
            "eligible": eligible,
            "engage_operators": self.engage_operators,
            "signatures": signatures,
            "reset": reset,
            "accepted": accepted,
            "token_ttl_s": self.rules.engage_rules().token_ttl(),
            "max_signing_s": self.rules.engage_rules().max_signing_s,
            // the plan the first signature froze: every later signer signs it exactly
            "plan": record.and_then(|e| e.plan.as_ref()).map(|p| json!({
                "by": p.by,
                "t": p.t,
                "effect_point": p.preview.effect_point,
                "effect_t": p.preview.effect_t,
                "effect_radius_m": p.preview.effect_radius_m,
                "track": p.track,
            })),
        });
        // rules 9: the track as decisions read it: its declaration and the basis of it (an
        // operator's, or the hostile-act rule's with its evidence) from the engine's ledger
        let filled = core.engine.identified(world);
        let now_track: Option<&TrackFacts> = filled.tracks.get(track);
        if let Some(t) = now_track {
            let age = world.t.ms_since(t.last_seen_t) as f64 / 1000.0;
            v["track_now"] = json!({
                "position": engage::track_at(t, world.t),
                "pos_sigma_m": t.pos_sigma_m,
                "age_s": age,
                "affiliation": t.affiliation.value,
                "declared_by": t.declared_by,
                "declaration": t.declaration,
                "basis": t.declaration.as_ref().map(|d| d.describe()),
                "review_flags": t.review_flags,
                "identity": t.identity,
                "identity_confidence": t.identity_confidence,
            });
        } else {
            v["track_now"] = Value::Null;
        }
        if i.state() == State::PendingApproval
            && let Some((checks, pv)) = core.engine.engage_live(i.id(), world, &self.terrain.oracle())
        {
            let changed = changes(i.checks(), &checks);
            // how far the track is now from where the nomination's preview put it now
            let drift = match (i.engagement().and_then(|e| e.preview.as_ref()), now_track) {
                (Some(nom), Some(t)) => {
                    let then = nom.track_path.first().copied();
                    then.map(|p0| {
                        let dt = world.t.ms_since(nom.t) as f64 / 1000.0;
                        let [e, n] = nom
                            .track_path
                            .get(1)
                            .map_or([0.0, 0.0], |p1| {
                                let (de, dn) = engage::enu(p0, *p1);
                                let span = nom.effect_t.ms_since(nom.t) as f64 / 1000.0;
                                if span > 0.0 { [de / span, dn / span] } else { [0.0, 0.0] }
                            });
                        let expected = engage::offset(p0, e * dt, n * dt);
                        murmur_decide::geo::haversine_m(expected, engage::track_at(t, world.t))
                    })
                }
                _ => None,
            };
            v["live"] = json!({
                "t": world.t,
                "passed": murmur_decide::rules::passed(&checks, murmur_decide::TaskKind::Engage),
                "checks": checks,
                "preview": pv,
                "changed": changed,
                "track_drift_m": drift,
            });
        }
        Some(v)
    }
}

/// `POST /api/engage/options`: which armed platforms could take a nomination now.
#[derive(Debug, Clone, Deserialize)]
pub struct OptionsRequest {
    pub track: String,
    pub effect_radius_m: f64,
    #[serde(default)]
    pub window_s: Option<u32>,
    #[serde(default)]
    pub fallback: Option<String>,
    /// Only these vehicles (default: every armed vehicle in the fleet).
    #[serde(default)]
    pub uids: Option<Vec<crate::vehicle::Uid>>,
}

impl Decision {
    /// A dry run for the nomination form (read-only, not a decision, not logged): for each
    /// armed vehicle, the checks a nomination with it would get on the world now, and its
    /// preview. Listed by uid, with no ranking and no recommendation: the operator picks.
    /// Murmur-12e: each option also carries `keep_outs`, the keep-out explanation on the
    /// same inputs (`murmur_decide::keepout`: which track or vehicle of ours blocks or nearly
    /// blocks the keep-out, why, by how much, and what would clear it), read-only like the
    /// rest; `keep_outs_none` says why there is none (no preview, say).
    pub async fn engage_options(&self, req: OptionsRequest) -> Result<Value, DecisionError> {
        let fleet = self.fleet();
        let armed: Vec<crate::vehicle::Uid> = fleet
            .iter()
            .filter(|(u, _)| req.uids.as_ref().is_none_or(|l| l.contains(u)))
            .filter(|(_, c)| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("platform"))
                        .and_then(|p| self.catalog.get(p))
                        .is_some_and(|p| p.armed)
                })
            })
            .map(|(u, _)| *u)
            .collect();
        let fallback: Fallback = req
            .fallback
            .as_deref()
            .and_then(|f| serde_json::from_value(json!(f)).ok())
            .unwrap_or(Fallback::Return);
        let now = self.clock.now();
        let window = EngageWindow {
            from: None,
            until: now.plus_ms(
                i64::from(
                    req.window_s
                        .unwrap_or(self.rules.engage_rules().default_window_s),
                ) * 1000,
            ),
        };
        // murmur-12e: the explanation's words name our vehicles by callsign, and age the
        // picture with this run's tracker memories
        let names: BTreeMap<murmur_decide::Uid, String> = fleet
            .iter()
            .filter_map(|(u, c)| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("callsign"))
                        .map(|s| (duid(*u), s.to_string()))
                })
            })
            .collect();
        let opts = Arc::new(murmur_decide::keepout::Options {
            memory: self.tracks.as_ref().map_or_else(Default::default, |t| t.memory()),
            names,
        });
        let mut out = Vec::new();
        for uid in armed {
            let spec = murmur_decide::IntentSpec {
                id: IntentId(0),
                created_t: now,
                source: murmur_decide::Source::Human {
                    id: murmur_decide::OperatorId::new("dry-run"),
                },
                task: Task::Engage {
                    track: req.track.clone(),
                    effect_radius_m: req.effect_radius_m,
                    window: Some(window),
                    fallback,
                },
                uids: vec![duid(uid)],
                constraints: murmur_decide::Constraints::default(),
                rationale: None,
            };
            self.prefetch(&spec.task, &spec.uids).await;
            // the inputs a decision on it would read now, as the engine fills them (rules 9
            // identification; the engagements in play and, rules 12, the spent platforms),
            // nothing logged
            let inputs = {
                let w = self.snapshot();
                self.lock().engine.dry_run_inputs(&spec, &w, &self.terrain.oracle())
            };
            let checks = murmur_decide::rules::evaluate(&spec, &inputs, &self.rules);
            let passed = murmur_decide::rules::passed(&checks, murmur_decide::TaskKind::Engage);
            let preview = engage::preview(&spec, &inputs, &self.rules).ok();
            // murmur-12e: why the keep-outs block, on the same inputs, off the async runtime
            // (it scans effect and nomination times); shown only when its checks are the ones
            // just run (they are: the same functions on the same inputs)
            let (rules, o) = (self.rules.clone(), opts.clone());
            let explained =
                tokio::task::spawn_blocking(move || murmur_decide::keepout::explain(&spec, &inputs, &rules, &o)).await;
            let (keep_outs, keep_outs_none) = match explained {
                Ok(Ok(x)) if x.checks.iter().all(|c| checks.contains(c)) => (serde_json::to_value(&x).ok(), None),
                Ok(Ok(_)) => (None, Some("the explanation does not match the checks: not shown".to_string())),
                Ok(Err(e)) => (None, Some(e)),
                Err(e) => (None, Some(format!("the explanation failed: {e}"))),
            };
            let callsign = fleet.get(&uid).and_then(|c| {
                c.read(|v| {
                    v.identity
                        .as_ref()
                        .and_then(|i| i.str("callsign"))
                        .map(str::to_string)
                })
            });
            out.push(json!({
                "uid": uid,
                "callsign": callsign,
                "passed": passed,
                "checks": checks,
                "preview": preview,
                "keep_outs": keep_outs,
                "keep_outs_none": keep_outs_none,
            }));
        }
        // by uid (the fleet's order): no "best" first
        Ok(json!({
            "t": now,
            "track": req.track,
            "options": out,
            "note": "listed by uid; not a ranking or a recommendation. keep_outs explains each \
                     keep-out that blocks or nearly blocks (read-only: the checks decide, and \
                     nothing here relaxes one)",
        }))
    }

    /// Every engagement intent, newest first, as the console and agents see it (the intent
    /// with its engagement record, and the signatures and live picture).
    pub fn nominations_json(&self) -> Vec<Value> {
        let world = self.snapshot();
        let core = self.lock();
        let all: Vec<&Intent> = core
            .engine
            .intents()
            .filter(|i| matches!(i.spec().task, Task::Engage { .. }))
            .collect();
        all.into_iter()
            .rev()
            .map(|i| {
                let mut v = super::intent_json(i, core.taskings.get(&i.id()).map(|t| t.id), None);
                if let Some(e) = self.engage_view(&core, i, &world) {
                    v["engage"] = e;
                }
                v
            })
            .collect()
    }
}

/// The checks whose outcome is not what it was at nomination.
fn changes(then: &[CheckResult], now: &[CheckResult]) -> Vec<Value> {
    now.iter()
        .filter_map(|c| {
            let before = then
                .iter()
                .find(|b| b.rule == c.rule && b.uid == c.uid)
                .map(|b| b.outcome);
            (before != Some(c.outcome)).then(|| {
                json!({
                    "rule": c.rule.as_str(),
                    "was": before,
                    "now": c.outcome,
                    "detail": c.detail,
                    "worse": c.outcome != CheckOutcome::Pass,
                })
            })
        })
        .collect()
}
