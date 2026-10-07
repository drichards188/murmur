//! Why an engagement's keep-out blocks, and what would clear it (murmur-12e): a read-only
//! explanation for the nomination form's dry run (`POST /api/engage/options`) and agents'
//! reads. It is not a decision and is not logged: the decision is the nomination and its
//! signatures, each checked again by the engine. It relaxes nothing and overrides nothing.
//! It reads the functions the keep-out checks read ([`engage::track_standing`],
//! [`engage::friendly_standing`], [`engage::preview`], [`engage::effect_alt`]) on the
//! snapshot the dry run checked, and refuses to explain unless every track and vehicle it
//! calls blocking is exactly one the checks fail on (each check run on that item alone), so
//! it cannot disagree with them.
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
//!   - [`Item::clear_windows`], on the same picture: every stretch of effect times, from the
//!     earliest any platform could make (the dispatch delay, or the window's opening) to the
//!     latest a nomination allows (the window's close, or the longest transit), at which it
//!     is clear; [`Item::fits_until_t`] when the first starts at the earliest (a closer
//!     platform);
//!   - [`Item::wait`], on the picture carried forward ([`project`], "if it keeps behaving as
//!     now"): the first nomination time at which a dry run would find it clear, and that
//!     nomination's effect time (a young track gets its average velocity, a manoeuvre ages
//!     out of the tracker's memory, its predicted region moves off);
//!   - [`Item::not_by_waiting`] and [`Item::other`] when waiting does not clear it (lost, not
//!     in view), stated as what happens, with what else would (a report, an operator's logged
//!     acceptance).
//!
//! Rules 13: a contact the tracker dropped is an item too ([`Subject::LostContact`]: the
//! keep-out still counts it, [`crate::lost`]), with since when it is out of view, what the
//! check reads (its reach from its last fix) and what would clear it: an operator with
//! engagement authority accepting it for this engagement ([`Item::lost`] names what to accept),
//! or the contact seen again with its own AIS identity. Waiting never does.
//!
//! and, for the platform, [`MoveWithin`]: every stretch of effect times at which every
//! keep-out holds, as distances from the target when it holds where it is ("within X m",
//! "between X and Y m"); and [`Explanation::wait`]: the first nomination time at which every
//! keep-out would hold. `tests/keepout.rs` checks each claim against the checks themselves.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::catalog::Domain;
use crate::engage::{
    self, EffectAlt, EngagePreview, EngageRules, EngageWindow, FriendlyKind, FriendlyStanding,
    ReachKind, TrackStanding,
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
/// At most this many items are listed (blocking first, by shortfall); the rest are counted.
pub const MAX_ITEMS: usize = 12;
/// At most this many effect-time windows are listed, for an item or the platform.
pub const MAX_WINDOWS: usize = 4;
/// How far ahead [`Item::wait`] and [`Explanation::wait`] look, s.
pub const WAIT_HORIZON_S: i64 = 600;
/// Their grid, ms: the first passing nomination time on it.
pub const WAIT_STEP_MS: i64 = 1_000;
/// The grid of effect times the windows are scanned on, ms.
pub const SPAN_STEP_MS: i64 = 250;
/// How far clear of the keep-out a region must be at a grid point of the effect-time scan for
/// that point to count as clear, m (murmur-dr6). Between two grid points a region's edge can
/// dip below the line through them: for a relative motion at `v` passing the keep-out's edge
/// at the clearance `c`, by at most `v^2 h^2 / (8 c)`, `h` the grid's step; at 100 m/s and the
/// default 275 m clearance that is 0.28 m. So a stretch of passing grid points says every
/// effect time between them passes for anything moving up to 100 m/s; a smaller clearance
/// needs more ([`grid_margin_m`]). The explanation's windows read it; the checks never do.
pub const GRID_MARGIN_M: f64 = 0.3;
/// The relative speed [`grid_margin_m`] bounds the dip for, m/s.
pub const GRID_MARGIN_SPEED_MPS: f64 = 100.0;

/// The margin a grid point of the effect-time scan needs for a clearance of `clearance_m`
/// ([`GRID_MARGIN_M`], more for a clearance under the default's).
pub fn grid_margin_m(clearance_m: f64) -> f64 {
    let h = SPAN_STEP_MS as f64 / 1000.0;
    let dip = GRID_MARGIN_SPEED_MPS.powi(2) * h * h / (8.0 * clearance_m.max(1.0));
    GRID_MARGIN_M.max(dip)
}
/// A manoeuvre seen this recently before a track's estimate means it is manoeuvring now, s.
pub const MANOEUVRING_NOW_S: f64 = 30.0;

/// The tracker's memories a projection ages ([`project`]): how long a track must be followed
/// before it has an average velocity, over how long that average is taken, how long a
/// manoeuvre is remembered, after how long without a detection a track is stale (not in
/// view: it is no longer carried forward as seen) and after how long the tracker drops it.
/// The bridge passes its tracker's own; the defaults are them (`tracks/model.rs`:
/// `MEAN_MIN_S`, `MEAN_WINDOW_S`, `MANOEUVRE_MEMORY_S`, `STALE_S`; `MURMUR_TRACK_DROP_S`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TrackerMemory {
    pub mean_min_s: f64,
    pub mean_window_s: f64,
    pub manoeuvre_memory_s: f64,
    pub stale_after_s: f64,
    pub drop_after_s: f64,
}

impl Default for TrackerMemory {
    fn default() -> Self {
        TrackerMemory {
            mean_min_s: 45.0,
            mean_window_s: 60.0,
            manoeuvre_memory_s: 300.0,
            stale_after_s: 10.0,
            drop_after_s: 180.0,
        }
    }
}

/// What [`explain`] is told besides the picture: the tracker's memories, and what to call our
/// vehicles in its words (callsigns by uid; else "vehicle <uid>").
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub memory: TrackerMemory,
    pub names: BTreeMap<Uid, String>,
}

impl Options {
    fn name(&self, uid: &Uid) -> String {
        self.names.get(uid).cloned().unwrap_or_else(|| format!("vehicle {uid}"))
    }
}

/// See the module.
#[derive(Clone, Debug, Serialize)]
pub struct Explanation {
    /// The snapshot's time (the dry run's).
    pub t: Time,
    pub track: String,
    pub platform: Option<Uid>,
    /// The platform as the words call it.
    pub platform_name: Option<String>,
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
    /// How many tracks and vehicles of ours block it (all of them; `items` lists at most
    /// [`MAX_ITEMS`]).
    pub blocking: usize,
    /// The checks themselves on this snapshot, as the dry run reports them.
    pub checks: Vec<CheckResult>,
    /// Blocking first (the largest shortfall first), then those that nearly block.
    pub items: Vec<Item>,
    /// Items not listed (beyond [`MAX_ITEMS`]).
    pub omitted: usize,
    /// For the platform, when blocked: which effect times, and so which distances, would
    /// let every keep-out hold, on the picture now.
    pub move_within: Option<MoveWithin>,
    /// When blocked: the first nomination time at which every keep-out would hold, if
    /// everything keeps behaving as now.
    pub wait: Option<Wait>,
    /// When blocked and no such time is given: why.
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
    /// Rules 13: a contact the tracker dropped, still counted ([`crate::lost`]).
    LostContact,
}

/// Rules 13: what an operator with engagement authority would accept for a lost contact, as
/// the check names it (the track id it was dropped under and its last fix), and when the
/// tracker dropped it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LostInfo {
    pub track: String,
    pub since: Time,
    pub dropped_t: Time,
    /// Its AIS identity, when it broadcast one (seen again with it, unambiguously, it is
    /// retired).
    pub ais: Option<String>,
}

/// One track or vehicle of ours against the keep-out. See the module.
#[derive(Clone, Debug, Serialize)]
pub struct Item {
    /// The check it stands under: `keep_out_tracks` or `keep_out_friendly`.
    pub rule: RuleId,
    pub subject: Subject,
    /// The track id, or the vehicle's uid.
    pub id: String,
    /// "T-0003 (neutral, ferry)", or our vehicle's callsign.
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
    /// How altitude counted, when it is relevant (aircraft).
    pub altitude: Option<String>,
    /// In 2D inside the keep-out, but clear of it by altitude.
    pub clear_by_altitude: Option<String>,
    /// In plain words: why, with the numbers.
    pub reason: String,
    /// A track the tracker saw manoeuvre: whether that was within [`MANOEUVRING_NOW_S`] of
    /// its estimate (it is manoeuvring now, and a wait hint holds only if it stops).
    pub manoeuvring_now: Option<bool>,
    /// Same picture, when it blocks: every effect time from the earliest up to this one
    /// leaves it clear (the first of `clear_windows`, when it starts at the earliest).
    pub fits_until_t: Option<Time>,
    /// Same picture, when it blocks: the stretches of effect times (between the earliest and
    /// the latest a nomination allows) at which it is clear, at most [`MAX_WINDOWS`]; empty:
    /// none.
    pub clear_windows: Vec<EffectWindow>,
    /// When it blocks: the first nomination time at which a dry run would find it clear if
    /// it (and the target) keep behaving as now and the platform holds where it is.
    pub wait: Option<Wait>,
    /// When it blocks and no wait is given: what happens instead.
    pub not_by_waiting: Option<String>,
    /// Something else that would clear it (never an override).
    pub other: Option<String>,
    /// Rules 13: a lost contact's facts for an acceptance (none for anything else).
    pub lost: Option<LostInfo>,
}

/// One region of an item at the effect time.
#[derive(Clone, Debug, Serialize)]
pub struct Region {
    /// velocity, mean_velocity, manoeuvre, young, aircraft, no_velocity (a track); velocity,
    /// circling, cruise_reach, lost (one of ours).
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

/// A stretch of effect times, both ends on the scan's grid and passing, and, when the target
/// holds where it is, the platform's distances from it that make an effect then (its cruise
/// speed times the time from its start).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EffectWindow {
    pub from_t: Time,
    pub until_t: Time,
    pub from_m: Option<f64>,
    pub until_m: Option<f64>,
}

/// See [`Explanation::move_within`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MoveWithin {
    /// Whether some effect time between `earliest_t` and `latest_t` lets every keep-out hold.
    pub possible: bool,
    /// The earliest effect any position allows (the dispatch delay, or the window's opening)
    /// and the latest a nomination allows (the window's close, or the longest transit).
    pub earliest_t: Time,
    pub latest_t: Time,
    /// The stretches of effect times at which every keep-out holds (at most [`MAX_WINDOWS`]).
    pub windows: Vec<EffectWindow>,
    /// The first window, when it starts at the earliest: every effect up to this time passes.
    pub effect_by_t: Option<Time>,
    /// Where the target is predicted then: the platform within `within_m` of it intercepts
    /// by `effect_by_t`.
    pub point: Option<LonLat>,
    /// The platform's cruise speed times the time from its start to `effect_by_t`, m.
    pub within_m: Option<f64>,
    /// The platform's distance now from `point` (or from the target, holding), m.
    pub now_m: Option<f64>,
    /// The target is predicted where it is (holding): distances alone set the effect time.
    pub holding: bool,
    pub why: String,
}

const NOTE: &str = "Read-only: not a decision, not logged, never an override. The checks decide; this \
says why they block and what would clear them, from the same functions on the same picture.";

fn secs(ms: i64) -> f64 {
    ms as f64 / 1000.0
}

fn ms(s: f64) -> i64 {
    (s * 1000.0).round() as i64
}

/// A distance for a sentence: metres below a kilometre, else kilometres.
fn dist(m: f64) -> String {
    if m.abs() >= 1000.0 {
        format!("{:.2} km", m / 1000.0)
    } else {
        format!("{m:.0} m")
    }
}

/// "95 s", "3 min 36 s", relative to `now`.
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

/// Whether a track is in view now: detected within the tracker's stale limit.
fn in_view(t: &TrackFacts, now: Time, mem: &TrackerMemory) -> bool {
    secs(now.ms_since(t.last_seen_t)) <= mem.stale_after_s
}

/// A track carried from `now` to `w` "if it keeps behaving as now", into `out` (a copy of
/// `base`): in view, its estimate moves along its velocity and is stamped `w - now` later
/// (its sources and altitude as recent as now), its uncertainty stays, a manoeuvre older than
/// the memory is forgotten (no new one), a young track gets its velocity as its average once
/// it has been followed `mean_min_s`, and an average velocity becomes its velocity once a
/// whole `mean_window_s` has passed on its course; not in view, it stays where it was seen and
/// its uncertainty grows at its rate (the tracker's drop of a track long unseen is not
/// modelled: the explanation says when it happens, it does not hint at it).
pub fn project_track_into(
    base: &TrackFacts,
    out: &mut TrackFacts,
    now: Time,
    w: Time,
    mem: &TrackerMemory,
) {
    let d = w.ms_since(now);
    let dt = secs(d).max(0.0);
    if !in_view(base, now, mem) {
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
/// where it is (`holds`: the platform, or a vehicle on a circle that holds it, as the check
/// predicts it on its disc); not reporting, nothing changes (still lost: its reach keeps
/// growing from its last report).
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
            Some([e, n]) if !holds => engage::offset(p.value, e * dt, n * dt),
            _ => p.value,
        };
        Fact::new(at, p.t.plus_ms(d))
    });
}

/// How one of our vehicles is carried forward: whether it reports now (its position fresh),
/// its fresh velocity, and whether a circle holds it (as the check reads it).
fn reporting(v: &VehicleFacts, world: &WorldSnapshot, cfg: &RulesConfig) -> (bool, Option<[f64; 2]>, bool) {
    let pos = fresh(&v.position, v, "position", world, cfg).is_ok();
    let vel = v
        .velocity_mps
        .as_ref()
        .filter(|_| pos)
        .and_then(|f| fresh(&v.velocity_mps, v, "velocity", world, cfg).ok().map(|_| f.value));
    (pos, vel, engage::circle_holding(v, world, cfg).is_some())
}

/// The picture at `w` if everything keeps behaving as now ([`project_track_into`] for every
/// track, [`project_vehicle_into`] for every vehicle; the nomination's platform holds where
/// it is): what [`Item::wait`] and [`Explanation::wait`] read. A dry run of
/// [`spec_at`]`(spec, world.t, w)` on it is the test of a wait hint.
pub fn project(world: &WorldSnapshot, spec: &IntentSpec, w: Time, cfg: &RulesConfig, mem: &TrackerMemory) -> WorldSnapshot {
    let platform = spec.uids.first();
    let mut out = world.clone();
    out.t = w;
    for (id, t) in out.tracks.iter_mut() {
        if let Some(base) = world.tracks.get(id) {
            project_track_into(base, t, world.t, w, mem);
        }
    }
    for (uid, v) in out.vehicles.iter_mut() {
        if let Some(base) = world.vehicles.get(uid) {
            let (rep, vel, circle) = reporting(base, world, cfg);
            project_vehicle_into(base, v, Some(uid) == platform || circle, rep, vel, world.t, w);
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
    opts: &'a Options,
    platform: Option<&'a Uid>,
    track_id: &'a str,
    target: &'a TrackFacts,
    radius: f64,
    clearance: f64,
    window: EngageWindow,
    pv: EngagePreview,
}

impl Ctx<'_> {
    fn platform_name(&self) -> String {
        self.platform.map_or("the platform".to_string(), |u| self.opts.name(u))
    }
}

/// A track or vehicle as the scans read it.
#[derive(Clone)]
enum Probe<'a> {
    Track(&'a str, &'a TrackFacts),
    Vehicle(&'a Uid, &'a VehicleFacts),
    /// Rules 13: a lost contact the check judges (not live, exempt or accepted).
    Lost(&'a str, &'a TrackFacts),
}

impl Probe<'_> {
    /// As the words name it.
    fn label(&self, opts: &Options) -> String {
        match self {
            Probe::Track(id, t) => track_label(id, t),
            Probe::Vehicle(u, _) => format!("{} (ours)", opts.name(u)),
            Probe::Lost(id, t) => format!("lost contact {}", track_label(id, t)),
        }
    }
}

/// Rules 13: the lost contacts of `world` the keep-out on tracks judges by their regions (not
/// live again, exempt or accepted for this engagement), as [`engage::lost_standing`] decides.
fn judged_lost<'w>(world: &'w WorldSnapshot, target: &str, ec: &EngageRules) -> Vec<(&'w str, &'w crate::lost::LostContact)> {
    if !ec.lost() {
        return Vec::new();
    }
    world
        .lost_contacts
        .iter()
        .filter(|(id, l)| {
            !world.tracks.contains_key(id.as_str())
                && !engage::track_exempt(id, &l.facts, target, ec)
                && !world.lost_accepted.get(id.as_str()).is_some_and(|a| a.since == Some(l.since()))
        })
        .map(|(id, l)| (id.as_str(), l))
        .collect()
}

/// Whether a track is clear of the keep-out at (`point`, `t`), its altitude read at `now`:
/// [`engage::track_standing`], as [`engage::keep_out_tracks`] reads it.
#[allow(clippy::too_many_arguments)]
fn track_clear(t: &TrackFacts, point: LonLat, at: Time, clearance: f64, now: Time, ec: &EngageRules, alt: Option<EffectAlt>) -> bool {
    engage::track_standing(t, point, at, clearance, now, ec, alt, false).is_none_or(|s| !s.blocks(clearance))
}

/// Rules 13: [`track_clear`] for a lost contact (its facts at the drop): by its regions as one
/// ([`engage::lost_track_standing`]), as [`engage::lost_standing`] judges it.
#[allow(clippy::too_many_arguments)]
fn lost_clear(t: &TrackFacts, point: LonLat, at: Time, clearance: f64, now: Time, ec: &EngageRules, alt: Option<EffectAlt>) -> bool {
    engage::lost_track_standing(t, point, at, clearance, now, ec, alt, false).is_none_or(|s| !s.blocks(clearance))
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

/// The stretches of passing grid points, in order.
#[derive(Default)]
struct Runs {
    done: Vec<(Time, Time)>,
    open: Option<(Time, Time)>,
}

impl Runs {
    fn push(&mut self, t: Time, ok: bool) {
        if ok {
            match &mut self.open {
                Some((_, end)) => *end = t,
                None => self.open = Some((t, t)),
            }
        } else if let Some(w) = self.open.take() {
            self.done.push(w);
        }
    }

    fn finish(mut self) -> Vec<(Time, Time)> {
        if let Some(w) = self.open.take() {
            self.done.push(w);
        }
        self.done
    }
}

/// See the module. Err when there is nothing to explain (not an engagement, no preview, an
/// effect radius above the policy's maximum, a signature's frozen plan, rules before 6) or
/// when the explanation would not agree with the checks (then nothing is shown rather than
/// something wrong).
pub fn explain(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig, opts: &Options) -> Result<Explanation, String> {
    let ec = cfg.engage_rules();
    if !ec.v6() {
        return Err("the explanation reads the keep-out model of rules 6 and later".into());
    }
    if world.plan.is_some() {
        return Err("a frozen plan's checks are not explained here".into());
    }
    let (track_id, radius, _, _) = engage::params(spec).ok_or("not an engagement")?;
    if !(radius.is_finite() && radius > 0.0 && radius <= ec.max_effect_radius_m) {
        return Err(format!(
            "the effect radius ({radius} m) is outside the policy's 0 to {:.0} m: the keep-outs are not explained",
            ec.max_effect_radius_m
        ));
    }
    let window = engage::effective_window(spec, &ec).ok_or("not an engagement")?;
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
        opts,
        platform: spec.uids.first(),
        track_id,
        target,
        radius,
        clearance: pv.keep_out_m,
        window,
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
    // rules 13: the lost contacts it judges
    for (id, l) in judged_lost(world, track_id, &ec) {
        let Some(s) = engage::lost_track_standing(&l.facts, point, effect_t, clearance, world.t, &ec, alt, true) else {
            continue;
        };
        if s.blocks(clearance) || s.edge < NEAR_FACTOR * clearance {
            items.push((lost_item(&ctx, id, l, &s, alt), Probe::Lost(id, &l.facts)));
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
    agrees(&ctx, &checks, &items, alt)?;
    let blocking = items.iter().filter(|(i, _)| i.blocks).count();
    items.sort_by(|(a, _), (b, _)| {
        b.blocks
            .cmp(&a.blocks)
            .then(b.short_m.unwrap_or(f64::INFINITY).total_cmp(&a.short_m.unwrap_or(f64::INFINITY)))
            .then(a.id.cmp(&b.id))
    });
    let shown = items.len().min(MAX_ITEMS);
    let (mut move_within, mut wait, mut wait_none) = (None, None, None);
    if blocked {
        move_within = Some(same_picture(&ctx, &mut items[..shown], blocking > shown));
        (wait, wait_none) = carried_forward(&ctx, &mut items, shown);
    }
    let omitted = items.len() - shown;
    let items: Vec<Item> = items.into_iter().take(shown).map(|(i, _)| i).collect();
    Ok(Explanation {
        t: world.t,
        track: track_id.to_string(),
        platform: ctx.platform.cloned(),
        platform_name: ctx.platform.map(|u| opts.name(u)),
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
        blocking,
        checks,
        items,
        omitted,
        move_within,
        wait,
        wait_none,
        assumptions: format!(
            "Closer and farther are on the picture now (effect times on a {} s grid from the earliest to the latest a nomination allows, each grid point clear by a margin that covers what moves between them). Wait hints carry the picture forward: every track in view holds its course and speed and stays in view (no new manoeuvre; a young track's first minute as now), our vehicles keep doing what they do, the platform holds where it is; each is the first nomination time on a {} s grid within {} min.",
            SPAN_STEP_MS as f64 / 1000.0,
            WAIT_STEP_MS / 1000,
            WAIT_HORIZON_S / 60
        ),
        note: NOTE,
    })
}

/// The guard: every track and vehicle the keep-out checks judge, judged alone by the check
/// itself (a picture with only it), blocks exactly when the explanation lists it as blocking;
/// and each check fails exactly when some item under it blocks.
fn agrees(ctx: &Ctx, checks: &[CheckResult], items: &[(Item, Probe)], alt: Option<EffectAlt>) -> Result<(), String> {
    let (world, ec, cfg) = (ctx.world, &ctx.ec, ctx.cfg);
    let (point, effect_t, clearance) = (ctx.pv.effect_point, ctx.pv.effect_t, ctx.clearance);
    let listed = |s: Subject, id: &str| items.iter().any(|(i, _)| i.blocks && i.subject == s && i.id == id);
    let disagree = || Err("the explanation does not agree with the keep-out checks: not shown".to_string());
    for (id, t) in &world.tracks {
        if engage::track_exempt(id, t, ctx.track_id, ec) {
            continue;
        }
        let mut alone = WorldSnapshot::new(world.t);
        alone.tracks.insert(id.clone(), t.clone());
        let fails = engage::keep_out_tracks(point, effect_t, clearance, ctx.track_id, &alone, ec, alt).outcome != Outcome::Pass;
        if fails != listed(Subject::Track, id) {
            return disagree();
        }
    }
    for (uid, v) in &world.vehicles {
        if Some(uid) == ctx.platform {
            continue;
        }
        let mut alone = WorldSnapshot::new(world.t);
        alone.unknown_accepted = world.unknown_accepted.clone();
        alone.vehicles.insert(uid.clone(), v.clone());
        let fails = engage::keep_out_friendly(point, effect_t, clearance, ctx.platform, &alone, cfg, alt).outcome != Outcome::Pass;
        if fails != listed(Subject::Vehicle, &uid.to_string()) {
            return disagree();
        }
    }
    // rules 13: each lost contact judged alone (the live tracks and the acceptances as they are)
    for (id, l) in &world.lost_contacts {
        if world.tracks.contains_key(id) || !ec.lost() {
            continue;
        }
        let mut alone = WorldSnapshot::new(world.t);
        alone.lost_accepted = world.lost_accepted.clone();
        alone.lost_contacts.insert(id.clone(), l.clone());
        let fails = engage::keep_out_tracks(point, effect_t, clearance, ctx.track_id, &alone, ec, alt).outcome != Outcome::Pass;
        if fails != listed(Subject::LostContact, id) {
            return disagree();
        }
    }
    let fails = |r: RuleId| checks.iter().any(|c| c.rule == r && c.outcome != Outcome::Pass);
    let blocks = |s: Subject| items.iter().any(|(i, _)| i.blocks && i.subject == s);
    if (blocks(Subject::Track) || blocks(Subject::LostContact)) != fails(RuleId::KeepOutTracks)
        || blocks(Subject::Vehicle) != fails(RuleId::KeepOutFriendly)
    {
        return disagree();
    }
    Ok(())
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
        ReachKind::NoVelocity => {
            let lag = if t.domain == Domain::Air && ec.aircraft_reach() {
                format!(", and {:.0} m more for the tracker's lag behind an aircraft", engage::AIRCRAFT_LAG_M)
            } else {
                String::new()
            };
            format!(
                "No velocity estimate yet, so it could be anywhere within {r} of where it is: up to {:.0} m/s for {span:.0} s{lag}.",
                engage::top_speed_mps(t.domain)
            )
        }
        ReachKind::Aircraft => {
            let (from, to, _) = engage::aircraft_reach_m(engage::aircraft_speed_bound(t, ec), span);
            format!(
                "An aircraft may turn or speed up at any moment, so it could be anywhere within {r} of where it is: from {from:.1} m/s, up to {to:.1} m/s by the effect time ({span:.0} s), and {:.0} m more for the tracker's lag behind it.",
                engage::AIRCRAFT_LAG_M
            )
        }
        ReachKind::PlausibleSpeed => {
            let speed = (near.radius_m - k * t.pos_sigma_m) / span.max(1e-9);
            format!(
                "Out of view, it may have turned any way since its last fix, so it could be anywhere within {r} of where it was: up to {speed:.1} m/s, its own speed and {k:.0}-sigma of its velocity error (or the fastest it went), for {span:.0} s."
            )
        }
    };
    let edge = s.edge;
    // altitude matters for an aircraft (a vessel is at sea level either way)
    let altitude = match (alt, s.alt_known) {
        _ if t.domain != Domain::Air => None,
        (Some(_), true) if s.by_alt.is_none() && edge < clearance => {
            Some("In 3D too: its altitude puts it within the keep-out's vertical band.".to_string())
        }
        (Some(_), false) => Some(
            "Its altitude is not a recent ranged measurement, so the keep-out has no ceiling for it.".to_string(),
        ),
        (None, _) => Some(
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
        clear_windows: Vec::new(),
        wait: None,
        not_by_waiting: None,
        other: other_remedy(blocks, near.kind, t, ec),
        lost: None,
    }
}

/// Rules 16: a lost contact as an item: its regions (as a track's, from its facts at the drop),
/// since when it is out of view, and what would clear it.
fn lost_item(ctx: &Ctx, id: &str, l: &crate::lost::LostContact, s: &TrackStanding, alt: Option<EffectAlt>) -> Item {
    let mut item = track_item(ctx, id, &l.facts, s, alt);
    let now = ctx.world.t;
    let unseen = secs(now.ms_since(l.since())).max(0.0);
    let dropped = secs(now.ms_since(l.dropped_t)).max(0.0);
    item.rule = RuleId::KeepOutTracks;
    item.subject = Subject::LostContact;
    item.label = format!("lost contact {}", track_label(id, &l.facts));
    // where it was last seen: nothing places it now
    item.now = Some(l.facts.position.value);
    item.reason = format!(
        "Out of view since {} ago; the tracker dropped it {} ago. It still counts, from its last fix: {}",
        after_s(unseen),
        after_s(dropped),
        item.reason
    );
    item.manoeuvring_now = None;
    item.lost = Some(LostInfo {
        track: id.to_string(),
        since: l.since(),
        dropped_t: l.dropped_t,
        ais: l.ais.as_ref().map(|a| format!("{} ({})", a.mmsi, a.name)),
    });
    if item.blocks {
        item.not_by_waiting = Some("Waiting does not clear it: while it is out of view its reach only grows.".to_string());
        item.other = Some(match &l.ais {
            Some(a) => format!(
                "An operator with engagement authority accepting it for this engagement would (logged, at the nomination or before the first signature); so would the tracker seeing it again with its AIS identity ({}), unambiguously.",
                a.mmsi
            ),
            None => "An operator with engagement authority accepting it for this engagement would (logged, at the nomination or before the first signature). It broadcast no AIS identity, so a track seen later is never taken for it.".to_string(),
        });
    }
    item
}

/// "95 s", "3 min 36 s".
fn after_s(s: f64) -> String {
    let s = s.round() as i64;
    if s.abs() < 120 {
        format!("{s} s")
    } else {
        format!("{} min {:02} s", s / 60, s.abs() % 60)
    }
}

/// What else would clear a track whose reach blocks. A velocity estimate turns a surface or
/// ground track's reach into a prediction along its course; an aircraft's (rules 14) it does
/// not: an aircraft counts anywhere it could turn or speed up to, with a velocity or without.
fn other_remedy(blocks: bool, kind: ReachKind, t: &TrackFacts, ec: &EngageRules) -> Option<String> {
    if !blocks {
        return None;
    }
    let aircraft = t.domain == Domain::Air && ec.aircraft_reach();
    match kind {
        ReachKind::NoVelocity | ReachKind::Aircraft if aircraft => Some(
            "A velocity estimate would not clear it: an aircraft counts anywhere it could turn or speed up to, with a velocity or without. What would: its altitude, ranged, above the keep-out's ceiling (a radar that measures height), the effect sooner (the platform closer), or the aircraft farther from the point than it could fly by then.".to_string(),
        ),
        ReachKind::NoVelocity => Some(
            "When the tracker has a velocity estimate for it (it gives none while its velocity is uncertain, as in a turn), this reach becomes a prediction along its course.".to_string(),
        ),
        _ => None,
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
    let base = Item {
        rule: RuleId::KeepOutFriendly,
        subject: Subject::Vehicle,
        id: uid.to_string(),
        label: ctx.opts.name(uid),
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
        clear_windows: Vec::new(),
        wait: None,
        not_by_waiting: None,
        other: None,
        lost: None,
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
            // altitude matters for one of our aircraft
            let air = v.platform.as_ref().is_some_and(|p| p.domain == Domain::Air);
            let altitude = match alt {
                _ if !air => None,
                Some(_) if blocks && alt_known => {
                    Some("In 3D too: its altitude puts it within the keep-out's vertical band (or is not fresh).".to_string())
                }
                Some(_) if !alt_known => Some("It reports no altitude, so the keep-out has no ceiling for it.".to_string()),
                None => Some(
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

/// Same picture: the stretches of effect times, from the earliest any position allows to
/// the latest a nomination allows, at which each listed blocking item is clear, and at which
/// every keep-out holds (the platform's distances, when the target holds where it is).
/// `more_blocking`: some blocking items are not listed (their windows are not computed).
fn same_picture(ctx: &Ctx, items: &mut [(Item, Probe)], more_blocking: bool) -> MoveWithin {
    let (world, ec, cfg, opts) = (ctx.world, &ctx.ec, ctx.cfg, ctx.opts);
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
        .chain(judged_lost(world, ctx.track_id, ec).into_iter().map(|(id, l)| Probe::Lost(id, &l.facts)))
        .collect();
    // murmur-dr6: a grid point counts as clear only by a margin that covers how far a region's
    // edge can dip between grid points (the windows below claim every effect time between)
    let scan = ctx.clearance + grid_margin_m(ctx.clearance);
    let clear_at = |p: &Probe, point: LonLat, at: Time, alt: Option<EffectAlt>| match p {
        Probe::Track(_, t) => track_clear(t, point, at, scan, world.t, ec, alt),
        Probe::Lost(_, t) => lost_clear(t, point, at, scan, world.t, ec, alt),
        Probe::Vehicle(u, v) => vehicle_clear(u, v, point, at, scan, world, cfg, alt),
    };
    let start = ctx.pv.start_t;
    // L1: an effect never comes before the window opens (the preview waits for it)
    let opens = ctx.window.from.filter(|f| *f > start);
    let earliest = opens.unwrap_or(start);
    let longest = start.plus_ms(ms(ec.max_transit_s));
    let latest = ctx.window.until.min(longest);
    let holding = engage::target_velocity(ctx.target, ec) == [0.0, 0.0];
    let cruise = ctx.pv.cruise_mps;
    let window_of = |(a, b): (Time, Time)| EffectWindow {
        from_t: a,
        until_t: b,
        from_m: holding.then(|| cruise * secs(a.ms_since(start))),
        until_m: holding.then(|| cruise * secs(b.ms_since(start))),
    };
    let blocking: Vec<usize> = (0..items.len()).filter(|&i| items[i].0.blocks).collect();
    let mut item_runs: Vec<Runs> = blocking.iter().map(|_| Runs::default()).collect();
    let mut all_runs = Runs::default();
    let mut last_fail: Option<usize> = None;
    let mut earliest_blocker: Option<String> = None;
    if latest >= earliest {
        for at in grid(earliest, latest, SPAN_STEP_MS) {
            let point = engage::target_at(ctx.target, at, ec);
            let alt = engage::effect_alt(Some(ctx.target), world.t, at, ctx.radius, ec);
            for (k, &i) in blocking.iter().enumerate() {
                item_runs[k].push(at, clear_at(&items[i].1, point, at, alt));
            }
            // every keep-out: the probe that failed last time first (it usually fails again)
            let ok = if let Some(j) = last_fail
                && !clear_at(&all[j], point, at, alt)
            {
                false
            } else {
                match all.iter().position(|p| !clear_at(p, point, at, alt)) {
                    None => true,
                    Some(j) => {
                        last_fail = Some(j);
                        false
                    }
                }
            };
            if at == earliest && !ok {
                earliest_blocker = last_fail.map(|j| all[j].label(opts));
            }
            all_runs.push(at, ok);
        }
    }
    for (k, runs) in item_runs.into_iter().enumerate() {
        let windows = runs.finish();
        let item = &mut items[blocking[k]].0;
        item.fits_until_t = windows.first().filter(|w| w.0 == earliest).map(|w| w.1);
        item.clear_windows = windows.into_iter().take(MAX_WINDOWS).map(window_of).collect();
    }
    let windows = all_runs.finish();
    let name = ctx.platform_name();
    let from_now = |t: Time| after(t, world.t);
    let range = format!(
        "between {} from now (the earliest: {}) and {} from now ({})",
        from_now(earliest),
        if opens.is_some() {
            "the window opens then".to_string()
        } else {
            format!("the {:.0} s dispatch delay, the platform already at the point", ec.dispatch_delay_s)
        },
        from_now(latest),
        if latest == ctx.window.until {
            "the window closes"
        } else {
            "the longest transit"
        }
    );
    let unlisted = if more_blocking {
        " (more items block than are listed)"
    } else {
        ""
    };
    let first = windows.first().copied();
    let listed: Vec<EffectWindow> = windows.iter().copied().take(MAX_WINDOWS).map(window_of).collect();
    let mut mw = MoveWithin {
        possible: first.is_some(),
        earliest_t: earliest,
        latest_t: latest,
        windows: listed,
        effect_by_t: None,
        point: None,
        within_m: None,
        now_m: holding.then(|| haversine_m(ctx.pv.start, engage::target_at(ctx.target, world.t, ec))),
        holding,
        why: String::new(),
    };
    let blocker = earliest_blocker.map_or(String::new(), |b| format!(", by {b}"));
    match first {
        None => {
            mw.why = format!(
                "No position of {name} clears it on this picture: no effect time on a {} s grid {range} passes every keep-out{unlisted}; the earliest is blocked{blocker}.",
                SPAN_STEP_MS as f64 / 1000.0
            );
        }
        Some((a, b)) if a == earliest => {
            let p = engage::target_at(ctx.target, b, ec);
            let within = cruise * secs(b.ms_since(start));
            let now_m = haversine_m(ctx.pv.start, p);
            mw.effect_by_t = Some(b);
            mw.point = Some(p);
            mw.within_m = Some(within);
            mw.now_m = Some(now_m);
            let of = if holding { "the target" } else { "where the target will be then" };
            mw.why = format!(
                "Every keep-out holds for an effect up to {} from now: {name} within {} of {of} (at {cruise:.0} m/s) would make it; it is {} away now.{}",
                from_now(b),
                dist(within),
                dist(now_m),
                farther(&windows[1..], &name, holding, world.t, &window_of)
            );
        }
        Some(_) => {
            mw.why = format!(
                "No closer position clears it: the earliest effect is blocked{blocker}.{}",
                farther(&windows, &name, holding, world.t, &window_of)
            );
        }
    }
    mw
}

/// The later windows in words: distances when the target holds, effect times otherwise.
fn farther(windows: &[(Time, Time)], name: &str, holding: bool, now: Time, window_of: &dyn Fn((Time, Time)) -> EffectWindow) -> String {
    if windows.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = windows
        .iter()
        .take(MAX_WINDOWS)
        .map(|w| {
            let e = window_of(*w);
            match (e.from_m, e.until_m) {
                (Some(a), Some(b)) => format!("{} to {} out (effect {} to {} from now)", dist(a), dist(b), after(w.0, now), after(w.1, now)),
                _ => format!("an effect {} to {} from now", after(w.0, now), after(w.1, now)),
            }
        })
        .collect();
    if holding {
        format!(" Farther, every keep-out holds with {name} {}.", parts.join("; or "))
    } else {
        format!(
            " Later, every keep-out holds for {}; with the target moving, the platform's distance alone does not set the effect time.",
            parts.join("; or ")
        )
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
    let reach = now_kinds.iter().any(|k| ["young", "manoeuvre", "no_velocity", "aircraft"].contains(k));
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

/// The picture carried forward: for each listed blocking item (the first `shown`) that
/// waiting may clear, the first nomination time a dry run would find it clear; and the first
/// at which every keep-out holds. A track out of view and a lost vehicle of ours are not
/// carried forward as seen: the first says what happens (the tracker drops a track long
/// unseen), the second that its reach grows.
fn carried_forward(ctx: &Ctx, items: &mut [(Item, Probe)], shown: usize) -> (Option<Wait>, Option<String>) {
    let (world, ec, cfg, mem) = (ctx.world, &ctx.ec, ctx.cfg, &ctx.opts.memory);
    let now = world.t;
    // out of view, lost, or not placed at all: no wait; each says what happens instead
    let mut stuck: Vec<String> = Vec::new();
    for (item, probe) in items.iter_mut().filter(|(i, _)| i.blocks) {
        let out_of_view = match probe {
            Probe::Track(_, t) => (!in_view(t, now, mem)).then(|| secs(now.ms_since(t.last_seen_t))),
            Probe::Vehicle(..) | Probe::Lost(..) => None,
        };
        if let Some(unseen) = out_of_view {
            item.not_by_waiting = Some(if ec.lost() {
                // rules 13: dropped, it stays in the keep-out as a lost contact
                format!(
                    "Not seen for {unseen:.0} s: its region grows while it is unseen. The tracker drops a track unseen for {:.0} s (about {:.0} s from now, unless it is seen again); it then stays in the keep-out as a lost contact, its region still growing, until an operator with engagement authority accepts it for an engagement.",
                    mem.drop_after_s,
                    (mem.drop_after_s - unseen).max(0.0)
                )
            } else {
                format!(
                    "Not seen for {unseen:.0} s: its region grows while it is unseen. The tracker drops a track unseen for {:.0} s, after which it no longer counts in the keep-out (about {:.0} s from now, unless it is seen again).",
                    mem.drop_after_s,
                    (mem.drop_after_s - unseen).max(0.0)
                )
            });
        }
        if item.not_by_waiting.is_some() {
            stuck.push(item.label.clone());
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
        circle: bool,
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
            let (rep, vel, circle) = reporting(v, world, cfg);
            V {
                uid: u,
                base: v,
                rep,
                vel,
                circle,
                then: v.clone(),
            }
        })
        .collect();
    // rules 13: the lost contacts the keep-out judges
    let lost = judged_lost(world, ctx.track_id, ec);
    // the target and the platform, for the preview a nomination then would get
    let mut lean = WorldSnapshot::new(now);
    lean.unknown_accepted = world.unknown_accepted.clone();
    lean.tracks.insert(ctx.track_id.to_string(), ctx.target.clone());
    let plat = ctx.platform.and_then(|u| world.vehicles.get(u).map(|v| (u, v, reporting(v, world, cfg))));
    if let Some((u, v, _)) = plat {
        lean.vehicles.insert(u.clone(), v.clone());
    }
    let mut pending: Vec<usize> = (0..shown)
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
            project_track_into(ctx.target, t, now, w, mem);
        }
        if let Some((u, base, (rep, vel, _))) = plat
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
                project_track_into(t.base, &mut t.then, now, w, mem);
            }
        }
        for v in vehicles.iter_mut() {
            if want_all || pending.iter().any(|&i| items[i].0.subject == Subject::Vehicle && items[i].0.id == v.uid.to_string()) {
                project_vehicle_into(v.base, &mut v.then, v.circle, v.rep, v.vel, now, w);
            }
        }
        let track_ok = |t: &T| track_clear(&t.then, point, at, ctx.clearance, w, ec, alt);
        // rules 13: a lost contact stays as it is (its reach grows with the span by itself)
        let lost_ok = |l: &&crate::lost::LostContact| lost_clear(&l.facts, point, at, ctx.clearance, w, ec, alt);
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
                // rules 13: never pending (it is stuck: waiting only grows its reach)
                Subject::LostContact => (false, String::new()),
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
        if want_all && tracks.iter().all(track_ok) && vehicles.iter().all(vehicle_ok) && lost.iter().map(|(_, l)| l).all(lost_ok) {
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
        Some(format!(
            "No wait is given while {} cannot be carried forward (see each).",
            stuck.join(", ")
        ))
    } else {
        Some(format!(
            "No nomination within {} min has every keep-out holding at once, if everything keeps behaving as now.",
            WAIT_HORIZON_S / 60
        ))
    };
    (all, none)
}
