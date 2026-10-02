//! What goes to browsers: the full state once, then 4 times a second only the vehicles that
//! changed (with just their new trail points) and the task list only when it changed.
//!
//! A frame (every vehicle's snapshot, serialized once) is built per tick and published on a
//! watch channel; each WebSocket client diffs it against what that client already has. A slow
//! client skips frames instead of stalling the others, and its next delta is still correct.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::clock::{self, round};
use crate::vehicle::Uid;
use crate::world::World;

pub const PUSH_PERIOD: Duration = Duration::from_millis(250);

type Point = [f64; 3];

pub struct FrameVehicle {
    pub uid: Uid,
    /// the snapshot without its trail, as JSON
    pub snap: String,
    pub trail_count: u64,
    pub trail: Arc<Vec<Point>>,
}

pub struct PushFrame {
    pub t: f64,
    pub vehicles: Vec<FrameVehicle>,
    pub taskings: String,
}

/// Trails change once a second at most; reuse the copy from the last frame until they do.
#[derive(Default)]
pub struct TrailCache(HashMap<Uid, (u64, Arc<Vec<Point>>)>);

pub fn build_frame(world: &World, cache: &mut TrailCache) -> PushFrame {
    let now = clock::mono();
    let mut vehicles = Vec::new();
    for cell in world.vehicles() {
        let link = cell.link_snapshot(now);
        let (snap, trail_count, trail) = cell.read(|v| {
            let trail = match cache.0.get(&cell.uid) {
                Some((count, trail)) if *count == v.trail_count => trail.clone(),
                _ => Arc::new(v.trail.iter().copied().collect()),
            };
            (
                v.snapshot(cell.uid, cell.sysid, link, now),
                v.trail_count,
                trail,
            )
        });
        cache.0.insert(cell.uid, (trail_count, trail.clone()));
        let snap = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
        vehicles.push(FrameVehicle {
            uid: cell.uid,
            snap,
            trail_count,
            trail,
        });
    }
    let taskings = serde_json::to_string(&world.tasks.snapshot()).unwrap_or_else(|_| "[]".into());
    PushFrame {
        t: round(clock::wall(), 2),
        vehicles,
        taskings,
    }
}

fn json<T: serde::Serialize + ?Sized>(x: &T) -> String {
    serde_json::to_string(x).unwrap_or_else(|_| "null".into())
}

/// `{...snapshot, "<key>": <value>}` without re-serializing the snapshot.
fn with_key(snap: &str, key: &str, value: &str) -> String {
    let body = snap.strip_suffix('}').unwrap_or(snap);
    let sep = if body.len() > 1 { "," } else { "" };
    format!("{body}{sep}\"{key}\":{value}}}")
}

/// Trail points appended after the client's last count (capped to what is kept).
fn trail_since(trail: &[Point], trail_count: u64, count: u64) -> &[Point] {
    let new = usize::try_from(trail_count.saturating_sub(count))
        .unwrap_or(usize::MAX)
        .min(trail.len());
    &trail[trail.len() - new..]
}

/// The full state message: every vehicle with its whole trail, and the task list.
pub fn state_json(frame: &PushFrame) -> String {
    let vehicles: Vec<String> = frame
        .vehicles
        .iter()
        .map(|v| with_key(&v.snap, "trail", &json(&*v.trail)))
        .collect();
    format!(
        "{{\"type\":\"state\",\"t\":{},\"vehicles\":[{}],\"taskings\":{}}}",
        json(&frame.t),
        vehicles.join(","),
        frame.taskings
    )
}

/// What one browser already has, so each push sends only what changed.
#[derive(Default)]
pub struct ClientView {
    /// uid -> JSON of the last snapshot sent (without trail)
    sent: HashMap<Uid, String>,
    /// uid -> trail_count already sent
    trail: HashMap<Uid, u64>,
    taskings: Option<String>,
}

impl ClientView {
    /// The full state for a new client; everything in it counts as sent.
    pub fn full_state(frame: &PushFrame) -> (ClientView, String) {
        let mut view = ClientView::default();
        for v in &frame.vehicles {
            view.sent.insert(v.uid, v.snap.clone());
            view.trail.insert(v.uid, v.trail_count);
        }
        view.taskings = Some(frame.taskings.clone());
        (view, state_json(frame))
    }

    pub fn delta(&mut self, frame: &PushFrame) -> String {
        let mut changed = Vec::new();
        for v in &frame.vehicles {
            let add = trail_since(
                &v.trail,
                v.trail_count,
                self.trail.get(&v.uid).copied().unwrap_or(0),
            );
            if self.sent.get(&v.uid) == Some(&v.snap) && add.is_empty() {
                continue;
            }
            changed.push(with_key(&v.snap, "trail_add", &json(add)));
            self.sent.insert(v.uid, v.snap.clone());
            self.trail.insert(v.uid, v.trail_count);
        }
        let mut msg = format!(
            "{{\"type\":\"delta\",\"t\":{},\"vehicles\":[{}]",
            json(&frame.t),
            changed.join(",")
        );
        if self.taskings.as_deref() != Some(frame.taskings.as_str()) {
            msg.push_str(",\"taskings\":");
            msg.push_str(&frame.taskings);
            self.taskings = Some(frame.taskings.clone());
        }
        msg.push('}');
        msg
    }
}

/// Build a frame every tick while anyone is watching.
pub async fn pusher(world: Arc<World>, frames: Arc<watch::Sender<Option<Arc<PushFrame>>>>) {
    let mut cache = TrailCache::default();
    let mut interval = tokio::time::interval(PUSH_PERIOD);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if frames.receiver_count() > 0 {
            frames.send_replace(Some(Arc::new(build_frame(&world, &mut cache))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn frame(t: f64, vehicles: &[(Uid, &str, u64, Vec<Point>)], taskings: &str) -> PushFrame {
        PushFrame {
            t,
            vehicles: vehicles
                .iter()
                .map(|(uid, snap, count, trail)| FrameVehicle {
                    uid: *uid,
                    snap: (*snap).to_string(),
                    trail_count: *count,
                    trail: Arc::new(trail.clone()),
                })
                .collect(),
            taskings: taskings.into(),
        }
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("valid json")
    }

    #[test]
    fn full_then_changes_only() {
        let p = |i: f64| [i, i, 0.0];
        let f0 = frame(
            1.0,
            &[
                (1, r#"{"uid":1,"mode":"AUTO"}"#, 2, vec![p(1.0), p(2.0)]),
                (2, r#"{"uid":2}"#, 0, vec![]),
            ],
            "[]",
        );
        let (mut view, full) = ClientView::full_state(&f0);
        let full = parse(&full);
        assert_eq!(full["type"], "state");
        assert_eq!(
            full["vehicles"][0]["trail"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(full["vehicles"][0]["mode"], "AUTO");

        // nothing changed: an empty delta, no taskings key
        let d = parse(&view.delta(&f0));
        assert_eq!(d["type"], "delta");
        assert_eq!(d["vehicles"].as_array().map(Vec::len), Some(0));
        assert!(d.get("taskings").is_none());

        // vehicle 1 changed mode; vehicle 2 got its first trail point; taskings changed
        let f1 = frame(
            1.25,
            &[
                (1, r#"{"uid":1,"mode":"RTL"}"#, 2, vec![p(1.0), p(2.0)]),
                (2, r#"{"uid":2}"#, 1, vec![p(9.0)]),
            ],
            r#"[{"id":1}]"#,
        );
        let d = parse(&view.delta(&f1));
        let vs = d["vehicles"].as_array().expect("array");
        assert_eq!(vs.len(), 2);
        assert_eq!(vs[0]["mode"], "RTL");
        assert_eq!(vs[0]["trail_add"], serde_json::json!([]));
        assert_eq!(vs[1]["trail_add"], serde_json::json!([[9.0, 9.0, 0.0]]));
        assert_eq!(d["taskings"][0]["id"], 1);
        assert_eq!(
            parse(&view.delta(&f1))["vehicles"].as_array().map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn trail_increments_survive_skipped_frames_and_the_cap() {
        let pts: Vec<Point> = (0..120).map(|i| [f64::from(i), 0.0, 0.0]).collect();
        let f0 = frame(0.0, &[(5, r#"{"uid":5}"#, 200, pts.clone())], "[]");
        let (mut view, _) = ClientView::full_state(&f0);
        // the client missed frames: 3 new points since, all of them arrive
        let mut later = pts[3..].to_vec();
        later.extend([[120.0, 0.0, 0.0], [121.0, 0.0, 0.0], [122.0, 0.0, 0.0]]);
        let d = parse(&view.delta(&frame(
            1.0,
            &[(5, r#"{"uid":5}"#, 203, later.clone())],
            "[]",
        )));
        assert_eq!(
            d["vehicles"][0]["trail_add"].as_array().map(Vec::len),
            Some(3)
        );
        assert_eq!(d["vehicles"][0]["trail_add"][0][0], 120.0);
        // far behind (more new points than kept): capped at what the bridge keeps
        let d = parse(&view.delta(&frame(2.0, &[(5, r#"{"uid":5}"#, 900, later)], "[]")));
        assert_eq!(
            d["vehicles"][0]["trail_add"].as_array().map(Vec::len),
            Some(120)
        );
    }

    #[test]
    fn new_vehicle_appears_in_delta_with_its_trail() {
        let f0 = frame(0.0, &[], "[]");
        let (mut view, full) = ClientView::full_state(&f0);
        assert_eq!(parse(&full)["vehicles"], serde_json::json!([]));
        let d = parse(&view.delta(&frame(
            0.25,
            &[(7, r#"{"uid":7}"#, 1, vec![[1.0, 2.0, 3.0]])],
            "[]",
        )));
        assert_eq!(d["vehicles"][0]["uid"], 7);
        assert_eq!(
            d["vehicles"][0]["trail_add"],
            serde_json::json!([[1.0, 2.0, 3.0]])
        );
    }
}
