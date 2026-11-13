//! Rules 30 ([`crate::rules::NODE_LOSS_SINCE`], docs/NODE-LOSS.md section 3): where one of our
//! vehicles can be once it stops reporting.
//!
//! A vehicle whose position is not fresh is **silent**. Until rules 30 the friendly keep-out
//! counted it anywhere its catalog cruise speed reached since its last report, which grows
//! without bound, never says why, and understates a vehicle that runs faster than it cruises.
//! From rules 30 it counts within a union of hypotheses, each a pure function of facts every
//! decision logs and of the engine's own record of its orders (logged with the decision as
//! [`crate::world::WorldSnapshot::silent_orders`], which replay checks against its own):
//!
//! - **C, it runs its order** (the link may be cut one way): every order that may be running on
//!   it ([`in_force`]: the newest it started, and any newer whose start went out unconfirmed),
//!   laid out as the bridge flies it: from where it was last heard to any of its waypoints, the
//!   waypoints in order (a patrol's loop closed), an orbit's circle, an rtl's way home and its
//!   arrival; with no order known, the circle it was flying when last heard, or a hold where it
//!   was if it was still.
//! - **L, its lost-link failsafe fired** (it lost us too): its own failsafe
//!   ([`crate::world::VehicleFacts::lost_link`]) or an engagement's preload on it: an rtl
//!   anywhere on C then straight home (each of C's shapes' hull with home), a hold, loiter,
//!   brake or land on C, a glide from C, and on the water a passive hold, a depth hold, a
//!   surfacing or a disarm on C then adrift. With the plan not known, every action its autopilot
//!   has. Its timer is ignored: the region holds from the first second.
//! - **D, adrift or falling**: presumed lost, or past the endurance its last battery report
//!   leaves; alone, with a `mobility` damage report (it cannot run its order). On the surface a
//!   disc growing at [`SURFACE_DRIFT_MPS`], under water at [`SUBSURFACE_DRIFT_MPS`], on the
//!   ground [`GROUND_DRIFT_M`], a fixed wing its glide from its last altitude, a rotary its
//!   fall, an airship the wind.
//! - **F, nothing known**: no order, circle or stillness to go on (or its order ended so that
//!   what it does now is not known): anywhere its top speed reaches since its last report (the
//!   owner's decision 3: catalog `max_mps`, else the domain's).
//!
//! Every shape is capped by that same reach (a sound bound, whichever is farther counts), with
//! the water's current or the wind added for what they carry. Corridors and holds do not grow
//! with time: no one can retask a silent vehicle (the queue holds every order for it). A corridor
//! is [`PATH_TOLERANCE_M`] wide either side, plus where in its report interval it was and its
//! turning circle's diameter (a turn back at a waypoint); on the water, plus
//! [`CROSS_SET_SHARE`] of each leg's length times the current over its cruise (a vessel steering
//! at its next point is set off its leg by a cross current); a vessel (an airship) no faster than
//! the current (the wind) is carried off its way at that speed. The figures were checked in the
//! headless simulator against the stand-ins (`eval/headless/node_loss.py`, docs/NODE-LOSS.md
//! section 3, "As built"). They are constants here, as `CIRCLE_SLACK_M` and `LINE_OF_FIRE_M` are
//! in [`crate::engage`]: no policy schema change. Shapes are convex outlines with a pad: one point
//! (a disc), two (a corridor), more (a padded polygon, an rtl's way home from a segment).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::catalog::{AutopilotKind, Domain};
use crate::engage::{self, Fallback, FriendlyKind, FriendlyStanding};
use crate::engine::Order;
use crate::geo::{LonLat, haversine_m};
use crate::ids::{IntentId, Uid};
use crate::intent::{Intent, IntentSpec, State, Task, TaskKind};
use crate::rules::{RulesConfig, fresh};
use crate::time::Time;
use crate::world::{LostLinkAction, VehicleFacts, WorldSnapshot};

/// A corridor's half-width before the vehicle's own speed and turns, m: a waypoint's acceptance
/// radius, GNSS at 2 sigma, cross-track error (the `CIRCLE_SLACK_M` figure; GUESS).
pub const PATH_TOLERANCE_M: f64 = 50.0;
/// A hold's disc before its braking and its report interval, m (GUESS).
pub const HOLD_RADIUS_M: f64 = 10.0;
/// How fast the water carries a vessel that makes no way, m/s: the Gate's spring current
/// (GUESS; tidewater's field at the Gate reaches 2.3 m/s).
pub const SURFACE_DRIFT_MPS: f64 = 2.5;
/// How fast the water carries a submerged vehicle that makes no way, m/s: the surface's (the spec's
/// 1.0 GUESS was too little for the headless check: the simulator's water runs the surface current
/// at every depth, docs/HEADLESS.md 17.4, and a vehicle near the surface meets it).
pub const SUBSURFACE_DRIFT_MPS: f64 = SURFACE_DRIFT_MPS;
/// How far a ground vehicle that makes no way may still end up, m (GUESS).
pub const GROUND_DRIFT_M: f64 = 10.0;
/// A fixed wing without power glides this many metres for each metre of altitude (GUESS).
pub const GLIDE_RATIO: f64 = 10.0;
/// A rotary wing without power falls this many metres sideways for each metre (GUESS).
pub const ROTARY_FALL_RATIO: f64 = 1.0;
/// How fast the wind carries an airship that makes no way, m/s (GUESS; the Gate's world draws
/// 3 to 8 m/s at the bridge).
pub const AIR_DRIFT_MPS: f64 = 10.0;
/// The turn rate assumed for one of ours whose catalog gives none, degrees a second: the
/// stand-ins' own default (GUESS for anything real). A turn back at a waypoint swings it out by
/// its turning circle's diameter (radius: cruise speed over turn rate).
pub const DEFAULT_TURN_DPS: f64 = 10.0;
/// How far a vessel steering at its next point in a cross current is set off the straight leg,
/// as a share of the leg's length times the current over its speed: a pursuit in a uniform
/// current is set off at most 1/e of that (0.37), and the field is not uniform (GUESS).
pub const CROSS_SET_SHARE: f64 = 0.5;
/// The battery left at which a vehicle's own battery failsafe may send it home, percent: the
/// stand-ins' low threshold, 20 %, and a margin (GUESS).
pub const BATTERY_FAILSAFE_PCT: f64 = 25.0;
/// The gentlest braking any of ours makes, m/s² (a hull's, the stand-ins' 0.5).
pub const BRAKE_MPS2: f64 = 0.5;

// ---- the engine's record: what may be running on it ------------------------------------------

/// Rules 30: what the engine's record says may be running on one of our silent vehicles, as a
/// decision's inputs carry it ([`WorldSnapshot::silent_orders`]): the orders that may be
/// running, newest first; when the newest one it started ended so that what it does now is not
/// known (an op failed after it started), why; and whether a presumption of its loss is in force
/// (`node.presumed`, the engine's node ledger: D counts too). Empty: no order of ours is known to
/// run on it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InForce {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orders: Vec<RunningOrder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub presumed: bool,
}

impl InForce {
    /// Whether some order of ours is known to have started on it (not only maybe).
    pub fn started(&self) -> bool {
        self.orders.iter().any(|o| !o.unconfirmed)
    }
}

/// Rules 30: one order that may be running on a silent vehicle, as the bridge flies it
/// ([`crate::engine::order_for`]): its waypoints in order (the transit's routed waypoints, then
/// its task path or its point), the circles it ends on, whether it goes home at the end, and an
/// engagement's fallback and lost-link preload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunningOrder {
    pub intent: IntentId,
    pub kind: TaskKind,
    /// Its start went out and nothing confirmed it (or it went out with no op reported yet): it
    /// may not be running, and the order before it may be.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unconfirmed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<LonLat>,
    /// The index in `path` the order loops back to after its last waypoint (a patrol's route).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_from: Option<usize>,
    /// The circles it flies at the end: an orbit's, an identify's (where it was sent and where
    /// it was moved last), an engagement's loiter over its point.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub circles: Vec<OrderCircle>,
    /// It goes home at the end (an rtl, a lay order).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub home: bool,
    /// An engagement: the fallback it was given (where a stop sends it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Fallback>,
    /// An engagement: the lost-link action preloaded on it (its failsafe while it is owed back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preload: Option<LostLinkAction>,
}

/// A circle an order flies: its centre and radius, m.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderCircle {
    pub center: LonLat,
    pub radius_m: f64,
}

/// One intent as [`in_force`] reads it: the engine's and replay's records give the same.
pub struct IntentView<'a> {
    pub id: IntentId,
    pub spec: &'a IntentSpec,
    /// Its state as what holds a platform reads it (`play_state`).
    pub state: State,
    /// Each vehicle's last op state on its current delivery.
    pub ops: &'a BTreeMap<Uid, String>,
    /// The vehicles that ever accepted and started it.
    pub committed: &'a BTreeSet<Uid>,
    /// The vehicles whose start went out and nothing confirmed.
    pub maybe_started: &'a BTreeSet<Uid>,
    /// The order it was (or would be) dispatched with ([`crate::engine::order_for`]).
    pub order: Option<Order>,
    /// An identify order: the orbit it flies now (its last re-centre).
    pub orbit_now: Option<OrderCircle>,
}

impl<'a> IntentView<'a> {
    /// The engine's intent as [`in_force`] reads it.
    pub fn of(i: &'a Intent, cfg: &RulesConfig) -> IntentView<'a> {
        let order = crate::engine::order_for(
            i.spec(),
            i.space_answer(),
            i.engagement().and_then(|e| e.snapshot.as_ref()),
            i.engagement().and_then(|e| e.loiter_radius_m),
            i.engagement().and_then(|e| e.preload.as_ref()),
            i.identify_plan(),
            i.route_plans(),
            cfg,
        )
        .ok();
        IntentView {
            id: i.id(),
            spec: i.spec(),
            state: i.play_state(),
            ops: i.ops(),
            committed: i.committed(),
            maybe_started: i.maybe_started(),
            order,
            orbit_now: i.identify_run().map(|r| OrderCircle {
                center: r.plan.center,
                radius_m: r.plan.radius_m,
            }),
        }
    }
}

/// Whether an intent of this kind puts one of our vehicles somewhere (not the space segment).
fn moves_a_vehicle(kind: TaskKind) -> bool {
    !matches!(kind, TaskKind::Collect | TaskKind::Cover)
}

/// The lost-link action an engagement's preload sets ([`engage::preload_for`]'s words).
fn preload_action(p: &engage::LostLinkPreload) -> Option<LostLinkAction> {
    Some(match p.action.as_str() {
        "RTL" => LostLinkAction::Rtl,
        "Brake or Land" => LostLinkAction::Brake,
        "Hold" => LostLinkAction::Hold,
        "Loiter or Hold" => LostLinkAction::Loiter,
        "Depth hold" => LostLinkAction::DepthHold,
        _ => return None,
    })
}

/// One order as the bridge flies it on `uid` (`view`'s), unconfirmed or not.
fn running(view: &IntentView, uid: &Uid, unconfirmed: bool) -> RunningOrder {
    let mut o = RunningOrder {
        intent: view.id,
        kind: view.spec.kind(),
        unconfirmed,
        path: Vec::new(),
        loop_from: None,
        circles: Vec::new(),
        home: false,
        fallback: None,
        preload: None,
    };
    match &view.order {
        Some(Order::Vehicles(d)) => {
            o.path = d.via.get(uid).cloned().unwrap_or_default();
            match &d.task {
                Task::Orbit { center, radius_m } => {
                    o.circles.push(OrderCircle {
                        center: *center,
                        radius_m: *radius_m,
                    });
                    // rules 21: an identify order's orbit moves with its track; it may fly the
                    // last one sent, or any between
                    if let Some(now) = view.orbit_now.filter(|c| c.center != *center || c.radius_m != *radius_m) {
                        o.circles.push(now);
                    }
                }
                Task::Patrol { route } => {
                    o.loop_from = Some(o.path.len());
                    o.path.extend(route.iter().copied());
                }
                Task::Lay { points, .. } => {
                    o.path.extend(points.iter().copied());
                    o.home = true;
                }
                Task::Rtl {} => o.home = true,
                _ => {}
            }
        }
        Some(Order::Engage(e)) => {
            let point = e.fire_from.unwrap_or(e.effect_point);
            o.path.push(point);
            o.circles.push(OrderCircle {
                center: point,
                radius_m: e.loiter_radius_m.unwrap_or(0.0),
            });
            o.fallback = Some(e.fallback);
            o.preload = e.preload.as_deref().and_then(preload_action);
        }
        // collect and cover put none of ours anywhere; no order: only its kind is known
        Some(Order::Collect(_) | Order::Cover(_)) | None => {
            if view.spec.kind() == TaskKind::Rtl {
                o.home = true;
            }
        }
    }
    o
}

/// Rules 30: for each of our vehicles in `world` whose position is not fresh (silent), but the
/// `platform` of the decision, what the record (`intents`) says may be running on it (an empty
/// [`InForce`] when nothing is known). Pure: the engine fills a decision's
/// [`WorldSnapshot::silent_orders`] with it, and replay recomputes it from its own record.
///
/// Newest first, for each intent naming the vehicle: one it started (an op `acked`, `active` or
/// `done`, in this run or one before) runs, and nothing older does (unless it is an identify,
/// which sends the vehicle back to the order it replaced: the walk goes on); one that failed
/// after it started leaves what it does now unknown; an engagement dispatched (under way,
/// effected, stopped, ended) or an order whose start may have gone out (an op `starting` or
/// `unconfirmed`, or dispatched with no op reported that rules it out: not `requeued`,
/// `failed`, `superseded` or `done`) may run, and so may what came before it.
pub fn in_force(
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    platform: Option<&Uid>,
    intents: &[IntentView],
    nodes: &crate::nodes::NodeLedger,
) -> BTreeMap<Uid, InForce> {
    silent_uids(world, cfg, platform)
        .into_iter()
        .map(|uid| {
            let mut f = in_force_on(&uid, intents);
            f.presumed = nodes.presumption(&uid).is_some();
            (uid, f)
        })
        .collect()
}

/// Rules 30: our vehicles in `world` that are silent (a position, not fresh), but `platform`.
pub fn silent_uids(world: &WorldSnapshot, cfg: &RulesConfig, platform: Option<&Uid>) -> BTreeSet<Uid> {
    world
        .vehicles
        .iter()
        .filter(|(uid, v)| {
            Some(*uid) != platform && v.position.is_some() && fresh(&v.position, v, "position", world, cfg).is_err()
        })
        .map(|(uid, _)| uid.clone())
        .collect()
}

/// [`in_force`] for one vehicle.
pub fn in_force_on(uid: &Uid, intents: &[IntentView]) -> InForce {
    let mut mine: Vec<&IntentView> = intents
        .iter()
        .filter(|i| moves_a_vehicle(i.spec.kind()) && i.spec.uids.contains(uid))
        .collect();
    mine.sort_by_key(|a| std::cmp::Reverse(a.id));
    let mut f = InForce::default();
    for view in mine {
        let op = view.ops.get(uid).map(String::as_str);
        if view.spec.kind() == TaskKind::Engage {
            if matches!(
                view.state,
                State::Dispatched | State::Effected | State::Aborted | State::Done | State::Failed
            ) {
                f.orders.push(running(view, uid, true));
            }
            continue;
        }
        if view.committed.contains(uid) {
            if op == Some("failed") {
                f.unknown = Some(format!(
                    "its {} (intent {}) failed after it started: what it does now is not known",
                    view.spec.kind().as_str(),
                    view.id
                ));
                break;
            }
            f.orders.push(running(view, uid, false));
            if view.spec.kind() != TaskKind::Identify {
                break;
            }
            continue;
        }
        let may = view.maybe_started.contains(uid)
            || (view.state == State::Dispatched
                && !matches!(op, Some("requeued" | "failed" | "superseded" | "done")));
        if may {
            f.orders.push(running(view, uid, true));
        }
    }
    f
}

// ---- shapes -----------------------------------------------------------------------------------

/// A convex outline (one point: a disc; two: a corridor; more: a polygon) and a pad around it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Shape {
    pub points: Vec<LonLat>,
    pub pad_m: f64,
}

/// East and north metres of `p` from `o` (flat, as [`engage::enu`]).
fn xy(o: LonLat, p: LonLat) -> (f64, f64) {
    engage::enu(o, p)
}

fn cross(o: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

/// The nearest point of segment a-b to the origin, and its distance (flat metres).
fn seg_nearest(a: (f64, f64), b: (f64, f64)) -> ((f64, f64), f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (-(a.0 * dx + a.1 * dy) / len2).clamp(0.0, 1.0)
    };
    let p = (a.0 + t * dx, a.1 + t * dy);
    (p, p.0.hypot(p.1))
}

impl Shape {
    pub fn disc(center: LonLat, radius_m: f64) -> Shape {
        Shape {
            points: vec![center],
            pad_m: radius_m.max(0.0),
        }
    }

    pub fn corridor(a: LonLat, b: LonLat, half_width_m: f64) -> Shape {
        Shape {
            points: if a == b { vec![a] } else { vec![a, b] },
            pad_m: half_width_m.max(0.0),
        }
    }

    /// The convex hull of `points` (Andrew's monotone chain, flat around the first), padded.
    pub fn hull(points: &[LonLat], pad_m: f64) -> Shape {
        let Some(&o) = points.first() else {
            return Shape { points: Vec::new(), pad_m };
        };
        let mut pts: Vec<(f64, f64, LonLat)> = points
            .iter()
            .map(|p| {
                let (x, y) = xy(o, *p);
                (x, y, *p)
            })
            .collect();
        pts.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6 && (a.1 - b.1).abs() < 1e-6);
        if pts.len() < 3 {
            return Shape {
                points: pts.iter().map(|p| p.2).collect(),
                pad_m: pad_m.max(0.0),
            };
        }
        let c = |a: &(f64, f64, LonLat), b: &(f64, f64, LonLat), d: &(f64, f64, LonLat)| {
            cross((a.0, a.1), (b.0, b.1), (d.0, d.1))
        };
        let mut lower: Vec<(f64, f64, LonLat)> = Vec::new();
        for p in &pts {
            while lower.len() >= 2 && c(&lower[lower.len() - 2], &lower[lower.len() - 1], p) <= 0.0 {
                lower.pop();
            }
            lower.push(*p);
        }
        let mut upper: Vec<(f64, f64, LonLat)> = Vec::new();
        for p in pts.iter().rev() {
            while upper.len() >= 2 && c(&upper[upper.len() - 2], &upper[upper.len() - 1], p) <= 0.0 {
                upper.pop();
            }
            upper.push(*p);
        }
        lower.pop();
        upper.pop();
        lower.extend(upper);
        Shape {
            points: lower.into_iter().map(|p| p.2).collect(),
            pad_m: pad_m.max(0.0),
        }
    }

    /// This shape with `m` more pad.
    pub fn widened(&self, m: f64) -> Shape {
        Shape {
            points: self.points.clone(),
            pad_m: self.pad_m + m.max(0.0),
        }
    }

    /// The outline's nearest point to `q` (`q` itself inside it) and its distance, m.
    pub fn nearest(&self, q: LonLat) -> (LonLat, f64) {
        let pts: Vec<(f64, f64)> = self.points.iter().map(|p| xy(q, *p)).collect();
        let n = pts.len();
        if n == 0 {
            return (q, f64::INFINITY);
        }
        if n >= 3 {
            // counter-clockwise: inside when left of (or on) every edge
            let inside = (0..n).all(|i| cross(pts[i], pts[(i + 1) % n], (0.0, 0.0)) >= 0.0);
            if inside {
                return (q, 0.0);
            }
        }
        let mut best = ((pts[0].0, pts[0].1), pts[0].0.hypot(pts[0].1));
        let edges = if n == 1 { 0 } else if n == 2 { 1 } else { n };
        for i in 0..edges {
            let (p, d) = seg_nearest(pts[i], pts[(i + 1) % n]);
            if d < best.1 {
                best = (p, d);
            }
        }
        (engage::offset(q, best.0.0, best.0.1), best.1)
    }

    /// The edge's distance from `q`: positive outside the shape, negative inside (down to minus
    /// its pad), m.
    pub fn edge_m(&self, q: LonLat) -> f64 {
        self.nearest(q).1 - self.pad_m
    }
}

// ---- the region ---------------------------------------------------------------------------

/// Which hypothesis a part of a silent vehicle's region stands for (docs/NODE-LOSS.md 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hypothesis {
    /// C: it runs its order.
    Order,
    /// L: its lost-link failsafe fired.
    Failsafe,
    /// D: adrift or falling.
    Drift,
    /// F: nothing known, its top speed.
    TopSpeed,
}

/// One shape of a silent vehicle's region: under which hypothesis, how the keep-out's words
/// name it, and what it is in words.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Part {
    pub hypothesis: Hypothesis,
    pub kind: FriendlyKind,
    pub shape: Shape,
    pub what: String,
}

/// Where a silent vehicle can be at `at`: its last report (`p0` at `t0`), the parts (a union),
/// and the reach that caps them all (its top speed, with the water's current or the wind, since
/// `t0`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Region {
    pub p0: LonLat,
    pub t0: Time,
    pub at: Time,
    pub span_s: f64,
    /// The speed of the reach, m/s.
    pub reach_mps: f64,
    pub reach: Shape,
    pub parts: Vec<Part>,
}

impl Region {
    /// The region's edge from `q` (positive outside, negative inside), and the part that sets
    /// it (none: the reach does): the nearest part's, or the reach's when that is farther (the
    /// region is the union capped by the reach, so the farther of the two never overstates the
    /// clearance).
    pub fn edge_m(&self, q: LonLat) -> (f64, Option<usize>) {
        let reach = self.reach.edge_m(q);
        let nearest = self
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| (p.shape.edge_m(q), i))
            .min_by(|a, b| a.0.total_cmp(&b.0));
        match nearest {
            Some((d, i)) if d >= reach => (d, Some(i)),
            // no part (cannot happen: F is a part then) or the reach is farther
            _ => (reach, None),
        }
    }

    /// Whether `q` (a true position) is inside the region, with its margin (negative: by how
    /// much it is outside), m.
    pub fn margin_m(&self, q: LonLat) -> f64 {
        -self.edge_m(q).0
    }
}

/// What a silent vehicle's region is laid out from (its kinematics and its facts at its last
/// report).
struct Kin {
    p0: LonLat,
    span: f64,
    domain: Domain,
    autopilot: Option<AutopilotKind>,
    fixed_wing: bool,
    /// a corridor's half-width, m
    h: f64,
    /// a hold's radius where it stops, m
    hold_r: f64,
    /// a fixed wing's circle radius (its loiter or rtl circle), when its plan says it
    circle_r: Option<f64>,
    /// the tightest circle a fixed wing flies, m
    min_turn: f64,
    home: Option<LonLat>,
    /// how far the water or the wind carries it over the span, m
    drift: f64,
    /// how fast it may go on its own, m/s
    top: f64,
    /// its altitude when last heard (an aircraft's glide or fall), m
    alt: Option<f64>,
    /// the water's (or for an airship the wind's) speed over its own cruise, when it can make way
    /// against it (below 1): a leg is set off by its length times this share
    cross: f64,
    /// what the water or the wind may carry it beyond its route when it cannot make way against
    /// them (its cruise no faster), m
    swept: f64,
}

impl Kin {
    /// A leg from `a` to `b`, `pad` either side, and as far again as a cross current sets it off.
    fn leg(&self, a: LonLat, b: LonLat, pad: f64) -> Shape {
        Shape::corridor(a, b, pad + haversine_m(a, b) * self.cross)
    }
}

fn kind_word(k: TaskKind) -> &'static str {
    k.as_str()
}

/// The speed one of ours may go at on its own, m/s: catalog `max_mps`, else its domain's
/// ([`engage::top_speed_mps`]), and never less than its cruise.
pub fn top_speed(v: &VehicleFacts) -> f64 {
    let p = v.platform.as_ref();
    let domain = p.map_or(Domain::Air, |p| p.domain);
    let cruise = p.and_then(|p| p.cruise_mps).filter(|c| c.is_finite()).unwrap_or(0.0);
    p.and_then(|p| p.max_mps)
        .filter(|m| m.is_finite())
        .unwrap_or_else(|| engage::top_speed_mps(domain))
        .max(cruise)
}

/// How fast the water or the wind may carry one of ours beyond its own speed, m/s: the current
/// on the water (both the surface's figure: the simulator's water runs it at every depth), the
/// wind for an airship, nothing else.
pub fn carried_mps(v: &VehicleFacts) -> f64 {
    let p = v.platform.as_ref();
    match (p.map(|p| p.domain), engage::platform_autopilot(v)) {
        (Some(Domain::Surface), _) => SURFACE_DRIFT_MPS,
        (Some(Domain::Subsurface), _) => SUBSURFACE_DRIFT_MPS,
        (_, Some(AutopilotKind::Blimp)) => AIR_DRIFT_MPS,
        _ => 0.0,
    }
}

/// Its drift speed when it makes no way (D), m/s: the water's, the wind's for an airship.
fn drift_mps(v: &VehicleFacts) -> f64 {
    let p = v.platform.as_ref();
    match (p.map(|p| p.domain), engage::platform_autopilot(v)) {
        (Some(Domain::Surface), _) => SURFACE_DRIFT_MPS,
        (Some(Domain::Subsurface), _) => SUBSURFACE_DRIFT_MPS,
        (_, Some(AutopilotKind::Blimp)) => AIR_DRIFT_MPS,
        _ => 0.0,
    }
}

/// Rules 30: where silent vehicle `uid` can be at `at` (docs/NODE-LOSS.md 3), from its facts in
/// `world`, the orders `world.silent_orders` says may be running on it (none listed: nothing is
/// known of them, F) and whether it is presumed lost (`presumed`, or the presumption its
/// `silent_orders` carries: D). None without a position.
pub fn region(
    uid: &Uid,
    v: &VehicleFacts,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    at: Time,
    presumed: bool,
) -> Option<Region> {
    let pos = v.position.as_ref()?;
    let (p0, t0) = (pos.value, pos.t);
    let span = (at.ms_since(t0) as f64 / 1000.0).max(0.0);
    let p = v.platform.as_ref();
    let domain = p.map_or(Domain::Air, |p| p.domain);
    let autopilot = engage::platform_autopilot(v);
    let fixed_wing = autopilot.is_some_and(AutopilotKind::is_fixed_wing);
    let ec = cfg.engage_rules();
    let cruise = p.and_then(|p| p.cruise_mps).filter(|c| c.is_finite() && *c >= 0.0).unwrap_or(0.0);
    // its velocity with its last position (the same report), else none known
    let vel = v
        .velocity_mps
        .as_ref()
        .filter(|f| f.t.ms_since(t0).abs() <= i64::from(v.report_interval_ms.unwrap_or(1000)))
        .map(|f| f.value);
    let speed0 = vel.map(|[e, n]| e.hypot(n));
    let interval_s = f64::from(v.report_interval_ms.unwrap_or(1000)) / 1000.0;
    let moving = speed0.unwrap_or(cruise).max(0.0);
    // its turning radius: the catalog's, else at DEFAULT_TURN_DPS (a copter and an airship turn
    // in place); a fixed wing's never under the policy's floor for an orbit
    let turn_r = if matches!(autopilot, Some(AutopilotKind::Copter | AutopilotKind::Blimp)) {
        0.0
    } else {
        p.and_then(|p| p.min_turn_radius_m).unwrap_or(cruise / DEFAULT_TURN_DPS.to_radians())
    };
    let min_turn = if fixed_wing { turn_r.max(cfg.fixed_wing_default_min_turn_radius_m) } else { 0.0 };
    let turn_r = turn_r.max(min_turn);
    // where in its report interval it was, and the swing of a turn back at a waypoint (its
    // diameter to the side)
    let h = PATH_TOLERANCE_M + moving * interval_s / 2.0 + 2.0 * turn_r;
    // the water (the wind for an airship) against its own speed
    let carried = carried_mps(v);
    let (cross, swept) = if carried <= 0.0 {
        (0.0, 0.0)
    } else if cruise > carried {
        (CROSS_SET_SHARE * carried / cruise, 0.0)
    } else {
        (0.0, carried * span)
    };
    // a fixed wing holds and arrives home on a circle of its own loiter radius, a parameter of
    // the vehicle's that the bridge reads with its lost-link plan: not known, nothing bounds
    // that circle but its reach
    let top = top_speed(v);
    let circle_r = v.lost_link.as_ref().and_then(|l| l.value.circle_m).map(|r| r.max(min_turn));
    let hold_r = if fixed_wing {
        // it circles where it is: a circle through or near where it was heard
        circle_r.map_or(top * span, |r| 2.0 * r + h)
    } else {
        // braking from its speed, and circling back on its turn
        HOLD_RADIUS_M + moving * interval_s / 2.0 + moving * moving / (2.0 * BRAKE_MPS2) + 2.0 * turn_r
    };
    let home = v.home.as_ref().map(|h| h.value);
    let drift = drift_mps(v) * span;
    let reach_mps = top + carried;
    let k = Kin {
        p0,
        span,
        domain,
        autopilot,
        fixed_wing,
        h,
        hold_r,
        circle_r,
        min_turn,
        home,
        drift,
        top,
        alt: v.alt_msl_m.as_ref().map(|a| a.value.max(0.0)),
        cross,
        swept,
    };
    let reach = Shape::disc(p0, reach_mps * span);
    let mut parts: Vec<Part> = Vec::new();
    let mobility = v.damage.as_ref().is_some_and(|d| d.value.mobility);
    let in_force = world.silent_orders.get(uid);
    let presumed = presumed || in_force.is_some_and(|f| f.presumed);
    // D: presumed lost, out of power, or (alone) its propulsion out
    let endurance_s = p.and_then(|p| p.endurance_h).map(|e| e * 3600.0);
    let battery = v.battery_pct.as_ref();
    let left_s = match (battery, endurance_s) {
        (Some(b), Some(e)) => Some(b.value / 100.0 * e - at.ms_since(b.t) as f64 / 1000.0),
        _ => None,
    };
    let out_of_power = left_s.is_none_or(|l| l <= 0.0);
    let drifts = autopilot == Some(AutopilotKind::Drifter);
    if mobility || presumed || out_of_power || drifts {
        let why = if mobility {
            "its propulsion is out (its own damage report)".to_string()
        } else if drifts {
            "a drifter: the water carries it".to_string()
        } else if presumed {
            "presumed lost".to_string()
        } else if left_s.is_none() {
            "its endurance left is not known".to_string()
        } else {
            "past the endurance its last battery report leaves".to_string()
        };
        parts.push(drift_part(&k, &why));
    }
    if !mobility && !drifts {
        let (mut c, known) = order_parts(&k, v, in_force, &ec);
        // it cannot make way against the water (or an airship the wind): carried off its way
        if k.swept > 0.0 {
            for p in &mut c {
                p.shape = p.shape.widened(k.swept);
                p.what = format!("{}, and carried off it by the water or the wind (up to {:.0} m)", p.what, k.swept);
            }
        }
        // F: nothing it was told is known to run, and no circle or stillness to go on
        if !known {
            let why = match in_force {
                None => "the orders that may be running on it are not known here".to_string(),
                Some(f) => f
                    .unknown
                    .clone()
                    .unwrap_or_else(|| "no order of ours is known to run on it, and it was neither circling nor still".into()),
            };
            parts.push(Part {
                hypothesis: Hypothesis::TopSpeed,
                kind: FriendlyKind::TopSpeed,
                shape: reach.clone(),
                what: format!("anywhere its top speed ({top:.0} m/s) reaches since its last report: {why}"),
            });
        }
        // L: its lost-link failsafe, over C (its own plan, and an engagement's preload on it)
        let mut actions: Vec<(LostLinkAction, String)> = Vec::new();
        match &v.lost_link {
            Some(l) => actions.push((l.value.action, format!("its {} failsafe", action_word(l.value.action)))),
            None => {
                for a in all_actions(autopilot, domain) {
                    actions.push((a, format!("its failsafe (not known here: say {})", action_word(a))));
                }
            }
        }
        for o in in_force.map_or(&[][..], |f| &f.orders) {
            if let Some(a) = o.preload {
                actions.push((a, format!("the {} preloaded by engagement {}", action_word(a), o.intent)));
            }
        }
        // the battery's own failsafe may send it home (or down) before its endurance runs out
        let low = match (battery, endurance_s) {
            (Some(b), Some(e)) if e > 0.0 => b.value - 100.0 * (at.ms_since(b.t) as f64 / 1000.0) / e <= BATTERY_FAILSAFE_PCT,
            _ => true,
        };
        if low && has_rtl(autopilot) {
            actions.push((LostLinkAction::Rtl, "its battery failsafe".to_string()));
        }
        let mut seen: Vec<LostLinkAction> = Vec::new();
        for (a, why) in actions {
            if seen.contains(&a) {
                continue;
            }
            seen.push(a);
            failsafe_parts(&k, &c, a, &why, &mut parts);
        }
        parts.extend(c);
    }
    Some(Region {
        p0,
        t0,
        at,
        span_s: span,
        reach_mps,
        reach,
        parts,
    })
}

fn action_word(a: LostLinkAction) -> &'static str {
    match a {
        LostLinkAction::Off => "off",
        LostLinkAction::Continue => "continue",
        LostLinkAction::Rtl => "rtl",
        LostLinkAction::SmartRtl => "smart rtl",
        LostLinkAction::Hold => "hold",
        LostLinkAction::Loiter => "loiter",
        LostLinkAction::Brake => "brake",
        LostLinkAction::Land => "land",
        LostLinkAction::Glide => "glide",
        LostLinkAction::DepthHold => "depth hold",
        LostLinkAction::Surface => "surface",
        LostLinkAction::Disarm => "disarm",
    }
}

/// Whether its autopilot has a way home of its own (an rtl).
fn has_rtl(a: Option<AutopilotKind>) -> bool {
    !matches!(a, Some(AutopilotKind::Sub | AutopilotKind::Tracker | AutopilotKind::Drifter))
}

/// Every lost-link action an autopilot of this kind has (the plan not known: each may be it).
fn all_actions(a: Option<AutopilotKind>, domain: Domain) -> Vec<LostLinkAction> {
    use LostLinkAction as L;
    match a {
        Some(AutopilotKind::Plane | AutopilotKind::Vtol) => vec![L::Rtl, L::Loiter, L::Glide],
        Some(AutopilotKind::Copter) => vec![L::Rtl, L::Brake, L::Land],
        Some(AutopilotKind::Boat) => vec![L::Rtl, L::Hold, L::Loiter, L::Disarm],
        Some(AutopilotKind::Ugv) => vec![L::Rtl, L::Hold, L::Disarm],
        Some(AutopilotKind::Sub) => vec![L::DepthHold, L::Surface, L::Disarm],
        Some(AutopilotKind::Blimp) => vec![L::Disarm, L::Land],
        Some(AutopilotKind::Tracker | AutopilotKind::Drifter) => Vec::new(),
        None => match domain {
            Domain::Air => vec![L::Rtl, L::Loiter, L::Glide, L::Land],
            Domain::Ground => vec![L::Rtl, L::Hold],
            Domain::Surface => vec![L::Rtl, L::Hold, L::Disarm],
            Domain::Subsurface => vec![L::DepthHold, L::Surface, L::Disarm],
        },
    }
}

/// D: adrift or falling from where it was last heard.
fn drift_part(k: &Kin, why: &str) -> Part {
    let alt = k.alt;
    let (r, what) = match (k.domain, k.autopilot) {
        (Domain::Surface, _) => (k.drift, format!("adrift at up to {SURFACE_DRIFT_MPS} m/s since its last report ({why})")),
        (Domain::Subsurface, _) => (k.drift, format!("adrift at up to {SUBSURFACE_DRIFT_MPS} m/s since its last report ({why})")),
        (Domain::Ground, _) => (GROUND_DRIFT_M, format!("within {GROUND_DRIFT_M:.0} m of where it was last heard ({why})")),
        (Domain::Air, Some(AutopilotKind::Blimp)) => (k.drift, format!("carried by the wind at up to {AIR_DRIFT_MPS} m/s ({why})")),
        (Domain::Air, _) => match alt {
            Some(a) => {
                let ratio = if k.fixed_wing { GLIDE_RATIO } else { ROTARY_FALL_RATIO };
                (a * ratio, format!("coming down from {a:.0} m, at most {ratio:.0} m sideways for each metre ({why})"))
            }
            // no altitude to bound its fall by: its reach
            None => (k.top * k.span, format!("coming down from an altitude not known: its reach ({why})")),
        },
    };
    Part {
        hypothesis: Hypothesis::Drift,
        kind: FriendlyKind::Drift,
        shape: Shape::disc(k.p0, r),
        what,
    }
}

/// C: the shapes of every order that may be running on it, its circle and its stillness when
/// last heard; and whether anything places it (an order it started, a circle, stillness).
fn order_parts(
    k: &Kin,
    v: &VehicleFacts,
    in_force: Option<&InForce>,
    ec: &engage::EngageRules,
) -> (Vec<Part>, bool) {
    let mut out: Vec<Part> = Vec::new();
    // placed: an order it started, a circle it flew or stillness when last heard; not when its
    // orders are not known here or the newest it started failed, nor when an order that may run
    // goes somewhere not known (home)
    let mut placed = in_force.is_some_and(InForce::started);
    let mut unbounded = in_force.is_none_or(|f| f.unknown.is_some());
    let route = |shape: Shape, what: String| Part {
        hypothesis: Hypothesis::Order,
        kind: FriendlyKind::Route,
        shape,
        what,
    };
    // a tracker never moves
    if k.autopilot == Some(AutopilotKind::Tracker) {
        out.push(route(Shape::disc(k.p0, HOLD_RADIUS_M), "an antenna tracker: where it stands".into()));
        return (out, true);
    }
    for o in in_force.map_or(&[][..], |f| &f.orders) {
        let name = format!(
            "its {} (intent {}{})",
            kind_word(o.kind),
            o.intent,
            if o.unconfirmed { ", its start not confirmed" } else { "" }
        );
        let h = k.h;
        // from where it was heard to any of its waypoints, then along them
        for w in &o.path {
            out.push(route(k.leg(k.p0, *w, h), format!("on {name}: on its way to a waypoint, within {h:.0} m")));
        }
        for pair in o.path.windows(2) {
            out.push(route(k.leg(pair[0], pair[1], h), format!("on {name}: along its route, within {h:.0} m")));
        }
        if let (Some(j), Some(last)) = (o.loop_from, o.path.last())
            && let Some(first) = o.path.get(j)
        {
            out.push(route(k.leg(*last, *first, h), format!("on {name}: along its route's loop, within {h:.0} m")));
        }
        for c in &o.circles {
            let r = if k.fixed_wing { c.radius_m.max(k.min_turn) } else { c.radius_m };
            let pad = r + h;
            out.push(route(Shape::disc(c.center, pad), format!("on {name}: its circle of {r:.0} m, within {h:.0} m")));
            out.push(route(k.leg(k.p0, c.center, pad), format!("on {name}: on its way to its circle of {r:.0} m")));
            if let Some(last) = o.path.last() {
                out.push(route(k.leg(*last, c.center, pad), format!("on {name}: on its way to its circle of {r:.0} m")));
            }
        }
        if o.home
            && let Some(home) = k.home
        {
            out.push(route(k.leg(k.p0, home, h), format!("on {name}: on its way home, within {h:.0} m")));
            if let Some(last) = o.path.last() {
                out.push(route(k.leg(*last, home, h), format!("on {name}: on its way home, within {h:.0} m")));
            }
            out.push(route(home_arrival(k), format!("on {name}: at home")));
        } else if o.home {
            // home not known: nothing bounds its way there
            unbounded = true;
        }
        // stopped where it is (a hold, a tracker's track) or broken off on its way (an
        // engagement's fallback hold or loiter)
        if matches!(o.kind, TaskKind::Hold | TaskKind::Track) {
            out.push(route(Shape::disc(k.p0, k.hold_r), format!("{name}: holding where it was heard, within {:.0} m", k.hold_r)));
        }
        match o.fallback {
            Some(Fallback::Return) => {
                if let Some(home) = k.home {
                    for w in &o.path {
                        out.push(route(k.leg(*w, home, h), format!("{name}: its fallback home")));
                    }
                    out.push(route(k.leg(k.p0, home, h), format!("{name}: its fallback home")));
                    out.push(route(home_arrival(k), format!("{name}: its fallback home, arrived")));
                } else {
                    unbounded = true;
                }
            }
            Some(Fallback::Abort | Fallback::Loiter) => {
                for w in &o.path {
                    out.push(route(k.leg(k.p0, *w, h + k.hold_r), format!("{name}: its fallback, held where it broke off")));
                }
            }
            None => {}
        }
    }
    // what it was doing when last heard: flying a circle, or still
    if let Some(c) = circle_at_report(v) {
        let pad = c.1 + engage::CIRCLE_SLACK_M + k.h;
        out.push(route(Shape::disc(c.0, pad), format!("on the circle of {:.0} m it was flying when last heard", c.1)));
        placed = true;
    }
    let still = v
        .velocity_mps
        .as_ref()
        .filter(|f| f.t.ms_since(v.position.as_ref().map_or(f.t, |p| p.t)).abs() <= i64::from(v.report_interval_ms.unwrap_or(1000)))
        .is_some_and(|f| f.value[0].hypot(f.value[1]) < ec.holding_speed());
    if still {
        out.push(route(Shape::disc(k.p0, k.hold_r), format!("still when last heard: within {:.0} m of where it was", k.hold_r)));
        placed = true;
    }
    (out, placed && !unbounded)
}

/// Its arrival home: a fixed wing circles it, anything else stops (or holds station) there.
fn home_arrival(k: &Kin) -> Shape {
    let home = k.home.unwrap_or(k.p0);
    if k.fixed_wing {
        Shape::disc(home, k.circle_r.map_or(k.top * k.span, |r| r + k.h))
    } else {
        // stopped or holding station there, after circling back on its turn (in `h`)
        Shape::disc(home, HOLD_RADIUS_M + k.h)
    }
}

/// The circle a vehicle was flying when last heard: its circle fact, reported with its last
/// position (not older than [`engage::CIRCLE_CENTER_MAX_AGE_MS`] then) and holding it there
/// (within the radius and [`engage::CIRCLE_SLACK_M`] of the centre), as rules 3 judge it on a
/// fresh one ([`engage::circle_holding`]).
fn circle_at_report(v: &VehicleFacts) -> Option<(LonLat, f64)> {
    let pos = v.position.as_ref()?;
    let c = v.circling.as_ref()?;
    let interval = i64::from(v.report_interval_ms.unwrap_or(1000));
    let center_t = c.value.center_t.unwrap_or(c.t);
    let fresh_then = pos.t.ms_since(c.t) <= interval.max(engage::CIRCLE_CENTER_MAX_AGE_MS)
        && pos.t.ms_since(center_t) <= engage::CIRCLE_CENTER_MAX_AGE_MS;
    (fresh_then && haversine_m(pos.value, c.value.center) <= c.value.radius_m + engage::CIRCLE_SLACK_M)
        .then_some((c.value.center, c.value.radius_m))
}

/// L: its lost-link action `a` over C's shapes `c` (why: whose action it is).
fn failsafe_parts(k: &Kin, c: &[Part], a: LostLinkAction, why: &str, out: &mut Vec<Part>) {
    use LostLinkAction as L;
    let on_water = matches!(k.domain, Domain::Surface | Domain::Subsurface);
    let widen = |w: f64, kind: FriendlyKind, what: String, out: &mut Vec<Part>| {
        for p in c {
            out.push(Part {
                hypothesis: Hypothesis::Failsafe,
                kind,
                shape: p.shape.widened(w),
                what: what.clone(),
            });
        }
        // nothing in C (it was placed by its reach alone): from where it was heard
        if c.is_empty() {
            out.push(Part {
                hypothesis: Hypothesis::Failsafe,
                kind,
                shape: Shape::disc(k.p0, k.hold_r + w),
                what: what.clone(),
            });
        }
    };
    match a {
        L::Off | L::Continue => {}
        L::Rtl | L::SmartRtl => {
            let Some(home) = k.home else {
                // no home known: nothing bounds its way there but its reach
                out.push(Part {
                    hypothesis: Hypothesis::Failsafe,
                    kind: FriendlyKind::TopSpeed,
                    shape: Shape::disc(k.p0, k.top * k.span),
                    what: format!("if it lost us too, {why} takes it to a home not known here: its reach"),
                });
                return;
            };
            let what = format!("if it lost us too, {why}: from anywhere on its way, straight home");
            for p in c {
                let mut pts = p.shape.points.clone();
                // the longest way home from it, set off by a cross current
                let far = pts.iter().map(|q| haversine_m(*q, home)).fold(0.0, f64::max) + p.shape.pad_m;
                pts.push(home);
                out.push(Part {
                    hypothesis: Hypothesis::Failsafe,
                    kind: FriendlyKind::FailsafeHome,
                    shape: Shape::hull(&pts, p.shape.pad_m.max(k.h) + far * k.cross),
                    what: what.clone(),
                });
            }
            out.push(Part {
                hypothesis: Hypothesis::Failsafe,
                kind: FriendlyKind::FailsafeHome,
                shape: k.leg(k.p0, home, k.h),
                what: what.clone(),
            });
            out.push(Part {
                hypothesis: Hypothesis::Failsafe,
                kind: FriendlyKind::FailsafeHome,
                shape: home_arrival(k),
                what: format!("if it lost us too, {why}: at home"),
            });
        }
        L::Hold | L::Brake | L::Land if !on_water || k.domain == Domain::Ground => {
            widen(0.0, FriendlyKind::Route, format!("if it lost us too, {why}: stopped on its way"), out);
        }
        L::Loiter if k.fixed_wing => {
            let r = k.circle_r.map_or(k.top * k.span, |r| 2.0 * r);
            widen(r, FriendlyKind::Route, format!("if it lost us too, {why}: circling where it was on its way"), out);
        }
        L::Loiter if !on_water || k.autopilot == Some(AutopilotKind::Boat) => {
            widen(0.0, FriendlyKind::Route, format!("if it lost us too, {why}: holding station on its way"), out);
        }
        L::Glide => {
            // its altitude bounds the glide (none known: its reach)
            let r = k.alt.map_or(k.top * k.span, |a| a * GLIDE_RATIO);
            widen(r, FriendlyKind::Drift, format!("if it lost us too, {why}: gliding down from its way"), out);
        }
        L::Disarm if k.domain == Domain::Air && k.autopilot != Some(AutopilotKind::Blimp) => {
            let ratio = if k.fixed_wing { GLIDE_RATIO } else { ROTARY_FALL_RATIO };
            let r = k.alt.map_or(k.top * k.span, |a| a * ratio);
            widen(r, FriendlyKind::Drift, format!("if it lost us too, {why}: falling from its way"), out);
        }
        L::Disarm if k.domain == Domain::Ground => {
            widen(0.0, FriendlyKind::Route, format!("if it lost us too, {why}: stopped on its way"), out);
        }
        // a passive hold, a depth hold, a surfacing or a disarm on the water (an airship's in the
        // wind): stopped on its way, then adrift
        L::Hold | L::Brake | L::Land | L::Loiter | L::DepthHold | L::Surface | L::Disarm => {
            widen(k.drift, FriendlyKind::Drift, format!("if it lost us too, {why}: stopped on its way, then adrift"), out);
        }
    }
}

// ---- the keep-out -----------------------------------------------------------------------------

/// Rules 30: a silent vehicle (`v`, not the platform; its position not fresh) against a
/// keep-out reaching `keep_out_m` from `point` at `effect_t`, as
/// [`engage::friendly_standing`] judges it from rules 30: its [`region`] at the effect time, the
/// part nearest the point named in `how`. `presumed`: presumed lost (D).
#[allow(clippy::too_many_arguments)]
pub fn standing(
    uid: &Uid,
    v: &VehicleFacts,
    point: LonLat,
    effect_t: Time,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    presumed: bool,
    words: bool,
) -> FriendlyStanding {
    let Some(r) = region(uid, v, world, cfg, effect_t, presumed) else {
        return FriendlyStanding::Unclearable {
            kind: FriendlyKind::NoPosition,
            detail: format!("vehicle {uid} has no position: cannot clear the keep-out"),
        };
    };
    let silent_s = world.t.ms_since(r.t0) as f64 / 1000.0;
    let (edge, part) = r.edge_m(point);
    let (kind, center, radius_m, outline, what) = match part.map(|i| &r.parts[i]) {
        Some(p) => {
            let (c, _) = p.shape.nearest(point);
            let outline = if p.shape.points.len() > 1 { p.shape.points.clone() } else { Vec::new() };
            (p.kind, c, p.shape.pad_m, outline, p.what.as_str())
        }
        None => (
            FriendlyKind::TopSpeed,
            r.p0,
            r.reach.pad_m,
            Vec::new(),
            "capped by its reach at top speed",
        ),
    };
    let how: std::borrow::Cow<'static, str> = if words {
        format!("silent {silent_s:.0} s: {what}").into()
    } else {
        "".into()
    };
    FriendlyStanding::Judged {
        kind,
        center,
        radius_m,
        d: edge.max(0.0),
        how,
        by_alt: None,
        alt_known: v.alt_msl_m.is_some(),
        outline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ll(lon: f64, lat: f64) -> LonLat {
        LonLat::new(lon, lat)
    }

    #[test]
    fn a_disc_a_corridor_and_a_hull_measure_their_edges() {
        let o = ll(-122.48, 37.82);
        let d = Shape::disc(o, 100.0);
        let east = engage::offset(o, 300.0, 0.0);
        assert!((d.edge_m(east) - 200.0).abs() < 0.5);
        assert!((d.edge_m(o) + 100.0).abs() < 1e-6);
        let c = Shape::corridor(o, east, 50.0);
        let mid_north = engage::offset(o, 150.0, 120.0);
        assert!((c.edge_m(mid_north) - 70.0).abs() < 0.5);
        let north = engage::offset(o, 0.0, 400.0);
        let h = Shape::hull(&[o, east, north, engage::offset(o, 50.0, 50.0)], 10.0);
        assert_eq!(h.points.len(), 3, "the inner point is not on the hull");
        assert!((h.edge_m(engage::offset(o, 50.0, 50.0)) + 10.0).abs() < 1e-6, "inside: minus its pad");
        let beyond = engage::offset(o, -100.0, 0.0);
        assert!((h.edge_m(beyond) - 90.0).abs() < 0.5);
    }
}
