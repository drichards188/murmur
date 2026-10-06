//! Why an engagement's keep-out blocks, and what would clear it (murmur-12e): a read-only
//! explanation for the nomination form's dry run (`POST /api/engage/options`) and agents'
//! reads. It is not a decision and is not logged: the decision is the nomination and its
//! signatures, each checked again by the engine. It relaxes nothing and overrides nothing.
//! It reads the functions the keep-out checks read ([`engage::track_standing`],
//! [`engage::friendly_standing`], [`engage::preview`], [`engage::effect_alt`]) on the
//! snapshot the dry run checked, and refuses to explain when its blocking items do not
//! match the checks' outcomes, so it cannot disagree with them.
//!
//! For every track or vehicle of ours that blocks the keep-out, or nearly does (its nearest
//! region's edge within [`NEAR_FACTOR`] times the clearance of the effect point), an
//! [`Item`] says:
//!
//! - which region made it block: along its velocity, along its average velocity, a
//!   manoeuvre's reach at the fastest it went, a young track's reach, no velocity; for ours,
//!   along its velocity, its circle, its cruise reach, lost; and whether altitude counted;
//! - the numbers: that region at the effect time (centre, radius), the effect circle, the
//!   clearance the keep-out needs (the effect radius plus `keep_out_m`) and by how much the
//!   region fails it;
//! - what would clear it, each under a stated assumption:
//!   - [`Item::fits_until_t`], on the same picture: the latest effect time up to which every
//!     earlier effect leaves it clear (a shorter span: the platform closer);
//!   - [`Item::wait`], on the picture carried forward ([`project`], "if it keeps behaving as
//!     now"): the first nomination time at which a dry run would find it clear, and that
//!     nomination's effect time (a young track gets its average velocity, a manoeuvre ages
//!     out of the tracker's memory, its predicted region moves off);
//!   - [`Item::not_by_waiting`] and [`Item::other`] when waiting does not clear it (lost,
//!     not in view), with what would (a report, an operator's logged acceptance).
//!
//! and, for the platform, [`MoveWithin`]: how close it would need to be for every keep-out
//! to hold, and [`Explanation::wait`]: the first nomination time at which every keep-out
//! would hold. `tests/keepout.rs` checks each claim against the checks themselves: a dry run
//! at the stated time, or with the platform within the stated distance, passes the keep-outs;
//! a second earlier (where testable, one step farther) it does not.

use serde::Serialize;

use crate::catalog::Domain;
use crate::engage::{
    self, EffectAlt, EngagePreview, EngageRules, FriendlyKind, FriendlyStanding, ReachKind,
    TrackStanding,
};
use crate::geo::{LonLat, haversine_m};
use crate::ids::Uid;
use crate::intent::{IntentSpec, Task};
use crate::rules::{CheckResult, Outcome, RuleId, RulesConfig, fresh};
use crate::time::Time;
use crate::world::{Fact, TrackFacts, VehicleFacts, WorldSnapshot};

/// An item nearly blocks when its nearest region's edge is within this many times the
/// clearance of the effect point (and it does not block).
pub const NEAR_FACTOR: f64 = 2.0;
/// How far ahead [`Item::wait`] and [`Explanation::wait`] look, s.
pub const WAIT_HORIZON_S: i64 = 600;
/// Their grid, ms: the first passing nomination time on it.
pub const WAIT_STEP_MS: i64 = 1_000;
/// The grid of effect times [`Item::fits_until_t`] and [`MoveWithin`] scan, ms.
pub const SPAN_STEP_MS: i64 = 250;
/// A manoeuvre seen this recently before a track's estimate means it is manoeuvring now, s.
pub const MANOEUVRING_NOW_S: f64 = 30.0;

/// The tracker's memories a projection ages ([`project`]): how long a track must be followed
/// before it has an average velocity, over how long that average is taken, and how long a
/// manoeuvre is remembered. The bridge passes its tracker's own constants; the defaults are
/// them (`tracks/model.rs`: `MEAN_MIN_S`, `MEAN_WINDOW_S`, `MANOEUVRE_MEMORY_S`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TrackerMemory {
    pub mean_min_s: f64,
    pub mean_window_s: f64,
    pub manoeuvre_memory_s: f64,
}

impl Default for TrackerMemory {
    fn default() -> Self {
        TrackerMemory {
            mean_min_s: 45.0,
            mean_window_s: 60.0,
            manoeuvre_memory_s: 300.0,
        }
    }
}

/// See the module.
#[derive(Clone, Debug, Serialize)]
pub struct Explanation {
    /// The snapshot's time (the dry run's).
    pub t: Time,
    pub track: String,
    pub platform: Option<Uid>,
    pub effect_point: LonLat,
    pub effect_t: Time,
    pub effect_radius_m: f64,
    /// How far every region must stay from the effect point: the effect radius plus the
    /// policy's `keep_out_m` (the preview's `keep_out_m`), m.
    pub clearance_m: f64,
    /// The effect's altitude band when the keep-out is 3D (the target's altitude known).
    pub vertical: Option<Vertical>,
    /// Whether either keep-out check fails on this snapshot.
    pub blocked: bool,
    /// The checks themselves on this snapshot, as the dry run reports them.
    pub checks: Vec<CheckResult>,
    /// Blocking first (the largest shortfall first), then those that nearly block.
    pub items: Vec<Item>,
    /// For the platform, when blocked: how close it would need to be.
    pub move_within: Option<MoveWithin>,
    /// When blocked: the first nomination time at which every keep-out would hold, if
    /// everything keeps behaving as now.
    pub wait: Option<Wait>,
    /// When blocked and no such time is found: why.
    pub wait_none: Option<String>,
    /// The assumptions behind the hints.
    pub assumptions: String,
    pub note: &'static str,
}

/// See [`Explanation::vertical`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Vertical {
    pub alt_m: f64,
    pub margin_m: f64,
    pub half_height_m: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    Track,
    Vehicle,
}

/// One track or vehicle of ours against the keep-out. See the module.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    /// The check it stands under: `keep_out_tracks` or `keep_out_friendly`.
    pub rule: RuleId,
    pub subject: Subject,
    /// The track id, or the vehicle's uid.
    pub id: String,
    /// As a check's detail names it: "T-0003 (neutral, ferry)", "vehicle 4".
    pub label: String,
    pub domain: Option<Domain>,
    /// Where it is now: a track's estimate carried to now, a vehicle's last report.
    pub now: Option<LonLat>,
    /// Whether it keeps the keep-out from holding (the check fails on it).
    pub blocks: bool,
    /// The region the check reads (its edge nearest the effect point); none for a vehicle
    /// that can't be placed at all.
    pub region: Option<Region>,
    /// Every region it counts in (a track's), the nearest among them.
    pub regions: Vec<Region>,
    /// The nearest region's edge from the effect point (negative: the region covers it by
    /// that much), m.
    pub edge_m: Option<f64>,
    /// The clearance less that edge: positive, by how much it fails; negative, by how much
    /// it clears, m.
    pub short_m: Option<f64>,
    /// How altitude counted, when it is relevant.
    pub altitude: Option<String>,
    /// In 2D inside the keep-out, but clear of it by altitude.
    pub clear_by_altitude: Option<String>,
    /// In plain words: why, with the numbers.
    pub reason: String,
    /// A track the tracker saw manoeuvre: whether that was within [`MANOEUVRING_NOW_S`] of
    /// its estimate (it is manoeuvring now, and a wait hint holds only if it stops).
    pub manoeuvring_now: Option<bool>,
    /// Same picture, when it blocks: every effect time from the earliest (the dispatch
    /// delay, the platform already at the point) up to this one leaves it clear: the span
    /// its reach fits in. None: not even the earliest.
    pub fits_until_t: Option<Time>,
    /// When it blocks: the first nomination time at which a dry run would find it clear if
    /// it (and the target) keep behaving as now and the platform holds where it is.
    pub wait: Option<Wait>,
    /// When it blocks and waiting does not clear it: why.
    pub not_by_waiting: Option<String>,
    /// Something else that would clear it (never an override).
    pub other: Option<String>,
}

/// One region of an item at the effect time.
#[derive(Clone, Debug, Serialize)]
pub struct Region {
    /// velocity, mean_velocity, manoeuvre, young, no_velocity (a track); velocity, circling,
    /// cruise_reach, lost (one of ours).
    pub kind: &'static str,
    pub center: LonLat,
    pub radius_m: f64,
    /// Its edge from the effect point (negative inside), m.
    pub edge_m: f64,
    /// Whether this region alone reaches into the clearance (in 2D).
    pub inside: bool,
    /// The check's own words for it.
    pub how: String,
}

/// A nomination time and its effect time: see [`Item::wait`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Wait {
    pub nominate_t: Time,
    pub effect_t: Time,
    pub why: String,
}

/// See [`Explanation::move_within`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MoveWithin {
    /// Whether any position of the platform lets every keep-out hold on this picture.
    pub possible: bool,
    /// Every effect time from the earliest up to this one passes the keep-outs.
    pub effect_by_t: Option<Time>,
    /// Where the target is predicted then: the platform within `within_m` of it intercepts
    /// by `effect_by_t`.
    pub point: Option<LonLat>,
    /// The platform's cruise speed times the time from its start to `effect_by_t`, m.
    pub within_m: Option<f64>,
    /// The platform's distance from `point` now, m.
    pub now_m: Option<f64>,
    /// The target is predicted where it is (holding): `point` is its position.
    pub holding: bool,
    pub why: String,
}

const NOTE: &str = "Read-only: not a decision, not logged, never an override. The checks decide; this \
says why they block and what would clear them, from the same functions on the same picture.";

fn secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

/// A distance for a sentence: metres below a kilometre, else kilometres.
fn dist(m: f64) -> String {
    if m.abs() >= 1000.0 {
        format!("{:.2} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
    }
}

/// "+95 s" relative to `now`, or minutes and seconds.
fn after(t: Time, now: Time) -> String {
    let s = secs(t.ms_since(now)).round() as i64;
    if s.abs() < 120 {
        format!("{s} s")
    } else {
        format!("{} min {:02} s", s / 60, s.abs() % 60)
    }
}

/// The nomination `spec` made `w` instead of `now`: created then, its window as long.
pub fn spec_at(spec: &IntentSpec, now: Time, w: Time) -> IntentSpec {
    let d = w.ms_since(now);
    let mut s = spec.clone();
    s.created_t = spec.created_t.plus_ms(d);
    if let Task::Engage {
        window: Some(win), ..
    } = &mut s.task
    {
        win.until = win.until.plus_ms(d);
        win.from = win.from.map(|f| f.plus_ms(d));
    }
    s
}

/// Whether a track is in view now: detected within the target recency limit
/// (`max_track_age_s`).
fn in_view(t: &TrackFacts, now: Time, ec: &EngageRules) -> bool {
    secs(now.ms_since(t.last_seen_t)) <= ec.max_track_age_s
}

/// A track carried from `now` to `w` "if it keeps behaving as now", into `out` (a copy of
/// `base`): in view, its estimate moves along its velocity and is stamped `w - now` later
/// (its sources and altitude as recent as now), its uncertainty stays, a manoeuvre older than
/// the memory is forgotten (no new one), a young track gets its velocity as its average once
/// it has been followed `mean_min_s`, and an average velocity becomes its velocity once a
/// whole `mean_window_s` has passed on its course; not in view, it stays where it was seen and
/// its uncertainty grows at its rate.
pub fn project_track_into(
    base: &TrackFacts,
    out: &mut TrackFacts,
    now: Time,
    w: Time,
    mem: &TrackerMemory,
    ec: &EngageRules,
) {
    let d = w.ms_since(now);
    let dt = secs(d).max(0.0);
    if !in_view(base, now, ec) {
        out.pos_sigma_m = base.pos_sigma_m + engage::sigma_growth(base) * dt;
        return;
    }
    let est_t = base.position.t.plus_ms(d);
    out.position = Fact::new(engage::track_at(base, est_t), est_t);
    out.last_seen_t = base.last_seen_t.plus_ms(d);
    out.alt_t = base.alt_t.map(|t| t.plus_ms(d));
    for (k, v) in out.source_t.iter_mut() {
        if let Some(b) = base.source_t.get(k) {
            *v = b.plus_ms(d);
        }
    }
    out.pos_sigma_m = base.pos_sigma_m;
    out.manoeuvre = base
        .manoeuvre
        .filter(|m| secs(est_t.ms_since(m.t)) <= mem.manoeuvre_memory_s);
    out.mean_velocity_mps = match base.mean_velocity_mps {
        Some(mv) if dt >= mem.mean_window_s => Some(base.velocity_mps.unwrap_or(mv)),
        Some(mv) => Some(mv),
        None => base.velocity_mps.filter(|_| {
            base.first_seen_t
                .is_some_and(|f| secs(est_t.ms_since(f)) >= mem.mean_min_s)
        }),
    };
}

fn shifted<T: Clone>(f: &Option<Fact<T>>, d: i64) -> Option<Fact<T>> {
    f.as_ref().map(|f| Fact::new(f.value.clone(), f.t.plus_ms(d)))
}

/// One of our vehicles carried from `now` to `w` "if it keeps doing what it does", into
/// `out` (a copy of `base`): reporting now (`reporting`: its position fresh), every report is
/// `w - now` later and its position moves along its fresh velocity (`vel`), unless it holds
/// (the platform: where it is) or flies a circle (it stays on it); not reporting, nothing
/// changes (still lost: its reach keeps growing from its last report).
pub fn project_vehicle_into(
    base: &VehicleFacts,
    out: &mut VehicleFacts,
    holds: bool,
    reporting: bool,
    vel: Option<[f64; 2]>,
    now: Time,
    w: Time,
) {
    if !reporting {
        return;
    }
    let d = w.ms_since(now);
    let dt = secs(d).max(0.0);
    out.autopilot = shifted(&base.autopilot, d);
    out.nav_ready = shifted(&base.nav_ready, d);
    out.battery_pct = shifted(&base.battery_pct, d);
    out.nav_source = shifted(&base.nav_source, d);
    out.velocity_mps = shifted(&base.velocity_mps, d);
    out.gps_fix = shifted(&base.gps_fix, d);
    out.gps_glitch = shifted(&base.gps_glitch, d);
    out.alt_msl_m = shifted(&base.alt_msl_m, d);
    out.circling = base.circling.as_ref().map(|c| {
        let mut v = c.value;
        v.center_t = v.center_t.map(|t| t.plus_ms(d));
        Fact::new(v, c.t.plus_ms(d))
    });
    out.position = base.position.as_ref().map(|p| {
        let at = match vel {
            Some([e, n]) if !holds && base.circling.is_none() => engage::offset(p.value, e * dt, n * dt),
            _ => p.value,
        };
        Fact::new(at, p.t.plus_ms(d))
    });
}

/// Whether one of our vehicles reports now (its position fresh), and its fresh velocity.
fn reporting(v: &VehicleFacts, world: &WorldSnapshot, cfg: &RulesConfig) -> (bool, Option<[f64; 2]>) {
    let pos = fresh(&v.position, v, "position", world, cfg).is_ok();
    let vel = v
        .velocity_mps
        .as_ref()
        .filter(|_| pos)
        .and_then(|f| fresh(&v.velocity_mps, v, "velocity", world, cfg).ok().map(|_| f.value));
    (pos, vel)
}

/// The picture at `w` if everything keeps behaving as now ([`project_track_into`] for every
/// track, [`project_vehicle_into`] for every vehicle; the nomination's platform holds where
/// it is): what [`Item::wait`] and [`Explanation::wait`] read. A dry run of
/// [`spec_at`]`(spec, world.t, w)` on it is the test of a wait hint.
pub fn project(world: &WorldSnapshot, spec: &IntentSpec, w: Time, cfg: &RulesConfig, mem: &TrackerMemory) -> WorldSnapshot {
    let ec = cfg.engage_rules();
    let platform = spec.uids.first();
    let mut out = world.clone();
    out.t = w;
    for (id, t) in out.tracks.iter_mut() {
        if let Some(base) = world.tracks.get(id) {
            project_track_into(base, t, world.t, w, mem, &ec);
        }
    }
    for (uid, v) in out.vehicles.iter_mut() {
        if let Some(base) = world.vehicles.get(uid) {
            let (rep, vel) = reporting(base, world, cfg);
            project_vehicle_into(base, v, Some(uid) == platform, rep, vel, world.t, w);
        }
    }
    out
}

/// What the explanation knows of the nomination being explained.
struct Ctx<'a> {
    spec: &'a IntentSpec,
    world: &'a WorldSnapshot,
    cfg: &'a RulesConfig,
    ec: EngageRules,
    mem: &'a TrackerMemory,
    platform: Option<&'a Uid>,
    track_id: &'a str,
    target: &'a TrackFacts,
    radius: f64,
    clearance: f64,
    pv: EngagePreview,
}

/// A track or vehicle as the scans read it.
#[derive(Clone)]
enum Probe<'a> {
    Track(&'a str, &'a TrackFacts),
    Vehicle(&'a Uid, &'a VehicleFacts),
}

impl Probe<'_> {
    fn id(&self) -> String {
        match self {
            Probe::Track(id, _) => (*id).to_string(),
            Probe::Vehicle(u, _) => u.to_string(),
        }
    }
}

/// Whether a track is clear of the keep-out at (`point`, `t`), its altitude read at `now`:
/// [`engage::track_standing`], as [`engage::keep_out_tracks`] reads it.
#[allow(clippy::too_many_arguments)]
fn track_clear(t: &TrackFacts, point: LonLat, at: Time, clearance: f64, now: Time, ec: &EngageRules, alt: Option<EffectAlt>) -> bool {
    engage::track_standing(t, point, at, clearance, now, ec, alt, false).is_none_or(|s| !s.blocks(clearance))
}

/// Whether one of our vehicles is clear of the keep-out: [`engage::friendly_standing`], as
/// [`engage::keep_out_friendly`] reads it (`world` gives the time and the acceptances).
#[allow(clippy::too_many_arguments)]
fn vehicle_clear(
    uid: &Uid,
    v: &VehicleFacts,
    point: LonLat,
    at: Time,
    clearance: f64,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    alt: Option<EffectAlt>,
) -> bool {
    match engage::friendly_standing(uid, v, point, at, clearance, world, cfg, alt, false) {
        FriendlyStanding::Accepted { .. } => true,
        FriendlyStanding::Unclearable { .. } => false,
        FriendlyStanding::Judged { d, by_alt, .. } => d >= clearance || by_alt.is_some(),
    }
}

/// The effect times `from`, `from + step`, ... up to and including `to`.
fn grid(from: Time, to: Time, step: i64) -> Vec<Time> {
    let mut out = Vec::new();
    let mut t = from;
    while t < to {
        out.push(t);
        t = t.plus_ms(step);
    }
    out.push(to.max(from));
    out
}

/// See the module. Err when there is nothing to explain (not an engagement, no preview, a
/// signature's frozen plan, rules before 6) or when the explanation would not agree with the
/// checks (then nothing is shown rather than something wrong).
pub fn explain(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
    mem: &TrackerMemory,
) -> Result<Explanation, String> {
    let ec = cfg.engage_rules();
    if !ec.v6() {
        return Err("the explanation reads the keep-out model of rules 6 and later".into());
    }
    if world.plan.is_some() {
        return Err("a frozen plan's checks are not explained here".into());
    }
    let (track_id, radius, _, _) = engage::params(spec).ok_or("not an engagement")?;
    let pv = engage::preview(spec, world, cfg).map_err(|e| format!("no preview: {e}"))?;
    let target = world
        .tracks
        .get(track_id)
        .ok_or_else(|| format!("track {track_id} is not in the picture"))?;
    let ctx = Ctx {
        spec,
        world,
        cfg,
        ec: ec.clone(),
        mem,
        platform: spec.uids.first(),
        track_id,
        target,
        radius,
        clearance: pv.keep_out_m,
        pv,
    };
    let (point, effect_t, clearance) = (ctx.pv.effect_point, ctx.pv.effect_t, ctx.clearance);
    let alt = engage::effect_alt(Some(target), world.t, effect_t, ctx.pv.effect_radius_m, &ec);
    let checks = vec![
        engage::keep_out_friendly(point, effect_t, clearance, ctx.platform, world, cfg, alt),
        engage::keep_out_tracks(point, effect_t, clearance, track_id, world, &ec, alt),
    ];
    let blocked = checks.iter().any(|c| c.outcome != Outcome::Pass);
    let mut items: Vec<(Item, Probe)> = Vec::new();
    for (id, t) in &world.tracks {
        if engage::track_exempt(id, t, track_id, &ec) {
            continue;
        }
        let Some(s) = engage::track_standing(t, point, effect_t, clearance, world.t, &ec, alt, true) else {
            continue;
        };
        if s.blocks(clearance) || s.edge < NEAR_FACTOR * clearance {
            items.push((track_item(&ctx, id, t, &s, alt), Probe::Track(id, t)));
        }
    }
    for (uid, v) in &world.vehicles {
        if Some(uid) == ctx.platform {
            continue;
        }
        let st = engage::friendly_standing(uid, v, point, effect_t, clearance, world, cfg, alt, true);
        if let Some(item) = vehicle_item(&ctx, uid, v, st, alt) {
            items.push((item, Probe::Vehicle(uid, v)));
        }
    }
    // the blocking items are exactly what the checks fail on, or nothing is shown
    let fails = |r: RuleId| checks.iter().any(|c| c.rule == r && c.outcome != Outcome::Pass);
    let blocks = |s: Subject| items.iter().any(|(i, _)| i.blocks && i.subject == s);
    if blocks(Subject::Track) != fails(RuleId::KeepOutTracks) || blocks(Subject::Vehicle) != fails(RuleId::KeepOutFriendly) {
        return Err("the explanation does not agree with the keep-out checks: not shown".into());
    }
    let (mut move_within, mut wait, mut wait_none) = (None, None, None);
    if blocked {
        move_within = Some(same_picture(&ctx, &mut items));
        (wait, wait_none) = carried_forward(&ctx, &mut items);
    }
    let mut items: Vec<Item> = items.into_iter().map(|(i, _)| i).collect();
    items.sort_by(|a, b| {
        b.blocks
            .cmp(&a.blocks)
            .then(b.short_m.unwrap_or(f64::INFINITY).total_cmp(&a.short_m.unwrap_or(f64::INFINITY)))
            .then(a.id.cmp(&b.id))
    });
    Ok(Explanation {
        t: world.t,
        track: track_id.to_string(),
        platform: ctx.platform.cloned(),
        effect_point: point,
        effect_t,
        effect_radius_m: ctx.pv.effect_radius_m,
        clearance_m: clearance,
        vertical: alt.map(|a| Vertical {
            alt_m: a.alt_m,
            margin_m: a.margin_m,
            half_height_m: a.half_height_m,
        }),
        blocked,
        checks,
        items,
        move_within,
        wait,
        wait_none,
        assumptions: format!(
            "Move-within is on the picture now. Wait hints carry it forward: every track holds its course and speed and stays in view (no new manoeuvre; a young track's first minute as now), our vehicles keep doing what they do, the platform holds where it is; each is the first nomination time on a {} s grid within {} min.",
            WAIT_STEP_MS / 1000,
            WAIT_HORIZON_S / 60
        ),
        note: NOTE,
    })
}

/// The verdict sentence for a region whose edge is `edge` from the effect point.
fn verdict(blocks: bool, edge: f64, clearance: f64) -> String {
    if blocks {
        if edge < 0.0 {
            format!(
                "That covers the effect point and reaches {} past it; the effect needs {} clear: short by {}.",
                dist(-edge),
                dist(clearance),
                dist(clearance - edge)
            )
        } else {
            format!(
                "That comes within {} of the effect point; the effect needs {} clear: short by {}.",
                dist(edge),
                dist(clearance),
                dist(clearance - edge)
            )
        }
    } else {
        format!(
            "That stays {} from the effect point, {} more than the {} needed.",
            dist(edge),
            dist(edge - clearance),
            dist(clearance)
        )
    }
}

fn track_item(ctx: &Ctx, id: &str, t: &TrackFacts, s: &TrackStanding, alt: Option<EffectAlt>) -> Item {
    let (ec, clearance, effect_t) = (&ctx.ec, ctx.clearance, ctx.pv.effect_t);
    let point = ctx.pv.effect_point;
    let blocks = s.blocks(clearance);
    let regions: Vec<Region> = s
        .regions
        .iter()
        .map(|r| {
            let edge = haversine_m(r.center, point) - r.radius_m;
            Region {
                kind: r.kind.as_str(),
                center: r.center,
                radius_m: r.radius_m,
                edge_m: edge,
                inside: edge < clearance,
                how: r.how.clone(),
            }
        })
        .collect();
    let near = &s.regions[s.nearest];
    let span = secs(effect_t.ms_since(t.position.t)).max(0.0);
    let k = ec.keep_out_sigma;
    let r = dist(near.radius_m);
    let what = match near.kind {
        ReachKind::Velocity => {
            let v = t.velocity_mps.map_or(0.0, |[e, n]| e.hypot(n));
            format!(
                "On its course at {v:.1} m/s it is predicted {} from the effect point at the effect time, give or take {r} ({k:.0}-sigma over {span:.0} s).",
                dist(s.d)
            )
        }
        ReachKind::MeanVelocity => {
            let v = t.mean_velocity_mps.map_or(0.0, |[e, n]| e.hypot(n));
            format!(
                "Along its average course of the last minute ({v:.1} m/s) it is predicted {} from the effect point, give or take {r}.",
                dist(s.d)
            )
        }
        ReachKind::Manoeuvre => {
            let (ago, change, speed) = t.manoeuvre.map_or((0.0, 0.0, 0.0), |m| {
                (secs(t.position.t.ms_since(m.t)).max(0.0), m.change_mps, m.speed_mps)
            });
            format!(
                "The tracker saw it manoeuvre {ago:.0} s before its estimate (its velocity changed by {change:.1} m/s), so it could be anywhere within {r} of where it is: up to {speed:.1} m/s for {span:.0} s."
            )
        }
        ReachKind::Young => {
            let tracked = t.first_seen_t.map_or(0.0, |f| secs(t.position.t.ms_since(f)).max(0.0));
            let speed = (near.radius_m - k * t.pos_sigma_m) / span.max(1e-9);
            format!(
                "Tracked for only {tracked:.0} s, too short to rule out a manoeuvre, so it could be anywhere within {r} of where it is: up to {speed:.1} m/s for {span:.0} s."
            )
        }
        ReachKind::NoVelocity => format!(
            "No velocity estimate yet, so it could be anywhere within {r} of where it is: up to {:.0} m/s for {span:.0} s.",
            engage::top_speed_mps(t.domain)
        ),
    };
    let edge = s.edge;
    let altitude = match (alt, s.alt_known) {
        (Some(_), true) if s.by_alt.is_none() && edge < clearance => {
            Some("In 3D too: its altitude puts it within the keep-out's vertical band.".to_string())
        }
        (Some(_), false) => Some(
            "Its altitude is not a recent ranged measurement, so the keep-out has no ceiling for it.".to_string(),
        ),
        (None, _) if t.domain == Domain::Air => Some(
            "The target's altitude is not a recent ranged measurement, so the keep-out has no ceiling.".to_string(),
        ),
        _ => None,
    };
    let clear_by_altitude = s.by_alt.map(|(a, c, m)| {
        format!("Clear by altitude: at {a:.0} m it stays {c:.0} m outside the vertical keep-out, with {m:.0} m of altitude uncertainty.")
    });
    let reason = match &clear_by_altitude {
        Some(c) => format!("{what} {c}"),
        None => format!("{what} {}", verdict(blocks, edge, clearance)),
    };
    let manoeuvring_now = t
        .manoeuvre
        .filter(|_| ec.v9())
        .map(|m| secs(t.position.t.ms_since(m.t)) <= MANOEUVRING_NOW_S);
    Item {
        rule: RuleId::KeepOutTracks,
        subject: Subject::Track,
        id: id.to_string(),
        label: track_label(id, t),
        domain: Some(t.domain),
        now: Some(engage::track_at(t, ctx.world.t)),
        blocks,
        region: regions.get(s.nearest).cloned(),
        regions,
        edge_m: Some(edge),
        short_m: Some(clearance - edge),
        altitude,
        clear_by_altitude,
        reason,
        manoeuvring_now,
        fits_until_t: None,
        wait: None,
        not_by_waiting: None,
        other: None,
    }
}

/// A track as a check's detail names it.
fn track_label(id: &str, t: &TrackFacts) -> String {
    format!(
        "{id} ({}, {})",
        t.affiliation.value.as_str(),
        t.identity.as_deref().unwrap_or("unclassified")
    )
}

fn vehicle_item(ctx: &Ctx, uid: &Uid, v: &VehicleFacts, st: FriendlyStanding, alt: Option<EffectAlt>) -> Option<Item> {
    let clearance = ctx.clearance;
    let point = ctx.pv.effect_point;
    let label = format!("vehicle {uid}");
    let callsign = v.platform.as_ref().map(|p| p.id.clone());
    let base = Item {
        rule: RuleId::KeepOutFriendly,
        subject: Subject::Vehicle,
        id: uid.to_string(),
        label: match &callsign {
            Some(p) => format!("{label} ({p})"),
            None => label,
        },
        domain: v.platform.as_ref().map(|p| p.domain),
        now: v.position.as_ref().map(|p| p.value),
        blocks: true,
        region: None,
        regions: Vec::new(),
        edge_m: None,
        short_m: None,
        altitude: None,
        clear_by_altitude: None,
        reason: String::new(),
        manoeuvring_now: None,
        fits_until_t: None,
        wait: None,
        not_by_waiting: None,
        other: None,
    };
    match st {
        FriendlyStanding::Accepted { .. } => None,
        FriendlyStanding::Unclearable { kind, detail } => Some(Item {
            reason: match kind {
                FriendlyKind::NoPosition => "Ours: it has never reported a position, so it can't be cleared.".to_string(),
                _ => format!("Ours: it can't be predicted ({detail})."),
            },
            not_by_waiting: Some("Waiting does not clear it: nothing places it.".to_string()),
            other: Some(match kind {
                FriendlyKind::NoPosition => "A report from it would; so would an operator with engagement authority accepting its unknown position for this engagement (logged, at the nomination).".to_string(),
                _ => "A fresh report of its velocity would.".to_string(),
            }),
            ..base
        }),
        FriendlyStanding::Judged {
            kind,
            center,
            radius_m,
            d,
            how,
            by_alt,
            alt_known,
        } => {
            let blocks = d < clearance && by_alt.is_none();
            if !blocks && d >= NEAR_FACTOR * clearance {
                return None;
            }
            let edge = haversine_m(center, point) - radius_m;
            let span = v.position.as_ref().map_or(0.0, |p| secs(ctx.pv.effect_t.ms_since(p.t)).max(0.0));
            let what = match kind {
                FriendlyKind::Velocity => format!(
                    "Ours: on its course it is predicted {} from the effect point at the effect time, counting {} of uncertainty over {span:.0} s.",
                    dist(haversine_m(center, point)),
                    dist(radius_m)
                ),
                FriendlyKind::Circling => format!(
                    "Ours: it flies a circle; counted anywhere on it with its uncertainty, within {} of its centre, {} from the effect point.",
                    dist(radius_m),
                    dist(haversine_m(center, point))
                ),
                FriendlyKind::CruiseReach => format!(
                    "Ours: no fresh velocity, so anywhere its cruise speed reaches in {span:.0} s: within {} of its position.",
                    dist(radius_m)
                ),
                FriendlyKind::Lost => format!(
                    "Ours: its position is {:.0} s old (a lost or late link), so anywhere its cruise speed reaches since: within {} of its last report.",
                    v.position.as_ref().map_or(0.0, |p| secs(ctx.world.t.ms_since(p.t))),
                    dist(radius_m)
                ),
                FriendlyKind::NoPosition | FriendlyKind::Unpredictable => "Ours.".to_string(),
            };
            let clear_by_altitude = by_alt.map(|(a, c, m)| {
                format!("Clear by altitude: at {a:.0} m it stays {c:.0} m outside the vertical keep-out, with {m:.0} m of altitude uncertainty.")
            });
            let altitude = match alt {
                Some(_) if blocks && alt_known => {
                    Some("In 3D too: its altitude puts it within the keep-out's vertical band (or is not fresh).".to_string())
                }
                Some(_) if !alt_known => Some("It reports no altitude, so the keep-out has no ceiling for it.".to_string()),
                None if v.platform.as_ref().is_some_and(|p| p.domain == Domain::Air) => Some(
                    "The target's altitude is not a recent ranged measurement, so the keep-out has no ceiling.".to_string(),
                ),
                _ => None,
            };
            let reason = match &clear_by_altitude {
                Some(c) => format!("{what} {c}"),
                None => format!("{what} {}", verdict(blocks, edge, clearance)),
            };
            let lost = kind == FriendlyKind::Lost;
            let region = Region {
                kind: kind.as_str(),
                center,
                radius_m,
                edge_m: edge,
                inside: d < clearance,
                how: how.into_owned(),
            };
            Some(Item {
                blocks,
                region: Some(region.clone()),
                regions: vec![region],
                edge_m: Some(edge),
                short_m: Some(clearance - edge),
                altitude,
                clear_by_altitude,
                reason,
                not_by_waiting: (lost && blocks)
                    .then(|| "Waiting does not clear it: while it is lost its reach only grows.".to_string()),
                other: (lost && blocks).then(|| {
                    "A fresh report from it would; so would an operator with engagement authority accepting its unknown position for this engagement (logged, at the nomination).".to_string()
                }),
                ..base
            })
        }
    }
}

/// Whether an item could clear by waiting (else its `not_by_waiting` says why).
fn waitable(ctx: &Ctx, item: &Item, probe: &Probe) -> bool {
    if item.not_by_waiting.is_some() {
        return false;
    }
    match probe {
        Probe::Track(_, t) => in_view(t, ctx.world.t, &ctx.ec),
        Probe::Vehicle(..) => true,
    }
}

/// Same picture: for each blocking item, the latest effect time up to which it stays clear
/// from the earliest; for the platform, how close it would need to be for every keep-out to
/// hold.
fn same_picture(ctx: &Ctx, items: &mut [(Item, Probe)]) -> MoveWithin {
    let (world, ec, cfg) = (ctx.world, &ctx.ec, ctx.cfg);
    let all: Vec<Probe> = world
        .tracks
        .iter()
        .filter(|(id, t)| !engage::track_exempt(id, t, ctx.track_id, ec))
        .map(|(id, t)| Probe::Track(id, t))
        .chain(
            world
                .vehicles
                .iter()
                .filter(|(u, _)| Some(*u) != ctx.platform)
                .map(|(u, v)| Probe::Vehicle(u, v)),
        )
        .collect();
    let clear_at = |p: &Probe, point: LonLat, at: Time, alt: Option<EffectAlt>| match p {
        Probe::Track(_, t) => track_clear(t, point, at, ctx.clearance, world.t, ec, alt),
        Probe::Vehicle(u, v) => vehicle_clear(u, v, point, at, ctx.clearance, world, cfg, alt),
    };
    let start = ctx.pv.start_t;
    let mut pending: Vec<usize> = (0..items.len()).filter(|&i| items[i].0.blocks).collect();
    let mut all_until: Option<Time> = None;
    let mut all_done = false;
    let mut first_block: Option<String> = None;
    for at in grid(start, ctx.pv.effect_t, SPAN_STEP_MS) {
        let point = engage::target_at(ctx.target, at, ec);
        let alt = engage::effect_alt(Some(ctx.target), world.t, at, ctx.radius, ec);
        pending.retain(|&i| {
            let ok = clear_at(&items[i].1, point, at, alt);
            if ok {
                items[i].0.fits_until_t = Some(at);
            }
            ok
        });
        if !all_done {
            match all.iter().find(|p| !clear_at(p, point, at, alt)) {
                None => all_until = Some(at),
                Some(p) => {
                    all_done = true;
                    if all_until.is_none() {
                        first_block = Some(p.id());
                    }
                }
            }
        }
        if all_done && pending.is_empty() {
            break;
        }
    }
    let holding = engage::target_velocity(ctx.target, ec) == [0.0, 0.0];
    let name = ctx.platform.map_or("the platform".to_string(), |u| format!("vehicle {u}"));
    let of = if holding { "the target" } else { "where the target will be then" };
    match all_until {
        Some(by) => {
            let point = engage::target_at(ctx.target, by, ec);
            let within = ctx.pv.cruise_mps * secs(by.ms_since(start));
            let now_m = haversine_m(ctx.pv.start, point);
            MoveWithin {
                possible: true,
                effect_by_t: Some(by),
                point: Some(point),
                within_m: Some(within),
                now_m: Some(now_m),
                holding,
                why: format!(
                    "Every keep-out holds for an effect up to {} from now: {name} within {} of {of} (at {:.0} m/s, after the {:.0} s dispatch delay) would make it; it is {} away now.",
                    after(by, world.t),
                    dist(within),
                    ctx.pv.cruise_mps,
                    ec.dispatch_delay_s,
                    dist(now_m)
                ),
            }
        }
        None => MoveWithin {
            possible: false,
            effect_by_t: None,
            point: None,
            within_m: None,
            now_m: None,
            holding,
            why: format!(
                "No position of {name} clears it: even the earliest effect ({:.0} s from now, the dispatch delay, with the platform already at the point) is blocked{}.",
                ec.dispatch_delay_s,
                first_block.map_or(String::new(), |id| format!(" by {id}"))
            ),
        },
    }
}

/// Why a track cleared for a nomination at `w` (its effect at `w_effect`): compared with its
/// regions now, and the time from nomination to effect now (`span_now_ms`) and then.
fn track_why(
    now_kinds: &[&str],
    then: &TrackFacts,
    w: Time,
    w_effect: Time,
    span_now_ms: i64,
    ec: &EngageRules,
    mem: &TrackerMemory,
) -> String {
    let then_kinds: Vec<ReachKind> = engage::track_regions(then, w_effect, ec).iter().map(|r| r.kind).collect();
    let reach = now_kinds.iter().any(|k| ["young", "manoeuvre", "no_velocity"].contains(k));
    if now_kinds.contains(&"young") && !then_kinds.contains(&ReachKind::Young) {
        format!(
            "by then it has been tracked {:.0} s, long enough for an average velocity: a manoeuvre is no longer assumed",
            mem.mean_min_s
        )
    } else if now_kinds.contains(&"manoeuvre") && !then_kinds.contains(&ReachKind::Manoeuvre) {
        format!(
            "by then its last manoeuvre is more than {:.0} min old, out of the tracker's memory",
            mem.manoeuvre_memory_s / 60.0
        )
    } else if reach && w_effect.ms_since(w) + 5_000 <= span_now_ms {
        format!(
            "by then the effect would come {:.0} s after the nomination instead of {:.0} s (the target nearer the platform), so its reach is shorter",
            secs(w_effect.ms_since(w)),
            secs(span_now_ms)
        )
    } else {
        "by then its predicted region and the effect point have moved apart".to_string()
    }
}

/// The picture carried forward: for each blocking item that waiting may clear, the first
/// nomination time a dry run would find it clear; and the first at which every keep-out
/// holds.
fn carried_forward(ctx: &Ctx, items: &mut [(Item, Probe)]) -> (Option<Wait>, Option<String>) {
    let (world, ec, cfg, mem) = (ctx.world, &ctx.ec, ctx.cfg, ctx.mem);
    let now = world.t;
    // what can't clear by waiting says so
    let mut stuck: Vec<String> = Vec::new();
    for (item, probe) in items.iter_mut().filter(|(i, _)| i.blocks) {
        if !waitable(ctx, item, probe) {
            if item.not_by_waiting.is_none() {
                item.not_by_waiting =
                    Some("Waiting does not clear it: it is not in view, and its region only grows while unseen.".to_string());
            }
            stuck.push(item.id.clone());
        }
    }
    // scratch copies of everything the keep-outs read, carried forward in place each step
    struct T<'a> {
        id: &'a str,
        base: &'a TrackFacts,
        now_kinds: Vec<&'static str>,
        then: TrackFacts,
    }
    struct V<'a> {
        uid: &'a Uid,
        base: &'a VehicleFacts,
        rep: bool,
        vel: Option<[f64; 2]>,
        then: VehicleFacts,
    }
    let mut tracks: Vec<T> = world
        .tracks
        .iter()
        .filter(|(id, t)| !engage::track_exempt(id, t, ctx.track_id, ec))
        .map(|(id, t)| T {
            id,
            base: t,
            now_kinds: items
                .iter()
                .find(|(i, _)| i.subject == Subject::Track && i.id == *id)
                .map(|(i, _)| i.regions.iter().map(|r| r.kind).collect())
                .unwrap_or_default(),
            then: t.clone(),
        })
        .collect();
    let mut vehicles: Vec<V> = world
        .vehicles
        .iter()
        .filter(|(u, _)| Some(*u) != ctx.platform)
        .map(|(u, v)| {
            let (rep, vel) = reporting(v, world, cfg);
            V {
                uid: u,
                base: v,
                rep,
                vel,
                then: v.clone(),
            }
        })
        .collect();
    // the target and the platform, for the preview a nomination then would get
    let mut lean = WorldSnapshot::new(now);
    lean.unknown_accepted = world.unknown_accepted.clone();
    lean.tracks.insert(ctx.track_id.to_string(), ctx.target.clone());
    let plat = ctx.platform.and_then(|u| world.vehicles.get(u).map(|v| (u, v, reporting(v, world, cfg))));
    if let Some((u, v, _)) = plat {
        lean.vehicles.insert(u.clone(), v.clone());
    }
    let mut pending: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].0.blocks && items[i].0.not_by_waiting.is_none())
        .collect();
    let mut all: Option<Wait> = None;
    let all_possible = stuck.is_empty();
    for k in 1..=(WAIT_HORIZON_S * 1000 / WAIT_STEP_MS) {
        if pending.is_empty() && (all.is_some() || !all_possible) {
            break;
        }
        let w = now.plus_ms(k * WAIT_STEP_MS);
        lean.t = w;
        if let Some(t) = lean.tracks.get_mut(ctx.track_id) {
            project_track_into(ctx.target, t, now, w, mem, ec);
        }
        if let Some((u, base, (rep, vel))) = plat
            && let Some(v) = lean.vehicles.get_mut(u)
        {
            project_vehicle_into(base, v, true, rep, vel, now, w);
        }
        let Ok(pw) = engage::preview(&spec_at(ctx.spec, now, w), &lean, cfg) else {
            continue;
        };
        let Some(target) = lean.tracks.get(ctx.track_id) else {
            continue;
        };
        let alt = engage::effect_alt(Some(target), w, pw.effect_t, ctx.radius, ec);
        let (point, at) = (pw.effect_point, pw.effect_t);
        let want_all = all.is_none() && all_possible;
        for t in tracks.iter_mut() {
            if want_all || pending.iter().any(|&i| items[i].0.subject == Subject::Track && items[i].0.id == t.id) {
                project_track_into(t.base, &mut t.then, now, w, mem, ec);
            }
        }
        for v in vehicles.iter_mut() {
            if want_all || pending.iter().any(|&i| items[i].0.subject == Subject::Vehicle && items[i].0.id == v.uid.to_string()) {
                project_vehicle_into(v.base, &mut v.then, false, v.rep, v.vel, now, w);
            }
        }
        let track_ok = |t: &T| track_clear(&t.then, point, at, ctx.clearance, w, ec, alt);
        let vehicle_ok = |v: &V| vehicle_clear(v.uid, &v.then, point, at, ctx.clearance, &lean, cfg, alt);
        pending.retain(|&i| {
            let (item, _) = &items[i];
            let (ok, why) = match item.subject {
                Subject::Track => match tracks.iter().find(|t| t.id == item.id) {
                    Some(t) if track_ok(t) => (
                        true,
                        track_why(&t.now_kinds, &t.then, w, at, ctx.pv.effect_t.ms_since(now), ec, mem),
                    ),
                    _ => (false, String::new()),
                },
                Subject::Vehicle => match vehicles.iter().find(|v| v.uid.to_string() == item.id) {
                    Some(v) if vehicle_ok(v) => (true, "by then it has moved on along its course".to_string()),
                    _ => (false, String::new()),
                },
            };
            if ok {
                let cond = match item.manoeuvring_now {
                    Some(true) => ", if it stops manoeuvring now and holds its course (it was still manoeuvring at its estimate)",
                    _ => ", if it holds its course and speed",
                };
                items[i].0.wait = Some(Wait {
                    nominate_t: w,
                    effect_t: at,
                    why: format!("Clear for a nomination {} from now (effect {} from now): {why}{cond}.", after(w, now), after(at, now)),
                });
            }
            !ok
        });
        if want_all && tracks.iter().all(track_ok) && vehicles.iter().all(vehicle_ok) {
            let manoeuvring: Vec<&str> = items
                .iter()
                .filter(|(i, _)| i.blocks && i.manoeuvring_now == Some(true))
                .map(|(i, _)| i.id.as_str())
                .collect();
            all = Some(Wait {
                nominate_t: w,
                effect_t: at,
                why: format!(
                    "Every keep-out holds for a nomination {} from now (effect {} from now), if everything keeps behaving as now{}.",
                    after(w, now),
                    after(at, now),
                    if manoeuvring.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "; {} still manoeuvring at its estimate, so this holds only if it stops",
                            manoeuvring.join(", ")
                        )
                    }
                ),
            });
        }
    }
    for &i in &pending {
        items[i].0.not_by_waiting = Some(format!(
            "Not within {} min if it keeps behaving as now.",
            WAIT_HORIZON_S / 60
        ));
    }
    let none = if all.is_some() {
        None
    } else if !all_possible {
        Some(format!("Waiting alone does not clear it: {} (see each).", stuck.join(", ")))
    } else {
        Some(format!(
            "Every keep-out does not hold together within {} min if everything keeps behaving as now.",
            WAIT_HORIZON_S / 60
        ))
    };
    (all, none)
}
