//! Autonomous nodes (docs/AUTONOMOUS-NODES.md 2.2 to 2.6, bd murmur-y85j.2): a contact given an
//! order does it by its catalog kind (sim/contacts.toml, [`crate::catalog`]), not by the file.
//! The file times the order ("at 200 s the boat attacks HUNTER 1"); the node then runs its own
//! sequence: it perceives its target or not, picks its weapon, closes or opens out to its range
//! band, waits its reload or burst interval, counts its rounds, and its shots fly and strike as
//! truth. A scenario never creates a weapon in the air, sets where one flies or decides what it
//! hits.
//!
//! - **Orders** ([`OrderSpec`]): `transit`, `hold`, `orbit`, `track`, `attack`, `break_off`,
//!   `flee`, `resume`, from a file's `[[order]]` (checked at load, [`check`]) or the contacts
//!   process's own API ([`crate::api`], into [`Nodes::order`]). One at a time: a new one
//!   supersedes the one running. An order ends saying why (`target_lost`, `out_of_rounds`,
//!   `no_weapon`, `salvo_fired`, `until`, `no_way`, `done`, `superseded`, `break_off`,
//!   `destroyed`, `expended`), and the node then holds where it is, or, out of rounds altogether,
//!   follows its kind's `on_empty` (default: it flees along its way in).
//! - **Steering** ([`crate::motion::Steer`]): with the kind's turn rate, at its `max_mps` closing,
//!   opening out and fleeing, its `cruise_mps` otherwise. A straight leg over the wrong surface for
//!   its domain, by the land/water grid the scenario names (`[sensing] terrain`, [`LandWater`]), is
//!   not taken: the node holds and logs `no_way` (no planner on the truth side). Without a grid
//!   nothing is checked. A node whose propulsion is lost moves no more (an aircraft: at half speed).
//! - **Perception** (decision D2, [`perceives`]): a target of ours is what the bridge's answer
//!   says of our vehicles near the node's `look` (the positions our vehicles report to our C2), a
//!   contact its truth; the node keeps it within its kind's `sensors` reach (eye and radar above
//!   the water, within the radio horizon of both heights; sonar in it) and nothing else. Unseen,
//!   it closes on where it saw the target last; unseen for [`LOST_S`], the order ends
//!   `target_lost`.
//! - **Fire control** (2.3): the named weapon, else the first of the loadout meant for the
//!   target's domain with rounds left; in its band (slant range to the lead point) and ready, it
//!   fires: a missile, rocket or torpedo is an act with one flight; a gun's bursts within
//!   [`EPISODE_S`] of the last against the same victim are one act, each burst a flight with its
//!   rounds; a `self` weapon is the node steering into its target, an abstract act and a strike at
//!   the moment it gets there, and it is gone the second after. Out of action (a firepower kill),
//!   it holds its fire.
//! - **Flights** are straight 3D lines at the weapon's speed from the launch to the lead point
//!   ([`crate::weapon::lead3`]), real heights at both ends: from an aircraft, at an aircraft, a
//!   torpedo below the surface. Act ids are `<contact>-<n>` in firing order, so the damage model's
//!   draws (`act:<id>`, a burst's `act:<id>:<burst>`) are the same run after run.
//! - **Every decision** goes to the truth message's `node_events` (evaluation only), each change of
//!   what the node is doing once: order taken, closing, opening, held and why, fired, struck,
//!   ended and why.

use std::io::Read;
use std::path::Path;

use murmur_standins::geo::{M_PER_DEG, bearing_deg, distance_m};
use serde::{Deserialize, Serialize};

use crate::catalog::{Catalog, Kind, OnEmpty, Sensors, Weapon};
use crate::motion::{Mover, Steer, offset};
use crate::scenario::{Domain, Scenario, SensingModel};
use crate::truth::{ActTruth, ImpactTruth, Look, WeaponTruth};
use crate::weapon::{Armory, OursAt, Strike, lead3};

/// A target unseen this long ends its order `target_lost` [E].
pub const LOST_S: f64 = 60.0;
/// A gun's bursts within this long of the last against the same victim are one firing episode:
/// one act (docs/AUTONOMOUS-NODES.md 2.3).
pub const EPISODE_S: f64 = 30.0;
/// A node that is its own weapon strikes when its step passes this close to its target [E: a
/// contact fuze, or a terminal dive's last metres].
pub const STRIKE_M: f64 = 15.0;
/// The land/water grid is read every this many metres along a leg (as rules 17's `along`).
pub const ALONG_M: f64 = 20.0;
/// Water where the surface grid is within this of the sea level it was sampled at, m (the rule
/// eval/headless/families.py reads the same grid by).
const WET_M: f64 = 1.0;
/// The first metres of a leg are where the node already is: not read.
const LEG_START_M: f64 = 30.0;
/// The way in: a crumb every this many metres, the latest this many kept (50 km).
const CRUMB_M: f64 = 100.0;
const CRUMBS: usize = 500;
/// With no way in, a node flees this far straight away from its threat [E].
const FLEE_M: f64 = 5000.0;
/// Closing stops at this share of the weapon's maximum range; opening out goes to this multiple
/// of its minimum plus this many metres (within the band) [E: margins so a target's drift does
/// not take it straight back out of the band].
const CLOSE_TO: f64 = 0.9;
const OPEN_TO: f64 = 1.5;
const OPEN_PLUS_M: f64 = 50.0;
/// The radio horizon over a 4/3 earth: 4.12 km per root metre of each height.
const HORIZON_M_PER_ROOT_M: f64 = 4120.0;
/// Eye and radar see from, and see, a height of at least this over the water [E: a lookout's eye,
/// a small craft's hull].
const EYE_M: f64 = 2.0;
/// A submerged node sees above the water from periscope depth or shallower only [E].
const PERISCOPE_M: f64 = -20.0;
/// A launcher stands this high on a surface or ground node, and a shot at a surface or ground
/// target is aimed this high on it (its superstructure) [E].
const MAST_M: f64 = 2.0;
/// A torpedo runs at least this deep, and strikes a ship this far under the water [E].
const TORPEDO_RUN_M: f64 = -5.0;
const KEEL_M: f64 = -3.0;
/// A node of a kind the catalog does not list moves at this speed when its file gives none [E].
const FALLBACK_MPS: f64 = 5.0;

// ---------------------------------------------------------------------------------------------
// orders

/// What an order tells a node to do (docs/AUTONOMOUS-NODES.md 2.2; names as our tasks').
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Do {
    /// Once along `route` at `speed_mps` (its cruise), then hold.
    Transit,
    /// Stop where it is, or go to `at` and stop there.
    Hold,
    /// Circle `center` at `radius_m`.
    Orbit,
    /// Keep `standoff_m` from `target` as it moves (shadowing).
    Track,
    /// Fire control on `target`.
    Attack,
    /// Cease fire, keep its motion.
    BreakOff,
    /// Away from `from` (or the nearest of ours it perceives) at its top speed, along its way in.
    Flee,
    /// Back to the file's motion.
    Resume,
}

impl Do {
    pub fn as_str(self) -> &'static str {
        match self {
            Do::Transit => "transit",
            Do::Hold => "hold",
            Do::Orbit => "orbit",
            Do::Track => "track",
            Do::Attack => "attack",
            Do::BreakOff => "break_off",
            Do::Flee => "flee",
            Do::Resume => "resume",
        }
    }

    /// The keys besides `at_s`, `node`, `do` and `label` an order of this kind takes.
    pub fn takes(self) -> &'static [&'static str] {
        match self {
            Do::Transit => &["route", "speed_mps"],
            Do::Hold => &["at"],
            Do::Orbit => &["center", "radius_m", "speed_mps"],
            Do::Track => &["target", "standoff_m"],
            Do::Attack => &["target", "weapon", "rounds", "until_s", "stay"],
            Do::Flee => &["from", "speed_mps"],
            Do::BreakOff | Do::Resume => &[],
        }
    }
}

/// Whom an order is about: one of ours by its fleet uid (`{ vehicle = 1 }`), or another contact
/// (`{ contact = "<id>" }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetRef {
    Vehicle(u64),
    Contact(String),
}

impl TargetRef {
    fn describe(&self) -> String {
        match self {
            TargetRef::Vehicle(u) => format!("our vehicle {u}"),
            TargetRef::Contact(c) => format!("contact {c}"),
        }
    }

    fn split(&self) -> (Option<u64>, Option<String>) {
        match self {
            TargetRef::Vehicle(u) => (Some(*u), None),
            TargetRef::Contact(c) => (None, Some(c.clone())),
        }
    }
}

/// One order as a file's `[[order]]` writes it (and the contacts process's API, [`crate::api`],
/// takes it as JSON, `at_s` absent meaning now). Points are `[lon, lat]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderSpec {
    /// The scenario second it is taken at, on the contacts' clock (a file's names one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_s: Option<f64>,
    pub node: String,
    #[serde(rename = "do")]
    pub what: Do,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetRef>,
    /// The loadout weapon to fire, by name (none: the first meant for the target's domain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<String>,
    /// A salvo's cap: rounds fired under this order at most (a gun's by rounds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<u32>,
    /// The scenario second the order ends at (`until`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_s: Option<f64>,
    /// Fire only from where it is: no closing, no opening out (its motion stays what it was).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stay: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standoff_m: Option<f64>,
    /// What a flight is from (none: the nearest of ours it perceives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<TargetRef>,
    /// A truth-side note, carried on its acts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl OrderSpec {
    /// An order with nothing but its node and what to do.
    pub fn bare(node: &str, what: Do) -> OrderSpec {
        OrderSpec {
            at_s: None,
            node: node.into(),
            what,
            target: None,
            weapon: None,
            rounds: None,
            until_s: None,
            stay: false,
            route: Vec::new(),
            speed_mps: None,
            at: None,
            center: None,
            radius_m: None,
            standoff_m: None,
            from: None,
            label: None,
        }
    }

    fn given(&self) -> [(&'static str, bool); 12] {
        [
            ("target", self.target.is_some()),
            ("weapon", self.weapon.is_some()),
            ("rounds", self.rounds.is_some()),
            ("until_s", self.until_s.is_some()),
            ("stay", self.stay),
            ("route", !self.route.is_empty()),
            ("speed_mps", self.speed_mps.is_some()),
            ("at", self.at.is_some()),
            ("center", self.center.is_some()),
            ("radius_m", self.radius_m.is_some()),
            ("standoff_m", self.standoff_m.is_some()),
            ("from", self.from.is_some()),
        ]
    }

    /// The first key it gives that its kind of order does not take (`at_s`, `node`, `do` and
    /// `label` aside), if any: the refusal [`check`] makes first.
    pub fn stray_key(&self) -> Option<&'static str> {
        let takes = self.what.takes();
        self.given().into_iter().find(|(k, g)| *g && !takes.contains(k)).map(|(k, _)| k)
    }

    fn describe(&self) -> String {
        let mut d = self.what.as_str().to_string();
        if let Some(t) = &self.target {
            d += &format!(" {}", t.describe());
        }
        if let Some(w) = &self.weapon {
            d += &format!(" with its {w}");
        }
        if self.stay {
            d += ", from where it is";
        }
        if let Some(l) = &self.label {
            d += &format!(" ({l})");
        }
        d
    }
}

/// Whether `o` can stand in scenario `s` (a file's order names its second: `file`): the node is a
/// contact of the scenario that can take orders, the order takes the keys it gives and they make
/// sense, an attack is by an armed kind with sensors at a target it has a weapon for, a speed is
/// within its kind's top speed.
pub fn check(o: &OrderSpec, s: &Scenario, file: bool) -> Result<(), String> {
    let at = match o.at_s {
        Some(a) => a,
        None if file => return Err("a file's order names its at_s".into()),
        None => 0.0,
    };
    if !(at.is_finite() && at >= 0.0) {
        return Err("at_s must be a scenario second".into());
    }
    let c = s
        .contacts
        .iter()
        .find(|c| c.id == o.node)
        .ok_or_else(|| format!("node {:?} is no contact of the scenario", o.node))?;
    if c.motion == "recorded" {
        return Err("a recorded contact replays its fixes and takes no order".into());
    }
    if c.weapon.is_some() {
        return Err("a contact with a weapon of its own (`weapon = {...}`, the scripted path) takes no order".into());
    }
    let kind = Catalog::shipped().get(&c.kind);
    let takes = o.what.takes();
    if let Some(k) = o.stray_key() {
        let list = if takes.is_empty() { "nothing".to_string() } else { takes.join(", ") };
        return Err(format!("{} takes {list} (and label), not {k}", o.what.as_str()));
    }
    let point = |p: &[f64; 2]| p.iter().all(|x| x.is_finite()) && p[0].abs() <= 180.0 && p[1].abs() <= 90.0;
    let target = |t: &Option<TargetRef>, need: bool| -> Result<(), String> {
        match t {
            None if need => Err(format!("{} needs a target", o.what.as_str())),
            Some(TargetRef::Contact(id)) if *id == o.node || !s.contacts.iter().any(|c| c.id == *id) => {
                Err(format!("contact {id:?} is no other contact of the scenario"))
            }
            _ => Ok(()),
        }
    };
    if let Some(v) = o.speed_mps {
        if !(v.is_finite() && v > 0.0) {
            return Err("speed_mps must be above 0".into());
        }
        if let Some(k) = kind
            && v > k.max_mps
        {
            return Err(format!("{v} m/s is beyond its kind's top speed, {} m/s", k.max_mps));
        }
    }
    let sensed = || {
        if kind.is_some_and(|k| k.sensors.is_some()) {
            Ok(())
        } else {
            Err(format!("its kind {:?} lists no sensors to perceive a target by", c.kind))
        }
    };
    match o.what {
        Do::Transit => {
            if o.route.is_empty() || !o.route.iter().all(point) {
                return Err("transit needs a route of [lon, lat] points".into());
            }
        }
        Do::Hold => {
            if o.at.as_ref().is_some_and(|p| !point(p)) {
                return Err("at is not [lon, lat]".into());
            }
        }
        Do::Orbit => {
            if !o.center.as_ref().is_some_and(point) || !o.radius_m.is_some_and(|r| r.is_finite() && r > 0.0) {
                return Err("orbit needs a center [lon, lat] and radius_m above 0".into());
            }
        }
        Do::Track => {
            target(&o.target, true)?;
            sensed()?;
            if !o.standoff_m.is_some_and(|r| r.is_finite() && r > 0.0) {
                return Err("track needs standoff_m above 0".into());
            }
        }
        Do::Attack => {
            target(&o.target, true)?;
            let k = kind.filter(|k| k.armed).ok_or_else(|| format!("its kind {:?} carries no weapon", c.kind))?;
            sensed()?;
            let suits = |w: &Weapon, d: Domain| w.kind != "mine" && w.targets.contains(&d);
            if let Some(name) = &o.weapon {
                let w = k
                    .loadout
                    .iter()
                    .find(|w| w.name == *name && w.kind != "mine")
                    .ok_or_else(|| format!("its kind {:?} carries no {name:?} to fire", c.kind))?;
                if let Some(TargetRef::Contact(id)) = &o.target
                    && let Some(t) = s.contacts.iter().find(|t| t.id == *id)
                    && !suits(w, t.domain)
                {
                    return Err(format!("its {name} is not meant for a {:?} target", t.domain));
                }
            } else if let Some(TargetRef::Contact(id)) = &o.target
                && let Some(t) = s.contacts.iter().find(|t| t.id == *id)
                && !k.loadout.iter().any(|w| suits(w, t.domain))
            {
                return Err(format!("its kind {:?} carries nothing meant for a {:?} target", c.kind, t.domain));
            }
            if o.rounds == Some(0) {
                return Err("rounds caps a salvo at 1 or more".into());
            }
            if o.until_s.is_some_and(|u| !(u.is_finite() && u > at)) {
                return Err("until_s must be a scenario second after at_s".into());
            }
        }
        Do::Flee => target(&o.from, false)?,
        Do::BreakOff | Do::Resume => {}
    }
    Ok(())
}

/// Every `[[order]]` of a file ([`check`]); an attack needs the physical sensing model (weapons
/// stay refused under the legacy one); and no scripted act may take an id a node's shots will
/// (`<contact>-<n>`).
pub fn check_file(s: &Scenario) -> Result<(), String> {
    for (k, o) in s.orders.iter().enumerate() {
        check(o, s, true).map_err(|e| format!("order {} ({} by {:?}): {e}", k + 1, o.what.as_str(), o.node))?;
    }
    let attacks = |node: &str| s.orders.iter().any(|o| o.what == Do::Attack && o.node == node);
    if s.sensing.model == Some(SensingModel::Legacy) && s.orders.iter().any(|o| o.what == Do::Attack) {
        return Err("orders to attack need the physical sensing model: this file names the legacy one".into());
    }
    for a in &s.acts {
        if let Some((c, n)) = a.id.rsplit_once('-')
            && n.parse::<u32>().is_ok()
            && attacks(c)
        {
            return Err(format!("act {:?}: an id of the form a node's own shots take (<contact>-<n>)", a.id));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// what nodes say they did

/// One decision of a node, for the truth message's `node_events` (evaluation only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEvent {
    pub node: String,
    /// Scenario seconds.
    pub s: f64,
    /// `order` (taken), `refused`, `closing`, `opening`, `tracking`, `fleeing`, `moving`,
    /// `holding`, `held` (its fire, or its way), `fired`, `struck`, `ended`.
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn ev(node: &str, s: f64, what: &str, order: Option<u64>, why: Option<&str>) -> NodeEvent {
    NodeEvent {
        node: node.into(),
        s: round(s, 3),
        what: what.into(),
        order,
        why: why.map(str::to_string),
        act: None,
        weapon: None,
        note: None,
    }
}

/// What the nodes did in one tick: their acts and `self` strikes for the truth message (their
/// flights are in the [`Armory`]), and their events.
#[derive(Debug, Default)]
pub struct Tick {
    pub acts: Vec<ActTruth>,
    pub impacts: Vec<ImpactTruth>,
    pub events: Vec<NodeEvent>,
}

// ---------------------------------------------------------------------------------------------
// the land/water grid

#[derive(Debug, Clone, Deserialize)]
struct Header {
    lat0: f64,
    lon0: f64,
    dlat: f64,
    dlon: f64,
    nlat: usize,
    nlon: usize,
    scale: f64,
    nodata: i16,
}

/// The surface grid a scenario names (`[sensing] terrain`, sim/headless/terrain/README.md: gzip
/// of a JSON header line, then little-endian i16 surface heights row by row from the south-west):
/// water where the surface is within [`WET_M`] of the sea level it was sampled at, land above;
/// nothing known outside it or where it has no data.
pub struct LandWater {
    h: Header,
    data: Vec<i16>,
    /// The file it was read from.
    pub source: String,
}

impl LandWater {
    pub fn load(path: &Path) -> Result<LandWater, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(file).read_to_end(&mut raw).map_err(|e| format!("{}: {e}", path.display()))?;
        let nl = raw.iter().position(|b| *b == b'\n').ok_or_else(|| format!("{}: no header line", path.display()))?;
        let h: Header = serde_json::from_slice(&raw[..nl]).map_err(|e| format!("{}: header: {e}", path.display()))?;
        let body = &raw[nl + 1..];
        if body.len() != h.nlat * h.nlon * 2 {
            return Err(format!("{}: {} bytes of samples for a {}x{} grid", path.display(), body.len(), h.nlat, h.nlon));
        }
        let data = body.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
        Ok(LandWater { h, data, source: path.display().to_string() })
    }

    /// A grid of `nlat` x `nlon` samples from (`lat0`, `lon0`) at (`dlat`, `dlon`), each the
    /// surface height `height(lat, lon)` gives, m (to the decimetre).
    pub fn from_fn(lat0: f64, lon0: f64, dlat: f64, dlon: f64, nlat: usize, nlon: usize, height: impl Fn(f64, f64) -> f64) -> LandWater {
        let mut data = Vec::with_capacity(nlat * nlon);
        for i in 0..nlat {
            for j in 0..nlon {
                let v = height(lat0 + i as f64 * dlat, lon0 + j as f64 * dlon) * 10.0;
                data.push(v.round().clamp(-32767.0, 32767.0) as i16);
            }
        }
        LandWater { h: Header { lat0, lon0, dlat, dlon, nlat, nlon, scale: 0.1, nodata: i16::MIN }, data, source: "built".into() }
    }

    /// Water (true) or land (false) at a point, by its nearest sample; none outside the grid or
    /// where it has no data.
    pub fn wet(&self, lat: f64, lon: f64) -> Option<bool> {
        let i = ((lat - self.h.lat0) / self.h.dlat).round();
        let j = ((lon - self.h.lon0) / self.h.dlon).round();
        if !(i >= 0.0 && j >= 0.0 && (i as usize) < self.h.nlat && (j as usize) < self.h.nlon) {
            return None;
        }
        let v = self.data[i as usize * self.h.nlon + j as usize];
        (v != self.h.nodata).then(|| f64::from(v) * self.h.scale <= WET_M)
    }

    /// The first point (`lat`, `lon`) of the straight leg `from` to `to` over the wrong surface for
    /// `domain` (water for a boat or a submarine, land for a ground contact; an aircraft
    /// anywhere), read every [`ALONG_M`] past the leg's first metres; none: the leg keeps to it.
    pub fn blocked(&self, domain: Domain, from: (f64, f64), to: (f64, f64)) -> Option<(f64, f64)> {
        let wet = match domain {
            Domain::Air => return None,
            Domain::Surface | Domain::Subsurface => true,
            Domain::Ground => false,
        };
        let d = distance_m(from.0, from.1, to.0, to.1);
        let n = (d / ALONG_M).ceil().max(1.0) as usize;
        (1..=n)
            .map(|k| k as f64 / n as f64)
            .filter(|f| f * d > LEG_START_M)
            .map(|f| (from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f))
            .find(|p| self.wet(p.0, p.1).is_some_and(|w| w != wet))
    }
}

// ---------------------------------------------------------------------------------------------
// perception

/// A target as a node has it: where it is (`alt` metres above the sea), its velocity over the
/// ground (m/s east and north) and its domain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
    pub ve: f64,
    pub vn: f64,
    pub domain: Domain,
}

impl Seen {
    /// One of ours as the bridge's answer gives it (its domain as it reports it, else by height).
    pub fn of_ours(o: &OursAt) -> Seen {
        let domain = match o.domain.as_deref() {
            Some("air") => Domain::Air,
            Some("surface") => Domain::Surface,
            Some("subsurface") => Domain::Subsurface,
            Some("ground") => Domain::Ground,
            _ if o.alt_m > 50.0 => Domain::Air,
            _ if o.alt_m < -2.0 => Domain::Subsurface,
            _ => Domain::Surface,
        };
        Seen { lat: o.lat, lon: o.lon, alt: o.alt_m, ve: o.ve, vn: o.vn, domain }
    }

    /// Another contact, as truth has it.
    pub fn of_contact(m: &Mover) -> Seen {
        let c = m.st.course_deg.to_radians();
        Seen {
            lat: m.st.lat,
            lon: m.st.lon,
            alt: m.st.alt_m,
            ve: m.st.speed_mps * c.sin(),
            vn: m.st.speed_mps * c.cos(),
            domain: m.contact.domain,
        }
    }

    fn at(&self) -> [f64; 3] {
        [self.lat, self.lon, self.alt]
    }

    fn speed(&self) -> f64 {
        self.ve.hypot(self.vn)
    }
}

/// Whether a node of `domain` with `sensors` at `me` (`[lat, lon, alt]`) perceives `t` (decision
/// D2): by sonar when both are in the water, within its reach; above the water by eye or radar,
/// within the farther reach and the radio horizon of both heights (each at least [`EYE_M`]), and
/// only from periscope depth or shallower; a target under the water by sonar only. No terrain
/// masking, no draws: a reach and a horizon, as stated.
pub fn perceives(sensors: &Sensors, domain: Domain, me: [f64; 3], t: &Seen) -> bool {
    let ground = distance_m(me[0], me[1], t.lat, t.lon);
    let slant = ground.hypot(t.alt - me[2]);
    let in_water = |d: Domain| matches!(d, Domain::Surface | Domain::Subsurface);
    if sensors.sonar_m > 0.0 && in_water(domain) && in_water(t.domain) && slant <= sensors.sonar_m {
        return true;
    }
    if t.domain == Domain::Subsurface || me[2] < PERISCOPE_M {
        return false;
    }
    let horizon = HORIZON_M_PER_ROOT_M * (me[2].max(EYE_M).sqrt() + t.alt.max(EYE_M).sqrt());
    slant <= sensors.eye_m.max(sensors.radar_m) && ground <= horizon
}

fn resolve(t: &TargetRef, movers: &[Mover], ours: &[OursAt], me: usize) -> Option<Seen> {
    match t {
        TargetRef::Vehicle(uid) => ours.iter().find(|o| o.uid == *uid).map(Seen::of_ours),
        TargetRef::Contact(id) => movers
            .iter()
            .enumerate()
            .find(|(j, m)| *j != me && m.contact.id == *id && m.st.exists)
            .map(|(_, m)| Seen::of_contact(m)),
    }
}

// ---------------------------------------------------------------------------------------------
// the nodes

struct Pending {
    id: u64,
    spec: OrderSpec,
}

/// The order a node runs.
struct Running {
    id: u64,
    spec: OrderSpec,
    since_s: f64,
    /// Steering set (an order whose motion is set once: transit, hold, orbit, flee).
    started: bool,
    /// When it last perceived its target, and as what.
    seen_s: Option<f64>,
    last: Option<Seen>,
    /// Rounds fired under it (its salvo cap).
    fired: u32,
    /// What it last said it was doing (an event goes out on a change).
    status: Option<(String, Option<String>)>,
}

impl Running {
    fn new(id: u64, spec: OrderSpec, s: f64) -> Running {
        Running { id, spec, since_s: s, started: false, seen_s: None, last: None, fired: 0, status: None }
    }

    /// Say what it is doing now, once per change.
    fn status(&mut self, node: &str, cx: &mut Cx, what: &str, why: &str, note: Option<String>) {
        let key = (what.to_string(), Some(why.to_string()));
        if self.status.as_ref() == Some(&key) {
            return;
        }
        self.status = Some(key);
        let mut e = ev(node, cx.s, what, Some(self.id), Some(why));
        e.note = note;
        cx.out.events.push(e);
    }
}

/// The order a node runs, as it stands (read only).
#[derive(Debug, Clone, Copy)]
pub struct RunningView<'a> {
    pub id: u64,
    pub spec: &'a OrderSpec,
    /// The scenario second it was taken at.
    pub since_s: f64,
    /// When it last perceived its target, and as what.
    pub seen_s: Option<f64>,
    pub last: Option<Seen>,
    /// Rounds fired under it.
    pub fired: u32,
    /// What it last said it was doing, and why (`closing`, `out_of_range`; `fired`).
    pub status: Option<(&'a str, Option<&'a str>)>,
}

/// A gun's firing episode: one act, its bursts.
struct Episode {
    act: String,
    victim: TargetRef,
    last_t: f64,
    bursts: u32,
}

/// What a tick's nodes share.
struct Cx<'a> {
    s: f64,
    t: f64,
    ours: &'a [OursAt],
    grid: Option<&'a LandWater>,
    armory: &'a mut Armory,
    out: &'a mut Tick,
    next_id: &'a mut u64,
}

/// What a target looks like to a node right now.
enum Sight {
    Now(Seen),
    /// Not perceived now; where it was last, if ever.
    Unseen(Option<Seen>),
    Lost,
}

enum Choice {
    Use(usize),
    NoWeapon,
    Empty,
}

/// The named weapon, else the first of the loadout meant for `domain` with rounds left (the rule
/// our engine's `loadout::choose` follows); mines are laid, never fired.
fn choose(kind: &Kind, rounds: &[u32], named: Option<&str>, domain: Domain) -> Choice {
    let suits = |w: &Weapon| w.kind != "mine" && w.targets.contains(&domain);
    if let Some(name) = named {
        return match kind.loadout.iter().position(|w| w.name == name) {
            Some(i) if suits(&kind.loadout[i]) => {
                if rounds[i] > 0 {
                    Choice::Use(i)
                } else {
                    Choice::Empty
                }
            }
            _ => Choice::NoWeapon,
        };
    }
    let mut any = false;
    for (i, w) in kind.loadout.iter().enumerate() {
        if suits(w) {
            any = true;
            if rounds[i] > 0 {
                return Choice::Use(i);
            }
        }
    }
    if any { Choice::Empty } else { Choice::NoWeapon }
}

fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

fn pos(m: &Mover) -> [f64; 3] {
    [m.st.lat, m.st.lon, m.st.alt_m]
}

fn slant(a: [f64; 3], b: [f64; 3]) -> f64 {
    distance_m(a[0], a[1], b[0], b[1]).hypot(b[2] - a[2])
}

/// How close the step `p` to `q` passed `at`, m (local metres about `at`).
fn pass_m(p: [f64; 3], q: [f64; 3], at: [f64; 3]) -> f64 {
    let k = M_PER_DEG * at[0].to_radians().cos();
    let local = |x: [f64; 3]| [(x[1] - at[1]) * k, (x[0] - at[0]) * M_PER_DEG, x[2] - at[2]];
    let (a, b) = (local(p), local(q));
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let dd: f64 = d.iter().map(|x| x * x).sum();
    let f = if dd > 0.0 { (-(a[0] * d[0] + a[1] * d[1] + a[2] * d[2]) / dd).clamp(0.0, 1.0) } else { 0.0 };
    (0..3).map(|i| (a[i] + d[i] * f).powi(2)).sum::<f64>().sqrt()
}

/// The point on the ground line from `aim` through `here` at which the slant to `aim` is `want`
/// (the heights as they are): where a node closes or opens out to. `(lat, lon)`.
fn band_point(here: [f64; 3], aim: [f64; 3], want: f64) -> (f64, f64) {
    let dz = here[2] - aim[2];
    let ground = if want > dz.abs() { (want * want - dz * dz).sqrt() } else { 0.0 };
    let b = if distance_m(aim[0], aim[1], here[0], here[1]) > 1.0 { bearing_deg(aim[0], aim[1], here[0], here[1]) } else { 0.0 };
    let a = b.to_radians();
    offset(aim[0], aim[1], ground * a.sin(), ground * a.cos())
}

/// Where a weapon leaves a node of `domain` at `here`: a torpedo under the water, any other from
/// a launcher on deck (or the aircraft itself).
fn launch_from(w: &Weapon, domain: Domain, here: [f64; 3]) -> [f64; 3] {
    let z = if w.kind == "torpedo" {
        here[2].min(TORPEDO_RUN_M)
    } else if matches!(domain, Domain::Surface | Domain::Ground) {
        here[2] + MAST_M
    } else {
        here[2]
    };
    [here[0], here[1], z]
}

/// Where a weapon is aimed on a target: a ship under its waterline for a torpedo, a surface or
/// ground target's superstructure, an aircraft or a submarine itself.
fn aim_at(w: &Weapon, t: &Seen) -> [f64; 3] {
    let z = match (w.kind == "torpedo", t.domain) {
        (true, Domain::Subsurface) => t.alt,
        (true, _) => KEEL_M,
        (false, Domain::Surface | Domain::Ground) => t.alt + MAST_M,
        (false, _) => t.alt,
    };
    [t.lat, t.lon, z]
}

/// One node: a contact of an armed kind, or one an order named.
pub struct Node {
    pub id: String,
    index: usize,
    kind: Option<&'static Kind>,
    domain: Domain,
    /// Rounds left of each loadout weapon (the catalog's order).
    pub rounds: Vec<u32>,
    /// When each loadout weapon last fired (unix s).
    last_shot: Vec<Option<f64>>,
    episode: Option<Episode>,
    /// Acts so far (its ids are `<contact>-<n>`).
    acts: u32,
    order: Option<Running>,
    /// Its way in: where it has been (`[lon, lat]`), a crumb every [`CRUMB_M`].
    trail: Vec<[f64; 2]>,
    /// Where it was at the end of the last tick.
    prev: Option<[f64; 3]>,
    /// It struck as its own weapon: it leaves the scenario at the next tick.
    expended: bool,
    gone: bool,
}

impl Node {
    fn new(index: usize, m: &Mover) -> Node {
        let kind = m.contact.entry();
        let rounds: Vec<u32> = kind.map_or(Vec::new(), |k| k.loadout.iter().map(|w| w.count).collect());
        Node {
            id: m.contact.id.clone(),
            index,
            kind,
            domain: m.contact.domain,
            last_shot: vec![None; rounds.len()],
            rounds,
            episode: None,
            acts: 0,
            order: None,
            trail: Vec::new(),
            prev: None,
            expended: false,
            gone: false,
        }
    }

    /// The order it runs, if any: its id and what it does.
    pub fn order(&self) -> Option<(u64, Do)> {
        self.order.as_ref().map(|o| (o.id, o.spec.what))
    }

    /// Its sensors' reaches (its kind's; none listed: none).
    pub fn sensors(&self) -> Sensors {
        self.kind.and_then(|k| k.sensors).unwrap_or_default()
    }

    /// Its contact's index among the movers.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Its catalog kind's entry, if the catalog lists it.
    pub fn kind(&self) -> Option<&'static Kind> {
        self.kind
    }

    /// Gone for good: spent as its own weapon (it leaves the scenario at the next tick), or
    /// already left.
    pub fn gone(&self) -> bool {
        self.gone || self.expended
    }

    /// The order it runs as it stands (the contacts process's API reads it: `GET /nodes`).
    pub fn running(&self) -> Option<RunningView<'_>> {
        self.order.as_ref().map(|o| RunningView {
            id: o.id,
            spec: &o.spec,
            since_s: o.since_s,
            seen_s: o.seen_s,
            last: o.last,
            fired: o.fired,
            status: o.status.as_ref().map(|(w, y)| (w.as_str(), y.as_deref())),
        })
    }

    /// Its cruise and top speeds (its kind's; the steering halves them with its propulsion
    /// damaged, `Mover::told`).
    fn speeds(&self, m: &Mover) -> (f64, f64) {
        let file = if m.contact.speed_mps > 0.0 { m.contact.speed_mps } else { FALLBACK_MPS };
        self.kind.map_or((file, file), |k| (k.cruise_mps, k.max_mps))
    }

    fn crumb(&mut self, m: &Mover) {
        if !m.st.exists {
            return;
        }
        let p = [m.st.lon, m.st.lat];
        if self.trail.last().is_none_or(|q| distance_m(q[1], q[0], p[1], p[0]) >= CRUMB_M) {
            self.trail.push(p);
            if self.trail.len() > CRUMBS {
                self.trail.remove(0);
            }
        }
    }

    /// Take an order now: the one running ends (`superseded`, or `break_off`), and a break-off or
    /// a resume is done at once.
    fn take(&mut self, id: u64, spec: OrderSpec, m: &mut Mover, cx: &mut Cx) {
        if let Some(old) = self.order.take() {
            let why = if spec.what == Do::BreakOff { "break_off" } else { "superseded" };
            cx.out.events.push(ev(&self.id, cx.s, "ended", Some(old.id), Some(why)));
        }
        let mut e = ev(&self.id, cx.s, "order", Some(id), None);
        e.note = Some(spec.describe());
        cx.out.events.push(e);
        match spec.what {
            // it keeps the motion it has (its steering stays as it is)
            Do::BreakOff => cx.out.events.push(ev(&self.id, cx.s, "ended", Some(id), Some("done"))),
            Do::Resume => {
                if movable(m) {
                    let speed = self.speeds(m).0;
                    m.steer = m.rejoin(speed);
                }
                cx.out.events.push(ev(&self.id, cx.s, "ended", Some(id), Some("done")));
            }
            _ => self.order = Some(Running::new(id, spec, cx.s)),
        }
    }

    /// End `o` saying why; the node holds where it is.
    fn end(&self, o: Running, why: &str, m: &mut Mover, cx: &mut Cx) -> Option<Running> {
        cx.out.events.push(ev(&self.id, cx.s, "ended", Some(o.id), Some(why)));
        if movable(m) {
            m.steer = Some(Steer::Hold);
        }
        None
    }

    /// Steer for `to` at `speed` (a dive to `alt`), saying so as `what` and `why`; a leg over the
    /// wrong surface is not taken: it holds, `no_way`. Returns whether it goes.
    #[allow(clippy::too_many_arguments)]
    fn go(&self, o: &mut Running, m: &mut Mover, cx: &mut Cx, to: (f64, f64), speed: f64, alt: Option<f64>, what: &str, why: &str) -> bool {
        if let Some(g) = cx.grid
            && let Some(bad) = g.blocked(m.contact.domain, (m.st.lat, m.st.lon), to)
        {
            m.steer = Some(Steer::Hold);
            let over = if m.contact.domain == Domain::Ground { "water" } else { "land" };
            o.status(&self.id, cx, "held", "no_way", Some(format!("the way crosses {over} at {:.5}, {:.5}", bad.1, bad.0)));
            return false;
        }
        m.steer = Some(Steer::To { lat: to.0, lon: to.1, speed_mps: speed, alt_m: alt });
        o.status(&self.id, cx, what, why, None);
        true
    }

    /// Its target now: perceived, unseen (where it was last), or lost.
    fn sight(&self, o: &mut Running, movers: &[Mover], cx: &Cx) -> Sight {
        let Some(target) = o.spec.target.as_ref() else { return Sight::Lost };
        let me = pos(&movers[self.index]);
        let sensors = self.sensors();
        match resolve(target, movers, cx.ours, self.index).filter(|t| perceives(&sensors, self.domain, me, t)) {
            Some(t) => {
                o.seen_s = Some(cx.s);
                o.last = Some(t);
                Sight::Now(t)
            }
            None if cx.s - o.seen_s.unwrap_or(o.since_s) >= LOST_S => Sight::Lost,
            None => Sight::Unseen(o.last),
        }
    }

    /// Unseen: close on where it was last perceived (not with `stay`); never seen, hold.
    fn search(&self, mut o: Running, last: Option<Seen>, m: &mut Mover, cx: &mut Cx) -> Option<Running> {
        match last {
            Some(l) if !o.spec.stay && movable(m) => {
                let max = self.speeds(m).1;
                self.go(&mut o, m, cx, (l.lat, l.lon), max, None, "closing", "unseen");
            }
            _ => o.status(&self.id, cx, "held", "not_perceived", None),
        }
        Some(o)
    }

    fn run(&mut self, movers: &mut [Mover], cx: &mut Cx) {
        let Some(o) = self.order.take() else { return };
        let i = self.index;
        if !movers[i].st.exists {
            let why = if movers[i].out_of_play { "destroyed" } else { "gone" };
            cx.out.events.push(ev(&self.id, cx.s, "ended", Some(o.id), Some(why)));
            return;
        }
        self.order = match o.spec.what {
            Do::Attack => self.attack(o, movers, cx),
            Do::Track => self.track(o, movers, cx),
            Do::Flee => self.flee(o, movers, cx),
            Do::Transit | Do::Hold | Do::Orbit => self.motion(o, &mut movers[i], cx),
            Do::BreakOff | Do::Resume => None,
        };
    }

    /// Transit, hold and orbit: steering set once (each leg read on the grid first), a transit
    /// ended `done` at its last point.
    fn motion(&self, mut o: Running, m: &mut Mover, cx: &mut Cx) -> Option<Running> {
        if o.started {
            if o.spec.what == Do::Transit && m.steer.as_ref().is_some_and(Steer::done) {
                return self.end(o, "done", m, cx);
            }
            return Some(o);
        }
        o.started = true;
        if !movable(m) {
            o.status(&self.id, cx, "held", "immobile", None);
            return Some(o);
        }
        let cruise = self.speeds(m).0;
        let speed = o.spec.speed_mps.unwrap_or(cruise);
        let here = (m.st.lat, m.st.lon);
        // the legs it would take, [lon, lat] each
        let (legs, steer, what): (Vec<[f64; 2]>, Steer, &str) = match o.spec.what {
            Do::Transit => (
                o.spec.route.clone(),
                Steer::Route { points: o.spec.route.clone(), speed_mps: speed, leg: 0, done: false },
                "moving",
            ),
            Do::Hold => match o.spec.at {
                Some(at) => (vec![at], Steer::To { lat: at[1], lon: at[0], speed_mps: speed, alt_m: None }, "moving"),
                None => (Vec::new(), Steer::Hold, "holding"),
            },
            _ => {
                let (c, r) = (o.spec.center.unwrap_or([m.st.lon, m.st.lat]), o.spec.radius_m.unwrap_or(1.0));
                let ring: Vec<[f64; 2]> = (0..=36)
                    .map(|k| {
                        let a = f64::from(k * 10).to_radians();
                        let (la, lo) = offset(c[1], c[0], r * a.sin(), r * a.cos());
                        [lo, la]
                    })
                    .collect();
                (ring, Steer::Orbit { center: c, radius_m: r, speed_mps: speed }, "moving")
            }
        };
        if let Some(g) = cx.grid {
            let mut from = here;
            for p in &legs {
                if let Some(bad) = g.blocked(m.contact.domain, from, (p[1], p[0])) {
                    o.status(&self.id, cx, "held", "no_way", Some(format!("the way crosses the wrong surface at {:.5}, {:.5}", bad.1, bad.0)));
                    return self.end(o, "no_way", m, cx);
                }
                from = (p[1], p[0]);
            }
        }
        m.steer = Some(steer);
        o.status(&self.id, cx, what, o.spec.what.as_str(), None);
        Some(o)
    }

    /// Keep its standoff from its target (shadowing): at its top speed while far off, else at the
    /// target's speed and what closes the gap in ten seconds.
    fn track(&self, mut o: Running, movers: &mut [Mover], cx: &mut Cx) -> Option<Running> {
        let i = self.index;
        let t = match self.sight(&mut o, movers, cx) {
            Sight::Lost => return self.end(o, "target_lost", &mut movers[i], cx),
            Sight::Unseen(last) => return self.search(o, last, &mut movers[i], cx),
            Sight::Now(t) => t,
        };
        let m = &mut movers[i];
        if !movable(m) {
            o.status(&self.id, cx, "held", "immobile", None);
            return Some(o);
        }
        let standoff = o.spec.standoff_m.unwrap_or(1000.0);
        let to = band_point(pos(m), [t.lat, t.lon, m.st.alt_m], standoff);
        let gap = distance_m(m.st.lat, m.st.lon, to.0, to.1);
        let max = self.speeds(m).1;
        let speed = if gap > 200.0 { max } else { (t.speed() + gap / 10.0).clamp(1.0, max) };
        self.go(&mut o, m, cx, to, speed, None, "tracking", "standoff");
        Some(o)
    }

    /// Away along its way in at its top speed; with no way in, straight away from its threat
    /// ([`FLEE_M`]); ended `done` at the end of it, or `no_way` with nowhere to go.
    fn flee(&self, mut o: Running, movers: &mut [Mover], cx: &mut Cx) -> Option<Running> {
        let i = self.index;
        if o.started {
            let m = &mut movers[i];
            let there = match &m.steer {
                Some(Steer::Route { done, .. }) => *done,
                Some(Steer::To { lat, lon, .. }) => distance_m(m.st.lat, m.st.lon, *lat, *lon) < 1.0,
                _ => false,
            };
            return if there { self.end(o, "done", m, cx) } else { Some(o) };
        }
        o.started = true;
        if !movable(&movers[i]) {
            o.status(&self.id, cx, "held", "immobile", None);
            return Some(o);
        }
        let max = self.speeds(&movers[i]).1;
        let speed = o.spec.speed_mps.map_or(max, |v| v.min(max));
        if self.trail.len() >= 2 {
            let points: Vec<[f64; 2]> = self.trail.iter().rev().copied().collect();
            movers[i].steer = Some(Steer::Route { points, speed_mps: speed, leg: 0, done: false });
            o.status(&self.id, cx, "fleeing", "way_in", None);
            return Some(o);
        }
        let me = pos(&movers[i]);
        let sensors = self.sensors();
        let threat = match &o.spec.from {
            Some(f) => resolve(f, movers, cx.ours, i),
            None => cx
                .ours
                .iter()
                .map(Seen::of_ours)
                .filter(|t| perceives(&sensors, self.domain, me, t))
                .min_by(|a, b| slant(me, a.at()).total_cmp(&slant(me, b.at()))),
        };
        let Some(t) = threat else {
            return self.end(o, "no_way", &mut movers[i], cx);
        };
        let b = bearing_deg(t.lat, t.lon, me[0], me[1]).to_radians();
        let to = offset(me[0], me[1], FLEE_M * b.sin(), FLEE_M * b.cos());
        if !self.go(&mut o, &mut movers[i], cx, to, speed, None, "fleeing", "from_threat") {
            return self.end(o, "no_way", &mut movers[i], cx);
        }
        Some(o)
    }

    /// Fire control (docs/AUTONOMOUS-NODES.md 2.3).
    fn attack(&mut self, mut o: Running, movers: &mut [Mover], cx: &mut Cx) -> Option<Running> {
        let i = self.index;
        let Some(kind) = self.kind else {
            return self.end(o, "no_weapon", &mut movers[i], cx);
        };
        if o.spec.until_s.is_some_and(|u| cx.s >= u) {
            return self.end(o, "until", &mut movers[i], cx);
        }
        // 1. its target, perceived
        let t = match self.sight(&mut o, movers, cx) {
            Sight::Lost => return self.end(o, "target_lost", &mut movers[i], cx),
            Sight::Unseen(last) => return self.search(o, last, &mut movers[i], cx),
            Sight::Now(t) => t,
        };
        // 2. its weapon
        let wi = match choose(kind, &self.rounds, o.spec.weapon.as_deref(), t.domain) {
            Choice::Use(wi) => wi,
            Choice::NoWeapon => return self.end(o, "no_weapon", &mut movers[i], cx),
            Choice::Empty => return self.empty(o, &mut movers[i], cx),
        };
        let w = &kind.loadout[wi];
        let m = &mut movers[i];
        let moves = !o.spec.stay && movable(m);
        if m.disarmed {
            if moves {
                m.steer = Some(Steer::Hold);
            }
            o.status(&self.id, cx, "held", "out_of_action", Some(format!("its {} is out of action (a firepower kill)", w.name)));
            return Some(o);
        }
        if w.kind == "self" {
            return self.strike_run(o, wi, t, m, cx);
        }
        // 3. its range band, slant to the lead point
        let here = pos(m);
        let from = launch_from(w, self.domain, here);
        let (aim, tof) = lead3(from, aim_at(w, &t), (t.ve, t.vn), w.speed_mps);
        let d = slant(from, aim);
        let outside = if d > w.max_range_m {
            Some(("closing", "out_of_range", w.max_range_m * CLOSE_TO))
        } else if d < w.min_range_m {
            Some(("opening", "inside_minimum", (w.min_range_m * OPEN_TO + OPEN_PLUS_M).min(w.max_range_m)))
        } else {
            None
        };
        if let Some((what, why, want)) = outside {
            let note = Some(format!("{:.0} m from its {}'s {:.0} to {:.0} m", d, w.name, w.min_range_m, w.max_range_m));
            if moves {
                let max = self.speeds(m).1;
                self.go(&mut o, m, cx, band_point(here, aim, want), max, None, what, why);
            } else {
                o.status(&self.id, cx, "held", if movable(m) { why } else { "immobile" }, note);
            }
            return Some(o);
        }
        // in its band: it stops to fire (with `stay` its motion stays what it was)
        if moves {
            m.steer = Some(Steer::Hold);
        }
        // 4. ready
        let gun = w.kind == "gun";
        let interval = if gun { w.burst_interval_s.unwrap_or(1.0) } else { w.reload_s.unwrap_or(0.0) };
        if self.last_shot[wi].is_some_and(|last| cx.t - last < interval - 1e-6) {
            o.status(&self.id, cx, "held", if gun { "burst_interval" } else { "reloading" }, None);
            return Some(o);
        }
        // 5. the shot
        let cap = o.spec.rounds.map(|r| r.saturating_sub(o.fired));
        self.fire(&mut o, wi, w, Shot { from, aim, tof, ground_m: distance_m(from[0], from[1], aim[0], aim[1]) }, cap, cx);
        if o.spec.rounds.is_some_and(|r| o.fired >= r) {
            return self.end(o, "salvo_fired", &mut movers[i], cx);
        }
        Some(o)
    }

    /// One shot of `w` (a missile, rocket or torpedo: an act and its flight; a gun: a burst of its
    /// episode's act), its rounds counted.
    fn fire(&mut self, o: &mut Running, wi: usize, w: &Weapon, shot: Shot, cap: Option<u32>, cx: &mut Cx) {
        let target = o.spec.target.clone().unwrap_or(TargetRef::Vehicle(0));
        let (against_vehicle, against_contact) = target.split();
        let t = round(cx.t, 3);
        let from = [round(shot.from[1], 7), round(shot.from[0], 7), round(shot.from[2], 1)];
        let to = [round(shot.aim[1], 7), round(shot.aim[0], 7), round(shot.aim[2], 1)];
        let mut flight = WeaponTruth {
            id: String::new(),
            act: String::new(),
            by: self.id.clone(),
            name: w.name.clone(),
            kind: w.sense.clone().unwrap_or_else(|| w.kind.clone()),
            speed_mps: w.speed_mps,
            launch_t: t,
            impact_t: round(cx.t + shot.tof, 3),
            from,
            to,
            against_vehicle,
            against_contact: against_contact.clone(),
            rounds: None,
        };
        let new_act = |n: &mut Node| {
            n.acts += 1;
            format!("{}-{}", n.id, n.acts)
        };
        let (act, opens, stream, spent) = if w.kind == "gun" {
            let n = w.burst_rounds.unwrap_or(1).min(self.rounds[wi]).min(cap.unwrap_or(u32::MAX));
            let same = self.episode.as_ref().is_some_and(|e| e.victim == target && cx.t - e.last_t <= EPISODE_S);
            if !same {
                let act = new_act(self);
                self.episode = Some(Episode { act, victim: target.clone(), last_t: cx.t, bursts: 0 });
            }
            let e = self.episode.as_mut().expect("an episode");
            e.bursts += 1;
            e.last_t = cx.t;
            flight.id = format!("{}:burst-{}", e.act, e.bursts);
            flight.rounds = Some(n);
            (e.act.clone(), !same, format!("act:{}:{}", e.act, e.bursts), n)
        } else {
            let act = new_act(self);
            flight.id = format!("{act}:weapon");
            let stream = format!("act:{act}");
            (act, true, stream, 1)
        };
        flight.act = act.clone();
        let here = from;
        if opens {
            cx.out.acts.push(ActTruth {
                id: act.clone(),
                s: round(cx.s, 3),
                t,
                by: self.id.clone(),
                lat: here[1],
                lon: here[0],
                against_vehicle,
                against_contact,
                label: o.spec.label.clone(),
                weapon: Some(flight.id.clone()),
                held: None,
            });
        }
        let mut e = ev(&self.id, cx.s, "fired", Some(o.id), None);
        e.act = Some(act);
        e.weapon = Some(w.name.clone());
        e.note = Some(format!(
            "{} at {}, {:.0} m, strikes in {:.1} s",
            flight.rounds.map_or_else(|| w.name.clone(), |r| format!("{r} rounds")),
            target.describe(),
            slant(shot.from, shot.aim),
            shot.tof
        ));
        cx.out.events.push(e);
        o.status = Some(("fired".into(), None));
        cx.armory.fly(flight, Strike { weapon: w.damage(), stream, out_m: Some(shot.ground_m) });
        self.rounds[wi] -= spent;
        self.last_shot[wi] = Some(cx.t);
        o.fired += spent;
    }

    /// Its own weapon: steer into the target at its top speed (diving to its height, an
    /// aircraft); a step that passes within [`STRIKE_M`] of it is the strike: an abstract act and
    /// an impact (`<contact>-self`) at that second, and the node leaves the scenario the next.
    fn strike_run(&mut self, mut o: Running, wi: usize, t: Seen, m: &mut Mover, cx: &mut Cx) -> Option<Running> {
        let here = pos(m);
        let target = o.spec.target.clone().unwrap_or(TargetRef::Vehicle(0));
        if pass_m(self.prev.unwrap_or(here), here, t.at()) <= STRIKE_M {
            let kind = self.kind.expect("an armed kind");
            let w = &kind.loadout[wi];
            self.acts += 1;
            let act = format!("{}-{}", self.id, self.acts);
            let weapon = format!("{}-self", self.id);
            let (against_vehicle, against_contact) = target.split();
            let (lat, lon) = (round(t.lat, 7), round(t.lon, 7));
            m.st.lat = t.lat;
            m.st.lon = t.lon;
            m.st.alt_m = t.alt;
            m.st.speed_mps = 0.0;
            m.steer = Some(Steer::Hold);
            cx.out.acts.push(ActTruth {
                id: act.clone(),
                s: round(cx.s, 3),
                t: round(cx.t, 3),
                by: self.id.clone(),
                lat,
                lon,
                against_vehicle,
                against_contact: against_contact.clone(),
                label: o.spec.label.clone(),
                weapon: None,
                held: None,
            });
            cx.out.impacts.push(ImpactTruth {
                weapon: weapon.clone(),
                act: act.clone(),
                t: round(cx.t, 3),
                lat,
                lon,
                against_vehicle,
                against_contact,
                damage: None,
            });
            cx.armory.note(&weapon, Strike { weapon: w.damage(), stream: format!("act:{act}"), out_m: None });
            self.rounds[wi] = 0;
            self.expended = true;
            let mut e = ev(&self.id, cx.s, "struck", Some(o.id), None);
            e.act = Some(act);
            e.weapon = Some(w.name.clone());
            e.note = Some(format!("into {}", target.describe()));
            cx.out.events.push(e);
            cx.out.events.push(ev(&self.id, cx.s, "ended", Some(o.id), Some("expended")));
            return None;
        }
        if o.spec.stay || !movable(m) {
            o.status(&self.id, cx, "held", if o.spec.stay { "out_of_range" } else { "immobile" }, None);
            return Some(o);
        }
        let max = self.speeds(m).1;
        let (lead, _) = lead3(here, t.at(), (t.ve, t.vn), max);
        let alt = (self.domain == Domain::Air).then_some(t.alt);
        self.go(&mut o, m, cx, (lead[0], lead[1]), max, alt, "closing", "strike_run");
        Some(o)
    }

    /// Out of rounds for its target: the order ends; with no round of anything left, the kind's
    /// `on_empty` (default: flee from its target along its way in), else it holds.
    fn empty(&self, o: Running, m: &mut Mover, cx: &mut Cx) -> Option<Running> {
        let from = o.spec.target.clone();
        self.end(o, "out_of_rounds", m, cx);
        if self.rounds.iter().any(|r| *r > 0) {
            return None;
        }
        let then = self.kind.and_then(|k| k.on_empty).unwrap_or_default();
        let id = *cx.next_id;
        *cx.next_id += 1;
        let mut e = ev(&self.id, cx.s, "order", Some(id), Some("on_empty"));
        e.note = Some(format!("{:?}", then).to_lowercase());
        cx.out.events.push(e);
        match then {
            OnEmpty::Flee => {
                let mut spec = OrderSpec::bare(&self.id, Do::Flee);
                spec.from = from;
                Some(Running::new(id, spec, cx.s))
            }
            OnEmpty::Hold => {
                cx.out.events.push(ev(&self.id, cx.s, "ended", Some(id), Some("done")));
                None
            }
            OnEmpty::Resume => {
                if movable(m) {
                    m.steer = m.rejoin(self.speeds(m).0);
                }
                cx.out.events.push(ev(&self.id, cx.s, "ended", Some(id), Some("done")));
                None
            }
        }
    }
}

/// A shot's geometry: from the launch to the lead point (`[lat, lon, alt]`), its time of flight
/// and its distance over the ground.
struct Shot {
    from: [f64; 3],
    aim: [f64; 3],
    tof: f64,
    ground_m: f64,
}

/// Whether an order can move it: its propulsion lost, only an aircraft flies on.
fn movable(m: &Mover) -> bool {
    !(m.crippled && m.contact.domain != Domain::Air)
}

/// Every node of a scenario and the orders still to come.
pub struct Nodes {
    nodes: Vec<Node>,
    pending: Vec<Pending>,
    next_id: u64,
    grid: Option<LandWater>,
}

impl Nodes {
    /// The scenario's nodes (its contacts of armed kinds and those its orders name) and its
    /// orders, numbered from 1 in file order and taken in order of their seconds.
    pub fn new(scenario: &Scenario, movers: &[Mover]) -> Nodes {
        let named = |id: &str| scenario.orders.iter().any(|o| o.node == id);
        let nodes = movers
            .iter()
            .enumerate()
            .filter(|(_, m)| m.contact.armed_kind() || named(&m.contact.id))
            .map(|(i, m)| Node::new(i, m))
            .collect();
        let mut n = Nodes { nodes, pending: Vec::new(), next_id: 1, grid: None };
        for o in &scenario.orders {
            n.order(o.clone());
        }
        n
    }

    /// The land/water grid its steering keeps to (`[sensing] terrain`).
    pub fn with_grid(mut self, grid: LandWater) -> Nodes {
        self.grid = Some(grid);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.pending.is_empty()
    }

    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Every node so far (a contact an order names becomes one when it takes it).
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The orders still to come, in the order they will be taken: each one's id and order.
    pub fn pending(&self) -> impl Iterator<Item = (u64, &OrderSpec)> {
        self.pending.iter().map(|p| (p.id, &p.spec))
    }

    /// An order to come (none `at_s`: at the next tick), checked by the caller ([`check`]); its
    /// id. Orders of the same second are taken in the order they came.
    pub fn order(&mut self, spec: OrderSpec) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let at = spec.at_s.unwrap_or(0.0);
        let k = self.pending.partition_point(|p| p.spec.at_s.unwrap_or(0.0) <= at);
        self.pending.insert(k, Pending { id, spec });
        id
    }

    /// One look per node with an order that needs a target (attack, track, flee): where it is
    /// and its sensors' farthest reach.
    pub fn looks(&self, movers: &[Mover]) -> Vec<Look> {
        self.nodes
            .iter()
            .filter(|n| n.order.as_ref().is_some_and(|o| matches!(o.spec.what, Do::Attack | Do::Track | Do::Flee)))
            .filter(|n| !n.expended && movers[n.index].st.exists)
            .filter_map(|n| {
                let s = n.sensors();
                let reach = s.eye_m.max(s.radar_m).max(s.sonar_m);
                let m = &movers[n.index];
                (reach > 0.0).then(|| Look {
                    node: n.id.clone(),
                    lat: round(m.st.lat, 7),
                    lon: round(m.st.lon, 7),
                    alt_m: round(m.st.alt_m, 1),
                    reach_m: reach,
                })
            })
            .collect()
    }

    /// One tick at scenario second `s` (unix `t`), after the movers stepped: a node spent by its
    /// own strike leaves; every node marks its way in; the orders whose second has come are
    /// taken; every node with an order runs it (its shots to `armory`), setting its steering for
    /// the next step. `ours`: our vehicles as the bridge's latest answer gave them.
    pub fn tick(&mut self, s: f64, t: f64, movers: &mut [Mover], ours: &[OursAt], armory: &mut Armory) -> Tick {
        let mut out = Tick::default();
        let Nodes { nodes, pending, next_id, grid } = self;
        for n in nodes.iter_mut() {
            if n.expended && !n.gone {
                let m = &mut movers[n.index];
                m.out_of_play = true;
                m.st.exists = false;
                m.steer = None;
                n.gone = true;
            }
            n.crumb(&movers[n.index]);
        }
        let mut cx = Cx { s, t, ours, grid: grid.as_ref(), armory, out: &mut out, next_id };
        let mut wait = Vec::new();
        for p in std::mem::take(pending) {
            if p.spec.at_s.is_some_and(|a| a > s) {
                wait.push(p);
                continue;
            }
            let ni = match nodes.iter().position(|n| n.id == p.spec.node) {
                Some(ni) => ni,
                None => match movers.iter().position(|m| m.contact.id == p.spec.node) {
                    Some(mi) => {
                        nodes.push(Node::new(mi, &movers[mi]));
                        nodes.len() - 1
                    }
                    None => {
                        cx.out.events.push(ev(&p.spec.node, s, "refused", Some(p.id), Some("unknown_node")));
                        continue;
                    }
                },
            };
            let n = &mut nodes[ni];
            let m = &mut movers[n.index];
            if n.gone || m.out_of_play || m.contact.end_s.is_some_and(|e| s >= e) {
                cx.out.events.push(ev(&n.id, s, "refused", Some(p.id), Some("node_gone")));
                continue;
            }
            if !m.st.exists {
                // not in the scenario yet: taken once it is
                wait.push(p);
                continue;
            }
            n.take(p.id, p.spec, m, &mut cx);
        }
        *pending = wait;
        for n in nodes.iter_mut() {
            n.run(movers, &mut cx);
            n.prev = Some(pos(&movers[n.index]));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::damage::DamageState;
    use crate::effect::{DamageCtx, OursDamaged};
    use crate::weapon::strike_by;

    /// 2026-06-14 19:00Z, local noon at the Gate: a clear day, no night factor in a draw.
    const T0: f64 = 1_781_463_600.0;
    /// Mid-strait west of the bridge, where the hostile-act boats hold, `[lon, lat]`.
    const AT: [f64; 2] = [-122.4950, 37.8120];
    const SEED: u64 = 17;
    const HOLDS: &str = "motion = \"station\"\nat = [-122.4950, 37.8120]";
    const ATGM: &str = "weapon = \"ATGM-class missile\"";
    const HMG: &str = "weapon = \"12.7 mm HMG (DShK/NSV-class)\"";

    /// `east` and `north` metres from AT, `[lon, lat]`.
    fn off(east: f64, north: f64) -> [f64; 2] {
        let (lat, lon) = offset(AT[1], AT[0], east, north);
        [lon, lat]
    }

    /// One of ours (uid 1) `east` metres east of AT, still: the Sea Hunter on the water, or an
    /// MQ-9B at `alt_m`.
    fn ours(east: f64, alt_m: f64) -> OursAt {
        let p = off(east, 0.0);
        let (platform, domain) = if alt_m > 50.0 { ("mq9b_armed", "air") } else { ("seahunter", "surface") };
        OursAt { uid: 1, lat: p[1], lon: p[0], alt_m, ve: 0.0, vn: 0.0, platform: Some(platform.into()), domain: Some(domain.into()), armed: false }
    }

    /// A scenario of one node, `raider`, of `kind`, moving as `motion` says, and its orders.
    fn scenario(kind: &str, domain: &str, motion: &str, orders: &str) -> String {
        format!(
            "title = \"t\"\nseed = {SEED}\n[[contact]]\nid = \"raider\"\naffiliation = \"hostile\"\ndomain = \"{domain}\"\nkind = \"{kind}\"\n{motion}\n{orders}"
        )
    }

    /// An attack on our vehicle 1 from second 1.
    fn attack(extra: &str) -> String {
        format!("[[order]]\nat_s = 1\nnode = \"raider\"\ndo = \"attack\"\ntarget = {{ vehicle = 1 }}\n{extra}\n")
    }

    /// The armed fast boat holding at AT under an attack order.
    fn boat(extra: &str) -> World {
        World::new(&scenario("armed_fast_boat", "surface", HOLDS, &attack(extra)), None)
    }

    #[derive(Debug, Default)]
    struct Log {
        acts: Vec<ActTruth>,
        weapons: Vec<WeaponTruth>,
        impacts: Vec<ImpactTruth>,
        events: Vec<NodeEvent>,
        hurt: Vec<OursDamaged>,
    }

    impl Log {
        fn said(&self, what: &str, why: &str) -> bool {
            self.events.iter().any(|e| e.what == what && e.why.as_deref() == Some(why))
        }
    }

    /// The contacts process's loop without the bridge: the movers step, the nodes run, the
    /// flights in the air and the strikes due, drawn by the damage model.
    struct World {
        movers: Vec<Mover>,
        nodes: Nodes,
        armory: Armory,
        s: f64,
    }

    impl World {
        fn new(toml: &str, grid: Option<LandWater>) -> World {
            let scenario = Scenario::from_toml(toml).expect("scenario");
            let movers: Vec<Mover> =
                scenario.contacts().expect("contacts").into_iter().enumerate().map(|(i, c)| Mover::new(c, SEED, i as u64)).collect();
            let mut nodes = Nodes::new(&scenario, &movers);
            if let Some(g) = grid {
                nodes = nodes.with_grid(g);
            }
            World { movers, nodes, armory: Armory::default(), s: 0.0 }
        }

        /// `secs` seconds, our vehicles each second as the bridge would answer them.
        fn run(&mut self, secs: u32, ours: impl Fn(f64) -> Vec<OursAt>) -> Log {
            let mut log = Log::default();
            let ctx = DamageCtx { seed: SEED, conditions: None, hit_override: None };
            for _ in 0..secs {
                let prev_t = T0 + self.s;
                self.s += 1.0;
                let (s, t) = (self.s, T0 + self.s);
                for m in &mut self.movers {
                    m.step(s, 1.0, None);
                }
                let now = ours(s);
                let tick = self.nodes.tick(s, t, &mut self.movers, &now, &mut self.armory);
                for f in self.armory.in_flight(prev_t, t) {
                    if !log.weapons.iter().any(|x| x.id == f.id) {
                        log.weapons.push(f);
                    }
                }
                let mut impacts = self.armory.impacts(t);
                impacts.extend(tick.impacts);
                let map: BTreeMap<u64, OursAt> = now.iter().map(|o| (o.uid, o.clone())).collect();
                let armory = &mut self.armory;
                log.hurt.extend(strike_by(&mut impacts, &mut self.movers, &map, &ctx, |i| armory.take(&i.weapon)));
                log.acts.extend(tick.acts);
                log.impacts.extend(impacts);
                log.events.extend(tick.events);
            }
            log
        }

        fn raider(&self) -> &Mover {
            &self.movers[0]
        }

        fn moved_m(&self) -> f64 {
            distance_m(self.raider().st.lat, self.raider().st.lon, AT[1], AT[0])
        }
    }

    fn slant_of(f: &WeaponTruth) -> f64 {
        distance_m(f.from[1], f.from[0], f.to[1], f.to[0]).hypot(f.to[2] - f.from[2])
    }

    /// From where it is, the boat's missile holds inside its 100 m minimum and fires in its band;
    /// its machine gun holds beyond its 2 km.
    #[test]
    fn fires_only_inside_its_band() {
        let mut w = boat(&format!("{ATGM}\nstay = true"));
        let log = w.run(10, |_| vec![ours(60.0, 0.0)]);
        assert!(log.acts.is_empty() && log.said("held", "inside_minimum"), "{:?}", log.events);
        let log = w.run(1, |_| vec![ours(1500.0, 0.0)]);
        assert_eq!((log.acts.len(), log.weapons.len()), (1, 1));
        assert!((100.0..=5500.0).contains(&slant_of(&log.weapons[0])));
        let mut w = boat(&format!("{HMG}\nstay = true"));
        let log = w.run(10, |_| vec![ours(3000.0, 0.0)]);
        assert!(log.acts.is_empty() && log.said("held", "out_of_range"), "{:?}", log.events);
    }

    /// Four missiles and no fifth; a salvo's cap counts a gun's rounds, the last burst cut to it.
    #[test]
    fn never_past_its_rounds() {
        let mut w = boat(&format!("{ATGM}\nstay = true"));
        let log = w.run(300, |_| vec![ours(1500.0, 0.0)]);
        assert_eq!(log.acts.len(), 4);
        assert_eq!(w.nodes.node("raider").expect("the node").rounds[0], 0);
        assert!(log.said("ended", "out_of_rounds"));
        // its gun's rounds are left: it holds rather than following on_empty
        assert!(!log.said("order", "on_empty"));
        let mut w = boat(&format!("{HMG}\nstay = true\nrounds = 10"));
        let log = w.run(60, |_| vec![ours(1000.0, 0.0)]);
        assert_eq!(log.weapons.iter().map(|f| f.rounds).collect::<Vec<_>>(), vec![Some(6), Some(4)]);
        assert!(log.said("ended", "salvo_fired"));
    }

    /// A missile team's reload between shots (30 s), a gun's burst interval (5 s).
    #[test]
    fn waits_its_reload_or_burst_interval() {
        let gaps = |log: &Log| log.weapons.windows(2).map(|f| f[1].launch_t - f[0].launch_t).collect::<Vec<_>>();
        let log = boat(&format!("{ATGM}\nstay = true")).run(100, |_| vec![ours(1500.0, 0.0)]);
        assert_eq!(gaps(&log), vec![30.0; 3]);
        assert!(log.said("held", "reloading"));
        let log = boat(&format!("{HMG}\nstay = true")).run(21, |_| vec![ours(1000.0, 0.0)]);
        assert_eq!(gaps(&log), vec![5.0; 4]);
        assert!(log.said("held", "burst_interval"));
    }

    /// Its weapons put out of action (a firepower kill), it holds its fire.
    #[test]
    fn holds_fire_when_its_weapons_are_out_of_action() {
        let mut w = boat(ATGM);
        w.movers[0].damage(DamageState::Firepower, T0);
        let log = w.run(30, |_| vec![ours(1500.0, 0.0)]);
        assert!(log.acts.is_empty() && log.said("held", "out_of_action"), "{:?}", log.events);
    }

    /// Out of its gun's reach it closes until in it, then fires; inside its missile's minimum it
    /// opens out, then fires. (With `stay` it holds: `fires_only_inside_its_band`.)
    #[test]
    fn closes_when_out_of_reach_and_opens_out_inside_its_minimum() {
        let mut w = boat(HMG);
        let log = w.run(200, |_| vec![ours(3000.0, 0.0)]);
        assert!(log.said("closing", "out_of_range"), "{:?}", log.events);
        let f = log.weapons.first().expect("a burst once within reach");
        assert!(slant_of(f) <= 2000.0 && distance_m(f.from[1], f.from[0], AT[1], AT[0]) > 900.0);
        let mut w = boat(ATGM);
        let log = w.run(120, |_| vec![ours(60.0, 0.0)]);
        assert!(log.said("opening", "inside_minimum"), "{:?}", log.events);
        let f = log.weapons.first().expect("a missile once past its minimum");
        assert!(slant_of(f) >= 100.0, "{}", slant_of(f));
    }

    /// Unseen for LOST_S the order ends `target_lost`; out of every round it ends
    /// `out_of_rounds` and the node follows its kind's on_empty: away along its way in.
    #[test]
    fn ends_on_a_lost_target_and_out_of_rounds() {
        let mut w = boat(&format!("{ATGM}\nstay = true"));
        assert_eq!(w.run(2, |_| vec![ours(1500.0, 0.0)]).acts.len(), 1);
        let log = w.run(70, |_| Vec::new());
        assert!(log.said("held", "not_perceived"));
        let lost = log.events.iter().find(|e| e.why.as_deref() == Some("target_lost")).expect("lost");
        assert!((lost.s - (2.0 + LOST_S)).abs() <= 1.0, "{}", lost.s);
        // the armed UAS comes in 3 km from the west, holds, and fires its two missiles
        let inbound = "motion = \"transit\"\nspeed_mps = 25\nroute = [[-122.5291, 37.8120], [-122.4950, 37.8120]]";
        let order = "[[order]]\nat_s = 200\nnode = \"raider\"\ndo = \"attack\"\ntarget = { vehicle = 1 }\n";
        let mut w = World::new(&scenario("armed_uas", "air", inbound, order), None);
        let log = w.run(280, |_| vec![ours(2000.0, 0.0)]);
        assert_eq!(log.acts.len(), 2);
        assert!(log.said("ended", "out_of_rounds") && log.said("order", "on_empty") && log.said("fleeing", "way_in"), "{:?}", log.events);
        assert!(w.raider().st.lon < AT[0] - 0.005, "back west along its way in: {}", w.raider().st.lon);
    }

    /// From the armed UAS at 120 m, its missile at an aircraft at 1,500 m flies to that height,
    /// its time of flight the slant at its speed.
    #[test]
    fn a_missile_at_an_aircraft_flies_to_its_height() {
        let mut w = World::new(&scenario("armed_uas", "air", HOLDS, &attack("")), None);
        let log = w.run(1, |_| vec![ours(3000.0, 1500.0)]);
        let f = log.weapons.first().expect("a missile at the aircraft");
        assert_eq!(f.kind, "air_missile");
        assert!((f.from[2] - 120.0).abs() < 0.5 && (f.to[2] - 1500.0).abs() < 0.5, "{:?} {:?}", f.from, f.to);
        assert!((f.impact_t - f.launch_t - slant_of(f) / 510.0).abs() < 0.01);
    }

    /// A gun's bursts at one victim are one act, each burst its own flight with its rounds and
    /// its own strike; after a pause past EPISODE_S the next burst opens a new act.
    #[test]
    fn a_gun_episode_is_one_act_with_several_bursts() {
        let mut w = boat(&format!("{HMG}\nstay = true"));
        let log = w.run(21, |_| vec![ours(1000.0, 0.0)]);
        assert_eq!(log.acts.len(), 1);
        let act = &log.acts[0];
        assert_eq!(act.id, "raider-1");
        assert_eq!(log.weapons.len(), 5);
        assert!(log.weapons.iter().all(|f| f.act == act.id && f.kind == "gun" && f.rounds == Some(6)));
        assert_eq!(act.weapon.as_deref(), Some(log.weapons[0].id.as_str()));
        assert!(log.impacts.len() >= 4 && log.impacts.iter().all(|i| i.act == act.id));
        w.run(40, |_| Vec::new());
        let log = w.run(5, |_| vec![ours(1000.0, 0.0)]);
        assert_eq!(log.acts.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), vec!["raider-2"]);
    }

    /// The explosive boat is its own weapon: nothing flies; it steers into HUNTER 1, and at the
    /// strike there is an abstract act and an impact drawn by the damage model; it is gone after.
    #[test]
    fn a_self_node_strikes_by_steering_in() {
        let mut w = World::new(&scenario("explosive_boat", "surface", HOLDS, &attack("")), None);
        let log = w.run(200, |_| vec![ours(1000.0, 0.0)]);
        assert!(log.weapons.is_empty());
        assert!(log.said("closing", "strike_run") && log.said("ended", "expended"));
        assert_eq!(log.acts.len(), 1);
        let (a, i) = (&log.acts[0], &log.impacts[0]);
        assert!(a.weapon.is_none() && (i.weapon.as_str(), i.act.as_str()) == ("raider-self", "raider-1"));
        let at = ours(1000.0, 0.0);
        assert!(distance_m(a.lat, a.lon, at.lat, at.lon) < 1.0);
        assert_eq!(i.damage.as_ref().and_then(|d| d.figures.as_deref()), Some("fishing_vessel"));
        assert!(!w.raider().st.exists && w.raider().out_of_play);
    }

    /// The boat's missile reaches 5.5 km, its eyes 5 km: at 5.2 km it neither fires nor moves,
    /// at 4.8 km it fires. A radar's 15 km stops at the radio horizon of both heights.
    #[test]
    fn a_target_is_engaged_only_when_perceived() {
        let mut w = boat(ATGM);
        let log = w.run(20, |_| vec![ours(5200.0, 0.0)]);
        assert!(log.acts.is_empty() && log.said("held", "not_perceived") && w.moved_m() < 1.0, "{:?}", log.events);
        assert_eq!(w.nodes.looks(&w.movers)[0].reach_m, 5000.0);
        assert_eq!(w.run(1, |_| vec![ours(4800.0, 0.0)]).acts.len(), 1);
        let radar = Catalog::shipped().get("naval_auxiliary").and_then(|k| k.sensors).expect("its sensors");
        let here = [AT[1], AT[0], 0.0];
        let (boat, plane) = (Seen::of_ours(&ours(13_000.0, 0.0)), Seen::of_ours(&ours(13_000.0, 1000.0)));
        assert!(!perceives(&radar, Domain::Surface, here, &boat) && perceives(&radar, Domain::Surface, here, &plane));
    }

    /// Each missile's strike on HUNTER 1 is drawn by the damage model with the catalog's figures
    /// (the Sea Hunter reads as a fishing vessel: 0.70), on the act's stream; what it hurt is
    /// handed back for the vehicle simulation.
    #[test]
    fn a_strike_on_ours_with_catalog_figures_is_drawn() {
        let log = boat(&format!("{ATGM}\nstay = true")).run(100, |_| vec![ours(1500.0, 0.0)]);
        assert_eq!(log.impacts.len(), 4);
        for i in &log.impacts {
            let d = i.damage.as_ref().expect("drawn");
            assert_eq!((d.who.as_str(), d.figures.as_deref(), d.p_hit), ("ours:1", Some("fishing_vessel"), 0.7));
            assert_eq!(d.u_hit, crate::damage::uniform(SEED, &format!("act:{}:hit", i.act)));
            let hurt = log.hurt.iter().any(|h| h.cause.ends_with(&format!("({})", i.act)));
            assert_eq!(hurt, d.hit && d.state != DamageState::Undamaged);
        }
    }

    /// Water but for a strip of land 1 km east of the boat: it does not take a leg across it
    /// (it holds, `no_way`); a transit across it ends `no_way`; one on its side of it goes.
    #[test]
    fn steering_keeps_to_its_domain_on_the_grid() {
        let grid = || {
            LandWater::from_fn(37.79, -122.53, 0.00018, 0.00023, 250, 400, |_, lon| {
                let east = (lon - AT[0]) * M_PER_DEG * AT[1].to_radians().cos();
                if (950.0..1050.0).contains(&east) { 10.0 } else { 0.0 }
            })
        };
        let mut w = World::new(&scenario("armed_fast_boat", "surface", HOLDS, &attack(HMG)), Some(grid()));
        let log = w.run(30, |_| vec![ours(3000.0, 0.0)]);
        assert!(log.acts.is_empty() && log.said("held", "no_way") && w.moved_m() < 1.0, "{:?}", log.events);
        let transit = |to: &str| format!("[[order]]\nat_s = 1\nnode = \"raider\"\ndo = \"transit\"\nroute = [{to}]\n");
        let mut w = World::new(&scenario("armed_fast_boat", "surface", HOLDS, &transit("[-122.4700, 37.8120]")), Some(grid()));
        assert!(w.run(5, |_| Vec::new()).said("ended", "no_way") && w.moved_m() < 1.0);
        let mut w = World::new(&scenario("armed_fast_boat", "surface", HOLDS, &transit("[-122.4900, 37.8150]")), Some(grid()));
        let log = w.run(120, |_| Vec::new());
        assert!(log.said("moving", "transit") && log.said("ended", "done") && w.moved_m() > 500.0, "{:?}", log.events);
        // the Golden Gate's own grid reads mid-strait as water and Hawk Hill (Marin) as land
        let gate = LandWater::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sim/headless/terrain/golden-gate.grid.gz")).expect("the grid");
        assert_eq!((gate.wet(AT[1], AT[0]), gate.wet(37.8270, -122.4995)), (Some(true), Some(false)));
    }

    /// An order that cannot stand fails the file's load, saying why.
    #[test]
    fn orders_are_checked_at_load() {
        let boat = |orders: &str| scenario("armed_fast_boat", "surface", HOLDS, orders);
        assert!(Scenario::from_toml(&boat(&attack(ATGM))).is_ok());
        for (toml, why) in [
            (scenario("small_fast_boat", "surface", HOLDS, &attack("")), "carries no weapon"),
            (boat(&attack("weapon = \"railgun\"")), "carries no \"railgun\""),
            (boat(&attack("route = [[-122.5, 37.8]]")), "not route"),
            (boat("[[order]]\nat_s = 1\nnode = \"raider\"\ndo = \"attack\"\ntarget = { contact = \"raider\" }\n"), "no other contact"),
            (boat("[[order]]\nnode = \"raider\"\ndo = \"hold\"\n"), "at_s"),
            (boat("[[order]]\nat_s = 1\nnode = \"nobody\"\ndo = \"hold\"\n"), "no contact"),
            (boat("[[order]]\nat_s = 1\nnode = \"raider\"\ndo = \"flee\"\nspeed_mps = 40\n"), "top speed"),
            (format!("{}[sensing]\nmodel = \"legacy\"\n", boat(&attack(""))), "physical sensing model"),
        ] {
            let e = Scenario::from_toml(&toml).expect_err(why);
            assert!(e.contains(why), "{why}: {e}");
        }
    }
}
