//! The ground a task commits one vehicle to, in three parts: the **transit** leg from where
//! the vehicle is to the task's first waypoint, the **task path** itself, and the
//! **return** leg from where the task ends to home (what RTL flies, in a straight line).
//!
//! | kind          | transit                 | task path | return               |
//! |---------------|-------------------------|-----------|----------------------|
//! | orbit, patrol | position to waypoint 1  | yes       | task end to home     |
//! | rtl           | none                    | none      | position to home     |
//! | hold, track   | none                    | none      | none                 |
//! | engage        | position to effect point| none      | effect point to home |
//!
//! An engagement's effect point is predicted (`crate::engage::preview`), not in its spec.
//!
//! A looped task (orbit, patrol) ends where it closes, at waypoint 1. An RTL commanded
//! mid-loop flies home from wherever the vehicle is; that sweep is not checked (README).

use crate::geo::{LonLat, TaskPath};
use crate::intent::{IntentSpec, Task};
use crate::rules::{RulesConfig, fresh};
use crate::world::{VehicleFacts, WorldSnapshot};

/// Which part of a route a problem is on. Detail strings name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegKind {
    Transit,
    Task,
    Return,
}

impl LegKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LegKind::Transit => "transit leg",
            LegKind::Task => "task path",
            LegKind::Return => "return leg",
        }
    }
}

/// One straight leg between two labelled points.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: LegKind,
    pub from: LonLat,
    pub to: LonLat,
    pub from_label: String,
    pub to_label: String,
    /// Starts at the vehicle's current position (not a point the task chose).
    pub from_vehicle: bool,
}

impl Segment {
    /// `"transit leg (position to waypoint 1)"`.
    pub fn name(&self) -> String {
        format!(
            "{} ({} to {})",
            self.kind.as_str(),
            self.from_label,
            self.to_label
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    pub transit: Option<Segment>,
    pub task: Option<TaskPath>,
    pub ret: Option<Segment>,
}

impl Route {
    pub fn is_empty(&self) -> bool {
        self.transit.is_none() && self.task.is_none() && self.ret.is_none()
    }

    /// The single legs, transit then return.
    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.transit.iter().chain(self.ret.iter())
    }

    /// Which parts exist, for pass details: `"transit, task path, return"`.
    pub fn parts(&self) -> String {
        let mut v = Vec::new();
        if self.transit.is_some() {
            v.push("transit");
        }
        if self.task.is_some() {
            v.push("task path");
        }
        if self.ret.is_some() {
            v.push("return");
        }
        v.join(", ")
    }
}

/// The route `spec` commits vehicle `v` to. Needs a fresh position and a known home for
/// any kind that moves the vehicle; `Err` says why not ("cannot evaluate: ...").
pub fn route_for(
    spec: &IntentSpec,
    v: &VehicleFacts,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> Result<Route, String> {
    let task = spec.task.path();
    let engage = matches!(spec.task, Task::Engage { .. });
    let moves = task.is_some() || engage || matches!(spec.task, Task::Rtl {});
    if !moves {
        return Ok(Route::default());
    }
    let pos = *fresh(&v.position, v, "position", world, cfg)?;
    let home = v
        .home
        .as_ref()
        .ok_or("cannot evaluate: no home position")?
        .value;
    if engage {
        // engage: to the predicted effect point (crate::engage::preview), then home
        let p = crate::engage::preview(spec, world, cfg)
            .map_err(|e| format!("cannot evaluate: no preview ({e})"))?;
        return Ok(Route {
            transit: Some(Segment {
                kind: LegKind::Transit,
                from: pos,
                to: p.effect_point,
                from_label: "position".into(),
                to_label: "effect point".into(),
                from_vehicle: true,
            }),
            task: None,
            ret: Some(Segment {
                kind: LegKind::Return,
                from: p.effect_point,
                to: home,
                from_label: "effect point".into(),
                to_label: "home".into(),
                from_vehicle: false,
            }),
        });
    }
    let home_leg = |from: LonLat, from_label: String, from_vehicle: bool| Segment {
        kind: LegKind::Return,
        from,
        to: home,
        from_label,
        to_label: "home".into(),
        from_vehicle,
    };
    let Some(path) = task else {
        // rtl: straight home from where the vehicle is.
        return Ok(Route {
            ret: Some(home_leg(pos, "position".into(), true)),
            ..Route::default()
        });
    };
    let (Some(first), Some((end_n, end))) = (path.points.first().copied(), path.end()) else {
        return Err("cannot evaluate: task path has no waypoints".into());
    };
    Ok(Route {
        transit: Some(Segment {
            kind: LegKind::Transit,
            from: pos,
            to: first,
            from_label: "position".into(),
            to_label: "waypoint 1".into(),
            from_vehicle: true,
        }),
        ret: Some(home_leg(end, format!("waypoint {end_n}"), false)),
        task: Some(path),
    })
}
