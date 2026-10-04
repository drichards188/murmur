//! What goes to browsers: the full state once, then 4 times a second only the vehicles that
//! changed (with just their new trail points) and the task list only when it changed.
//!
//! A frame (every vehicle's snapshot, serialized once) is built per tick and published on a
//! watch channel; each WebSocket client diffs it against what that client already has. A slow
//! client skips frames instead of stalling the others, and its next delta is still correct.
//!
//! Two feeds share the frame (murmur-and):
//!
//! - `/ws`: the feed as it always was. Every vehicle at full detail, its identity in every
//!   update, every trail point.
//! - `/ws?lod=1`: level of detail by what the client looks at. The client sends its view
//!   (`{"type":"view","bounds":[w,s,e,n],"zoom":z,"pin":[uid,...]}`) whenever it moves, and
//!   from then on vehicles in (or near) the view, and the pinned ones (selected, followed, in
//!   the inbox), come at full detail at 4 Hz with their trail sampled to the zoom; the rest
//!   come as a *lite* snapshot (`"lod":"lite"`: identity, a coarse position, mode, link
//!   state) once a second. Identity goes once, then only when it changes; every update is a
//!   partial object the client merges over what it has. When more than `max_full` vehicles
//!   are in view (zoomed out over the fleet) none of them is full: the delta carries
//!   `clusters` instead, counts per map tile by domain, link state and track affiliation,
//!   which the console draws as cluster markers. A fleet of `min_fleet` vehicles or fewer gets
//!   the plain feed through either URL, so a small fleet behaves as before.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use murmur_decide::catalog::Domain;
use murmur_decide::world::Affiliation;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::clock::{self, round};
use crate::link::LinkSnapshot;
use crate::mav::Kind;
use crate::vehicle::{Identity, NavSource, Uid, VehicleSnapshot};
use crate::world::World;

pub const PUSH_PERIOD: Duration = Duration::from_millis(250);
/// Lite snapshots and cluster counts go out every this many frames: once a second.
pub const LITE_EVERY: u64 = 4;
/// The view is grown by this fraction of its width and height on each side before deciding
/// what is in it, so a pan reveals vehicles that already have full detail.
const VIEW_MARGIN: f64 = 0.25;

type Point = [f64; 3];

/// Half of a snapshot's body: its members, and its `link` object's members, each without
/// braces and ready to splice into one object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    pub inner: String,
    pub link: String,
}

pub struct FrameVehicle {
    pub uid: Uid,
    /// the snapshot without its identity and trail, as JSON
    pub body: String,
    /// the body split for the level-of-detail feed: what changes with every report, and
    /// what seldom does (`split_body`), diffed apart so an update is mostly the former
    pub fast: Part,
    pub slow: Part,
    /// the identity as JSON (`null` without a manifest entry), shared across frames
    pub identity: Arc<str>,
    /// the lite snapshot (what the fleet list and the clusters need), as JSON
    pub lite: String,
    pub trail_count: u64,
    pub trail: Arc<Vec<Point>>,
    /// lon, lat
    pub pos: Option<[f64; 2]>,
    pub domain: Domain,
    /// the link counts as lost (`LinkSnapshot::stale_after_s`)
    pub stale: bool,
}

/// A track's last fix and estimated affiliation, for the cluster counts.
pub struct TrackMark {
    pub lon: f64,
    pub lat: f64,
    pub affiliation: Affiliation,
}

pub struct PushFrame {
    pub t: f64,
    pub vehicles: Vec<FrameVehicle>,
    pub taskings: String,
    pub tracks: Vec<TrackMark>,
}

/// What a frame reuses from the last one: trails change once a second at most, identities
/// hardly ever, so their JSON is kept until they do.
#[derive(Default)]
pub struct FrameCache {
    trails: HashMap<Uid, (u64, Arc<Vec<Point>>)>,
    /// uid -> (identity Arc address, its JSON)
    identities: HashMap<Uid, (usize, Arc<str>)>,
}

impl FrameCache {
    fn identity_json(&mut self, uid: Uid, identity: Option<&Arc<Identity>>) -> Arc<str> {
        let addr = identity.map_or(0, |i| Arc::as_ptr(i) as usize);
        match self.identities.get(&uid) {
            Some((a, json)) if *a == addr => json.clone(),
            _ => {
                let json: Arc<str> = identity
                    .map_or_else(|| "null".to_string(), |i| json(&**i))
                    .into();
                self.identities.insert(uid, (addr, json.clone()));
                json
            }
        }
    }
}

/// The lite link: lost or not (the bridge's own rule, so a live link costs nothing to keep
/// reporting), the last packet time only once lost (for the age), loss in bands of 10%.
/// No round trip: every TIMESYNC sample lands somewhere else (tens of ms of jitter on any
/// link), so any figure would be a change every second for every vehicle; it is detail
/// for the vehicle's card, which comes in full.
#[derive(Serialize)]
struct LiteLink<'a> {
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_rx_t: Option<f64>,
    loss_pct: Option<i64>,
    expected_interval_ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    emu: Option<&'a crate::emu::EmuBrief>,
}

/// What the fleet list, the top bar and the cluster markers need of a vehicle that is not
/// in view: identity (sent separately), a coarse position, mode, link state.
#[derive(Serialize)]
struct Lite<'a> {
    uid: Uid,
    sysid: u8,
    kind: Option<Kind>,
    mode: Option<&'a str>,
    armed: bool,
    position: Option<[f64; 2]>,
    alt_msl: Option<f64>,
    groundspeed: Option<f64>,
    nav_ready: bool,
    nav_source: NavSource,
    link: LiteLink<'a>,
    lod: &'static str,
}

fn coarse(x: Option<i64>, to: i64) -> Option<i64> {
    x.map(|v| (v as f64 / to as f64).round() as i64 * to)
}

fn lite_link(link: &LinkSnapshot, stale: bool) -> LiteLink<'_> {
    LiteLink {
        stale,
        last_rx_t: if stale { link.last_rx_t } else { None },
        loss_pct: coarse(link.loss_pct, 10),
        expected_interval_ms: link.expected_interval_ms,
        emu: link.emu.as_ref(),
    }
}

fn lite_of(s: &VehicleSnapshot, stale: bool) -> Lite<'_> {
    Lite {
        uid: s.uid,
        sysid: s.sysid,
        kind: s.kind,
        mode: s.mode.as_deref(),
        armed: s.armed,
        position: s.position.map(|[lon, lat]| [round(lon, 4), round(lat, 4)]),
        alt_msl: s.alt_msl.map(|a| round(a, 0)),
        groundspeed: s.groundspeed.map(|g| round(g, 0)),
        nav_ready: s.nav_ready,
        nav_source: s.nav_source,
        link: lite_link(&s.link, stale),
        lod: "lite",
    }
}

/// The catalog's domain, else the one the autopilot type implies (as the console does).
fn domain_of(identity: Option<&Identity>, kind: Option<Kind>) -> Domain {
    match identity.and_then(|i| i.str("domain")) {
        Some("air") => Domain::Air,
        Some("ground") => Domain::Ground,
        Some("surface") => Domain::Surface,
        Some("subsurface") => Domain::Subsurface,
        _ => match kind {
            Some(Kind::Plane | Kind::Vtol | Kind::Copter | Kind::Blimp) => Domain::Air,
            Some(Kind::Boat) => Domain::Surface,
            Some(Kind::Sub) => Domain::Subsurface,
            _ => Domain::Ground,
        },
    }
}

pub fn build_frame(world: &World, cache: &mut FrameCache) -> PushFrame {
    let now = clock::mono();
    let wall = clock::wall();
    let mut vehicles = Vec::new();
    for cell in world.vehicles() {
        let link = cell.link_snapshot(now);
        let (mut snap, trail_count, trail) = cell.read(|v| {
            let trail = match cache.trails.get(&cell.uid) {
                Some((count, trail)) if *count == v.trail_count => trail.clone(),
                _ => Arc::new(v.trail.iter().copied().collect()),
            };
            (
                v.snapshot(cell.uid, cell.sysid, link, now),
                v.trail_count,
                trail,
            )
        });
        cache.trails.insert(cell.uid, (trail_count, trail.clone()));
        let identity = snap.identity.take();
        let identity_json = cache.identity_json(cell.uid, identity.as_ref());
        let domain = domain_of(identity.as_deref(), snap.kind);
        let stale = snap
            .link
            .last_rx_t
            .is_none_or(|rx| wall - rx > snap.link.stale_after_s());
        let lite = json(&lite_of(&snap, stale));
        let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
        let (fast, slow) = split_body(&body);
        vehicles.push(FrameVehicle {
            uid: cell.uid,
            body,
            fast,
            slow,
            identity: identity_json,
            lite,
            trail_count,
            trail,
            pos: snap.position,
            domain,
            stale,
        });
    }
    let taskings = serde_json::to_string(&world.tasks.snapshot()).unwrap_or_else(|_| "[]".into());
    let tracks = crate::tracks::store().map_or_else(Vec::new, |t| t.marks());
    PushFrame {
        t: round(wall, 2),
        vehicles,
        taskings,
        tracks,
    }
}

fn json<T: serde::Serialize + ?Sized>(x: &T) -> String {
    serde_json::to_string(x).unwrap_or_else(|_| "null".into())
}

/// The members of a JSON object, as (key, raw value) slices. The input is what serde_json
/// wrote, so it is well formed; anything else ends the scan early.
fn members(obj: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let b = obj.as_bytes();
    let mut i = 1; // past '{'
    while i < b.len() {
        // key
        while i < b.len() && (b[i] == b' ' || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() || b[i] != b'"' {
            break;
        }
        let key_start = i + 1;
        i += 1;
        while i < b.len() && b[i] != b'"' {
            i += if b[i] == b'\\' { 2 } else { 1 };
        }
        let key = &obj[key_start..i];
        i += 1; // closing quote
        while i < b.len() && (b[i] == b' ' || b[i] == b':') {
            i += 1;
        }
        // value: to the next comma or brace at depth 0, outside strings
        let value_start = i;
        let mut depth = 0i32;
        let mut in_str = false;
        while i < b.len() {
            let c = b[i];
            if in_str {
                if c == b'\\' {
                    i += 1;
                } else if c == b'"' {
                    in_str = false;
                }
            } else {
                match c {
                    b'"' => in_str = true,
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' if depth > 0 => depth -= 1,
                    b',' | b'}' if depth == 0 => break,
                    _ => {}
                }
            }
            i += 1;
        }
        out.push((key, &obj[value_start..i]));
    }
    out
}

/// Snapshot keys that seldom change; everything else (and any key added later) changes
/// with every report and goes in the fast part.
const SLOW_KEYS: [&str; 13] = [
    "sysid",
    "kind",
    "mode",
    "armed",
    "tracking",
    "nav_ready",
    "nav_source",
    "battery_pct",
    "gps_fix",
    "satellites",
    "mission_seq",
    "home",
    "statustext",
];
const SLOW_LINK_KEYS: [&str; 3] = ["expected_interval_ms", "emu", "telemetry"];

fn push_member(into: &mut String, key: &str, value: &str) {
    if !into.is_empty() {
        into.push(',');
    }
    into.push('"');
    into.push_str(key);
    into.push_str("\":");
    into.push_str(value);
}

/// Split a serialized snapshot into the part that changes with every report (position,
/// attitude, speeds, the link's packet time, loss and round trip, and any key this module
/// does not know) and the part that seldom does (mode, nav state, home, STATUSTEXT, the
/// link's plan and emulator state). Neither carries the uid; `assemble` adds it.
pub fn split_body(body: &str) -> (Part, Part) {
    let mut fast = Part::default();
    let mut slow = Part::default();
    for (key, value) in members(body) {
        match key {
            "uid" => {}
            "link" => {
                for (k, v) in members(value) {
                    let into = if SLOW_LINK_KEYS.contains(&k) { &mut slow.link } else { &mut fast.link };
                    push_member(into, k, v);
                }
            }
            k if SLOW_KEYS.contains(&k) => push_member(&mut slow.inner, k, value),
            k => push_member(&mut fast.inner, k, value),
        }
    }
    (fast, slow)
}

/// One update object from the parts being sent plus extra keys.
fn assemble(uid: Uid, parts: &[&Part], extra: &[(&str, &str)]) -> String {
    let mut out = format!("{{\"uid\":{uid}");
    let mut link = String::new();
    for p in parts {
        if !p.inner.is_empty() {
            out.push(',');
            out.push_str(&p.inner);
        }
        if !p.link.is_empty() {
            if !link.is_empty() {
                link.push(',');
            }
            link.push_str(&p.link);
        }
    }
    if !link.is_empty() {
        out.push_str(",\"link\":{");
        out.push_str(&link);
        out.push('}');
    }
    for (key, value) in extra {
        out.push_str(",\"");
        out.push_str(key);
        out.push_str("\":");
        out.push_str(value);
    }
    out.push('}');
    out
}

/// `{...body, "<key>": <value>, ...}` without re-serializing the body.
fn with_keys(body: &str, extra: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(body.len() + extra.iter().map(|(k, v)| k.len() + v.len() + 4).sum::<usize>());
    out.push_str(body.strip_suffix('}').unwrap_or(body));
    for (key, value) in extra {
        if out.len() > 1 {
            out.push(',');
        }
        out.push('"');
        out.push_str(key);
        out.push_str("\":");
        out.push_str(value);
    }
    out.push('}');
    out
}

/// Every `step`-th trail point by its index in the whole trail ever recorded (the oldest
/// kept point is number `trail_count - len`), from `since` on. Sampling by that index makes
/// the selection the same whether a trail goes out whole or in increments, and keeps it in
/// step with the trail the client already has.
fn sample(trail: &[Point], trail_count: u64, since: u64, step: u64) -> Vec<Point> {
    let step = step.max(1);
    let first = trail_count.saturating_sub(trail.len() as u64);
    trail
        .iter()
        .enumerate()
        .filter(|(j, _)| {
            let g = first + *j as u64;
            g >= since && g.is_multiple_of(step)
        })
        .map(|(_, p)| *p)
        .collect()
}

/// Seconds between trail points worth sending at a zoom: about a pixel of movement for a
/// vehicle at 20 m/s (MapLibre zoom levels, 37 degrees north).
pub fn trail_step(zoom: f64) -> u64 {
    if zoom >= 12.5 {
        1
    } else if zoom >= 11.5 {
        2
    } else if zoom >= 10.5 {
        4
    } else if zoom >= 9.5 {
        8
    } else {
        16
    }
}

/// The tile zoom whose tiles are 90 to 180 screen pixels wide at the client's zoom.
pub fn tile_zoom(zoom: f64) -> u8 {
    ((zoom + 0.5).floor() as i32 + 1).clamp(1, 22) as u8
}

/// Web Mercator tile of a point at tile zoom `z`.
pub fn tile(lon: f64, lat: f64, z: u8) -> (i64, i64) {
    let n = f64::from(1u32 << z);
    let x = ((lon + 180.0) / 360.0 * n).floor();
    let lat = lat.clamp(-85.0511, 85.0511).to_radians();
    let y = ((1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * n).floor();
    (x as i64, y as i64)
}

/// The full state message as `/api/state` serves it: every vehicle with its identity and
/// whole trail, and the task list.
pub fn state_json(frame: &PushFrame) -> String {
    let vehicles: Vec<String> = frame
        .vehicles
        .iter()
        .map(|v| with_keys(&v.body, &[("identity", &v.identity), ("trail", &json(&*v.trail))]))
        .collect();
    format!(
        "{{\"type\":\"state\",\"t\":{},\"vehicles\":[{}],\"taskings\":{}}}",
        json(&frame.t),
        vehicles.join(","),
        frame.taskings
    )
}

/// Level-of-detail limits (`MURMUR_LOD_MIN_FLEET`, `MURMUR_LOD_MAX_FULL`).
#[derive(Clone, Copy, Debug)]
pub struct LodSettings {
    /// a fleet this size or smaller gets the plain feed whatever the client asked
    pub min_fleet: usize,
    /// more vehicles than this in view: clusters instead of full detail
    pub max_full: usize,
}

impl Default for LodSettings {
    fn default() -> Self {
        LodSettings {
            min_fleet: 100,
            max_full: 800,
        }
    }
}

impl LodSettings {
    pub fn from_env() -> Self {
        let d = LodSettings::default();
        let get = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(d)
        };
        LodSettings {
            min_fleet: get("MURMUR_LOD_MIN_FLEET", d.min_fleet),
            max_full: get("MURMUR_LOD_MAX_FULL", d.max_full),
        }
    }
}

/// What a client looks at: `{"type":"view","bounds":[w,s,e,n],"zoom":z,"pin":[uid,...]}`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Subscription {
    /// west, south, east, north; none while the client has no map yet
    #[serde(default)]
    pub bounds: Option<[f64; 4]>,
    #[serde(default)]
    pub zoom: f64,
    /// always at full detail: selected, followed, in the inbox
    #[serde(default)]
    pub pin: HashSet<Uid>,
}

impl Subscription {
    /// The view message, if `text` is one.
    pub fn parse(text: &str) -> Option<Subscription> {
        #[derive(Deserialize)]
        struct Msg {
            #[serde(rename = "type")]
            kind: String,
            #[serde(flatten)]
            sub: Subscription,
        }
        let msg: Msg = serde_json::from_str(text).ok()?;
        (msg.kind == "view").then_some(msg.sub)
    }

    /// Whether a point is in the view grown by the margin.
    fn contains(&self, [lon, lat]: [f64; 2]) -> bool {
        let Some([w, s, e, n]) = self.bounds else {
            return false;
        };
        if !(w.is_finite() && s.is_finite() && e.is_finite() && n.is_finite()) {
            return false;
        }
        let dy = (n - s) * VIEW_MARGIN;
        if lat < s - dy || lat > n + dy {
            return false;
        }
        if w > e {
            // across the antimeridian
            return lon >= w || lon <= e;
        }
        let dx = (e - w) * VIEW_MARGIN;
        lon >= w - dx && lon <= e + dx
    }
}

/// What one browser already has, so each push sends only what changed.
pub struct ClientView {
    /// level of detail on; None is the plain feed
    lod: Option<LodSettings>,
    sub: Option<Subscription>,
    sent: HashMap<Uid, Sent>,
    taskings: Option<String>,
    frames: u64,
    /// the last clusters JSON sent, while the client is in cluster mode
    clusters: Option<String>,
}

struct Sent {
    /// the whole body (plain feed) or the lite snapshot, whichever went last
    body: String,
    /// the two parts, for a full vehicle on the level-of-detail feed
    fast: Part,
    slow: Part,
    identity: Arc<str>,
    full: bool,
    /// trail points the client has (as a count), and the step they were sampled at
    trail: u64,
    step: u64,
}

/// Which vehicles of a frame a client gets at full detail, and how.
struct Plan {
    full: Vec<bool>,
    clustered: bool,
    step: u64,
    tile_zoom: u8,
}

impl Default for ClientView {
    fn default() -> Self {
        ClientView::new(None)
    }
}

impl ClientView {
    pub fn new(lod: Option<LodSettings>) -> ClientView {
        ClientView {
            lod,
            sub: None,
            sent: HashMap::new(),
            taskings: None,
            frames: 0,
            clusters: None,
        }
    }

    /// The client's view changed; the next delta follows it.
    pub fn subscribe(&mut self, sub: Subscription) {
        if self.lod.is_some() {
            self.sub = Some(sub);
        }
    }

    fn plan(&self, frame: &PushFrame) -> Plan {
        let n = frame.vehicles.len();
        let all = |full: bool| Plan {
            full: vec![full; n],
            clustered: false,
            step: 1,
            tile_zoom: 1,
        };
        let Some(lod) = self.lod else {
            return all(true);
        };
        if n <= lod.min_fleet {
            return all(true);
        }
        let Some(sub) = &self.sub else {
            return all(false);
        };
        let in_view: Vec<bool> = frame
            .vehicles
            .iter()
            .map(|v| v.pos.is_some_and(|p| sub.contains(p)))
            .collect();
        let clustered = in_view.iter().filter(|b| **b).count() > lod.max_full;
        Plan {
            full: frame
                .vehicles
                .iter()
                .zip(&in_view)
                .map(|(v, seen)| sub.pin.contains(&v.uid) || (!clustered && *seen))
                .collect(),
            clustered,
            step: trail_step(sub.zoom),
            tile_zoom: tile_zoom(sub.zoom),
        }
    }

    /// The full state for a new client; everything in it counts as sent.
    pub fn full_state(&mut self, frame: &PushFrame) -> String {
        let plan = self.plan(frame);
        self.sent.clear();
        let mut vehicles = Vec::with_capacity(frame.vehicles.len());
        for (v, full) in frame.vehicles.iter().zip(&plan.full) {
            let trail = if *full {
                sample(&v.trail, v.trail_count, 0, plan.step)
            } else {
                Vec::new()
            };
            let body = if *full { &v.body } else { &v.lite };
            vehicles.push(with_keys(
                body,
                &[("identity", &v.identity), ("trail", &json(&trail))],
            ));
            self.sent.insert(
                v.uid,
                Sent {
                    body: body.clone(),
                    fast: if *full { v.fast.clone() } else { Part::default() },
                    slow: if *full { v.slow.clone() } else { Part::default() },
                    identity: v.identity.clone(),
                    full: *full,
                    trail: if *full { v.trail_count } else { 0 },
                    step: plan.step,
                },
            );
        }
        self.taskings = Some(frame.taskings.clone());
        let mut msg = format!(
            "{{\"type\":\"state\",\"t\":{},\"vehicles\":[{}],\"taskings\":{}",
            json(&frame.t),
            vehicles.join(","),
            frame.taskings
        );
        self.clusters = None;
        if plan.clustered {
            let c = clusters_json(frame, plan.tile_zoom);
            msg.push_str(",\"clusters\":");
            msg.push_str(&c);
            self.clusters = Some(c);
        }
        msg.push('}');
        msg
    }

    pub fn delta(&mut self, frame: &PushFrame) -> String {
        self.frames += 1;
        let lite_frame = self.frames.is_multiple_of(LITE_EVERY);
        let plan = self.plan(frame);
        let lod = self.lod.is_some();
        let mut changed = Vec::new();
        for (v, full) in frame.vehicles.iter().zip(&plan.full) {
            let prev = self.sent.get(&v.uid);
            let identity_changed = prev.is_none_or(|p| !same(&p.identity, &v.identity));
            let mut extra: Vec<(&str, String)> = Vec::new();
            // the plain feed carries the identity in every update, as it always did
            if identity_changed || !lod {
                extra.push(("identity", v.identity.to_string()));
            }
            if *full {
                let was_full = prev.is_some_and(|p| p.full);
                // the plain feed compares and sends the whole body; the level-of-detail
                // feed its two parts apart, so a moving vehicle costs its fast part
                let (fast_changed, slow_changed) = if lod {
                    (
                        prev.is_none_or(|p| !p.full || p.fast != v.fast),
                        prev.is_none_or(|p| !p.full || p.slow != v.slow),
                    )
                } else {
                    let changed = prev.is_none_or(|p| p.body != v.body);
                    (changed, changed)
                };
                // the client's trail: extended, or replaced when it has none or the
                // sampling step changed (the plain feed always extends, as it always did)
                let replace = lod && prev.is_none_or(|p| !p.full || p.step != plan.step);
                let since = if replace { 0 } else { prev.map_or(0, |p| p.trail) };
                let pts = sample(&v.trail, v.trail_count, since, plan.step);
                if !fast_changed && !slow_changed && !identity_changed && was_full && pts.is_empty() {
                    continue;
                }
                if lod && prev.is_some_and(|p| !p.full) {
                    extra.push(("lod", "\"full\"".into()));
                }
                if replace {
                    extra.push(("trail", json(&pts)));
                } else if !lod || !pts.is_empty() {
                    extra.push(("trail_add", json(&pts)));
                }
                let refs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                if lod {
                    let mut parts: Vec<&Part> = Vec::with_capacity(2);
                    if fast_changed {
                        parts.push(&v.fast);
                    }
                    if slow_changed {
                        parts.push(&v.slow);
                    }
                    changed.push(assemble(v.uid, &parts, &refs));
                } else {
                    changed.push(with_keys(&v.body, &refs));
                }
                self.sent.insert(
                    v.uid,
                    Sent {
                        body: if lod { String::new() } else { v.body.clone() },
                        fast: if lod { v.fast.clone() } else { Part::default() },
                        slow: if lod { v.slow.clone() } else { Part::default() },
                        identity: v.identity.clone(),
                        full: true,
                        trail: v.trail_count,
                        step: plan.step,
                    },
                );
            } else {
                if !lite_frame {
                    continue;
                }
                if !identity_changed && prev.is_some_and(|p| !p.full && p.body == v.lite) {
                    continue;
                }
                let refs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                changed.push(with_keys(&v.lite, &refs));
                self.sent.insert(
                    v.uid,
                    Sent {
                        body: v.lite.clone(),
                        fast: Part::default(),
                        slow: Part::default(),
                        identity: v.identity.clone(),
                        full: false,
                        trail: 0,
                        step: 0,
                    },
                );
            }
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
        if plan.clustered {
            // the first clustered frame at once, then at the lite cadence
            if lite_frame || self.clusters.is_none() {
                let c = clusters_json(frame, plan.tile_zoom);
                if self.clusters.as_deref() != Some(c.as_str()) {
                    msg.push_str(",\"clusters\":");
                    msg.push_str(&c);
                    self.clusters = Some(c);
                }
            }
        } else if self.clusters.take().is_some() {
            msg.push_str(",\"clusters\":null");
        }
        msg.push('}');
        msg
    }
}

fn same(a: &Arc<str>, b: &Arc<str>) -> bool {
    Arc::ptr_eq(a, b) || **a == **b
}

/// Counts per tile: vehicles by domain and lost links, tracks by affiliation, and the
/// centroid of what is in the tile (where the marker goes).
#[derive(Default)]
struct Cell {
    n: u32,
    d: [u32; 4],
    lost: u32,
    tk: [u32; 5],
    lon: f64,
    lat: f64,
}

#[derive(Serialize)]
struct CellJson {
    x: i64,
    y: i64,
    c: [f64; 2],
    n: u32,
    d: [u32; 4],
    lost: u32,
    tk: [u32; 5],
}

/// `{"z": tile zoom, "cells": [...]}` for every tile with something in it.
pub fn clusters_json(frame: &PushFrame, z: u8) -> String {
    let mut cells: BTreeMap<(i64, i64), Cell> = BTreeMap::new();
    for v in &frame.vehicles {
        let Some([lon, lat]) = v.pos else { continue };
        let c = cells.entry(tile(lon, lat, z)).or_default();
        c.n += 1;
        c.d[v.domain as usize] += 1;
        c.lost += u32::from(v.stale);
        c.lon += lon;
        c.lat += lat;
    }
    for t in &frame.tracks {
        let c = cells.entry(tile(t.lon, t.lat, z)).or_default();
        c.tk[match t.affiliation {
            Affiliation::Hostile => 0,
            Affiliation::Suspect => 1,
            Affiliation::Unknown => 2,
            Affiliation::Neutral => 3,
            Affiliation::AssumedFriend => 4,
        }] += 1;
        c.lon += t.lon;
        c.lat += t.lat;
    }
    let cells: Vec<CellJson> = cells
        .into_iter()
        .map(|((x, y), c)| {
            let k = f64::from(c.n + c.tk.iter().sum::<u32>()).max(1.0);
            CellJson {
                x,
                y,
                c: [round(c.lon / k, 4), round(c.lat / k, 4)],
                n: c.n,
                d: c.d,
                lost: c.lost,
                tk: c.tk,
            }
        })
        .collect();
    format!("{{\"z\":{z},\"cells\":{}}}", json(&cells))
}

/// Build a frame every tick while anyone is watching.
pub async fn pusher(world: Arc<World>, frames: Arc<watch::Sender<Option<Arc<PushFrame>>>>) {
    let mut cache = FrameCache::default();
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

    const ID: &str = r#"{"callsign":"A 1","domain":"air"}"#;

    fn frame(t: f64, vehicles: &[(Uid, &str, u64, Vec<Point>)], taskings: &str) -> PushFrame {
        PushFrame {
            t,
            vehicles: vehicles
                .iter()
                .map(|(uid, snap, count, trail)| FrameVehicle {
                    uid: *uid,
                    body: (*snap).to_string(),
                    fast: split_body(snap).0,
                    slow: split_body(snap).1,
                    identity: ID.into(),
                    lite: format!("{{\"uid\":{uid},\"lod\":\"lite\"}}"),
                    trail_count: *count,
                    trail: Arc::new(trail.clone()),
                    pos: trail.last().map(|p| [p[0], p[1]]),
                    domain: Domain::Air,
                    stale: false,
                })
                .collect(),
            taskings: taskings.into(),
            tracks: Vec::new(),
        }
    }

    /// `n` vehicles on a line from lon 0 to lon 1 (lat 0), each with one trail point.
    fn spread(n: usize, t: f64) -> PushFrame {
        let vs: Vec<(Uid, String, u64, Vec<Point>)> = (0..n)
            .map(|i| {
                let lon = i as f64 / n as f64;
                (
                    i as Uid + 1,
                    format!(r#"{{"uid":{},"t":{t}}}"#, i + 1),
                    1,
                    vec![[lon, 0.0, 0.0]],
                )
            })
            .collect();
        let refs: Vec<(Uid, &str, u64, Vec<Point>)> =
            vs.iter().map(|(u, s, c, tr)| (*u, s.as_str(), *c, tr.clone())).collect();
        frame(t, &refs, "[]")
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).expect("valid json")
    }

    fn lod(min_fleet: usize, max_full: usize) -> ClientView {
        ClientView::new(Some(LodSettings { min_fleet, max_full }))
    }

    fn sub(w: f64, s: f64, e: f64, n: f64, zoom: f64, pin: &[Uid]) -> Subscription {
        Subscription {
            bounds: Some([w, s, e, n]),
            zoom,
            pin: pin.iter().copied().collect(),
        }
    }

    fn uids(d: &Value) -> Vec<u64> {
        d["vehicles"]
            .as_array()
            .expect("array")
            .iter()
            .map(|v| v["uid"].as_u64().expect("uid"))
            .collect()
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
        let mut view = ClientView::default();
        let full = parse(&view.full_state(&f0));
        assert_eq!(full["type"], "state");
        assert_eq!(
            full["vehicles"][0]["trail"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(full["vehicles"][0]["mode"], "AUTO");
        assert_eq!(full["vehicles"][0]["identity"]["callsign"], "A 1");

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
        // the plain feed carries the identity in every update, as it always did
        assert_eq!(vs[0]["identity"]["callsign"], "A 1");
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
        let mut view = ClientView::default();
        view.full_state(&f0);
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
        let mut view = ClientView::default();
        assert_eq!(parse(&view.full_state(&f0))["vehicles"], serde_json::json!([]));
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

    #[test]
    fn api_state_has_identity_and_whole_trail() {
        let f = frame(0.0, &[(3, r#"{"uid":3}"#, 2, vec![[1.0, 1.0, 0.0], [2.0, 2.0, 0.0]])], "[]");
        let s = parse(&state_json(&f));
        assert_eq!(s["vehicles"][0]["identity"]["domain"], "air");
        assert_eq!(s["vehicles"][0]["trail"].as_array().map(Vec::len), Some(2));
    }

    const BODY: &str = r#"{"uid":7,"sysid":7,"kind":"plane","mode":"AUTO","armed":true,"position":[-122.5,37.8],"alt_msl":120.0,"heading":90.0,"tracking":null,"nav_ready":true,"groundspeed":20.0,"home":[-122.4,37.7],"link":{"last_rx_t":100.5,"loss_pct":0,"rtt_ms":12,"expected_interval_ms":1000,"emu":{"state":"ok","cause":null},"telemetry":{"tier":"full","profile":"c2, lte"},"ack_timeout_ms":1000},"statustext":[{"t":1.0,"severity":6,"text":"a \"quoted\" {brace}, comma"}],"estimate":{"position":[1.0,2.0]}}"#;

    #[test]
    fn body_splits_into_fast_and_slow_parts_and_back() {
        let (fast, slow) = split_body(BODY);
        let f = parse(&assemble(7, &[&fast], &[]));
        let s = parse(&assemble(7, &[&slow], &[]));
        for key in ["position", "alt_msl", "heading", "groundspeed", "estimate"] {
            assert!(f.get(key).is_some(), "{key} is fast");
            assert!(s.get(key).is_none(), "{key} is not slow");
        }
        for key in ["sysid", "kind", "mode", "armed", "tracking", "nav_ready", "home", "statustext"] {
            assert!(s.get(key).is_some(), "{key} is slow");
            assert!(f.get(key).is_none(), "{key} is not fast");
        }
        assert_eq!(f["link"]["last_rx_t"], 100.5);
        assert_eq!(f["link"]["ack_timeout_ms"], 1000);
        assert!(f["link"].get("telemetry").is_none());
        assert_eq!(s["link"]["telemetry"]["profile"], "c2, lte");
        assert_eq!(s["link"]["emu"]["state"], "ok");
        assert_eq!(s["statustext"][0]["text"], "a \"quoted\" {brace}, comma");
        assert_eq!(f["uid"], 7);
        // both parts together are the whole body, keys apart
        let whole = parse(&assemble(7, &[&fast, &slow], &[("trail", "[]")]));
        let original = parse(BODY);
        for (k, v) in original.as_object().expect("object") {
            assert_eq!(&whole[k], v, "{k}");
        }
        assert_eq!(whole["trail"], serde_json::json!([]));
        // no link: no link object
        let (fast, _) = split_body(r#"{"uid":1,"position":null}"#);
        assert_eq!(assemble(1, &[&fast], &[]), r#"{"uid":1,"position":null}"#);
    }

    #[test]
    fn lod_updates_carry_only_the_part_that_changed() {
        let body = |mode: &str, lon: f64| format!(r#"{{"uid":1,"mode":"{mode}","position":[{lon},0.0],"link":{{"last_rx_t":1.0,"telemetry":null}}}}"#);
        // (the test frame takes a vehicle's position from its last trail point)
        let f0 = frame(0.0, &[(1, &body("AUTO", 0.5), 1, vec![[0.5, 0.0, 0.0]])], "[]");
        let mut view = lod(0, 300);
        view.subscribe(sub(0.0, -1.0, 1.0, 1.0, 13.0, &[]));
        let s = parse(&view.full_state(&f0));
        assert_eq!(s["vehicles"][0]["mode"], "AUTO");
        assert_eq!(s["vehicles"][0]["link"]["telemetry"], Value::Null);
        // the position moved: the fast part only (position and the link's packet time)
        let d = parse(&view.delta(&frame(1.0, &[(1, &body("AUTO", 0.51), 1, vec![[0.5, 0.0, 0.0]])], "[]")));
        let v = &d["vehicles"][0];
        assert_eq!(v["position"][0], 0.51);
        assert!(v.get("mode").is_none());
        assert_eq!(v["link"]["last_rx_t"], 1.0);
        assert!(v["link"].get("telemetry").is_none());
        // the mode changed: the slow part only
        let d = parse(&view.delta(&frame(2.0, &[(1, &body("RTL", 0.51), 1, vec![[0.5, 0.0, 0.0]])], "[]")));
        let v = &d["vehicles"][0];
        assert_eq!(v["mode"], "RTL");
        assert!(v.get("position").is_none());
        assert_eq!(v["link"]["telemetry"], Value::Null);
        assert!(v["link"].get("last_rx_t").is_none());
        // nothing changed: nothing
        assert_eq!(uids(&parse(&view.delta(&frame(3.0, &[(1, &body("RTL", 0.51), 1, vec![[0.5, 0.0, 0.0]])], "[]")))), Vec::<u64>::new());
        // the plain feed still sends the whole body
        let mut plain = ClientView::default();
        plain.full_state(&f0);
        let d = parse(&plain.delta(&frame(1.0, &[(1, &body("AUTO", 0.51), 1, vec![[0.5, 0.0, 0.0]])], "[]")));
        assert_eq!(d["vehicles"][0]["mode"], "AUTO");
        assert_eq!(d["vehicles"][0]["position"][0], 0.51);
    }

    #[test]
    fn with_keys_handles_empty_and_full_bodies() {
        assert_eq!(with_keys("{}", &[("a", "1")]), r#"{"a":1}"#);
        assert_eq!(with_keys(r#"{"x":2}"#, &[("a", "1"), ("b", "[]")]), r#"{"x":2,"a":1,"b":[]}"#);
        assert_eq!(with_keys(r#"{"x":2}"#, &[]), r#"{"x":2}"#);
    }

    #[test]
    fn sampling_by_global_index_is_stable_across_increments() {
        let pts: Vec<Point> = (0..20).map(|i| [f64::from(i), 0.0, 0.0]).collect();
        // trail_count 25, 20 kept: the oldest kept point is number 5
        let whole = sample(&pts, 25, 0, 4);
        let idx: Vec<f64> = whole.iter().map(|p| p[0]).collect();
        // numbers 8, 12, 16, 20, 24 are points 3, 7, 11, 15, 19
        assert_eq!(idx, vec![3.0, 7.0, 11.0, 15.0, 19.0]);
        // the increment since count 13 continues the same series
        let inc = sample(&pts, 25, 13, 4);
        assert_eq!(inc.iter().map(|p| p[0]).collect::<Vec<_>>(), vec![11.0, 15.0, 19.0]);
        // step 1 from 0 is everything, step 0 counts as 1
        assert_eq!(sample(&pts, 25, 0, 1).len(), 20);
        assert_eq!(sample(&pts, 25, 0, 0).len(), 20);
        // since beyond the end: nothing
        assert!(sample(&pts, 25, 25, 1).is_empty());
    }

    #[test]
    fn lite_link_figures_are_coarse() {
        assert_eq!(coarse(Some(14), 10), Some(10));
        assert_eq!(coarse(Some(66), 10), Some(70));
        assert_eq!(coarse(None, 10), None);
        let link = LinkSnapshot {
            last_rx_t: Some(100.0),
            loss_pct: Some(14),
            rtt_ms: Some(83),
            expected_interval_ms: 1000,
            emu: None,
            telemetry: None,
            ack_timeout_ms: 1000,
        };
        let live = parse(&json(&lite_link(&link, false)));
        assert_eq!(live["stale"], false);
        assert!(live.get("last_rx_t").is_none(), "a live link's packet time is not a change");
        assert!(live.get("rtt_ms").is_none(), "no round trip in lite");
        assert_eq!(live["loss_pct"], 10);
        let lost = parse(&json(&lite_link(&link, true)));
        assert_eq!(lost["stale"], true);
        assert_eq!(lost["last_rx_t"], 100.0);
    }

    #[test]
    fn trail_step_and_tiles() {
        assert_eq!(trail_step(14.0), 1);
        assert_eq!(trail_step(12.0), 2);
        assert_eq!(trail_step(11.0), 4);
        assert_eq!(trail_step(10.0), 8);
        assert_eq!(trail_step(5.0), 16);
        assert_eq!(tile_zoom(10.0), 11);
        assert_eq!(tile_zoom(10.6), 12);
        assert_eq!(tile(0.0, 0.0, 1), (1, 1));
        assert_eq!(tile(-122.4, 37.8, 10), (163, 395));
        assert_eq!(tile(-180.0, 85.0, 3), (0, 0));
    }

    #[test]
    fn lod_client_without_a_view_gets_everything_lite_once() {
        let f = spread(5, 0.0);
        let mut view = lod(0, 300);
        let s = parse(&view.full_state(&f));
        assert_eq!(s["vehicles"].as_array().map(Vec::len), Some(5));
        assert_eq!(s["vehicles"][0]["lod"], "lite");
        assert_eq!(s["vehicles"][0]["identity"]["callsign"], "A 1");
        assert_eq!(s["vehicles"][0]["trail"], serde_json::json!([]));
        assert!(s.get("clusters").is_none());
        // lite is sent on the lite cadence only, and only when it changed: nothing for 4 frames
        for _ in 0..8 {
            assert_eq!(uids(&parse(&view.delta(&f))), Vec::<u64>::new());
        }
    }

    #[test]
    fn small_fleet_is_full_detail_whatever_the_view() {
        let f = spread(5, 0.0);
        let mut view = lod(100, 300);
        view.subscribe(sub_at(0.0, 0.1, &[]));
        let s = parse(&view.full_state(&f));
        assert!(s["vehicles"][4].get("lod").is_none());
        assert_eq!(s["vehicles"][4]["trail"].as_array().map(Vec::len), Some(1));
        assert!(s.get("clusters").is_none());
    }

    fn sub_at(w: f64, e: f64, pin: &[Uid]) -> Subscription {
        sub(w, -0.1, e, 0.1, 13.0, pin)
    }

    #[test]
    fn in_view_full_at_4hz_rest_lite_at_1hz_identity_once() {
        let f0 = spread(10, 0.0);
        let mut view = lod(0, 300);
        view.full_state(&f0);
        // lon 0.0 .. 0.9: a view over 0.3..0.5 (grown by a quarter: 0.25..0.55) holds 4, 5, 6
        view.subscribe(sub_at(0.3, 0.5, &[]));
        let d = parse(&view.delta(&spread(10, 1.0)));
        assert_eq!(uids(&d), vec![4, 5, 6]);
        let v = &d["vehicles"][0];
        assert_eq!(v["lod"], "full", "lite to full says so");
        assert_eq!(v["t"], 1.0);
        assert!(v.get("identity").is_none(), "identity went with the state");
        assert_eq!(v["trail"].as_array().map(Vec::len), Some(1), "the trail is replaced");
        // frames 2 and 3: full ones again, nothing lite
        for t in [2.0, 3.0] {
            let d = parse(&view.delta(&spread(10, t)));
            assert_eq!(uids(&d), vec![4, 5, 6]);
            assert!(d["vehicles"][0].get("lod").is_none());
            assert!(d["vehicles"][0].get("trail").is_none());
            assert!(d["vehicles"][0].get("trail_add").is_none(), "no new points: no key");
        }
        // frame 4 is a lite frame, but the lite snapshots are unchanged: still just the full ones
        let d = parse(&view.delta(&spread(10, 4.0)));
        assert_eq!(uids(&d), vec![4, 5, 6]);
        // a pin brings a vehicle outside the view in at full detail
        view.subscribe(sub_at(0.3, 0.5, &[9]));
        let d = parse(&view.delta(&spread(10, 5.0)));
        assert_eq!(uids(&d), vec![4, 5, 6, 9]);
        assert_eq!(d["vehicles"][3]["lod"], "full");
        assert_eq!(d["vehicles"][3]["trail"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn leaving_the_view_turns_lite_on_the_next_lite_frame() {
        let mut f = spread(10, 0.0);
        let mut view = lod(0, 300);
        view.subscribe(sub_at(0.3, 0.5, &[]));
        view.full_state(&f);
        view.subscribe(sub_at(0.6, 0.8, &[]));
        // frames 1..3: 7, 8, 9 come in full; 4, 5, 6 wait for the lite frame
        let d = parse(&view.delta(&f));
        assert_eq!(uids(&d), vec![7, 8, 9]);
        f.t = 1.0;
        view.delta(&f);
        view.delta(&f);
        // frame 4: lite for the ones that left (and only those: the rest were lite already)
        let d = parse(&view.delta(&f));
        assert_eq!(uids(&d), vec![4, 5, 6]);
        assert_eq!(d["vehicles"][0]["lod"], "lite");
        assert!(d["vehicles"][0].get("identity").is_none());
        // back in view: full again, with the trail replaced
        view.subscribe(sub_at(0.3, 0.5, &[]));
        let d = parse(&view.delta(&f));
        assert_eq!(uids(&d), vec![4, 5, 6]);
        assert_eq!(d["vehicles"][0]["lod"], "full");
        assert!(d["vehicles"][0].get("trail").is_some());
    }

    #[test]
    fn zoom_change_resamples_the_trail() {
        let pts: Vec<Point> = (0..40).map(|i| [0.5, f64::from(i) * 0.001, 0.0]).collect();
        let f = frame(0.0, &[(1, r#"{"uid":1}"#, 40, pts.clone())], "[]");
        let mut view = lod(0, 300);
        view.subscribe(sub(0.0, -1.0, 1.0, 1.0, 13.0, &[]));
        let s = parse(&view.full_state(&f));
        assert_eq!(s["vehicles"][0]["trail"].as_array().map(Vec::len), Some(40));
        // zoom out to 11: every 4th point, the whole trail again
        view.subscribe(sub(0.0, -1.0, 1.0, 1.0, 11.0, &[]));
        let d = parse(&view.delta(&f));
        assert_eq!(d["vehicles"][0]["trail"].as_array().map(Vec::len), Some(10));
        // 4 new points: one of them is on the grid
        let mut more = pts.clone();
        more.extend((40..44).map(|i| [0.5, f64::from(i) * 0.001, 0.0]));
        let f2 = frame(1.0, &[(1, r#"{"uid":1}"#, 44, more)], "[]");
        let d = parse(&view.delta(&f2));
        assert_eq!(d["vehicles"][0]["trail_add"], serde_json::json!([[0.5, 0.04, 0.0]]));
    }

    #[test]
    fn too_many_in_view_means_clusters_not_detail() {
        let f = spread(10, 0.0);
        let mut view = lod(0, 4);
        view.subscribe(sub(-1.0, -1.0, 2.0, 1.0, 10.0, &[2]));
        let s = parse(&view.full_state(&f));
        // only the pinned one is full
        assert!(s["vehicles"][1].get("lod").is_none());
        assert_eq!(s["vehicles"][0]["lod"], "lite");
        let c = &s["clusters"];
        assert_eq!(c["z"], 11);
        let cells = c["cells"].as_array().expect("cells");
        assert_eq!(cells.iter().map(|c| c["n"].as_u64().unwrap_or(0)).sum::<u64>(), 10);
        assert_eq!(cells[0]["d"][0], cells[0]["n"], "all air");
        // the same picture is not sent again; zooming in past the cap ends cluster mode
        assert!(parse(&view.delta(&f)).get("clusters").is_none());
        view.subscribe(sub(0.3, -0.1, 0.5, 0.1, 13.0, &[]));
        let d = parse(&view.delta(&f));
        assert!(d["clusters"].is_null());
        assert!(d.get("clusters").is_some());
        assert_eq!(uids(&d), vec![4, 5, 6]);
        // and nothing about clusters after that
        assert!(parse(&view.delta(&f)).get("clusters").is_none());
    }

    #[test]
    fn clusters_count_tracks_by_affiliation_and_lost_links() {
        let mut f = spread(3, 0.0);
        f.vehicles[0].stale = true;
        f.tracks = vec![
            TrackMark { lon: 0.0, lat: 0.0, affiliation: Affiliation::Hostile },
            TrackMark { lon: 0.0, lat: 0.0, affiliation: Affiliation::Neutral },
            TrackMark { lon: 100.0, lat: 50.0, affiliation: Affiliation::Unknown },
        ];
        let c = parse(&clusters_json(&f, 1));
        let cells = c["cells"].as_array().expect("cells");
        assert_eq!(cells.len(), 2);
        let near = cells.iter().find(|c| c["n"] == 3).expect("the vehicles' cell");
        assert_eq!(near["lost"], 1);
        assert_eq!(near["tk"], serde_json::json!([1, 0, 0, 1, 0]));
        let far = cells.iter().find(|c| c["n"] == 0).expect("a tracks-only cell");
        assert_eq!(far["tk"], serde_json::json!([0, 0, 1, 0, 0]));
        assert_eq!(far["c"], serde_json::json!([100.0, 50.0]));
    }

    #[test]
    fn view_message_parses_and_antimeridian_wraps() {
        let s = Subscription::parse(r#"{"type":"view","bounds":[170,-10,-170,10],"zoom":9.5,"pin":[3]}"#)
            .expect("a view");
        assert!(s.contains([175.0, 0.0]));
        assert!(s.contains([-175.0, 0.0]));
        assert!(!s.contains([0.0, 0.0]));
        assert!(s.pin.contains(&3));
        assert!(Subscription::parse(r#"{"type":"other"}"#).is_none());
        assert!(Subscription::parse("nonsense").is_none());
        let none = Subscription::parse(r#"{"type":"view"}"#).expect("bounds optional");
        assert!(!none.contains([0.0, 0.0]));
    }
}
