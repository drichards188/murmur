//! Swarms (Saturation, bd murmur-ji4n; murmur-z07i): groups of vehicles the fleet manifest
//! names, summarised for the console (`GET /api/swarms`) and the AI watch officer (MCP
//! `get_swarms`), and the inbox's swarm alerts, which fold many members' alerts of one kind into
//! one card. Read-only views: nothing here is logged, no decision reads it, and every
//! per-vehicle alert, the audit log and every check stay exactly as they are.
//!
//! The manifest (`sim/run/fleet.json`, written by sim/swarm.py) may carry a top-level
//! `"swarms": [{"id", "name", "role", "color"}]` and on each vehicle `"swarm": "<id>"`. A
//! manifest with neither (every one before Saturation) has no swarms: the identities, the inbox
//! and every other answer are then byte for byte what they were.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::decision::alerts::{Alert, Severity};
use crate::tasks::{OpSnapshot, OpState};
use crate::vehicle::Uid;
use crate::world::World;

/// `/api/swarms` is computed at most this often; requests in between get the cached answer.
pub const SUMMARY_EVERY: Duration = Duration::from_secs(1);
/// A swarm alert needs at least this many members holding an alert of one kind...
pub const FOLD_MIN: usize = 3;
/// ...or this share of the swarm, whichever is larger.
pub const FOLD_SHARE: f64 = 0.05;
/// Role of a swarm the manifest's `swarms` list does not declare (only its members name it).
const UNDECLARED_ROLE: &str = "other";

/// One swarm as the manifest declares it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwarmDef {
    pub id: String,
    pub name: String,
    /// picket | search | relay | surface | undersea | reserve | anything else the launcher says
    pub role: String,
    pub color: Option<String>,
}

/// The manifest's swarms: their definitions (declared ones in the manifest's order, then any a
/// vehicle names without a declaration, in the order first named) and their members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Swarms {
    pub defs: Vec<SwarmDef>,
    /// swarm id -> member uids, ascending
    pub members: BTreeMap<String, Vec<Uid>>,
    /// uid -> swarm id
    pub of: HashMap<Uid, String>,
}

impl Swarms {
    /// From a fleet manifest. Vehicles are keyed as [`crate::world::Manifest::parse`] keys them
    /// (`uid`, else `sysid` for a manifest from before uids).
    pub fn parse(data: &Value) -> Swarms {
        let mut sw = Swarms::default();
        let mut seen = std::collections::HashSet::new();
        for d in data.get("swarms").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = d.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue;
            }
            let s = |k: &str| d.get(k).and_then(Value::as_str).map(str::to_string);
            sw.defs.push(SwarmDef {
                id: id.to_string(),
                name: s("name").unwrap_or_else(|| id.to_string()),
                role: s("role").unwrap_or_else(|| UNDECLARED_ROLE.to_string()),
                color: s("color"),
            });
        }
        for v in data.get("vehicles").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = v.get("swarm").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                continue;
            };
            let Some(uid) = v
                .get("uid")
                .and_then(Value::as_u64)
                .or_else(|| v.get("sysid").and_then(Value::as_u64))
            else {
                continue;
            };
            if seen.insert(id.to_string()) {
                sw.defs.push(SwarmDef {
                    id: id.to_string(),
                    name: id.to_string(),
                    role: UNDECLARED_ROLE.to_string(),
                    color: None,
                });
            }
            sw.members.entry(id.to_string()).or_default().push(uid);
            sw.of.insert(uid, id.to_string());
        }
        for m in sw.members.values_mut() {
            m.sort_unstable();
            m.dedup();
        }
        sw
    }

    /// Whether the manifest has any swarm: only then do identities carry `swarm` and the inbox
    /// `swarm_alerts`.
    pub fn any(&self) -> bool {
        !self.defs.is_empty()
    }

    pub fn count(&self, id: &str) -> usize {
        self.members.get(id).map_or(0, Vec::len)
    }

    fn def(&self, id: &str) -> Option<&SwarmDef> {
        self.defs.iter().find(|d| d.id == id)
    }
}

/// What the summary reads of one vehicle.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Member {
    /// (lon, lat)
    pub pos: Option<(f64, f64)>,
    /// Any packet ever came in.
    pub heard: bool,
    /// Lost link by the inbox's own rule (`decision::alerts`: silent past its stale threshold).
    pub lost: bool,
    pub nav_ready: bool,
    pub battery_pct: Option<i8>,
}

/// Every vehicle's [`Member`] facts now.
pub fn members_now(world: &World) -> HashMap<Uid, Member> {
    let (mono, wall) = (crate::clock::mono(), crate::clock::wall());
    world
        .vehicles()
        .into_iter()
        .map(|cell| {
            let link = cell.link_snapshot(mono);
            let lost = link.last_rx_t.is_some_and(|last| wall - last > link.stale_after_s());
            let m = cell.read(|v| Member {
                pos: v.pos.map(|(lat, lon)| (lon, lat)),
                heard: link.last_rx_t.is_some(),
                lost,
                nav_ready: v.has_position(mono),
                battery_pct: v.battery_pct,
            });
            (cell.uid, m)
        })
        .collect()
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// The convex hull of `points` ([lon, lat]), counter-clockwise, open (the first point is not
/// repeated at the end); one point or two give themselves. Andrew's monotone chain on the plane
/// of lon and lat, which a swarm a few kilometres across does not notice.
pub fn hull(points: &[(f64, f64)]) -> Vec<[f64; 2]> {
    let mut p: Vec<(f64, f64)> = points.iter().copied().filter(|(x, y)| x.is_finite() && y.is_finite()).collect();
    p.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    p.dedup();
    if p.len() < 3 {
        return p.into_iter().map(|(x, y)| [x, y]).collect();
    }
    let cross = |o: (f64, f64), a: (f64, f64), b: (f64, f64)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut h: Vec<(f64, f64)> = Vec::with_capacity(2 * p.len());
    for &q in &p {
        while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
            h.pop();
        }
        h.push(q);
    }
    let lower = h.len() + 1;
    for &q in p.iter().rev().skip(1) {
        while h.len() >= lower && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
            h.pop();
        }
        h.push(q);
    }
    h.pop();
    h.into_iter().map(|(x, y)| [x, y]).collect()
}

/// Whether a member's latest op counts as the task it is on: anything but a failed, superseded
/// or requeued one, or one whose start nothing confirmed.
fn on_task(op: &OpSnapshot) -> bool {
    !matches!(op.state, OpState::Failed | OpState::Superseded | OpState::Requeued | OpState::Unconfirmed)
}

/// Every swarm's summary, in the manifest's order, with its members and hull.
pub fn summary(sw: &Swarms, members: &HashMap<Uid, Member>, ops: &[OpSnapshot], reserve_pct: f64) -> Vec<Value> {
    let op_of: HashMap<Uid, &OpSnapshot> = ops.iter().filter(|o| on_task(o)).map(|o| (o.uid, o)).collect();
    sw.defs
        .iter()
        .map(|d| {
            let uids: &[Uid] = sw.members.get(&d.id).map_or(&[], Vec::as_slice);
            let (mut ready, mut lost, mut low, mut unheard) = (0, 0, 0, 0);
            let mut pts = Vec::new();
            let mut tasks: BTreeMap<(u64, &'static str), usize> = BTreeMap::new();
            for uid in uids {
                if let Some(op) = op_of.get(uid) {
                    *tasks.entry((op.intent_id, op.task.name())).or_default() += 1;
                }
                let Some(m) = members.get(uid) else {
                    unheard += 1;
                    continue;
                };
                if !m.heard {
                    unheard += 1;
                }
                if m.lost {
                    lost += 1;
                } else if m.heard && m.nav_ready {
                    ready += 1;
                }
                if m.battery_pct.is_some_and(|b| b >= 0 && f64::from(b) < reserve_pct) {
                    low += 1;
                }
                pts.extend(m.pos);
            }
            let centroid = (!pts.is_empty()).then(|| {
                let n = pts.len() as f64;
                let (x, y) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
                [round6(x / n), round6(y / n)]
            });
            let hull: Vec<[f64; 2]> = hull(&pts).into_iter().map(|[x, y]| [round6(x), round6(y)]).collect();
            let mut tasks: Vec<((u64, &str), usize)> = tasks.into_iter().collect();
            tasks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            json!({
                "id": d.id,
                "name": d.name,
                "role": d.role,
                "color": d.color,
                "count": uids.len(),
                "members": uids,
                "ready": ready,
                "lost_link": lost,
                "low_battery": low,
                "unheard": unheard,
                "centroid": centroid,
                "hull": hull,
                "tasks": tasks
                    .into_iter()
                    .map(|((intent, kind), count)| json!({ "intent": intent, "kind": kind, "count": count }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// The `/api/swarms` answer, cached for [`SUMMARY_EVERY`] (the lock is held while it is made,
/// so concurrent readers make it once).
#[derive(Default)]
pub struct Cache(Mutex<Option<(Instant, Arc<Value>)>>);

impl Cache {
    pub fn get(&self, make: impl FnOnce() -> Value) -> Arc<Value> {
        let mut c = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = c.as_ref()
            && at.elapsed() < SUMMARY_EVERY
        {
            return v.clone();
        }
        let v = Arc::new(make());
        *c = Some((Instant::now(), v.clone()));
        v
    }
}

/// `GET /api/swarms`: `{"t": <wall ms>, "hull_closed": false, "low_battery_below_pct", "swarms":
/// [...]}`, at most once a second. `low_battery` counts members below the policy's reserve
/// (`[rules] reserve_pct`); `ready` members heard, not lost and navigation-ready; `unheard`
/// members never heard; `tasks` the latest order of each member on one (intent and kind).
pub fn api_json(world: &World, reserve_pct: f64) -> Arc<Value> {
    world.swarm_cache.get(|| {
        crate::perf::time("swarms", || {
            let sw = world.manifest().swarms.clone();
            let list = if sw.any() {
                summary(&sw, &members_now(world), &world.tasks.current_ops(), reserve_pct)
            } else {
                Vec::new()
            };
            json!({
                "t": (crate::clock::wall() * 1000.0).round() as i64,
                "hull_closed": false,
                "low_battery_below_pct": reserve_pct,
                "swarms": list,
            })
        })
    })
}

/// One card standing for many members' alerts of one kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwarmAlert {
    /// `swarm:<swarm id>:<kind>`, stable while the fold lasts.
    pub id: String,
    pub swarm: String,
    pub kind: &'static str,
    /// The most severe of its members' alerts.
    pub severity: Severity,
    /// Members holding an alert of this kind.
    pub count: usize,
    /// Members of the swarm.
    pub of: usize,
    pub uids: Vec<Uid>,
    /// lost_link: members by the link emulator's cause (jam, terrain, range, ...; `unknown`
    /// without one). Other kinds: empty.
    pub causes: BTreeMap<String, usize>,
    /// When the fold's condition began: the time the threshold-th of the members' alerts still
    /// in force started (wall clock, seconds).
    pub since_t: f64,
    pub detail: String,
}

/// Members a swarm of `of` needs holding one kind of alert before they fold.
pub fn fold_threshold(of: usize) -> usize {
    FOLD_MIN.max((of as f64 * FOLD_SHARE).ceil() as usize)
}

/// The swarm alerts for `alerts` and, for each alert in order, the swarm alert it folds into.
/// `cause` names a lost link's cause (the emulator's), if known.
pub fn fold(alerts: &[Alert], sw: &Swarms, cause: &dyn Fn(Uid) -> Option<String>) -> (Vec<SwarmAlert>, Vec<Option<String>>) {
    // (swarm, kind) -> uid -> (earliest since_t, most severe)
    type ByMember = BTreeMap<Uid, (f64, Severity)>;
    let mut groups: BTreeMap<(&str, &'static str), ByMember> = BTreeMap::new();
    for a in alerts {
        let Some(s) = sw.of.get(&a.uid) else { continue };
        let e = groups.entry((s.as_str(), a.kind)).or_default().entry(a.uid).or_insert((a.since_t, a.severity));
        e.0 = e.0.min(a.since_t);
        e.1 = e.1.min(a.severity);
    }
    let mut out = Vec::new();
    for ((swarm, kind), by) in groups {
        let of = sw.count(swarm);
        let need = fold_threshold(of);
        if by.len() < need {
            continue;
        }
        let mut since: Vec<f64> = by.values().map(|v| v.0).collect();
        since.sort_by(f64::total_cmp);
        let uids: Vec<Uid> = by.keys().copied().collect();
        let mut causes = BTreeMap::new();
        if kind == "lost_link" {
            for u in &uids {
                *causes.entry(cause(*u).unwrap_or_else(|| "unknown".into())).or_insert(0) += 1;
            }
        }
        let name = sw.def(swarm).map_or(swarm, |d| d.name.as_str());
        let why = if causes.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                causes.iter().map(|(c, n)| format!("{c} {n}")).collect::<Vec<_>>().join(", ")
            )
        };
        out.push(SwarmAlert {
            id: format!("swarm:{swarm}:{kind}"),
            swarm: swarm.to_string(),
            kind,
            severity: by.values().map(|v| v.1).min().unwrap_or(Severity::Medium),
            count: uids.len(),
            of,
            detail: format!("{} of {of} in {name}: {kind}{why}", uids.len()),
            uids,
            causes,
            since_t: since[need - 1],
        });
    }
    out.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.since_t.total_cmp(&b.since_t)).then(a.id.cmp(&b.id)));
    let folded = alerts
        .iter()
        .map(|a| {
            let s = sw.of.get(&a.uid)?;
            out.iter().find(|f| f.swarm == *s && f.kind == a.kind).map(|f| f.id.clone())
        })
        .collect();
    (out, folded)
}

/// The inbox's alerts with each folded member's `folded_into`, and the swarm alerts; None when
/// the manifest has no swarms (the inbox then is as it was).
pub fn inbox_view(world: &World, alerts: &[Alert]) -> Option<(Vec<Value>, Vec<SwarmAlert>)> {
    let sw = world.manifest().swarms.clone();
    if !sw.any() {
        return None;
    }
    let cells: HashMap<Uid, Arc<crate::vehicle::VehicleCell>> =
        world.vehicles().into_iter().map(|c| (c.uid, c)).collect();
    let mono = crate::clock::mono();
    let cause = |uid: Uid| cells.get(&uid).and_then(|c| c.link_snapshot(mono).emu.and_then(|e| e.cause));
    let (swarm_alerts, folded) = fold(alerts, &sw, &cause);
    let list = alerts
        .iter()
        .zip(folded)
        .map(|(a, f)| {
            let mut v = serde_json::to_value(a).unwrap_or(Value::Null);
            if let Some(f) = f {
                v["folded_into"] = json!(f);
            }
            v
        })
        .collect();
    Some((list, swarm_alerts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskKind;

    fn manifest() -> Value {
        json!({
            "swarms": [
                {"id": "PICKET-1", "name": "Picket 1", "role": "picket", "color": "#ff8800"},
                {"id": "RELAY-1", "name": "Relay 1", "role": "relay"},
                {"id": "PICKET-1", "name": "a duplicate, ignored"},
            ],
            "vehicles": (1..=120).map(|u| {
                let swarm = match u { 1..=100 => json!("PICKET-1"), 101..=110 => json!("RELAY-1"), 111..=112 => json!("LOOSE"), _ => Value::Null };
                json!({"uid": u, "sysid": u % 250, "port": 16000 + u, "swarm": swarm})
            }).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn the_manifest_declares_swarms_and_members_name_them() {
        let sw = Swarms::parse(&manifest());
        assert!(sw.any());
        let ids: Vec<&str> = sw.defs.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["PICKET-1", "RELAY-1", "LOOSE"]);
        assert_eq!(sw.defs[0].name, "Picket 1");
        assert_eq!(sw.defs[0].color.as_deref(), Some("#ff8800"));
        assert_eq!(sw.defs[2], SwarmDef { id: "LOOSE".into(), name: "LOOSE".into(), role: "other".into(), color: None });
        assert_eq!(sw.count("PICKET-1"), 100);
        assert_eq!(sw.count("RELAY-1"), 10);
        assert_eq!(sw.of.get(&105).map(String::as_str), Some("RELAY-1"));
        assert!(!sw.of.contains_key(&120));
        // an old manifest has none
        let old = Swarms::parse(&json!({"vehicles": [{"uid": 1, "sysid": 1, "port": 14560}]}));
        assert!(!old.any() && old.of.is_empty());
    }

    #[test]
    fn the_hull_is_convex_counter_clockwise_and_open() {
        let pts = [(0.0, 0.0), (2.0, 0.0), (1.0, 1.0), (2.0, 2.0), (0.0, 2.0), (1.0, 0.5), (0.0, 0.0)];
        assert_eq!(hull(&pts), vec![[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]);
        assert_eq!(hull(&[(1.0, 1.0)]), vec![[1.0, 1.0]]);
        assert_eq!(hull(&[(1.0, 1.0), (1.0, 1.0), (2.0, 3.0)]), vec![[1.0, 1.0], [2.0, 3.0]]);
        // collinear points: the two ends
        assert_eq!(hull(&[(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)]), vec![[0.0, 0.0], [2.0, 2.0]]);
        assert!(hull(&[]).is_empty());
    }

    fn op(uid: Uid, intent: u64, task: TaskKind, state: OpState) -> OpSnapshot {
        OpSnapshot {
            id: uid * 10,
            tasking: intent,
            intent_id: intent,
            task,
            uid,
            sysid: 1,
            state,
            detail: String::new(),
            progress: None,
            history: Vec::new(),
        }
    }

    #[test]
    fn the_summary_counts_members_and_their_tasks() {
        let sw = Swarms::parse(&manifest());
        let mut members = HashMap::new();
        for u in 101..=110u64 {
            members.insert(u, Member {
                pos: Some((-122.47 + u as f64 * 1e-3, 37.80)),
                heard: true,
                lost: u >= 109,
                nav_ready: u != 101,
                battery_pct: Some(if u <= 102 { 15 } else { 80 }),
            });
        }
        // the rest on one line: the hull's corners are the line's two ends and 110
        members.get_mut(&110).expect("110").pos = Some((-122.36, 37.81));
        members.remove(&108); // never heard
        let ops = vec![
            op(101, 7, TaskKind::Orbit, OpState::Active),
            op(102, 7, TaskKind::Orbit, OpState::Done),
            op(103, 8, TaskKind::Hold, OpState::Acked),
            op(104, 9, TaskKind::Patrol, OpState::Failed),
            op(5, 7, TaskKind::Orbit, OpState::Active), // PICKET-1's
        ];
        let all = summary(&sw, &members, &ops, 20.0);
        let relay = all.iter().find(|s| s["id"] == "RELAY-1").expect("relay");
        assert_eq!(relay["count"], 10);
        assert_eq!(relay["members"], json!((101..=110).collect::<Vec<u64>>()));
        assert_eq!(relay["role"], "relay");
        assert_eq!(relay["color"], Value::Null);
        // 102..107 heard, not lost, ready; 101 not ready; 108 unheard; 109, 110 lost
        assert_eq!(relay["ready"], 6);
        assert_eq!(relay["lost_link"], 2);
        assert_eq!(relay["unheard"], 1);
        assert_eq!(relay["low_battery"], 2);
        assert_eq!(relay["tasks"], json!([
            {"intent": 7, "kind": "orbit", "count": 2},
            {"intent": 8, "kind": "hold", "count": 1},
        ]));
        let hull = relay["hull"].as_array().expect("hull");
        assert_eq!(hull.len(), 3, "{hull:?}");
        assert_eq!(hull[0], json!([-122.369, 37.8]));
        let c = relay["centroid"].as_array().expect("centroid");
        assert!((c[1].as_f64().expect("lat") - 37.801111).abs() < 1e-6, "{c:?}");
        // a swarm none of whose members is heard
        let loose = all.iter().find(|s| s["id"] == "LOOSE").expect("loose");
        assert_eq!(loose["unheard"], 2);
        assert_eq!(loose["centroid"], Value::Null);
        assert_eq!(loose["hull"], json!([]));
    }

    fn alert(uid: Uid, kind: &'static str, severity: Severity, since_t: f64) -> Alert {
        Alert {
            id: format!("{kind}:{uid}"),
            kind,
            severity,
            uid,
            callsign: None,
            detail: String::new(),
            since_t,
            op_id: None,
            intent_id: None,
        }
    }

    #[test]
    fn the_threshold_is_three_or_five_percent() {
        assert_eq!(fold_threshold(2), 3);
        assert_eq!(fold_threshold(10), 3);
        assert_eq!(fold_threshold(60), 3);
        assert_eq!(fold_threshold(61), 4);
        assert_eq!(fold_threshold(100), 5);
        assert_eq!(fold_threshold(101), 6);
    }

    #[test]
    fn many_members_alerts_of_one_kind_fold_into_one() {
        let sw = Swarms::parse(&manifest());
        // PICKET-1 (100): 5 lost links fold (the threshold is 5), 4 not-moving do not;
        // RELAY-1 (10): 3 not-moving fold; 120 is in no swarm
        let mut alerts: Vec<Alert> = (1..=5).map(|u| alert(u, "lost_link", Severity::High, 100.0 + u as f64)).collect();
        alerts.extend((6..=9).map(|u| alert(u, "not_moving", Severity::High, 50.0)));
        alerts.extend((101..=103).map(|u| alert(u, "not_moving", if u == 102 { Severity::High } else { Severity::Medium }, 10.0 * u as f64)));
        alerts.push(alert(120, "lost_link", Severity::High, 1.0));
        let cause = |u: Uid| (u <= 3).then(|| "jam".to_string());
        let (folds, into) = fold(&alerts, &sw, &cause);
        assert_eq!(folds.len(), 2, "{folds:?}");
        let lost = folds.iter().find(|f| f.kind == "lost_link").expect("lost");
        assert_eq!(lost.id, "swarm:PICKET-1:lost_link");
        assert_eq!((lost.count, lost.of), (5, 100));
        assert_eq!(lost.uids, vec![1, 2, 3, 4, 5]);
        assert_eq!(lost.causes, BTreeMap::from([("jam".to_string(), 3), ("unknown".to_string(), 2)]));
        // the fifth member's alert made the fold
        assert_eq!(lost.since_t, 105.0);
        assert_eq!(lost.severity, Severity::High);
        assert!(lost.detail.contains("5 of 100 in Picket 1") && lost.detail.contains("jam 3"), "{}", lost.detail);
        let still = folds.iter().find(|f| f.kind == "not_moving").expect("relay");
        assert_eq!(still.swarm, "RELAY-1");
        assert_eq!(still.severity, Severity::High);
        assert_eq!(still.since_t, 1030.0);
        assert!(still.causes.is_empty());
        // each member alert names its fold; the rest none
        for (a, f) in alerts.iter().zip(&into) {
            let want = match (a.uid, a.kind) {
                (1..=5, "lost_link") => Some("swarm:PICKET-1:lost_link"),
                (101..=103, _) => Some("swarm:RELAY-1:not_moving"),
                _ => None,
            };
            assert_eq!(f.as_deref(), want, "{}", a.id);
        }
        // one member's two alerts of one kind count once
        let mut twice = alerts[..2].to_vec();
        twice.push(alert(1, "lost_link", Severity::High, 0.0));
        let (folds, into) = fold(&twice, &sw, &cause);
        assert!(folds.is_empty() && into.iter().all(Option::is_none));
    }

    #[test]
    fn without_swarms_nothing_folds() {
        let sw = Swarms::default();
        let alerts: Vec<Alert> = (1..=50).map(|u| alert(u, "lost_link", Severity::High, 1.0)).collect();
        let (folds, into) = fold(&alerts, &sw, &|_| None);
        assert!(folds.is_empty());
        assert!(into.iter().all(Option::is_none));
    }
}
