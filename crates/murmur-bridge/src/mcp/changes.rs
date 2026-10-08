//! `get_changes`: what changed in the picture since a cursor, for agents that poll (the
//! watch officer, `agents/watch_officer.py`, wakes its model on these). Read-only and not
//! logged. Each read takes a digest of the same views the read tools serve (fleet, tracks,
//! inbox, intents, recent vehicle events), compares it with the digest the previous read
//! took, and numbers each difference into a bounded ring the cursor walks.
//!
//! The bridge keeps no history of an affiliation estimate or of an alert clearing, so a
//! cursor over the timestamps it does keep would miss those: hence the digest. Any agent's
//! read refreshes it (one shared feed, reads serialized), so a change is seen at most as
//! finely as agents read: a condition that came and went between two reads is not
//! reported. The audit log is the record; this is a notice board. Nothing here reaches the
//! engine except through the read-only views.

use std::collections::{BTreeMap, VecDeque};

use murmur_decide::AgentId;
use serde_json::{Value, json};

use super::{Mcp, world};
use crate::clock::{self, round};
use crate::decision::DecisionError;

/// Changes kept for cursors to walk (older ones are dropped; a cursor behind them is told).
pub const RING: usize = 2000;
/// Changes per answer unless the caller asks for fewer (at most [`MAX_LIMIT`]).
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 1000;
/// Vehicle events taken per read (newest first): more in one read than this is a burst
/// whose oldest part `get_recent_events` still has.
const EVENTS_PER_READ: usize = 100;

/// The shared feed (one per bridge).
#[derive(Default)]
pub struct Feed {
    state: tokio::sync::Mutex<FeedState>,
}

#[derive(Default)]
struct FeedState {
    /// The last change's number (0: none yet).
    seq: u64,
    ring: VecDeque<Change>,
    last: Option<Digest>,
}

#[derive(Clone)]
struct Change {
    seq: u64,
    t: f64,
    kind: &'static str,
    /// For an intent: the agent that proposed it (so each caller is told which are its own).
    agent: Option<String>,
    detail: Value,
}

/// What a read compares, per item: only the discrete facts a supervisor acts on (a track's
/// position moving is not a change; its affiliation estimate is).
#[derive(Clone, Default)]
struct Digest {
    vehicles: BTreeMap<u64, Veh>,
    tracks: BTreeMap<String, Trk>,
    /// Dropped tracks every engagement keep-out still counts (`get_tracks` `lost`), by id.
    lost: std::collections::BTreeSet<String>,
    alerts: BTreeMap<String, Value>,
    intents: BTreeMap<u64, Int>,
    /// Review flags by id (`F-<n>`): status, and what to say about a new one.
    flags: BTreeMap<String, (String, Value)>,
    /// The newest vehicle event already reported (wall seconds).
    events_t: f64,
}

#[derive(Clone, PartialEq)]
struct Veh {
    who: Value,
    lost: bool,
    nav: (Option<bool>, Option<String>),
    glitch: Option<bool>,
}

#[derive(Clone, PartialEq)]
struct Trk {
    summary: Value,
    affiliation: String,
    /// (affiliation, by, by rule, confirmed by)
    declared: Option<(String, String, bool, Option<String>)>,
    conflict: Option<String>,
    stale: bool,
}

#[derive(Clone, PartialEq)]
struct Int {
    state: String,
    summary: Value,
    agent: Option<String>,
    reason: Value,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// The picture's digest now, from the read tools' own views.
async fn digest(mcp: &Mcp) -> Result<Digest, DecisionError> {
    let mut d = Digest::default();
    let fleet = world::fleet(mcp, None);
    for v in fleet["vehicles"].as_array().into_iter().flatten() {
        let Some(uid) = v["uid"].as_u64() else {
            continue;
        };
        d.vehicles.insert(
            uid,
            Veh {
                who: json!({
                    "uid": uid,
                    "callsign": v["callsign"],
                    "platform": v["platform"]["id"],
                    "domain": v["platform"]["domain"],
                    "armed": v["platform"]["armed"],
                }),
                lost: v["link"]["lost"].as_bool().unwrap_or(false),
                nav: (v["nav"]["ready"].as_bool(), s(&v["nav"]["source"])),
                glitch: v["nav"]["gps_glitch"]["value"].as_bool(),
            },
        );
    }
    let tracks = world::tracks(mcp, None, None, true).await?;
    for t in tracks["tracks"].as_array().into_iter().flatten() {
        let Some(id) = s(&t["id"]) else { continue };
        let declared = t["declared"].as_object().map(|o| {
            (
                o.get("affiliation")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                o.get("by")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                o.get("rule").and_then(Value::as_bool).unwrap_or(false),
                o.get("confirmed_by")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        });
        let sources: Vec<Value> = t["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| json!({ "uid": s["uid"], "sensor": s["sensor"] }))
            .collect();
        d.tracks.insert(
            id.clone(),
            Trk {
                summary: json!({
                    "track": id,
                    "domain": t["domain"],
                    // the classification (`kind` is the change's own)
                    "identity": t["identity"]["kind"],
                    "affiliation": t["affiliation"],
                    "affiliation_confidence": t["affiliation_confidence"],
                    "position": t["position"],
                    "speed_mps": t["speed_mps"],
                    "course_deg": t["course_deg"],
                    "sources": sources,
                    "as_of": t["as_of"],
                }),
                affiliation: s(&t["affiliation"]).unwrap_or_default(),
                declared,
                conflict: s(&t["identity"]["conflict"]),
                stale: t["stale"].as_bool().unwrap_or(false),
            },
        );
    }
    for l in tracks["lost"].as_array().into_iter().flatten() {
        if let Some(id) = s(&l["id"]) {
            d.lost.insert(id);
        }
    }
    let inbox = world::inbox(mcp);
    for a in inbox["alerts"].as_array().into_iter().flatten() {
        if let Some(id) = s(&a["id"]) {
            d.alerts.insert(
                id.clone(),
                json!({
                    "alert": id,
                    "alert_kind": a["kind"],
                    "severity": a["severity"],
                    "uid": a["uid"],
                    "callsign": a["callsign"],
                    "detail": a["detail"],
                    "intent_id": a["intent_id"],
                }),
            );
        }
    }
    let review = &inbox["review"];
    for f in review["flags"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(review["resolved"].as_array().into_iter().flatten())
    {
        if let Some(id) = s(&f["id"]) {
            // `FlagStatus` is tagged: {"status": "open"}, {"status": "confirmed", "by": ...}
            let status = s(&f["status"]["status"])
                .or_else(|| s(&f["status"]))
                .unwrap_or_default();
            let about = json!({
                "flag": id,
                "flag_kind": f["flag_kind"],
                "tracks": f["tracks"],
                "evidence": f["evidence"],
                "raises_suspect": f["raises_suspect"],
            });
            d.flags.insert(id, (status, about));
        }
    }
    let intents = mcp.decision.intents_json();
    for i in intents["intents"].as_array().into_iter().flatten() {
        let Some(id) = i["id"].as_u64() else { continue };
        let agent = (i["source"]["kind"].as_str() == Some("agent"))
            .then(|| s(&i["source"]["id"]))
            .flatten();
        let last = i["history"].as_array().and_then(|h| h.last());
        d.intents.insert(
            id,
            Int {
                state: s(&i["state"]).unwrap_or_default(),
                summary: json!({
                    "intent": id,
                    "source": i["source"],
                    "task": i["kind"],
                    "uids": i["uids"],
                    "track": i["params"]["track"],
                }),
                agent,
                reason: last.map(|h| h["reason"].clone()).unwrap_or(Value::Null),
            },
        );
    }
    d.events_t = mcp
        .decision
        .recent_events(None, 1)
        .first()
        .map_or(0.0, |e| e.t);
    Ok(d)
}

/// Merge `extra` into the object `base`.
fn with(base: &Value, extra: Value) -> Value {
    let mut v = base.clone();
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in e {
            o.insert(k.clone(), val.clone());
        }
    }
    v
}

/// The differences from `a` to `b`, in a fixed order (intents, tracks, review, vehicles,
/// alerts, vehicle events), each as (kind, proposing agent, detail).
fn diff(mcp: &Mcp, a: &Digest, b: &Digest) -> Vec<(&'static str, Option<String>, Value)> {
    let mut out = Vec::new();
    for (id, i) in &b.intents {
        match a.intents.get(id) {
            None => out.push((
                "intent.new",
                i.agent.clone(),
                with(&i.summary, json!({ "state": i.state, "reason": i.reason })),
            )),
            Some(old) if old.state != i.state => out.push((
                "intent.state",
                i.agent.clone(),
                with(
                    &i.summary,
                    json!({ "from": old.state, "to": i.state, "reason": i.reason }),
                ),
            )),
            _ => {}
        }
    }
    for (id, t) in &b.tracks {
        let old = a.tracks.get(id);
        match old {
            None => out.push(("track.new", None, t.summary.clone())),
            Some(o) if o.affiliation != t.affiliation => out.push((
                "track.affiliation",
                None,
                with(
                    &t.summary,
                    json!({ "from": o.affiliation, "to": t.affiliation }),
                ),
            )),
            _ => {}
        }
        let was_declared = old.and_then(|o| o.declared.as_ref());
        match (was_declared, &t.declared) {
            (None, Some((aff, by, rule, confirmed))) => out.push((
                "track.declared",
                None,
                with(
                    &t.summary,
                    json!({ "declared": aff, "by": by, "rule": rule, "confirmed_by": confirmed }),
                ),
            )),
            (Some(_), None) => {
                out.push(("track.declaration_revoked", None, t.summary.clone()));
            }
            (Some(o), Some(n)) if o.3.is_none() && n.3.is_some() => out.push((
                "track.declaration_confirmed",
                None,
                with(
                    &t.summary,
                    json!({ "declared": n.0, "by": n.1, "confirmed_by": n.3 }),
                ),
            )),
            _ => {}
        }
        if t.conflict.is_some() && old.is_none_or(|o| o.conflict != t.conflict) {
            out.push((
                "track.identity_conflict",
                None,
                with(&t.summary, json!({ "conflict": t.conflict })),
            ));
        }
        if let Some(o) = old
            && o.stale != t.stale
        {
            let kind = if t.stale {
                "track.stale"
            } else {
                "track.active"
            };
            out.push((kind, None, t.summary.clone()));
        }
    }
    for (id, t) in &a.tracks {
        if !b.tracks.contains_key(id) {
            // lost: still counted by every engagement keep-out (get_tracks lists it there)
            out.push((
                "track.dropped",
                None,
                with(&t.summary, json!({ "lost": b.lost.contains(id) })),
            ));
        }
    }
    for (id, (status, about)) in &b.flags {
        match a.flags.get(id) {
            None => out.push((
                "review.flag",
                None,
                with(about, json!({ "status": status })),
            )),
            Some((old, _)) if old != status => out.push((
                "review.flag_status",
                None,
                with(about, json!({ "from": old, "to": status })),
            )),
            _ => {}
        }
    }
    for (uid, v) in &b.vehicles {
        let Some(o) = a.vehicles.get(uid) else {
            out.push(("vehicle.joined", None, v.who.clone()));
            continue;
        };
        if o.lost != v.lost {
            let kind = if v.lost {
                "vehicle.link_lost"
            } else {
                "vehicle.link_restored"
            };
            out.push((kind, None, v.who.clone()));
        }
        if o.nav != v.nav {
            out.push((
                "vehicle.nav",
                None,
                with(
                    &v.who,
                    json!({
                        "from": { "ready": o.nav.0, "source": o.nav.1 },
                        "to": { "ready": v.nav.0, "source": v.nav.1 },
                    }),
                ),
            ));
        }
        if o.glitch != v.glitch && v.glitch.is_some() {
            out.push((
                "vehicle.gps_glitch",
                None,
                with(&v.who, json!({ "glitch": v.glitch })),
            ));
        }
    }
    for (id, al) in &b.alerts {
        if !a.alerts.contains_key(id) {
            out.push(("alert.raised", None, al.clone()));
        }
    }
    for (id, al) in &a.alerts {
        if !b.alerts.contains_key(id) {
            out.push(("alert.cleared", None, al.clone()));
        }
    }
    if b.events_t > a.events_t {
        let mut fresh: Vec<Value> = mcp
            .decision
            .recent_events(None, EVENTS_PER_READ)
            .into_iter()
            .filter(|e| e.t > a.events_t)
            .map(|e| {
                json!({
                    "uid": e.uid, "callsign": e.callsign, "event_kind": e.kind, "text": e.text,
                    "as_of": e.t,
                })
            })
            .collect();
        fresh.reverse(); // oldest first, as the rest
        for e in fresh {
            out.push(("vehicle.event", None, e));
        }
    }
    out
}

impl Feed {
    /// Take a digest, number what differs from the last one, and answer `since`.
    pub async fn read(
        &self,
        mcp: &Mcp,
        agent: &AgentId,
        since: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Value, DecisionError> {
        let mut st = self.state.lock().await; // reads in order: each digest after the last
        let now = clock::wall();
        let d = digest(mcp).await?;
        if let Some(prev) = st.last.take() {
            for (kind, who, detail) in diff(mcp, &prev, &d) {
                st.seq += 1;
                let seq = st.seq;
                if st.ring.len() == RING {
                    st.ring.pop_front();
                }
                st.ring.push_back(Change {
                    seq,
                    t: now,
                    kind,
                    agent: who,
                    detail,
                });
            }
        }
        st.last = Some(d);
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let oldest = st.ring.front().map_or(st.seq + 1, |c| c.seq);
        // a cursor ahead of the feed comes from an earlier bridge: start again from now
        let reset = since.is_some_and(|s| s > st.seq);
        let from = match since {
            Some(s) if !reset => s,
            _ => st.seq,
        };
        let truncated = from + 1 < oldest && since.is_some() && !reset;
        let picked: Vec<&Change> = st
            .ring
            .iter()
            .filter(|c| c.seq > from)
            .take(limit)
            .collect();
        let more = st.ring.iter().filter(|c| c.seq > from).count() > picked.len();
        let cursor = picked.last().map_or(st.seq.max(from), |c| c.seq);
        let changes: Vec<Value> = picked
            .into_iter()
            .map(|c| {
                let mut v = with(
                    &c.detail,
                    json!({ "seq": c.seq, "t": c.t, "age_s": round((now - c.t).max(0.0), 1), "kind": c.kind }),
                );
                if c.kind.starts_with("intent.") {
                    v["mine"] = json!(c.agent.as_deref() == Some(agent.as_str()));
                }
                v
            })
            .collect();
        let run = mcp
            .decision
            .audit_path()
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned());
        Ok(json!({
            "t": now,
            "run": run,
            "since": since,
            "cursor": cursor,
            "reset": reset,
            "truncated": truncated,
            "more": more,
            "count": changes.len(),
            "note": "Changes seen between reads, oldest first: pass cursor back as since to \
                     get the next ones (more: there are more after these). Without since, \
                     or after reset (the bridge restarted), it answers no changes and a \
                     cursor to start from: read the picture with the other tools then. \
                     truncated: older changes were dropped. Any agent's read refreshes the \
                     feed, so a condition that came and went between two reads is not \
                     reported; the audit log is the record. intent changes say mine when \
                     the intent is yours. t and as_of are wall-clock seconds.",
            "changes": changes,
        }))
    }
}
