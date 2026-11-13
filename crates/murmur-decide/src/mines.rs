//! Rules 27 ([`crate::rules::MINE_SINCE`], bd murmur-lbw8.4, DECISIONS.md "Mines: laid on two
//! signatures, recorded, live for a set period"): minefields.
//!
//! A mine chooses its victim later, by its trigger, so the human judgement is made when it is
//! laid. A lay order ([`crate::intent::Task::Lay`]) names one platform, a line, the number of
//! mines spaced evenly along it ([`mine_points`]), the hours they stay live and the mine (the
//! platform's loadout weapon of kind `mine`). It is signed as an engagement is (two operators with
//! engagement authority, never its proposer, the authorize step closed to agents in code), and it
//! needs no declared target. Its checks ([`evaluate`]): the platform is armed, the mine is in its
//! loadout with enough rounds left and a longest live period no shorter than the order's
//! (`weapon`), the platform has no engagement in play, and the `minefield` check: what the engine
//! can see that the field could catch other than a declared hostile.
//!
//! The `minefield` check, in the form it can honestly take. A field is judged against the
//! domains its mine is set off by (the catalog's `targets`: a TM-62M by ground vehicles, a
//! Hammerhead by subsurface contacts) and nothing else, saying so:
//! - every live track of those domains not declared hostile, where it is now (its estimate, with
//!   twice its position error) and along its course now for [`TRACK_HORIZON_S`]; what it does
//!   after that, over a field's hours, is not something the picture can say;
//! - every lost contact of those domains (rules 16) that could be in the field by now, at its
//!   domain's top speed from its last fix;
//! - every vehicle of ours of those domains: where it is, its home (an rtl ends there), and the
//!   order it is on ([`orders_in_play`]: its task path, from where it is and back home);
//! - the layer itself: after each mine arms (its catalog `arm_s` after its laying) it must not
//!   come back within the field's reach of it along the rest of the line or on its way home, at
//!   its catalog cruise speed;
//! - the policy's no-strike areas (a bridge, a ferry terminal, a populated shore): the field's
//!   danger area touches none.
//!
//! The danger area is each mine's trigger reach ([`crate::catalog::WeaponFacts::trigger_reach_m`])
//! plus [`FIELD_MARGIN_M`]. Recorded lanes (shipping lanes, roads) are not loaded in Murmur, so a
//! field is judged on the traffic the picture holds now; the pass says so. A failure names what
//! is in the way and by how much, as the keep-outs do.
//!
//! The record (amended CCW Protocol II, Article 9 and its Technical Annex; Hague VIII, Articles 1
//! and 3): each mine laid is logged (`mine.laid`: its place, the time, the hours it is live, the
//! round it spends, the layer's reported position), the field is carried over a restart, and the
//! field going inert is logged (`minefield.inert`). The live period is mandatory at laying and no
//! longer than the mine's catalog `persist_h`. No anti-personnel mine is in the catalog.
//!
//! While a field of ours is live it is a no-go area ([`field_areas`]) for our vehicles of the
//! domains it is set off by: the route planner (rules 22) routes round it, the geofence refuses a
//! mission or a nomination across it, and an rtl across it goes as ordered with a warning (the
//! inbox alert `path_crosses_minefield`).
//!
//! Every function here is pure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::areas::{Area, AreaKind};
use crate::catalog::{Domain, PlatformFacts, WeaponFacts, WeaponKind};
use crate::engage::{enu, offset};
use crate::geo::{LonLat, Polygon, haversine_m};
use crate::ids::{IntentId, Uid};
use crate::intent::{IntentSpec, State, Task, TaskKind};
use crate::rules::{CheckResult, Outcome, RuleId, RulesConfig};
use crate::time::Time;
use crate::world::{Fact, WorldSnapshot};

/// The most points a lay order's line has.
pub const MAX_LINE_POINTS: usize = 20;

/// The most mines one lay order lays [E]: the TerMIT 2.0 lays 20 a run (docs/research/weapons.md
/// section 8); five runs' worth bounds a field's record.
pub const MAX_MINES: u32 = 100;

/// How near its place on the line the layer must be to lay a mine, m [E]: a vehicle's track
/// error along a line of a few hundred metres, and about 12 s at the UGV's 2 m/s.
pub const LAY_AT_M: f64 = 25.0;

/// A mine after the first is laid once the layer has come within this of its place along the
/// line, m [E]: the TM-62M's 1 to 4 m spacing in a row (docs/research/weapons.md section 8).
pub const LAY_AHEAD_M: f64 = 2.0;

/// How near the line's end the layer counts as having driven the whole line, m [E]: the task
/// layer's own tolerance for a lay order's end (`tasks::exec` `lay`), a little over a UGV's
/// waypoint radius. There every mine left is laid, so an order that ends never leaves its last
/// mines unlaid for want of a few metres.
pub const LINE_END_M: f64 = 10.0;

/// The danger area's margin beyond a mine's trigger reach, m [E]: the marking and the track error
/// of whoever passes, so that a planned path's own error does not take it over a mine.
pub const FIELD_MARGIN_M: f64 = 50.0;

/// How far ahead a track's course now is judged against a field, s [E]: the keep-outs' horizon
/// is a strike's seconds; a field's is its hours, which no course predicts, so the check judges
/// ten minutes of the course now and says it judged no more.
pub const TRACK_HORIZON_S: f64 = 600.0;

/// The step along a path or a course the check samples, m.
const SAMPLE_M: f64 = 10.0;

/// The step along a track's course, s.
const COURSE_STEP_S: f64 = 10.0;

/// A lay order's parameters: the line, the number of mines, the hours they are live, the mine
/// named (none: the platform's first).
pub fn params(spec: &IntentSpec) -> Option<(&[LonLat], u32, f64, Option<&str>)> {
    match &spec.task {
        Task::Lay {
            points,
            count,
            live_h,
            weapon,
        } => Some((points.as_slice(), *count, *live_h, weapon.as_deref())),
        _ => None,
    }
}

/// The places of `count` mines spaced evenly along the line through `points`, the first at its
/// start and the last at its end (one point: one mine there).
pub fn mine_points(points: &[LonLat], count: u32) -> Vec<LonLat> {
    let Some(first) = points.first().copied() else {
        return Vec::new();
    };
    if count <= 1 || points.len() < 2 {
        return vec![first];
    }
    let lengths: Vec<f64> = points.windows(2).map(|w| haversine_m(w[0], w[1])).collect();
    let total: f64 = lengths.iter().sum();
    let step = total / f64::from(count - 1);
    (0..count)
        .map(|i| at_along(points, &lengths, step * f64::from(i)))
        .collect()
}

/// The point `s` metres along the line through `points` (segment `lengths`), on its last point
/// past its end.
fn at_along(points: &[LonLat], lengths: &[f64], s: f64) -> LonLat {
    let mut left = s;
    for (i, len) in lengths.iter().enumerate() {
        if left <= *len || i + 1 == lengths.len() {
            let f = if *len > 0.0 { (left / len).clamp(0.0, 1.0) } else { 0.0 };
            let (a, b) = (points[i], points[i + 1]);
            let (e, n) = enu(a, b);
            return offset(a, e * f, n * f);
        }
        left -= len;
    }
    points[points.len() - 1]
}

/// Where `p` projects onto the line through `points`: its distance along the line and across it,
/// m (the nearest segment's).
pub fn along_line(points: &[LonLat], p: LonLat) -> (f64, f64) {
    if points.len() < 2 {
        return (0.0, points.first().map_or(f64::INFINITY, |a| haversine_m(*a, p)));
    }
    let mut best = (0.0, f64::INFINITY);
    let mut cum = 0.0;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (bx, by) = enu(a, b);
        let (px, py) = enu(a, p);
        let len2 = bx * bx + by * by;
        let f = if len2 > 0.0 { ((px * bx + py * by) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let cross = (px - bx * f).hypot(py - by * f);
        if cross < best.1 {
            best = (cum + len2.sqrt() * f, cross);
        }
        cum += len2.sqrt();
    }
    best
}

/// How many of a lay's mines (at `planned`, `laid` of them laid) the layer at `at` lays now: the
/// next one once it is within [`LAY_AT_M`] of its place (and, after the first, has come within
/// [`LAY_AHEAD_M`] of it along the line, or within [`LINE_END_M`] of the line's end), and each
/// after it in order on the same terms.
pub fn due(planned: &[LonLat], laid: usize, at: LonLat) -> usize {
    let mut i = laid;
    let (s_at, _) = along_line(planned, at);
    let at_end = planned.len() > 1 && planned.last().is_some_and(|e| haversine_m(at, *e) <= LINE_END_M);
    while i < planned.len() {
        let near = haversine_m(at, planned[i]) <= LAY_AT_M;
        let reached = i == 0 || at_end || s_at + LAY_AHEAD_M >= along_line(planned, planned[i]).0;
        if !(near && reached) {
            break;
        }
        i += 1;
    }
    i - laid
}

/// One mine as the record holds it: its number in the field (from 1), its place, when it was
/// laid, when it arms and when it goes inert (engine time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mine {
    pub n: u32,
    pub at: LonLat,
    pub laid_t: Time,
    pub arms_t: Time,
    pub inert_t: Time,
}

/// A field of ours as decisions read it (the engine's `minefields` fact) and a log carries it
/// over: the lay order that laid it, its layer, the mine (its name, what sets it off and how near,
/// the domains it is set off by) and every mine laid. `from`: the earlier log it was laid in, for
/// one carried over at a start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Minefield {
    pub intent: IntentId,
    pub uid: Uid,
    pub weapon: String,
    pub trigger: String,
    pub reach_m: f64,
    pub targets: Vec<Domain>,
    pub mines: Vec<Mine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl Minefield {
    /// Its name in words and on the map (`MF-12`; a carried one's adds the log).
    pub fn id(&self) -> String {
        match &self.from {
            Some(log) => format!("MF-{} of {log}", self.intent.0),
            None => format!("MF-{}", self.intent.0),
        }
    }

    /// When its last mine goes inert.
    pub fn inert_t(&self) -> Time {
        self.mines.iter().map(|m| m.inert_t).max().unwrap_or(Time(0))
    }

    /// Whether any mine of it is live at `t` (laid and not yet inert).
    pub fn live_at(&self, t: Time) -> bool {
        self.mines.iter().any(|m| m.inert_t > t)
    }

    /// Whether a vehicle or contact of `domain` can set it off.
    pub fn sets_off(&self, domain: Domain) -> bool {
        self.targets.contains(&domain)
    }

    /// How far its danger area reaches from each mine, m: the trigger reach and [`FIELD_MARGIN_M`].
    pub fn danger_m(&self) -> f64 {
        self.reach_m + FIELD_MARGIN_M
    }

    /// The mines' places, live at `t`.
    fn live_points(&self, t: Time) -> Vec<LonLat> {
        self.mines.iter().filter(|m| m.inert_t > t).map(|m| m.at).collect()
    }

    /// How far `p` is from the nearest of its mines live at `t`, and that mine's number.
    pub fn nearest(&self, p: LonLat, t: Time) -> Option<(f64, u32)> {
        self.mines
            .iter()
            .filter(|m| m.inert_t > t)
            .map(|m| (haversine_m(p, m.at), m.n))
            .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
    }

    /// How far `p` is from the field's line of mines live at `t` (its segments between
    /// consecutive mines, which lie on the lay line), m.
    pub fn distance(&self, p: LonLat, t: Time) -> f64 {
        line_distance(&self.live_points(t), p)
    }

    /// The danger area as a no-go area ([`field_areas`]): a capsule round each run of mines, its
    /// polygon circumscribing the circle so that it never under-reaches.
    pub fn area(&self, t: Time) -> Area {
        Area {
            id: format!("minefield {}", self.id()),
            name: format!("minefield {} ({} {})", self.id(), self.mines.len(), self.weapon),
            kind: AreaKind::NoGo,
            polygons: capsules(&self.live_points(t), self.danger_m()),
        }
    }

    /// The field in words: `"MF-12 (10 TM-62M mine, set off by ground within 1 m, live until 13:05:00Z)"`.
    pub fn describe(&self) -> String {
        format!(
            "{} ({} {}, set off by {} within {:.0} m, live until {})",
            self.id(),
            self.mines.len(),
            self.weapon,
            domains(&self.targets),
            self.reach_m,
            crate::space::fmt_time(self.inert_t())
        )
    }
}

fn domains(d: &[Domain]) -> String {
    d.iter().map(|d| d.as_str()).collect::<Vec<_>>().join(" and ")
}

/// How far `p` is from the line through `pts` (a point's distance for one), m.
fn line_distance(pts: &[LonLat], p: LonLat) -> f64 {
    match pts {
        [] => f64::INFINITY,
        [a] => haversine_m(*a, p),
        _ => along_line(pts, p).1,
    }
}

/// Capsules of half-width `r` round the runs of `pts` (a circle for one point), merging points on
/// one straight run.
fn capsules(pts: &[LonLat], r: f64) -> Vec<Polygon> {
    const ARC: usize = 8;
    // circumscribed: the polygon's edges stay outside the circle of radius r
    let rr = r / (std::f64::consts::PI / (2.0 * ARC as f64)).cos();
    let ring = |e: Vec<LonLat>| Polygon { exterior: e, holes: Vec::new() };
    match pts {
        [] => Vec::new(),
        [a] => {
            let n = 2 * ARC;
            vec![ring(
                (0..n)
                    .map(|i| {
                        let th = 2.0 * std::f64::consts::PI * i as f64 / n as f64;
                        offset(*a, rr * th.cos(), rr * th.sin())
                    })
                    .collect(),
            )]
        }
        _ => {
            // the runs: drop a point that lies on the straight line of its neighbours (within 1 m)
            let mut runs = vec![pts[0]];
            for i in 1..pts.len() - 1 {
                let (a, b, c) = (runs[runs.len() - 1], pts[i], pts[i + 1]);
                if line_distance(&[a, c], b) > 1.0 {
                    runs.push(b);
                }
            }
            runs.push(pts[pts.len() - 1]);
            runs.windows(2)
                .map(|w| {
                    let (a, b) = (w[0], w[1]);
                    let (e, n) = enu(a, b);
                    let heading = n.atan2(e);
                    let mut out = Vec::with_capacity(2 * ARC + 2);
                    // round b's end, then a's
                    for (c, from) in [(b, heading - std::f64::consts::FRAC_PI_2), (a, heading + std::f64::consts::FRAC_PI_2)] {
                        for i in 0..=ARC {
                            let th = from + std::f64::consts::PI * i as f64 / ARC as f64;
                            out.push(offset(c, rr * th.cos(), rr * th.sin()));
                        }
                    }
                    ring(out)
                })
                .collect()
        }
    }
}

/// Rules 27: our live fields that `uid`'s vehicle could set off (by its platform's domain), as
/// no-go areas, at `world`'s time: what the geofence and the route planner read beside the area
/// set's own no-go areas. Empty under older rules, for an unknown vehicle and with no field.
pub fn field_areas(world: &WorldSnapshot, uid: &Uid, cfg: &RulesConfig) -> Vec<Area> {
    if !cfg.mines() || world.minefields.is_empty() {
        return Vec::new();
    }
    let Some(domain) = world.vehicles.get(uid).and_then(|v| v.platform.as_ref()).map(|p| p.domain) else {
        return Vec::new();
    };
    world
        .minefields
        .iter()
        .filter(|f| f.sets_off(domain) && f.live_at(world.t))
        .map(|f| f.area(world.t))
        .collect()
}

/// Whether a no-go area is one of our fields ([`field_areas`]).
pub fn is_field(a: &Area) -> bool {
    a.id.starts_with("minefield ")
}

/// Rules 27: the live fields a decision on `spec` reads (the engine's `minefields` fact): those a
/// vehicle of the intent could set off, by the platforms `world` gives them, every field for a lay
/// order. Pure: the engine and replay call it on their ledgers.
pub fn fields_for(spec: &IntentSpec, world: &WorldSnapshot, fields: &[Minefield], cfg: &RulesConfig) -> Vec<Minefield> {
    if !cfg.mines() || spec.task.is_space() {
        return Vec::new();
    }
    let domains: Vec<Domain> = spec
        .uids
        .iter()
        .filter_map(|u| world.vehicles.get(u).and_then(|v| v.platform.as_ref()).map(|p| p.domain))
        .collect();
    fields
        .iter()
        .filter(|f| f.live_at(world.t))
        .filter(|f| spec.kind() == TaskKind::Lay || domains.iter().any(|d| f.sets_off(*d)))
        .cloned()
        .collect()
}

/// Rules 27: an order of ours in play on one of our vehicles, as a lay order's `minefield` check
/// reads it (the engine's `orders_in_play` fact): the intent, its kind and its task path (closed:
/// it comes back to its first point).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InPlayOrder {
    pub intent: IntentId,
    pub kind: TaskKind,
    pub path: Vec<LonLat>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
}

/// Rules 27: for a lay order (`spec`), the orders in play on our other vehicles (`intents`: every
/// intent with its spec and the state it plays as): each vehicle's newest dispatched order with a
/// task path. Empty for any other kind. Pure: the engine and replay call it on their intents.
/// Rules 30: `orders` without those of our vehicles confirmed lost (`nodes_lost`, a decision's
/// fact; a loss carried over only on the vehicle lost): the minefield check leaves such a vehicle
/// out, its orders with it. Nothing before rules 30.
pub fn without_lost(
    orders: &mut BTreeMap<Uid, InPlayOrder>,
    nodes_lost: &BTreeMap<Uid, crate::world::LostNode>,
    cfg: &RulesConfig,
) {
    if cfg.node_loss() {
        orders.retain(|u, _| !nodes_lost.get(u).is_some_and(crate::world::LostNode::holds));
    }
}

pub fn orders_in_play<'a>(
    intents: impl Iterator<Item = (IntentId, &'a IntentSpec, State)>,
    spec: &IntentSpec,
) -> BTreeMap<Uid, InPlayOrder> {
    let mut out: BTreeMap<Uid, InPlayOrder> = BTreeMap::new();
    if spec.kind() != TaskKind::Lay {
        return out;
    }
    for (id, s, state) in intents {
        if id == spec.id || state != State::Dispatched {
            continue;
        }
        let Some(path) = s.task.path() else { continue };
        for u in s.uids.iter().filter(|u| !spec.uids.contains(u)) {
            if out.get(u).is_some_and(|o| o.intent > id) {
                continue;
            }
            out.insert(
                u.clone(),
                InPlayOrder {
                    intent: id,
                    kind: s.kind(),
                    path: path.points.clone(),
                    closed: path.closed,
                },
            );
        }
    }
    out
}

/// The mine a lay order on `platform` lays: the one `named`, else the first of its loadout of
/// kind mine. `Err` says why there is none.
pub fn mine_of<'p>(platform: &'p PlatformFacts, named: Option<&str>) -> Result<&'p WeaponFacts, String> {
    let id = &platform.id;
    match named {
        Some(n) => {
            let w = platform.loadout.iter().find(|w| w.name == n).ok_or_else(|| {
                if platform.loadout.is_empty() {
                    format!("{id} carries no loadout: no mine named {n}")
                } else {
                    format!(
                        "{id}'s loadout has no weapon named {n} (it carries {})",
                        platform.loadout.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", ")
                    )
                }
            })?;
            if w.kind != WeaponKind::Mine {
                return Err(format!("{n} is a {}, not a mine: a lay order lays mines", w.kind.as_str()));
            }
            Ok(w)
        }
        None => platform
            .loadout
            .iter()
            .find(|w| w.kind == WeaponKind::Mine)
            .ok_or_else(|| format!("{id} carries no mine")),
    }
}

/// The `weapon` check of a lay order: the mine, its rounds against the count, its longest live
/// period against the order's, and the vehicle's own damage report (weapons out of action).
fn weapon_check(spec: &IntentSpec, world: &WorldSnapshot) -> CheckResult {
    let r = RuleId::Weapon;
    let uid = spec.uids.first();
    let res = |o, d: String| CheckResult::new(r, uid, o, d);
    let Some((_, count, live_h, named)) = params(spec) else {
        return res(Outcome::Fail, "not a lay order".into());
    };
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return res(Outcome::Fail, "cannot evaluate: the platform is not in the world model".into());
    };
    let Some(p) = &v.platform else {
        return res(Outcome::Fail, "cannot evaluate: platform not in the catalog".into());
    };
    if let Some(d) = v.damage.as_ref().filter(|d| d.value.firepower) {
        return res(
            Outcome::Fail,
            format!(
                "the vehicle reported its weapons out of action (reported {:.0} s ago): no weapon of {}'s loadout can be used",
                (world.t.ms_since(d.t) as f64 / 1000.0).max(0.0),
                p.id
            ),
        );
    }
    let w = match mine_of(p, named) {
        Ok(w) => w,
        Err(why) => return res(Outcome::Fail, why),
    };
    let left = crate::loadout::left(w, uid.and_then(|u| world.rounds.get(u))).unwrap_or(0);
    let persist = w.persist_h.unwrap_or(0.0);
    let what = format!(
        "{}{}, a mine for {} targets, set off by {} within {:.0} m, arms {:.0} s after laying, live at most {persist:.0} h; {left} of {} left",
        if named.is_some() { "named: " } else { "picked (none named): " },
        w.name,
        domains(&w.targets),
        w.trigger.as_deref().unwrap_or("its trigger"),
        w.trigger_reach_m(),
        w.arm_s.unwrap_or(0.0),
        w.count
    );
    if left < count {
        return res(Outcome::Fail, format!("{what}: the order lays {count}, more than are left"));
    }
    if live_h > persist {
        return res(
            Outcome::Fail,
            format!("{what}: the order's {live_h} h live is longer than the {persist:.0} h a {} stays live", w.name),
        );
    }
    res(Outcome::Pass, format!("{what}; the order lays {count}, live {live_h} h each"))
}

/// Rules 27: the checks of a lay order after the vehicle rules: `platform_armed`, `weapon`,
/// `platform_free` and `minefield` (the module doc). Nothing for another kind.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    if spec.kind() != TaskKind::Lay {
        return Vec::new();
    }
    let uid = spec.uids.first();
    vec![
        crate::engage::armed_check(uid, world),
        weapon_check(spec, world),
        crate::engage::free_check(uid, world),
        hazard_check(spec, world, cfg),
    ]
}

/// The field a lay order would lay, as planned (every mine at its place, laid at `t`).
pub fn planned_field(spec: &IntentSpec, w: &WeaponFacts, t: Time) -> Option<Minefield> {
    let (points, count, live_h, _) = params(spec)?;
    let uid = spec.uids.first()?.clone();
    let live_ms = (live_h * 3_600_000.0).round() as i64;
    Some(Minefield {
        intent: spec.id,
        uid,
        weapon: w.name.clone(),
        trigger: w.trigger.clone().unwrap_or_default(),
        reach_m: w.trigger_reach_m(),
        targets: w.targets.clone(),
        mines: mine_points(points, count)
            .into_iter()
            .enumerate()
            .map(|(i, at)| Mine {
                n: i as u32 + 1,
                at,
                laid_t: t,
                arms_t: t.plus_ms(w.arm_ms()),
                inert_t: t.plus_ms(live_ms),
            })
            .collect(),
        from: None,
    })
}

/// Points every [`SAMPLE_M`] along the line through `pts` (closed: back to the first), with the
/// distance along it of each.
fn samples(pts: &[LonLat], closed: bool) -> Vec<(LonLat, f64)> {
    let mut line: Vec<LonLat> = pts.to_vec();
    if closed && pts.len() > 1 {
        line.push(pts[0]);
    }
    let mut out = Vec::new();
    let mut cum = 0.0;
    if let Some(first) = line.first() {
        out.push((*first, 0.0));
    }
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = haversine_m(a, b);
        let n = (len / SAMPLE_M).ceil().max(1.0) as usize;
        let (e, nn) = enu(a, b);
        for k in 1..=n {
            let f = k as f64 / n as f64;
            out.push((offset(a, e * f, nn * f), cum + len * f));
        }
        cum += len;
    }
    out
}

/// What of `path` comes nearest the field, if within its danger area: the distance, the mine and
/// where.
fn path_intrusion(field: &Minefield, path: &[LonLat], closed: bool, t: Time) -> Option<(f64, u32, LonLat)> {
    let danger = field.danger_m();
    samples(path, closed)
        .into_iter()
        .filter_map(|(p, _)| field.nearest(p, t).map(|(d, n)| (d, n, p)))
        .filter(|(d, _, _)| *d <= danger)
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

fn at(p: LonLat) -> String {
    format!("({:.5}, {:.5})", p.lon, p.lat)
}

/// The `minefield` check (the module doc).
fn hazard_check(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> CheckResult {
    let r = RuleId::Minefield;
    let uid = spec.uids.first();
    let res = |o, d: String| CheckResult::new(r, uid, o, d);
    let ec = cfg.engage_rules();
    let Some(v) = uid.and_then(|u| world.vehicles.get(u)) else {
        return res(Outcome::Fail, "cannot evaluate: the platform is not in the world model".into());
    };
    let Some(p) = &v.platform else {
        return res(Outcome::Fail, "cannot evaluate: platform not in the catalog".into());
    };
    let named = params(spec).and_then(|x| x.3);
    let w = match mine_of(p, named) {
        Ok(w) => w,
        Err(why) => return res(Outcome::Fail, format!("cannot evaluate: no mine ({why})")),
    };
    let Some(field) = planned_field(spec, w, world.t) else {
        return res(Outcome::Fail, "cannot evaluate: not a lay order".into());
    };
    let t = world.t;
    let danger = field.danger_m();
    let need = |d: f64| danger - d;
    let mut blocks: Vec<String> = Vec::new();
    // tracks of the domains it is set off by: where they are now and on their course now
    let mut judged_tracks = 0usize;
    for (id, tr) in world.tracks.iter().filter(|(_, tr)| field.sets_off(tr.domain)) {
        if crate::engage::track_exempt(id, tr, "", &ec) {
            continue;
        }
        judged_tracks += 1;
        let sigma2 = 2.0 * tr.pos_sigma_m.max(0.0);
        let now = crate::engage::track_at(tr, t);
        let what = format!(
            "{id} ({}, {})",
            tr.identity.as_deref().unwrap_or("kind unknown"),
            format!("{:?}", tr.affiliation.value).to_lowercase()
        );
        if let Some((d, n)) = field.nearest(now, t)
            && d - sigma2 <= danger
        {
            blocks.push(format!(
                "{what} is {d:.0} m from mine {n} now, within the field's {danger:.0} m (with twice its {:.0} m position error): it is {:.0} m short",
                tr.pos_sigma_m,
                need(d - sigma2)
            ));
            continue;
        }
        let [ve, vn] = tr.velocity_mps.unwrap_or([0.0, 0.0]);
        if ve.hypot(vn) <= 0.0 {
            continue;
        }
        let steps = (TRACK_HORIZON_S / COURSE_STEP_S).round() as usize;
        for k in 1..=steps {
            let s = k as f64 * COURSE_STEP_S;
            let q = offset(now, ve * s, vn * s);
            let grow = sigma2 + 2.0 * crate::engage::sigma_growth(tr) * s;
            if let Some((d, n)) = field.nearest(q, t)
                && d - grow <= danger
            {
                blocks.push(format!(
                    "{what} on its course now passes {d:.0} m from mine {n} in {s:.0} s, within the field's {danger:.0} m: it is {:.0} m short",
                    need(d - grow)
                ));
                break;
            }
        }
    }
    // lost contacts of those domains that could be in the field by now
    for (id, lc) in world.lost_contacts.iter().filter(|(_, lc)| field.sets_off(lc.facts.domain)) {
        if crate::engage::track_exempt(id, &lc.facts, "", &ec) {
            continue;
        }
        let since = (t.ms_since(lc.since()) as f64 / 1000.0).max(0.0);
        let reach = crate::engage::top_speed_mps(lc.facts.domain) * since;
        if let Some((d, n)) = field.nearest(lc.facts.position.value, t)
            && d - reach <= danger
        {
            blocks.push(format!(
                "lost contact {id} could be in the field: its last fix {since:.0} s ago was {d:.0} m from mine {n}, within the {reach:.0} m it could have come since"
            ));
        }
    }
    // ours of those domains: where they are, their homes, the orders they are on (rules 30: but
    // one confirmed lost, left out with its orders)
    let mut judged_ours = 0usize;
    let mut lost_ours: Vec<String> = Vec::new();
    for (u, ov) in world.vehicles.iter().filter(|(u, _)| Some(*u) != uid) {
        let Some(op) = &ov.platform else { continue };
        if !field.sets_off(op.domain) {
            continue;
        }
        if let Some(note) = crate::engage::confirmed_lost(u, ov, world, cfg) {
            lost_ours.push(note);
            continue;
        }
        judged_ours += 1;
        if let Some(pos) = &ov.position
            && let Some((d, n)) = field.nearest(pos.value, t)
            && d <= danger
        {
            blocks.push(format!("our {u} ({}) is {d:.0} m from mine {n} now, within the field's {danger:.0} m", op.id));
            continue;
        }
        if let Some(home) = &ov.home
            && let Some((d, n)) = field.nearest(home.value, t)
            && d <= danger
        {
            blocks.push(format!("our {u} ({})'s home is {d:.0} m from mine {n}, within the field's {danger:.0} m (its rtl ends there)", op.id));
            continue;
        }
        if let Some(o) = world.orders_in_play.get(u) {
            let mut line: Vec<LonLat> = ov.position.as_ref().map(|p| vec![p.value]).unwrap_or_default();
            line.extend(o.path.iter().copied());
            if o.closed
                && let Some(f) = o.path.first()
            {
                line.push(*f);
            }
            if let Some(h) = &ov.home {
                line.push(h.value);
            }
            if let Some((d, n, q)) = path_intrusion(&field, &line, false, t) {
                blocks.push(format!(
                    "our {u} ({})'s {} (intent {}) passes {d:.0} m from mine {n} at {}, within the field's {danger:.0} m",
                    op.id,
                    o.kind.as_str(),
                    o.intent,
                    at(q)
                ));
            }
        }
    }
    // the layer itself, after each mine arms: along the rest of the line and home
    let mut layer_note = String::new();
    if field.sets_off(p.domain)
        && let Some((points, _, _, _)) = params(spec)
    {
        let cruise = p.cruise_mps.filter(|c| *c > 0.0);
        let mut route: Vec<LonLat> = points.to_vec();
        if let Some(h) = &v.home {
            route.push(h.value);
        }
        let pts = samples(&route, false);
        let arm_s = w.arm_s.unwrap_or(0.0);
        match cruise {
            None => blocks.push(format!("cannot evaluate the layer's way clear of its mines: {} has no cruise speed", p.id)),
            Some(c) => {
                let mut worst: Option<(f64, u32, LonLat)> = None;
                for m in &field.mines {
                    let (s_m, _) = along_line(points, m.at);
                    for (q, s) in pts.iter().filter(|(_, s)| *s > s_m) {
                        if (s - s_m) / c < arm_s {
                            continue;
                        }
                        let d = haversine_m(*q, m.at);
                        if d <= danger && worst.is_none_or(|w| d < w.0) {
                            worst = Some((d, m.n, *q));
                        }
                    }
                }
                match worst {
                    Some((d, n, q)) => blocks.push(format!(
                        "the layer comes back {d:.0} m from mine {n} at {} after it arms ({arm_s:.0} s after its laying), within the field's {danger:.0} m: lay the line so that its way home stays clear",
                        at(q)
                    )),
                    None => layer_note = format!("; the layer clear of each mine before it arms ({arm_s:.0} s)"),
                }
            }
        }
    }
    // the policy's no-strike areas
    if let Some(areas) = &world.areas {
        for a in areas.areas.iter().filter(|a| a.kind == AreaKind::NoStrike) {
            let d = field
                .mines
                .iter()
                .map(|m| if a.contains(m.at) { 0.0 } else { crate::engage::boundary_distance(m.at, a) })
                .fold(f64::INFINITY, f64::min);
            if d <= danger {
                blocks.push(format!("the field's danger area reaches no-strike area {} ({d:.0} m from a mine)", a.name));
            }
        }
    }
    if !blocks.is_empty() {
        return res(Outcome::Fail, format!("{} could be set off by more than a declared hostile: {}", field_words(&field), blocks.join("; ")));
    }
    let others: Vec<&str> = [Domain::Air, Domain::Surface, Domain::Ground, Domain::Subsurface]
        .into_iter()
        .filter(|d| !field.sets_off(*d))
        .map(Domain::as_str)
        .collect();
    // rules 30: ours confirmed lost were left out
    let lost_note = if lost_ours.is_empty() {
        String::new()
    } else {
        format!("; left out, confirmed lost: {}", lost_ours.join(", "))
    };
    res(
        Outcome::Pass,
        format!(
            "{} clear of {judged_tracks} protected track(s) of {} now and on their course for {TRACK_HORIZON_S:.0} s, of {judged_ours} of ours there, their homes and orders{layer_note}, and of the no-strike areas; {} traffic can't set it off; not judged: recorded lanes and roads (none loaded) and any course beyond {TRACK_HORIZON_S:.0} s{lost_note}",
            field_words(&field),
            domains(&field.targets),
            others.join(", ")
        ),
    )
}

/// `"the field of 10 TM-62M mine (set off within 1 m, danger area 51 m)"`.
fn field_words(f: &Minefield) -> String {
    format!(
        "the field of {} {} (set off within {:.0} m, danger area {:.0} m)",
        f.mines.len(),
        f.weapon,
        f.reach_m,
        f.danger_m()
    )
}

/// Rules 27: what the engine logs of one mine it lays (`mine.laid`, [`crate::audit::EventBody::MineLaid`])
/// and hands the bridge for the truth side: the field (its lay order), the layer, the mine's
/// number and place, the mine's catalog facts, when it arms and goes inert.
#[derive(Clone, Debug, PartialEq)]
pub struct MineOrder {
    pub intent: IntentId,
    pub uid: Uid,
    pub n: u32,
    pub at: LonLat,
    pub weapon: WeaponFacts,
    pub arms_t: Time,
    pub inert_t: Time,
}

/// Rules 27: a step of the lays (`Engine::step_lays`): the mines laid now (for the truth side) and
/// the fields gone inert.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayStep {
    pub laid: Vec<MineOrder>,
    pub inert: Vec<String>,
}

/// The layer's position fact a lay step reads: its reported position, fresh enough (the rules'
/// freshness), or none.
pub fn layer_at<'a>(world: &'a WorldSnapshot, uid: &Uid, cfg: &RulesConfig) -> Option<&'a Fact<LonLat>> {
    let v = world.vehicles.get(uid)?;
    let f = v.position.as_ref()?;
    crate::rules::fresh(&v.position, v, "position", world, cfg).ok()?;
    Some(f)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Mines spread evenly from the line's start to its end; the layer lays them in order as it
    /// passes, several in a step where they lie close.
    #[test]
    fn mines_are_spaced_along_the_line_and_laid_in_order() {
        let a = LonLat::new(-122.45, 37.80);
        let b = offset(a, 90.0, 0.0);
        let pts = mine_points(&[a, b], 10);
        assert_eq!(pts.len(), 10);
        assert!((haversine_m(pts[0], a)).abs() < 0.01);
        assert!((haversine_m(pts[9], b)).abs() < 0.01);
        assert!((haversine_m(pts[1], pts[2]) - 10.0).abs() < 0.1);
        // far: nothing; at the start: the first; 15 m on: the next one too
        assert_eq!(due(&pts, 0, offset(a, -60.0, 0.0)), 0);
        assert_eq!(due(&pts, 0, a), 1);
        assert_eq!(due(&pts, 1, offset(a, 15.0, 0.0)), 1);
        assert_eq!(due(&pts, 2, offset(a, 15.0, 0.0)), 0);
        assert_eq!(due(&pts, 2, offset(a, 40.0, 0.0)), 3);
        // within LINE_END_M of the end: the last one too, though 8 m short of it along the line
        assert_eq!(due(&pts, 8, offset(a, 82.0, 0.0)), 2);
        assert_eq!(mine_points(&[a], 1), vec![a]);
    }

    /// The danger area as polygons contains the line and its margin, not much beyond.
    #[test]
    fn the_danger_area_covers_the_line_and_its_margin() {
        let a = LonLat::new(-122.45, 37.80);
        let pts: Vec<LonLat> = (0..5).map(|i| offset(a, 10.0 * f64::from(i), 0.0)).collect();
        let polys = capsules(&pts, 51.0);
        assert_eq!(polys.len(), 1, "one straight run, one capsule");
        let area = Area { id: "x".into(), name: "x".into(), kind: AreaKind::NoGo, polygons: polys };
        assert!(area.contains(offset(a, 20.0, 50.0)));
        assert!(area.contains(offset(a, -50.0, 0.0)));
        assert!(!area.contains(offset(a, 20.0, 60.0)));
    }
}
