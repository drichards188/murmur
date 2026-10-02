//! Satellite tasking: the `collect` and `cover` intents, on the space segment
//! (`crates/murmur-space`).
//!
//! Satellites are requested, not flown (docs/DECISIONS.md, "Space as taskable
//! infrastructure"). A **collect** asks an imaging constellation (optical or SAR) for one
//! pass over an area in a window; a **cover** asks whether SATCOM or GNSS service holds over
//! an area for a window. Neither targets a fleet vehicle: [`crate::Task::targets`] says so,
//! and the vehicle rules never run for them.
//!
//! ## Determinism and replay
//!
//! A space query is SGP4 over cached element sets: it costs milliseconds to seconds and its
//! answer depends on the cache. The engine runs each query once through a [`SpaceOracle`]
//! and logs the query and its whole answer (the result's `time_map`, the segment identity
//! with `cache_meta`) as a `space.query` event with a content hash. Decision inputs
//! reference the answer by that hash, as they reference the area set, and the rules read
//! only the logged answer. So:
//!
//! - replay re-decides every collect and cover from the log alone, with no cache;
//! - with the same space segment loaded ([`crate::replay::replay_str_with_space`]), replay
//!   also re-runs every query and compares the answers byte for byte;
//! - if the loaded segment is not the one the query ran on (different element sets or
//!   catalog), replay reports a **cache mismatch**, separately from decision mismatches.
//!
//! The segment's identity ([`SegmentId`]) is a SHA-256 over the catalog and, per satellite,
//! its identity, element epoch, mean elements and SGP4 state at two instants (so every
//! element that changes a prediction changes the hash), with the cache's fetch metadata
//! alongside for people. `compute_ms` is zeroed in logged answers: it is the one
//! non-deterministic field.
//!
//! ## Scenario time
//!
//! Times in a collect or cover (window, deadline, chosen collect time) are **scenario
//! time**, as `murmur-space` reports it, in integer milliseconds like every [`Time`]. The
//! query's "now" (tasking lead time counts from it) is [`SpaceOracle::scenario_time`] of the
//! snapshot time: the same instant unless the bridge runs a scenario clock with an offset
//! ([`SpaceSegment::with_scenario_offset_ms`]) or at another rate
//! ([`SpaceSegment::with_scenario_map`], a [`ScenarioMap`]). The map and the segment's
//! pinned time shift are logged in `space.loaded`; replay reads both from there.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use murmur_space::access::Interval;
use murmur_space::cache::CacheMeta;
use murmur_space::space::{GnssQuery, LinkQuery};
use murmur_space::tasking::{NoCloudModel, Rank, TaskOption, TaskRequest, TaskingResult};
use murmur_space::{Capability, EpochMode, Geodetic, Space, TimeMap};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::geo::LonLat;
use crate::hash::sha256_hex;
use crate::ids::{IntentId, Uid};
use crate::intent::Task;
use crate::time::Time;

// ---- intent parameters ----------------------------------------------------------------

/// The ground a collect images or a cover serves. Points are `[lon, lat]` like every point
/// in this crate. Pointing and service are computed at the centroid (the vertex mean for a
/// polygon); the radius decides whether one scene covers the area.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpaceArea {
    Polygon { ring: Vec<LonLat> },
    Circle { center: LonLat, radius_km: f64 },
}

/// Largest area a collect or cover may name (radius from the centroid). Pointing at one
/// centroid stops meaning much beyond this.
pub const MAX_AREA_RADIUS_KM: f64 = 200.0;
/// Longest window a collect or cover may name: link queries scale with it.
pub const MAX_WINDOW_MS: i64 = 7 * 24 * 3600 * 1000;

impl SpaceArea {
    pub fn to_space(&self) -> murmur_space::Area {
        match self {
            SpaceArea::Polygon { ring } => murmur_space::Area::Polygon {
                ring: ring.iter().map(|p| [p.lon, p.lat]).collect(),
            },
            SpaceArea::Circle { center, radius_km } => murmur_space::Area::Circle {
                center: [center.lon, center.lat],
                radius_km: *radius_km,
            },
        }
    }

    /// Where pointing and service are computed.
    pub fn centroid(&self) -> LonLat {
        let g = self.to_space().centroid();
        LonLat::new(g.lon_deg, g.lat_deg)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        match self {
            SpaceArea::Polygon { ring } => {
                if ring.len() < 3 {
                    return Err("area polygon needs at least 3 points".into());
                }
                if !ring.iter().all(LonLat::is_valid) {
                    return Err("area point out of range".into());
                }
            }
            SpaceArea::Circle { center, radius_km } => {
                if !center.is_valid() {
                    return Err("area center out of range".into());
                }
                if !(radius_km.is_finite() && *radius_km > 0.0) {
                    return Err("area radius_km must be a positive number".into());
                }
            }
        }
        let r = self.to_space().radius_km();
        if !(r.is_finite() && r <= MAX_AREA_RADIUS_KM) {
            return Err(format!(
                "area radius {r:.0} km is larger than {MAX_AREA_RADIUS_KM:.0} km"
            ));
        }
        Ok(())
    }
}

/// A time window, scenario time, `from < to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub from: Time,
    pub to: Time,
}

impl Window {
    pub fn contains(&self, t: Time) -> bool {
        self.from <= t && t <= self.to
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        if to_utc(self.from).is_none() || to_utc(self.to).is_none() {
            return Err("window time out of range".into());
        }
        if self.to <= self.from {
            return Err("window end must be after its start".into());
        }
        if self.to.ms_since(self.from) > MAX_WINDOW_MS {
            return Err("window is longer than 7 days".into());
        }
        Ok(())
    }
}

/// Which imaging a collect will accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectCapability {
    Optical,
    Sar,
    /// Optical or SAR, whichever is feasible.
    Any,
}

impl CollectCapability {
    pub fn capabilities(self) -> Vec<Capability> {
        match self {
            CollectCapability::Optical => vec![Capability::Optical],
            CollectCapability::Sar => vec![Capability::Sar],
            CollectCapability::Any => vec![Capability::Optical, Capability::Sar],
        }
    }
}

/// The option a collect commits to: one satellite's pass, by NORAD id and collect time
/// (scenario time). Taken from a ranked option of an earlier query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChosenCollect {
    pub norad_id: u64,
    pub collect_time: Time,
}

impl ChosenCollect {
    /// The chosen option as an intent parameter (collect time truncated to milliseconds).
    pub fn from_option(o: &TaskOption) -> Self {
        ChosenCollect {
            norad_id: o.norad_id,
            collect_time: from_utc(o.collect_time),
        }
    }
}

/// What a cover asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoverService {
    /// SATCOM coverage from one catalog class (`starlink`, `iridium`, `wgs`).
    Satcom { class: String },
    /// GNSS availability from one constellation (`gps`).
    Gnss {
        #[serde(default = "gps")]
        class: String,
    },
}

fn gps() -> String {
    "gps".into()
}

impl CoverService {
    pub fn class(&self) -> &str {
        match self {
            CoverService::Satcom { class } | CoverService::Gnss { class } => class,
        }
    }

    /// The resource uid op states are reported under: `satcom:<class>` or `gnss:<class>`.
    pub fn resource(&self) -> Uid {
        match self {
            CoverService::Satcom { class } => Uid::new(format!("satcom:{class}")),
            CoverService::Gnss { class } => Uid::new(format!("gnss:{class}")),
        }
    }
}

// ---- configuration --------------------------------------------------------------------

/// Settings for the space rules: `[rules.space]` in `sim/policy.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpaceRules {
    /// How scenario time maps to the element sets' epoch (`auto`: shift whole days when the
    /// window is more than 2 days from the elements; see the murmur-space README).
    pub epoch_mode: EpochMode,
    /// How far (seconds) a fresh option of the chosen satellite may sit from the chosen
    /// collect time and still be the same opportunity. Options are the best sample of a
    /// pass, which moves as the lead-time cut-off moves; passes of one satellite are
    /// about 90 minutes apart.
    pub collect_match_tolerance_s: u32,
    /// Cover, SATCOM: minimum fraction of the window with a satellite above the mask, when
    /// the intent gives no `min_level`.
    pub cover_min_fraction: f64,
    /// Cover, GNSS: minimum satellites in view at every sample, when the intent gives no
    /// `min_level` (4 is the minimum for a 3D fix).
    pub gnss_min_visible: u32,
    /// Sample step for SATCOM coverage statistics, seconds.
    pub link_step_s: u32,
    /// Sample step for GNSS visibility, seconds.
    pub gnss_step_s: u32,
}

impl Default for SpaceRules {
    fn default() -> Self {
        SpaceRules {
            epoch_mode: EpochMode::Auto,
            collect_match_tolerance_s: 120,
            cover_min_fraction: 0.99,
            gnss_min_visible: 4,
            link_step_s: 60,
            gnss_step_s: 300,
        }
    }
}

impl SpaceRules {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.collect_match_tolerance_s == 0 {
            return Err("rules.space.collect_match_tolerance_s must be > 0".into());
        }
        if !(self.cover_min_fraction.is_finite() && (0.0..=1.0).contains(&self.cover_min_fraction))
        {
            return Err("rules.space.cover_min_fraction must be 0..1".into());
        }
        if self.link_step_s == 0 || self.gnss_step_s == 0 {
            return Err("rules.space step sizes must be > 0".into());
        }
        Ok(())
    }
}

// ---- queries and answers --------------------------------------------------------------

/// A query to the space segment, as logged.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "query", rename_all = "snake_case")]
pub enum SpaceQuery {
    /// Collect: ranked tasking options and exclusions.
    Tasking(TaskRequest),
    /// Cover, SATCOM: link windows and coverage from the area's centroid.
    Links(LinkQuery),
    /// Cover, GNSS: visibility and DOP at the area's centroid.
    Gnss(GnssQuery),
}

/// What the cover rule reads from a link query: coverage and outages, not the (possibly
/// tens of thousands of) individual windows.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LinkSummary {
    pub class_id: String,
    pub satellites: usize,
    pub stale_skipped: usize,
    pub not_yet_launched: usize,
    pub mask_deg: f64,
    pub windows: usize,
    pub coverage_fraction: f64,
    pub outages: Vec<Interval>,
    pub longest_outage_s: f64,
    pub visible_min: usize,
    pub visible_max: usize,
    pub visible_mean: f64,
    pub latency_ms_min: Option<f64>,
    pub latency_ms_median: Option<f64>,
    pub latency_ms_max: Option<f64>,
}

/// What the cover rule reads from a GNSS query.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GnssSummary {
    pub class_id: String,
    pub mask_deg: f64,
    pub satellites: usize,
    pub stale_skipped: usize,
    pub not_yet_launched: usize,
    pub samples: usize,
    pub visible_min: usize,
    pub visible_max: usize,
    /// First sample with the fewest satellites in view.
    pub visible_min_at: Option<DateTime<Utc>>,
    pub pdop_max: Option<f64>,
    pub pdop_mean: Option<f64>,
}

/// A query's output, as logged (`compute_ms` zeroed).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SpaceResult {
    Tasking(TaskingResult),
    Links(LinkSummary),
    Gnss(GnssSummary),
}

/// Which space segment answered: the catalog and element sets, by content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentId {
    /// SHA-256 over the catalog and every satellite (identity, element epoch, mean elements,
    /// SGP4 state at the reference time and a day later).
    pub sha256: String,
    /// Median element epoch: what "now" means for the elements.
    pub reference: DateTime<Utc>,
    pub satellites: usize,
    /// Per CelesTrak query: when the cached element sets were fetched (empty for in-memory
    /// sources such as test fixtures).
    pub cache_meta: Vec<CacheMeta>,
}

impl PartialEq for SegmentId {
    fn eq(&self, other: &Self) -> bool {
        self.sha256 == other.sha256
    }
}

impl SegmentId {
    /// The identity of a loaded [`Space`].
    pub fn of(space: &Space) -> Result<Self, String> {
        let mut h = Sha256::new();
        let catalog = serde_json::to_string(&space.catalog).map_err(|e| e.to_string())?;
        h.update(catalog.as_bytes());
        h.update(space.reference.to_rfc3339().as_bytes());
        let later = space.reference + Duration::days(1);
        let mut n = 0;
        for cs in &space.classes {
            h.update(b"\x00class:");
            h.update(cs.class.id.as_bytes());
            for s in &cs.sats {
                n += 1;
                h.update(b"\x00sat:");
                h.update(s.norad_id.to_le_bytes());
                h.update(s.name.as_bytes());
                h.update(s.epoch.to_rfc3339().as_bytes());
                for x in [s.mean_motion_rev_day, s.inclination_deg, s.eccentricity] {
                    h.update(x.to_bits().to_le_bytes());
                }
                for t in [&space.reference, &later] {
                    match s.state(t) {
                        Ok(st) => {
                            for x in st.teme_pos.0.iter().chain(st.teme_vel.0.iter()) {
                                h.update(x.to_bits().to_le_bytes());
                            }
                        }
                        Err(_) => h.update(b"err"),
                    }
                }
            }
        }
        let digest = h.finalize();
        Ok(SegmentId {
            sha256: sha256_hex_of_digest(&digest),
            reference: space.reference,
            satellites: n,
            cache_meta: space.cache_meta.clone(),
        })
    }

    /// `"segment 1a2b3c4d5e6f (fetched 2026-10-02T09:00:00Z)"`, for details.
    pub fn short(&self) -> String {
        let fetched = self
            .cache_meta
            .iter()
            .map(|m| m.fetched_at)
            .max()
            .map_or("in-memory elements".to_string(), |t| {
                format!("fetched {}", fmt_utc(t))
            });
        format!(
            "segment {} ({fetched})",
            &self.sha256[..12.min(self.sha256.len())]
        )
    }
}

fn sha256_hex_of_digest(d: &[u8]) -> String {
    use std::fmt::Write;
    d.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// One query and its answer, as logged in `space.query`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpaceAnswer {
    pub query: SpaceQuery,
    /// Scenario time versus propagation time (callers must show a shift).
    pub time_map: TimeMap,
    pub segment: SegmentId,
    pub result: SpaceResult,
}

/// Answers compare by content (canonical JSON): `murmur-space` types carry no `PartialEq`.
impl PartialEq for SpaceAnswer {
    fn eq(&self, other: &Self) -> bool {
        match (serde_json::to_string(self), serde_json::to_string(other)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

impl SpaceAnswer {
    /// SHA-256 of the canonical serialization (compact `serde_json`, declaration order).
    pub fn content_hash(&self) -> Result<String, serde_json::Error> {
        Ok(sha256_hex(serde_json::to_string(self)?.as_bytes()))
    }

    pub fn tasking(&self) -> Option<&TaskingResult> {
        match &self.result {
            SpaceResult::Tasking(r) => Some(r),
            _ => None,
        }
    }

    /// The query's "now" (tasking only).
    pub fn now(&self) -> Option<Time> {
        match &self.query {
            SpaceQuery::Tasking(r) => Some(from_utc(r.now)),
            _ => None,
        }
    }
}

/// How scenario time follows engine time: `scenario = scenario_origin + (t - engine_origin)
/// x rate`, integer milliseconds. A bridge whose scenario clock starts at `MURMUR_AT` and
/// runs at `MURMUR_RATE` is exactly this map; a fixed offset is rate 1. Logged in
/// `space.loaded`, so replay can recompute what depends on it (a collect's approval
/// deadline).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioMap {
    pub engine_origin: Time,
    pub scenario_origin: Time,
    /// Scenario milliseconds per engine millisecond (> 0).
    pub rate: f64,
}

impl Default for ScenarioMap {
    fn default() -> Self {
        ScenarioMap::IDENTITY
    }
}

impl ScenarioMap {
    /// Scenario time is engine time.
    pub const IDENTITY: ScenarioMap = ScenarioMap {
        engine_origin: Time(0),
        scenario_origin: Time(0),
        rate: 1.0,
    };

    /// Scenario time = engine time + `ms`.
    pub const fn offset(ms: i64) -> Self {
        ScenarioMap {
            engine_origin: Time(0),
            scenario_origin: Time(ms),
            rate: 1.0,
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == ScenarioMap::IDENTITY
    }

    /// Scenario time at engine time `t` (rounded down to the millisecond).
    pub fn scenario(&self, t: Time) -> Time {
        let d = t.ms_since(self.engine_origin);
        if self.rate == 1.0 {
            return self.scenario_origin.plus_ms(d);
        }
        self.scenario_origin
            .plus_ms((d as f64 * self.rate).floor() as i64)
    }

    /// The earliest engine time at which the scenario has reached `s`.
    pub fn engine(&self, s: Time) -> Time {
        let d = s.ms_since(self.scenario_origin);
        if self.rate == 1.0 {
            return self.engine_origin.plus_ms(d);
        }
        let mut t = self
            .engine_origin
            .plus_ms((d as f64 / self.rate).ceil() as i64);
        // float rounding: settle on the exact boundary
        while self.scenario(t) < s {
            t = t.plus_ms(1);
        }
        while self.scenario(t.plus_ms(-1)) >= s {
            t = t.plus_ms(-1);
        }
        t
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(self.rate.is_finite() && self.rate > 0.0) {
            return Err(format!("scenario rate must be > 0 (got {})", self.rate));
        }
        Ok(())
    }
}

/// Where space answers come from. The engine calls it while checking a collect or cover;
/// replay calls it to recompute logged answers.
pub trait SpaceOracle {
    /// The loaded segment's identity; logged with every answer.
    fn segment(&self) -> &SegmentId;
    /// Answer a query. Must be deterministic: same segment and query, same answer.
    fn answer(&self, query: &SpaceQuery) -> Result<SpaceAnswer, String>;
    /// How scenario time follows engine time (logged in `space.loaded`).
    fn scenario_map(&self) -> ScenarioMap {
        ScenarioMap::IDENTITY
    }
    /// The time shift pinned on the segment for the session, if any (logged in
    /// `space.loaded`; `murmur-replay --space` pins the same shift before re-running
    /// queries).
    fn pinned_shift_days(&self) -> Option<i64> {
        None
    }
    /// Scenario time at engine time `t`.
    fn scenario_time(&self, t: Time) -> Time {
        self.scenario_map().scenario(t)
    }
}

/// The real space segment: a loaded [`Space`] (cheap to share, immutable after load).
pub struct SpaceSegment {
    space: Arc<Space>,
    id: SegmentId,
    map: ScenarioMap,
}

impl std::fmt::Debug for SpaceSegment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpaceSegment")
            .field("id", &self.id.sha256)
            .field("scenario_map", &self.map)
            .finish_non_exhaustive()
    }
}

impl SpaceSegment {
    pub fn new(space: Arc<Space>) -> Result<Self, String> {
        let id = SegmentId::of(&space)?;
        Ok(SpaceSegment {
            space,
            id,
            map: ScenarioMap::IDENTITY,
        })
    }

    /// Scenario time = engine time + `ms` (a scenario clock at rate 1).
    pub fn with_scenario_offset_ms(mut self, ms: i64) -> Self {
        self.map = ScenarioMap::offset(ms);
        self
    }

    /// Scenario time follows `map` (a scenario clock at any rate). An invalid rate is
    /// refused.
    pub fn with_scenario_map(mut self, map: ScenarioMap) -> Result<Self, String> {
        map.validate()?;
        self.map = map;
        Ok(self)
    }

    pub fn space(&self) -> &Space {
        &self.space
    }
}

impl SpaceOracle for SpaceSegment {
    fn segment(&self) -> &SegmentId {
        &self.id
    }

    fn answer(&self, query: &SpaceQuery) -> Result<SpaceAnswer, String> {
        answer(&self.space, &self.id, query)
    }

    fn scenario_map(&self) -> ScenarioMap {
        self.map
    }

    fn pinned_shift_days(&self) -> Option<i64> {
        self.space.pinned_shift_days
    }
}

/// Replay with a segment that was pinned in the run: the shift `space.loaded` recorded.
/// `murmur-replay --space` pins its freshly loaded segment with this before re-running
/// queries (an unpinned segment picks the shift per window, which can differ by a day).
pub fn logged_pinned_shift(text: &str) -> Option<i64> {
    text.lines()
        .filter(|l| l.contains("\"space.loaded\""))
        .find_map(|l| {
            let e: crate::audit::Event = serde_json::from_str(l).ok()?;
            match e.body {
                crate::audit::EventBody::SpaceLoaded {
                    pinned_shift_days, ..
                } => pinned_shift_days,
                _ => None,
            }
        })
}

/// When a pending collect's approval expires under `[approval.kind.collect] until =
/// "tasking_lead"`: the last moment the chosen pass can still be tasked, the collect time
/// minus the class's tasking lead time (scenario time). Read from the answer the
/// submission's checks used: the matched option's `earliest_collect - request now` is the
/// lead. None when the intent is not a collect with a matched option.
pub fn tasking_deadline(task: &Task, answer: &SpaceAnswer, tolerance_s: u32) -> Option<Time> {
    let Task::Collect {
        chosen: Some(chosen),
        ..
    } = task
    else {
        return None;
    };
    let result = answer.tasking()?;
    let opt = matched_option(result, chosen, tolerance_s)?;
    let lead = from_utc(opt.earliest_collect).ms_since(from_utc(result.request.now));
    Some(chosen.collect_time.plus_ms(-lead.max(0)))
}

/// Run `query` on `space` and package the answer for the log.
pub fn answer(
    space: &Space,
    segment: &SegmentId,
    query: &SpaceQuery,
) -> Result<SpaceAnswer, String> {
    let (time_map, result) = match query {
        SpaceQuery::Tasking(req) => {
            let mut r = space
                .tasking_options(req, &NoCloudModel)
                .map_err(|e| format!("tasking query failed: {e}"))?;
            r.compute_ms = 0;
            (r.time_map, SpaceResult::Tasking(r))
        }
        SpaceQuery::Links(q) => {
            let [class] = q.classes.as_slice() else {
                return Err("a cover link query names exactly one class".into());
            };
            let cs = space.class(class).map_err(|e| e.to_string())?;
            if cs.class.capability != Capability::Satcom {
                return Err(format!("{class} is not a SATCOM class"));
            }
            let rep = space
                .links(q)
                .map_err(|e| format!("link query failed: {e}"))?;
            let Some(c) = rep.classes.into_iter().next() else {
                return Err(format!("link query returned nothing for {class}"));
            };
            (
                rep.time_map,
                SpaceResult::Links(LinkSummary {
                    class_id: c.class_id,
                    satellites: c.satellites,
                    stale_skipped: c.stale_skipped,
                    not_yet_launched: c.not_yet_launched,
                    mask_deg: c.mask_deg,
                    windows: c.windows.len(),
                    coverage_fraction: c.coverage_fraction,
                    outages: c.outages,
                    longest_outage_s: c.longest_outage_s,
                    visible_min: c.visible_min,
                    visible_max: c.visible_max,
                    visible_mean: c.visible_mean,
                    latency_ms_min: c.latency_ms_min,
                    latency_ms_median: c.latency_ms_median,
                    latency_ms_max: c.latency_ms_max,
                }),
            )
        }
        SpaceQuery::Gnss(q) => {
            let cs = space.class(&q.class).map_err(|e| e.to_string())?;
            if cs.class.capability != Capability::Gnss {
                return Err(format!("{} is not a GNSS class", q.class));
            }
            let rep = space
                .gnss(q)
                .map_err(|e| format!("GNSS query failed: {e}"))?;
            let visible_min_at = rep
                .samples
                .iter()
                .find(|s| s.visible == rep.visible_min)
                .map(|s| s.t);
            (
                rep.time_map,
                SpaceResult::Gnss(GnssSummary {
                    class_id: rep.class_id,
                    mask_deg: rep.mask_deg,
                    satellites: rep.satellites,
                    stale_skipped: rep.stale_skipped,
                    not_yet_launched: rep.not_yet_launched,
                    samples: rep.samples.len(),
                    visible_min: rep.visible_min,
                    visible_max: rep.visible_max,
                    visible_min_at,
                    pdop_max: rep.pdop_max,
                    pdop_mean: rep.pdop_mean,
                }),
            )
        }
    };
    Ok(SpaceAnswer {
        query: query.clone(),
        time_map,
        segment: segment.clone(),
        result,
    })
}

/// The query a collect or cover asks, at scenario time `now`. `None` for vehicle tasks.
/// Pure: the space rule rebuilds it from the logged answer's `now` and checks that the
/// answer is the answer to this intent's question.
pub fn query_for(task: &Task, now: Time, cfg: &SpaceRules) -> Option<Result<SpaceQuery, String>> {
    let utc = |t: Time, what: &str| to_utc(t).ok_or_else(|| format!("{what} out of range"));
    Some(match task {
        Task::Collect {
            area,
            window,
            capability,
            min_resolution_m,
            product_deadline,
            ..
        } => (|| {
            Ok(SpaceQuery::Tasking(TaskRequest {
                capabilities: capability.capabilities(),
                area: area.to_space(),
                from: utc(window.from, "window start")?,
                to: utc(window.to, "window end")?,
                now: utc(now, "now")?,
                product_deadline: product_deadline
                    .map(|d| utc(d, "product deadline"))
                    .transpose()?,
                max_resolution_m: *min_resolution_m,
                rank: Rank::EarliestProduct,
                classes: Vec::new(),
                include_untaskable: false,
                epoch_mode: cfg.epoch_mode,
            }))
        })(),
        Task::Cover {
            area,
            window,
            service,
            ..
        } => (|| {
            let c = area.centroid();
            let point = Geodetic::new(c.lat, c.lon, 0.0);
            let (from, to) = (
                utc(window.from, "window start")?,
                utc(window.to, "window end")?,
            );
            Ok(match service {
                CoverService::Satcom { class } => SpaceQuery::Links(LinkQuery {
                    point,
                    from,
                    to,
                    classes: vec![class.clone()],
                    epoch_mode: cfg.epoch_mode,
                    sample_step_s: f64::from(cfg.link_step_s),
                    include_samples: false,
                }),
                CoverService::Gnss { class } => SpaceQuery::Gnss(GnssQuery {
                    point,
                    from,
                    to,
                    step_s: f64::from(cfg.gnss_step_s),
                    class: class.clone(),
                    mask_deg: None,
                    epoch_mode: cfg.epoch_mode,
                }),
            })
        })(),
        _ => return None,
    })
}

/// The fresh option that is the chosen one: same satellite, collect time within the
/// tolerance, nearest first.
pub fn matched_option<'a>(
    result: &'a TaskingResult,
    chosen: &ChosenCollect,
    tolerance_s: u32,
) -> Option<&'a TaskOption> {
    let tol = i64::from(tolerance_s) * 1000;
    result
        .options
        .iter()
        .filter(|o| o.norad_id == chosen.norad_id)
        .map(|o| {
            (
                from_utc(o.collect_time).ms_since(chosen.collect_time).abs(),
                o,
            )
        })
        .filter(|(d, _)| *d <= tol)
        .min_by_key(|(d, o)| (*d, o.rank))
        .map(|(_, o)| o)
}

// ---- dispatch -------------------------------------------------------------------------

/// A collect, approved: a simulated tasking request for the bridge (no real satellite
/// operator API). See the crate docs for the contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CollectOrder {
    pub intent: IntentId,
    /// The satellite as a tasking resource, `norad:<id>`: the uid to report op states under.
    pub resource: Uid,
    pub norad_id: u64,
    pub satellite: String,
    pub class_id: String,
    pub capability: Capability,
    /// When the collection happens (scenario time): the "collection" event.
    pub collect_time: Time,
    /// When the product is in hand (scenario time): the "product ready" event. None when
    /// the class's timing is unknown.
    pub product_time: Option<Time>,
    pub resolution_m: Option<f64>,
    pub area: SpaceArea,
    /// Approximate scene on the ground.
    pub scene: Option<Vec<LonLat>>,
    /// The `space.query` answer the last check read.
    pub answer_sha256: String,
}

/// A cover, approved: the bridge reports service over the window (outages from the answer).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CoverOrder {
    pub intent: IntentId,
    /// `satcom:<class>` or `gnss:<class>`: the uid to report op states under.
    pub resource: Uid,
    pub service: CoverService,
    pub area: SpaceArea,
    pub window: Window,
    pub answer_sha256: String,
}

/// The space part of an `intent.dispatched` event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "order", rename_all = "snake_case")]
pub enum SpaceOrder {
    Collect(CollectOrder),
    Cover(CoverOrder),
}

// ---- time helpers ---------------------------------------------------------------------

/// A [`Time`] (ms since the Unix epoch) as a UTC instant.
pub fn to_utc(t: Time) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(t.0)
}

/// A UTC instant as a [`Time`], truncated to the millisecond.
pub fn from_utc(t: DateTime<Utc>) -> Time {
    Time(t.timestamp_millis())
}

/// `2026-06-14T15:09:01Z`: fixed format for detail strings.
pub fn fmt_utc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// [`fmt_utc`] of a [`Time`].
pub fn fmt_time(t: Time) -> String {
    to_utc(t).map_or_else(|| format!("{t}"), fmt_utc)
}
