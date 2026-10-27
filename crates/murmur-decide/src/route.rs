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
//!
//! Rules 17 ([`crate::rules::ROUTE_SURFACE_SINCE`]): the domain rule judges land or water at
//! points along every straight leg of a route ([`Route::legs`], [`along`]): every
//! [`LEG_STEP_M`] from the leg's far end back to its start, the ends themselves excluded (a
//! waypoint is answered exactly, as before; the vehicle is where it is; home is where it came
//! from).

use crate::geo::{LonLat, TaskPath, haversine_m};
use crate::intent::{IntentSpec, Task};
use crate::rules::{RulesConfig, fresh, required_surface};
use crate::world::{VehicleFacts, WorldSnapshot};

/// Rules 17: how far apart the domain rule judges the points along a leg, m (the spacing of
/// the baked land/water grid, eval/headless/bake_world.py).
pub const LEG_STEP_M: f64 = 20.0;

/// Rules 17: a leg from the vehicle may start on the wrong surface for this far, and a leg to
/// home may end on it for this far, m: a pier or a slipway, or a grid edge that reads the
/// vehicle's own berth ashore. Only while it has not yet reached (or no longer meets) the
/// right one: a crossing farther along still counts.
pub const LEG_ENDS_M: f64 = 40.0;

/// Rules 17: the farthest from a point along a leg that a source may answer it from, m (the
/// bridge's tidewater cache holds the points fetched just before the decision, and the vehicle
/// may have moved since; the baked grid and tests answer every point itself).
pub const LEG_NEAR_M: f64 = 25.0;

/// Rules 17: a leg longer than this is not sampled, m (the rule cannot evaluate it): no boat,
/// sub or ground vehicle here is sent so far in one leg, and the log carries a letter a point.
pub const LEG_MAX_M: f64 = 200_000.0;

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

/// Rules 17: one straight leg of a route as the domain rule judges it.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteLeg {
    /// `"transit leg (position to waypoint 1)"`, `"task path leg 3-4"`, `"return leg (waypoint 1
    /// to home)"`.
    pub name: String,
    pub from: LonLat,
    pub to: LonLat,
    /// Starts at the vehicle's position: it may leave the wrong surface it stands on
    /// ([`LEG_ENDS_M`]).
    pub departs: bool,
    /// Ends at home: it may arrive on the wrong surface there ([`LEG_ENDS_M`]).
    pub arrives_home: bool,
}

/// Rules 17: the points along a straight leg the domain rule judges, in travel order, each with
/// how far along the leg it is (m): every [`LEG_STEP_M`] from `to` back toward `from`, both ends
/// excluded, on the straight line in longitude and latitude ([`crate::geo`]). Measured from the
/// end that stays put (a waypoint, an effect point, home), so a vehicle that moved a little since
/// the points were fetched finds each one near where it was. `None` for a leg longer than
/// [`LEG_MAX_M`].
pub fn along(from: LonLat, to: LonLat) -> Option<Vec<(LonLat, f64)>> {
    let len = haversine_m(from, to);
    if len.is_nan() || len > LEG_MAX_M {
        return None;
    }
    let mut out = Vec::new();
    let mut k = 1u32;
    loop {
        let back = f64::from(k) * LEG_STEP_M;
        if back >= len {
            break;
        }
        let f = back / len;
        out.push((
            LonLat::new(to.lon + (from.lon - to.lon) * f, to.lat + (from.lat - to.lat) * f),
            len - back,
        ));
        k += 1;
    }
    out.reverse();
    Some(out)
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

    /// Rules 17: every straight leg in travel order: the transit, the task path's legs (the
    /// closing one of a loop included), the return.
    pub fn legs(&self) -> Vec<RouteLeg> {
        let seg = |s: &Segment| RouteLeg {
            name: s.name(),
            from: s.from,
            to: s.to,
            departs: s.from_vehicle,
            arrives_home: s.kind == LegKind::Return,
        };
        let mut out: Vec<RouteLeg> = self.transit.iter().map(seg).collect();
        if let Some(path) = &self.task {
            let n = path.points.len();
            for (i, a, b) in path.legs() {
                out.push(RouteLeg {
                    name: format!("{} leg {}-{}", LegKind::Task.as_str(), i + 1, (i + 1) % n + 1),
                    from: a,
                    to: b,
                    departs: false,
                    arrives_home: false,
                });
            }
        }
        out.extend(self.ret.iter().map(seg));
        out
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
    // rules 21: an identify order goes where its orbit is laid out around the track
    let orbit = crate::identify::orbit_view(spec, world, cfg);
    let spec = orbit.as_ref();
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

/// Rules 17: the legs the domain rule judges for `spec` on `world`: those of each vehicle that
/// must keep to water or land ([`required_surface`]) whose route can be worked out, each once
/// (vehicles share a task path's legs), in the intent's uid order and travel order. A vehicle
/// whose route can't be worked out has none here; the rule says why.
pub fn surface_legs(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<RouteLeg> {
    let mut out: Vec<RouteLeg> = Vec::new();
    for uid in &spec.uids {
        let Some(v) = world.vehicles.get(uid) else { continue };
        let needs = v
            .platform
            .as_ref()
            .is_some_and(|p| required_surface(p.domain, p.autopilot).is_some());
        if !needs {
            continue;
        }
        let Ok(route) = route_for(spec, v, world, cfg) else { continue };
        for leg in route.legs() {
            if !out.iter().any(|l| l.from.e7() == leg.from.e7() && l.to.e7() == leg.to.e7()) {
                out.push(leg);
            }
        }
    }
    out
}

/// Rules 17: every point along the legs [`surface_legs`] gives (what the bridge fetches before a
/// decision; the rule judges exactly these). Empty under older rules.
pub fn along_points(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<LonLat> {
    if !cfg.route_surface() {
        return Vec::new();
    }
    surface_legs(spec, world, cfg)
        .iter()
        .filter_map(|l| along(l.from, l.to))
        .flatten()
        .map(|(p, _)| p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_along_a_leg_every_step_from_its_far_end() {
        let a = LonLat::new(-122.47, 37.82);
        let b = crate::engage::offset(a, 0.0, 105.0);
        let pts = along(a, b).expect("short leg");
        // 100, 80, 60, 40, 20 m back from b: 5, 25, 45, 65, 85 m along from a
        assert_eq!(pts.len(), 5);
        for ((p, d), want) in pts.iter().zip([5.0, 25.0, 45.0, 65.0, 85.0]) {
            assert!((d - want).abs() < 0.5, "{d} vs {want}");
            assert!((haversine_m(a, *p) - want).abs() < 0.5);
        }
        // the ends are not among them; a leg shorter than a step has none
        assert!(along(a, crate::engage::offset(a, 0.0, 19.0)).expect("short").is_empty());
        assert!(along(a, LonLat::new(10.0, 37.82)).is_none(), "longer than LEG_MAX_M");
    }
}
