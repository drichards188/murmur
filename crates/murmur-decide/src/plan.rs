//! Rules 22 ([`crate::rules::ROUTE_PLAN_SINCE`], bd murmur-n6a, DECISIONS.md "Route planning
//! before a refusal"): a leg that the domain rule or the geofence would refuse is first routed
//! around what it meets.
//!
//! Which legs: the transit to a task and, for a patrol, each of its legs (the loop back
//! included), of an orbit, a patrol or an identify order (an orbit's ring is never re-planned: a
//! circle around land is no circle). A boat's or sub's leg that crosses anything but water, a
//! ground vehicle's that crosses anything but land, and any vehicle's that crosses a no-go area.
//! The return home, an rtl and an engagement's way stay straight: the first two are what the
//! autopilot flies on its own, the last is a frozen plan's.
//!
//! How: a grid over the leg's box widened by [`MARGIN_M`], cells of [`CELL_M`] (coarser when the
//! box would hold more than [`MAX_CELLS`]), each cell centre answered by the same land/water source
//! the domain rule reads (tidewater live, the baked grid headless; none for an aircraft, which
//! keeps only to the no-go areas). A cell is free on the right surface, at least
//! [`SURFACE_CLEARANCE_M`] from the wrong one and [`NOGO_CLEARANCE_M`] from a no-go area (the
//! clearance waived around the leg's own ends, which may lie close to a shore). An A* search over
//! the free cells, eight neighbours, integer costs and a fixed order (deterministic), then the path
//! pulled straight to as few waypoints as keep every stretch on free cells, at most
//! [`MAX_WAYPOINTS`]. The grid goes into the decision's inputs with the plan (`route_plans`), so
//! replay plans again from it; the routed legs are judged by the same rules as any leg, and the
//! order is refused only when no route exists, saying why.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};

use serde::{Deserialize, Serialize};

use crate::areas::Area;
use crate::engage::{enu, offset};
use crate::geo::{LonLat, haversine_m};
use crate::ids::Uid;
use crate::intent::{IntentSpec, Task};
use crate::route::{LEG_ENDS_M, LEG_NEAR_M, RouteLeg, along, route_for};
use crate::rules::{RulesConfig, required_surface};
use crate::terrain::TerrainOracle;
use crate::world::WorldSnapshot;

/// The grid's cell, m [E]: twice the domain rule's step along a leg (`route::LEG_STEP_M`), fine
/// enough to find the 1.3 km main span of the Golden Gate past a tower's pier, coarse enough to
/// keep a leg's grid to a few thousand cells.
pub const CELL_M: f64 = 40.0;

/// How far the grid reaches beyond the leg's own box, m [E]: room to go round a headland or a
/// pier a few hundred metres across; a detour wider than this is no route.
pub const MARGIN_M: f64 = 1_500.0;

/// The most cells one leg's grid holds [E]: its work bound (the search visits each at most once);
/// a longer leg gets coarser cells.
pub const MAX_CELLS: u64 = 40_000;

/// How far a planned leg keeps from the wrong surface, m [E]: about a large boat's length and a
/// half, a margin for the grid's own resolution and the vehicle's track error.
pub const SURFACE_CLEARANCE_M: f64 = 60.0;

/// How far a planned leg keeps from a no-go area's edge, m [E].
pub const NOGO_CLEARANCE_M: f64 = 100.0;

/// The most waypoints a planned leg adds [E]: more is a maze, not a detour.
pub const MAX_WAYPOINTS: usize = 12;

/// One leg's grid as logged: its south-west corner, its cell, its size, and each cell's surface,
/// row by row from the south-west, run-length coded (`"312w4l..."`: a count, then a letter: `w`
/// water, `d` water too shallow, `l` land, `o` outside coverage). Empty for an aircraft, whose
/// plan reads only the no-go areas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanGrid {
    pub sw: LonLat,
    pub cell_m: f64,
    pub cols: u32,
    pub rows: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cells: String,
}

/// A leg routed around what it met, or why it could not be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannedLeg {
    /// The leg as the rules name it (`"transit leg (position to waypoint 1)"`).
    pub leg: String,
    pub from: LonLat,
    pub to: LonLat,
    /// What the straight leg met (`"land at (-122.47640, 37.81347)"`, `"no-go gg-towers"`).
    pub met: String,
    /// The surface it keeps to (`water`, `land`); none for an aircraft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keeps: Option<String>,
    /// The waypoints it adds, in order (empty with `no_route`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<LonLat>,
    /// Why there is no route within the planner's bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_route: Option<String>,
    /// How much longer the routed leg is than the straight one, m.
    pub added_m: f64,
    pub grid: PlanGrid,
}

impl PlannedLeg {
    /// `"the transit leg (position to waypoint 1) routed around land at (...): 2 waypoints, +420
    /// m"`, or `"no route around land at (...) for the transit leg (...): <why>"`.
    pub fn summary(&self) -> String {
        match &self.no_route {
            Some(why) => format!("no route around {} for the {}: {why}", self.met, self.leg),
            None => format!(
                "the {} routed around {}: {} waypoint{}, +{:.0} m",
                self.leg,
                self.met,
                self.via.len(),
                if self.via.len() == 1 { "" } else { "s" },
                self.added_m
            ),
        }
    }
}

/// The plan for the leg from `from` to `to`, if `plans` has one.
pub fn leg_plan(plans: &BTreeMap<Uid, Vec<PlannedLeg>>, from: LonLat, to: LonLat) -> Option<&PlannedLeg> {
    plans
        .values()
        .flatten()
        .find(|p| p.from.e7() == from.e7() && p.to.e7() == to.e7())
}

/// The grid's geometry for a leg (no cells yet).
fn geometry(from: LonLat, to: LonLat) -> PlanGrid {
    let w = from.lon.min(to.lon);
    let s = from.lat.min(to.lat);
    let sw = offset(LonLat::new(w, s), -MARGIN_M, -MARGIN_M);
    let ne = offset(LonLat::new(from.lon.max(to.lon), from.lat.max(to.lat)), MARGIN_M, MARGIN_M);
    let (we, sn) = enu(sw, ne);
    let cell = CELL_M.max(((we * sn) / MAX_CELLS as f64).sqrt()).ceil();
    PlanGrid {
        sw,
        cell_m: cell,
        cols: (we / cell).ceil().max(1.0) as u32,
        rows: (sn / cell).ceil().max(1.0) as u32,
        cells: String::new(),
    }
}

impl PlanGrid {
    fn len(&self) -> usize {
        self.cols as usize * self.rows as usize
    }

    /// Cell `i`'s centre.
    fn centre(&self, i: usize) -> LonLat {
        let (c, r) = (i % self.cols as usize, i / self.cols as usize);
        offset(self.sw, (c as f64 + 0.5) * self.cell_m, (r as f64 + 0.5) * self.cell_m)
    }

    /// The cell a point lies in, if inside the grid.
    fn cell_of(&self, p: LonLat) -> Option<usize> {
        let (e, n) = enu(self.sw, p);
        let (c, r) = ((e / self.cell_m).floor(), (n / self.cell_m).floor());
        (c >= 0.0 && r >= 0.0 && c < f64::from(self.cols) && r < f64::from(self.rows))
            .then(|| r as usize * self.cols as usize + c as usize)
    }

    /// Every cell's centre, row by row (what the source is asked).
    pub fn centres(&self) -> Vec<LonLat> {
        (0..self.len()).map(|i| self.centre(i)).collect()
    }

    /// The cells' letters, decoded (none: an aircraft's grid, or a malformed one).
    fn letters(&self) -> Option<Vec<char>> {
        if self.cells.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(self.len());
        let mut n = 0usize;
        for ch in self.cells.chars() {
            if let Some(d) = ch.to_digit(10) {
                n = n * 10 + d as usize;
            } else {
                out.extend(std::iter::repeat_n(ch, n.max(1)));
                n = 0;
            }
        }
        (out.len() == self.len()).then_some(out)
    }
}

/// Run-length code the cells' letters.
fn encode(letters: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < letters.len() {
        let c = letters[i];
        let mut j = i;
        while j < letters.len() && letters[j] == c {
            j += 1;
        }
        out.push_str(&format!("{}{c}", j - i));
        i = j;
    }
    out
}

/// Plan the leg from `from` to `to` on `grid` (its cells: the surface to keep to, `keeps` its
/// letter; none for an aircraft) round `no_go`. The waypoints it adds, or why there is no route.
pub fn plan_leg(from: LonLat, to: LonLat, keeps: Option<char>, grid: &PlanGrid, no_go: &[&Area]) -> Result<Vec<LonLat>, String> {
    let n = grid.len();
    let letters = match keeps {
        Some(_) => Some(grid.letters().ok_or("the planner's grid is malformed")?),
        None => None,
    };
    let (Some(s), Some(g)) = (grid.cell_of(from), grid.cell_of(to)) else {
        return Err("the leg's ends lie outside its grid".into());
    };
    let cols = grid.cols as i64;
    let rows = grid.rows as i64;
    let rc = |i: usize| ((i as i64) % cols, (i as i64) / cols);
    // the wrong surface, and the cells within the clearance of it
    let wrong: Vec<bool> = (0..n).map(|i| letters.as_ref().is_some_and(|l| Some(l[i]) != keeps)).collect();
    let k = (SURFACE_CLEARANCE_M / grid.cell_m).ceil() as i64;
    let mut near = vec![false; n];
    if k > 0 {
        for i in (0..n).filter(|i| wrong[*i]) {
            let (c, r) = rc(i);
            for dr in -k..=k {
                for dc in -k..=k {
                    let (cc, rr) = (c + dc, r + dr);
                    if cc >= 0 && rr >= 0 && cc < cols && rr < rows && (dc * dc + dr * dr) as f64 <= (k * k) as f64 {
                        near[(rr * cols + cc) as usize] = true;
                    }
                }
            }
        }
    }
    // a no-go area, and its clearance
    let hard: Vec<bool> = (0..n)
        .map(|i| {
            let p = grid.centre(i);
            no_go.iter().any(|a| a.contains(p) || crate::engage::boundary_distance(p, a) < NOGO_CLEARANCE_M)
        })
        .collect();
    // the clearance from the wrong surface is waived within it of the leg's own ends
    let waived = |i: usize| {
        let p = grid.centre(i);
        haversine_m(p, from) <= SURFACE_CLEARANCE_M + grid.cell_m || haversine_m(p, to) <= SURFACE_CLEARANCE_M + grid.cell_m
    };
    let free: Vec<bool> = (0..n)
        .map(|i| i == s || i == g || (!hard[i] && !wrong[i] && (!near[i] || waived(i))))
        .collect();
    // A*, eight neighbours, integer costs, ties to the lower cell
    const STEP: u64 = 1_000;
    const DIAG: u64 = 1_414;
    let (gc, gr) = rc(g);
    let h = |i: usize| {
        let (c, r) = rc(i);
        let (dx, dy) = ((c - gc).unsigned_abs(), (r - gr).unsigned_abs());
        let (lo, hi) = (dx.min(dy), dx.max(dy));
        lo * DIAG + (hi - lo) * STEP
    };
    let mut cost = vec![u64::MAX; n];
    let mut came = vec![usize::MAX; n];
    let mut heap = BinaryHeap::new();
    cost[s] = 0;
    heap.push((Reverse(h(s)), Reverse(s)));
    let mut found = false;
    while let Some((_, Reverse(i))) = heap.pop() {
        if i == g {
            found = true;
            break;
        }
        let (c, r) = rc(i);
        for (dc, dr) in [(1, 0), (0, 1), (-1, 0), (0, -1), (1, 1), (-1, 1), (-1, -1), (1, -1)] {
            let (cc, rr) = (c + dc, r + dr);
            if cc < 0 || rr < 0 || cc >= cols || rr >= rows {
                continue;
            }
            let j = (rr * cols + cc) as usize;
            if !free[j] {
                continue;
            }
            let diag = dc != 0 && dr != 0;
            // no corner cut between two blocked cells
            if diag && (!free[(r * cols + cc) as usize] || !free[(rr * cols + c) as usize]) {
                continue;
            }
            let next = cost[i] + if diag { DIAG } else { STEP };
            if next < cost[j] {
                cost[j] = next;
                came[j] = i;
                heap.push((Reverse(next + h(j)), Reverse(j)));
            }
        }
    }
    if !found {
        let (we, sn) = enu(grid.sw, grid.centre(n - 1));
        let what = match keeps {
            Some('w') => "water",
            Some(_) => "land",
            None => "the air",
        };
        return Err(format!(
            "no way through {what} clear of the {} within {:.1} x {:.1} km around the leg (the planner's bounds)",
            if keeps.is_some() { "wrong surface and the no-go areas" } else { "no-go areas" },
            we / 1000.0,
            sn / 1000.0
        ));
    }
    let mut cells = vec![g];
    while let Some(&last) = cells.last() {
        if last == s {
            break;
        }
        cells.push(came[last]);
    }
    cells.reverse();
    // pulled straight: from where it is, to the farthest cell it can see over free cells
    let clear = |a: LonLat, b: LonLat| {
        let d = haversine_m(a, b);
        let steps = (d / (grid.cell_m / 4.0)).ceil().max(1.0) as usize;
        (0..=steps).all(|t| {
            let f = t as f64 / steps as f64;
            let p = LonLat::new(a.lon + (b.lon - a.lon) * f, a.lat + (b.lat - a.lat) * f);
            grid.cell_of(p).is_some_and(|i| free[i])
        })
    };
    let mut via: Vec<LonLat> = Vec::new();
    let mut at = from;
    let mut k = 0usize;
    while !clear(at, to) {
        let Some(j) = (k + 1..cells.len()).rev().find(|j| clear(at, grid.centre(cells[*j]))) else {
            return Err("the planned path could not be smoothed into straight stretches".into());
        };
        let p = grid.centre(cells[j]);
        via.push(p);
        if via.len() > MAX_WAYPOINTS {
            return Err(format!("a route around it would need more than {MAX_WAYPOINTS} waypoints"));
        }
        at = p;
        k = j;
    }
    Ok(via)
}

/// Why a straight leg needs a plan: the first point along it on the wrong surface (past its
/// departure's allowance, as the domain rule judges it), else the first no-go area it crosses.
/// None when it is clear, or when planning can't help (its end is inside a no-go area).
fn met(leg: &RouteLeg, need: Option<&str>, world: &WorldSnapshot, oracle: &dyn TerrainOracle) -> Option<String> {
    if let Some(need) = need
        && let Some(pts) = along(leg.from, leg.to)
        && !pts.is_empty()
        && let Ok(answers) = oracle.sample_along(world.t, &pts.iter().map(|(p, _)| *p).collect::<Vec<_>>(), LEG_NEAR_M)
    {
        let good = if need == "water" { 'w' } else { 'l' };
        let letters: Vec<char> = answers.iter().map(|a| a.surface.letter()).collect();
        let first_good = letters.iter().position(|c| *c == good).unwrap_or(letters.len());
        for (i, ((p, d), c)) in pts.iter().zip(&letters).enumerate() {
            if *c == good || *c == 'o' || (leg.departs && i < first_good && *d <= LEG_ENDS_M) {
                continue;
            }
            let what = if need == "water" { if *c == 'd' { "shallows" } else { "land" } } else { "water" };
            return Some(format!("{what} at ({:.5}, {:.5})", p.lon, p.lat));
        }
    }
    let areas = world.areas.as_ref()?;
    for a in areas.no_go() {
        if a.contains(leg.to) {
            return None;
        }
        if a.segment_touches_boundary(leg.from, leg.to) || (a.contains(leg.from) && !leg.departs) {
            return Some(format!("no-go {}", a.id));
        }
    }
    None
}

/// The legs of `spec`'s vehicles a plan may route: a vehicle's transit and, for a patrol, its
/// legs, each with what it met and the surface it keeps to.
fn needing(spec: &IntentSpec, world: &WorldSnapshot, oracle: &dyn TerrainOracle, cfg: &RulesConfig) -> Vec<(Uid, RouteLeg, String, Option<&'static str>)> {
    if !cfg.route_plan() || !matches!(spec.task, Task::Orbit { .. } | Task::Patrol { .. }) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for uid in &spec.uids {
        let Some(v) = world.vehicles.get(uid) else { continue };
        let need = v.platform.as_ref().and_then(|p| required_surface(p.domain, p.autopilot));
        let Ok(route) = route_for(spec, v, world, cfg) else { continue };
        let patrol = matches!(spec.task, Task::Patrol { .. });
        for leg in route.legs() {
            let plannable = leg.name.starts_with(crate::route::LegKind::Transit.as_str())
                || (patrol && leg.name.starts_with(crate::route::LegKind::Task.as_str()));
            if !plannable {
                continue;
            }
            if let Some(m) = met(&leg, need, world, oracle) {
                out.push((uid.clone(), leg, m, need));
            }
        }
    }
    out
}

/// Every cell centre the planner will ask the land/water source for `spec` on `world` (what the
/// bridge fetches before the decision): the grids of the legs that need a plan and keep to a
/// surface.
pub fn grid_points(spec: &IntentSpec, world: &WorldSnapshot, oracle: &dyn TerrainOracle, cfg: &RulesConfig) -> Vec<LonLat> {
    needing(spec, world, oracle, cfg)
        .into_iter()
        .filter(|(_, _, _, need)| need.is_some())
        .flat_map(|(_, leg, _, _)| geometry(leg.from, leg.to).centres())
        .collect()
}

/// The plans for `spec` on `world` (its straight route, no plans in it yet), by vehicle: each leg
/// that needs one, routed or not. Empty under older rules and when every leg is clear.
pub fn plans_for(spec: &IntentSpec, world: &WorldSnapshot, oracle: &dyn TerrainOracle, cfg: &RulesConfig) -> BTreeMap<Uid, Vec<PlannedLeg>> {
    let mut out: BTreeMap<Uid, Vec<PlannedLeg>> = BTreeMap::new();
    let no_go: Vec<&Area> = world.areas.as_ref().map(|a| a.no_go().collect()).unwrap_or_default();
    for (uid, leg, met, need) in needing(spec, world, oracle, cfg) {
        let mut grid = geometry(leg.from, leg.to);
        let keeps = need.map(|n| if n == "water" { 'w' } else { 'l' });
        let sampled = match need {
            None => Ok(()),
            Some(_) => match oracle.sample_along(world.t, &grid.centres(), grid.cell_m / 2.0) {
                Ok(a) if a.len() == grid.len() => {
                    grid.cells = encode(&a.iter().map(|s| s.surface.letter()).collect::<Vec<_>>());
                    Ok(())
                }
                Ok(_) => Err("the land/water source answered the planner's grid short".to_string()),
                Err(e) => Err(format!("no land/water answers for the planner's grid ({e})")),
            },
        };
        let planned = sampled.and_then(|()| plan_leg(leg.from, leg.to, keeps, &grid, &no_go));
        let straight = haversine_m(leg.from, leg.to);
        let (via, no_route) = match planned {
            Ok(v) => (v, None),
            Err(why) => (Vec::new(), Some(why)),
        };
        let mut length = 0.0;
        let mut at = leg.from;
        for p in via.iter().chain(std::iter::once(&leg.to)) {
            length += haversine_m(at, *p);
            at = *p;
        }
        out.entry(uid).or_default().push(PlannedLeg {
            leg: leg.name,
            from: leg.from,
            to: leg.to,
            met,
            keeps: need.map(str::to_string),
            added_m: if no_route.is_none() { (length - straight).max(0.0).round() } else { 0.0 },
            via,
            no_route,
            grid,
        });
    }
    out
}

/// Replay: the waypoints `p` adds, planned again from its own logged grid (and the no-go areas
/// the decision read), or why there is no route.
pub fn replan(p: &PlannedLeg, world: &WorldSnapshot) -> Result<Vec<LonLat>, String> {
    let no_go: Vec<&Area> = world.areas.as_ref().map(|a| a.no_go().collect()).unwrap_or_default();
    let keeps = p.keeps.as_deref().map(|n| if n == "water" { 'w' } else { 'l' });
    plan_leg(p.from, p.to, keeps, &p.grid, &no_go)
}

/// What a dispatch carries of `spec`'s plans: each vehicle's transit waypoints, and a patrol's
/// route with the waypoints its legs are routed through (none when no leg of it is).
pub fn dispatch_routes(spec: &IntentSpec, plans: &BTreeMap<Uid, Vec<PlannedLeg>>) -> (BTreeMap<Uid, Vec<LonLat>>, Option<Vec<LonLat>>) {
    let transit = crate::route::LegKind::Transit.as_str();
    let via: BTreeMap<Uid, Vec<LonLat>> = spec
        .uids
        .iter()
        .filter_map(|u| {
            let p = plans.get(u)?.iter().find(|p| p.leg.starts_with(transit) && p.no_route.is_none() && !p.via.is_empty())?;
            Some((u.clone(), p.via.clone()))
        })
        .collect();
    let path = match &spec.task {
        Task::Patrol { route } if route.len() >= 2 => {
            let n = route.len();
            let mut out = Vec::new();
            let mut any = false;
            let planned = |a: LonLat, b: LonLat| leg_plan(plans, a, b).filter(|p| p.no_route.is_none() && !p.via.is_empty());
            for (i, a) in route.iter().enumerate() {
                out.push(*a);
                let b = route[(i + 1) % n];
                if let Some(p) = planned(*a, b) {
                    out.extend_from_slice(&p.via);
                    any = true;
                } else if n == 2 && i == 1
                    && let Some(p) = planned(route[0], route[1])
                {
                    // a two-point patrol flies its one leg back the same way round
                    out.extend(p.via.iter().rev());
                }
            }
            any.then_some(out)
        }
        _ => None,
    };
    (via, path)
}
