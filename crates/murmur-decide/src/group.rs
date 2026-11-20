//! Group orders (decision rules 34, [`crate::rules::GROUP_ORDER_SINCE`]; docs/SWARMS.md, picked
//! up on 2026-11-20 for the ordinary tasks): one order to many of our vehicles, which the engine
//! lays out member by member, checks share by share, and dispatches as one-vehicle member orders
//! under the group's one approval.
//!
//! What the person approves is the **envelope**: the task and its geometry, how the members share
//! it (the [`Spread`]), the band their layers stay in, the minimum strength, the selection (the
//! group's `uids`) and whether it re-forms. Inside it the engine lays the members out
//! ([`layout`]): which member takes which slot of the ring, the route or the stations, at which
//! layer, from the members' logged facts and their types' catalog figures ([`Formation`]), so the
//! same facts give the same layout, every share is checked as a one-vehicle order is, and replay
//! recomputes it. A member order is an intent with one uid, its `parent` (the group's id) and its
//! [`Share`] (slot, layer, altitude, where it enters the ring or the route); the task layer flies
//! exactly that.
//!
//! This module holds the types every stage reads (the group's spec, a member's share, the
//! catalog's figures, the member classes) and the layout. Nothing here touches an engagement: a
//! member an engagement holds is left out of a group order, saying so, and a group order never
//! stops one (SWARMS.md decision 6, as the owner narrowed it).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::catalog::{AutopilotKind, Domain, PlatformFacts};
use crate::geo::LonLat;
use crate::ids::{IntentId, Uid};
use crate::intent::{IntentSpec, Task, TaskKind};
use crate::rules::{CheckResult, Outcome, RuleId, RulesConfig};
use crate::time::Time;
use crate::world::{VehicleFacts, WorldSnapshot};

/// How a group's members share its task (docs/SWARMS.md section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Spread {
    /// An orbit: n slots evenly round the circle, each member entering at its own and flying the
    /// ring (fixed wings too, as a waypoint ring no tighter than their turn).
    Ring,
    /// A patrol: members evenly along the closed route by distance; a two-point route becomes a
    /// racetrack whose legs are two turn radii (or the lateral minimum) apart.
    Along,
    /// A patrol: `lanes` copies of the route offset sideways by `lane_spacing_m`, the members
    /// shared evenly over them, then along each.
    Lanes,
    /// A hold: each member holds where it is, by its type. Never refused for its layout.
    Here,
    /// An rtl: each member to its own home. Never refused for its layout.
    Home,
    /// A transit: n points evenly along the route (its first and its last among them), each
    /// member flying straight to its own (entering the route there) and holding by its type.
    Stations,
    /// A transit: a grid round the route's end, ceil(sqrt(n)) columns the lateral minimum apart
    /// (a fixed wing's two loiter radii), each member flying the route moved as its cell sits from
    /// the end (the block keeping its shape along the way) and holding in its cell.
    Block,
}

impl Spread {
    pub fn as_str(self) -> &'static str {
        match self {
            Spread::Ring => "ring",
            Spread::Along => "along",
            Spread::Lanes => "lanes",
            Spread::Here => "here",
            Spread::Home => "home",
            Spread::Stations => "stations",
            Spread::Block => "block",
        }
    }

    /// The spread a kind's group order takes when the proposal names none; None: the kind has no
    /// group meaning (a tracker's track, an engagement, an identify, a lay, a satellite request).
    pub fn default_for(kind: TaskKind) -> Option<Spread> {
        match kind {
            TaskKind::Orbit => Some(Spread::Ring),
            TaskKind::Patrol => Some(Spread::Along),
            TaskKind::Hold => Some(Spread::Here),
            TaskKind::Rtl => Some(Spread::Home),
            TaskKind::Transit => Some(Spread::Stations),
            _ => None,
        }
    }

    /// Whether this spread shares a task of `kind`.
    pub fn fits(self, kind: TaskKind) -> bool {
        match self {
            Spread::Ring => kind == TaskKind::Orbit,
            Spread::Along | Spread::Lanes => kind == TaskKind::Patrol,
            Spread::Here => kind == TaskKind::Hold,
            Spread::Home => kind == TaskKind::Rtl,
            Spread::Stations | Spread::Block => kind == TaskKind::Transit,
        }
    }

    /// Whether the members of this spread are laid out at all: a hold holds where each member
    /// is, an rtl goes to each one's home.
    pub fn laid_out(self) -> bool {
        !matches!(self, Spread::Here | Spread::Home)
    }
}

/// A group order's own spec (`group` on a proposal and on the group's `intent.created`). The
/// engine fills what the proposal leaves out before it logs the intent ([`GroupSpec::normalized`]),
/// so the log says what was decided: the spread, the minimum and whether it re-forms. `swarm` is
/// a label for the console and agents, never a decision input (decision 0: who is in a swarm is
/// the fleet file's, and the engine logs the uids).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread: Option<Spread>,
    /// The minimum strength: fewer members able to take it (sent or waiting) and the order is
    /// refused, saying how many pass. Default: the policy's `[group] min_share` of the selection
    /// (an estimate), one for a hold or an rtl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_members: Option<u32>,
    /// The layer base, m (above the home's ground for aircraft, below the surface for subs, as
    /// the task layer flies a mission's altitude); absent: each type's cruise altitude.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
    /// How long a waiting member may join, s; absent: the queue's wait for the kind (a person's
    /// approval held at most `[queue] human_expire_s`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_s: Option<u32>,
    /// Whether the group re-forms itself when members drop (a later stage); never for a hold or
    /// an rtl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reform: Option<bool>,
    /// `lanes`: how many copies of the route, and how far apart sideways, m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lanes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane_spacing_m: Option<f64>,
}

/// The most members one group order may name (a fleet's largest swarm is 100; Saturation's
/// region fleet hands over orders, not groups, for the rest).
pub const MAX_MEMBERS: usize = 400;

impl GroupSpec {
    /// Why this spec can't be a group order of `kind` over `n` vehicles, if it can't.
    pub fn invalid(&self, kind: TaskKind, n: usize) -> Option<String> {
        if n < 2 {
            return Some("a group order names two vehicles or more (one vehicle's order is an order of its own)".into());
        }
        if n > MAX_MEMBERS {
            return Some(format!("a group order names at most {MAX_MEMBERS} vehicles"));
        }
        let Some(default) = Spread::default_for(kind) else {
            return Some(format!("{} has no group meaning: name one vehicle", kind.as_str()));
        };
        let spread = self.spread.unwrap_or(default);
        if !spread.fits(kind) {
            return Some(format!("{} spreads no {} order", spread.as_str(), kind.as_str()));
        }
        if let Some(m) = self.min_members
            && (m == 0 || m as usize > n)
        {
            return Some(format!("min_members is 1 to the {n} vehicles named"));
        }
        if let Some(a) = self.alt_m
            && !(a.is_finite() && (0.0..=10_000.0).contains(&a))
        {
            return Some("alt_m is the layer base, 0 to 10,000 m".into());
        }
        if self.wait_s.is_some_and(|w| w > 3600) {
            return Some("wait_s is 0 to 3,600 s".into());
        }
        if self.reform == Some(true) && matches!(kind, TaskKind::Hold | TaskKind::Rtl) {
            return Some("a hold or an rtl never re-forms".into());
        }
        if self.reform == Some(true) && kind == TaskKind::Transit {
            return Some("a transit's stations and block are not re-formed: a gap a member leaves stays open, shown on its swarm".into());
        }
        match (spread, self.lanes, self.lane_spacing_m) {
            (Spread::Lanes, Some(l), Some(s)) if (2..=50).contains(&l) && s.is_finite() && (10.0..=10_000.0).contains(&s) => {}
            (Spread::Lanes, _, _) => return Some("lanes needs lanes (2 to 50) and lane_spacing_m (10 to 10,000 m)".into()),
            (_, None, None) => {}
            _ => return Some("lanes and lane_spacing_m are for the lanes spread only".into()),
        }
        if self.swarm.as_ref().is_some_and(|s| s.trim().is_empty() || s.len() > 64) {
            return Some("swarm is a label of 1 to 64 characters".into());
        }
        None
    }

    /// The spec as the engine logs it for `kind` over `n` vehicles: the spread, the minimum
    /// (`min_share` of the selection, rounded up; one for a hold or an rtl) and whether it
    /// re-forms (never a hold, an rtl or a transit) filled in.
    pub fn normalized(&self, kind: TaskKind, n: usize, min_share: f64) -> GroupSpec {
        let safety = matches!(kind, TaskKind::Hold | TaskKind::Rtl);
        let min = if safety {
            1
        } else {
            ((n as f64 * min_share).ceil() as u32).clamp(1, n as u32)
        };
        GroupSpec {
            spread: self.spread.or_else(|| Spread::default_for(kind)),
            min_members: Some(self.min_members.unwrap_or(min)),
            reform: Some(self.reform.unwrap_or(!safety && kind != TaskKind::Transit)),
            ..self.clone()
        }
    }

    pub fn spread(&self) -> Spread {
        self.spread.unwrap_or(Spread::Ring)
    }

    pub fn min_members(&self) -> u32 {
        self.min_members.unwrap_or(1)
    }
}

/// One member's share of a group's task: what its member order flies, and what replay binds it
/// to (docs/SWARMS.md section 2.1).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Share {
    /// Its slot in the layout (0-based, in the spread's order: round the ring from its first
    /// point, along the route from its start).
    pub slot: u32,
    /// Its layer (0: the base).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub layer: u32,
    /// The altitude it flies, m, as the task layer flies a mission's (above the home's ground for
    /// aircraft, negative below the surface for subs); absent on the surface and for a hold or rtl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
    /// `ring`: where it enters the ring, degrees clockwise from north of the centre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_deg: Option<f64>,
    /// `along`, `lanes`: how far along its route it enters, m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_m: Option<f64>,
    /// Its slot's point (a ring's entry point, a route's, a station, a block's cell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<LonLat>,
    /// `ring` on the surface: the radius of the concentric ring it flies, m, where the order's
    /// ring would bring members closer than their lateral minimum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring_m: Option<f64>,
    /// `lanes`: the lane it flies (0: the leftmost, looking along the route).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<u32>,
    /// `along` on a two-point route: how far apart the legs of the racetrack it flies are, m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_m: Option<f64>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A member order's and a group order's own fields on [`crate::IntentSpec`] (flattened there):
/// all absent on any other intent, so every older log re-serializes byte for byte.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Grouping {
    /// A group order: its spec, normalized ([`GroupSpec::normalized`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupSpec>,
    /// A member order: the group order it is a share of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<IntentId>,
    /// A member order: its share.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<Share>,
}

impl Grouping {
    pub fn is_empty(&self) -> bool {
        self.group.is_none() && self.parent.is_none() && self.share.is_none()
    }
}

/// How a member came out of a group's check (docs/SWARMS.md section 4.1), on each of its failing
/// results ([`crate::CheckResult::class`]): such a failure classes the member instead of failing
/// the group, whose `strength` rule then counts what is left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberClass {
    /// A soft reason (its facts too old or not reported yet): its member order waits in the queue
    /// and joins if it passes its checks within the group's wait.
    Waiting,
    /// A hard reason (it can't do the task, spent, lost, propulsion damaged, short of its reserve,
    /// its share on the wrong surface, under the seabed, held by an engagement): no member order.
    Excluded,
}

impl MemberClass {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberClass::Waiting => "waiting",
            MemberClass::Excluded => "excluded",
        }
    }
}

/// A member's class from the group's check: sent (none), waiting or excluded.
pub type Class = Option<MemberClass>;

// ---- the catalog's figures ------------------------------------------------------------------

/// The band a type's layers stay in (docs/SWARMS.md section 3.3; every figure an estimate).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Band {
    /// Above the home's ground, m (a copter: 30 to 120).
    AboveGround { min_m: f64, max_m: f64 },
    /// The order's altitude plus or minus this, m (a small fixed wing: 150); the layers are
    /// centred on the order's altitude.
    AroundOrder { half_m: f64 },
    /// The surface: no layers in height; rings or lanes side by side instead.
    Surface,
    /// Below the surface by at least `top_m`, and above the seabed at the slot by `seabed_m`
    /// (an AUV: 10 and 10); the seabed is the decision's `depth` rule's to judge.
    Depth { top_m: f64, seabed_m: f64 },
}

/// A type's separation and band (docs/SWARMS.md section 3.3): a logged platform fact from rules
/// 34, the catalog's own (`sep_m`, `alt_band_m`, `depth_band_m`) or, where an entry says nothing,
/// the default by autopilot kind ([`Formation::default_for`]). Every figure is an estimate
/// (`guess`), the owner's level for a testbed (section 14, question 2).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Formation {
    /// No two members of one domain closer than this sideways at overlapping heights, m.
    pub lateral_m: f64,
    /// Layers this far apart, m; absent on the surface (no layers in height).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_m: Option<f64>,
    pub band: Band,
    /// A fixed wing's loiter circle radius, m (how far apart its stations and a racetrack's legs
    /// must be: two of it); absent for anything that hovers or stops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loiter_m: Option<f64>,
    /// The altitude it flies a mission at when the order names none, m (the catalog's
    /// `typical_alt_m`, or an AUV's `alt_m`, negative below the surface).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cruise_alt_m: Option<f64>,
    /// The figures are estimates (GUESS in the catalog, docs/SWARMS.md 3.3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub guess: bool,
}

impl Formation {
    /// The default by autopilot kind (docs/SWARMS.md section 3.3's table; every figure a GUESS),
    /// a fixed wing loitering at [`LOITER_DEFAULT_M`].
    pub fn default_for(p: &PlatformFacts) -> Formation {
        let ap = p.autopilot;
        let fixed_wing = ap.is_some_and(AutopilotKind::is_fixed_wing);
        let (lateral_m, vertical_m, band) = match (p.domain, ap) {
            (Domain::Air, _) if fixed_wing => (100.0, Some(30.0), Band::AroundOrder { half_m: 150.0 }),
            (Domain::Air, _) => (15.0, Some(10.0), Band::AboveGround { min_m: 30.0, max_m: 120.0 }),
            (Domain::Subsurface, _) => (50.0, Some(5.0), Band::Depth { top_m: 10.0, seabed_m: 10.0 }),
            (Domain::Surface, _) => (30.0, None, Band::Surface),
            (Domain::Ground, _) => (15.0, None, Band::Surface),
        };
        Formation {
            lateral_m,
            vertical_m,
            band,
            loiter_m: fixed_wing.then_some(LOITER_DEFAULT_M),
            cruise_alt_m: None,
            guess: true,
        }
    }
}

/// A fixed wing's loiter radius where its catalog entry gives none, m: the stand-ins' and SITL's
/// `WP_LOITER_RAD` (`crates/murmur-standins/src/params.rs`, `sim/params/plane.parm`).
pub const LOITER_DEFAULT_M: f64 = 150.0;

/// The catalog's raw figures for one platform (`sep_m`, `alt_band_m`, `depth_band_m`,
/// `loiter_radius_m`, `typical_alt_m` / `alt_m`), as [`crate::Catalog`] reads them.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct RawFormation {
    pub sep_m: Option<RawSep>,
    pub alt_band_m: Option<RawAltBand>,
    pub depth_band_m: Option<[f64; 2]>,
    pub loiter_radius_m: Option<f64>,
    pub cruise_alt_m: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawSep {
    pub lateral: f64,
    #[serde(default)]
    pub vertical: Option<f64>,
    #[serde(default)]
    pub guess: bool,
}

/// `alt_band_m = [min, max]` (above the home's ground) or `alt_band_m = { around = 150 }` (the
/// order's altitude plus or minus).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum RawAltBand {
    Between([f64; 2]),
    Around { around: f64 },
}

impl RawFormation {
    /// Whether the entry says anything of its own.
    pub fn any(&self) -> bool {
        self.sep_m.is_some() || self.alt_band_m.is_some() || self.depth_band_m.is_some()
    }

    /// Why the entry's figures can't be used, if they can't.
    pub fn invalid(&self, domain: Domain) -> Option<String> {
        let pos = |x: f64| x.is_finite() && x > 0.0;
        if let Some(s) = &self.sep_m {
            if !pos(s.lateral) || s.vertical.is_some_and(|v| !pos(v)) {
                return Some("sep_m: lateral (and vertical) above 0 m".into());
            }
            if s.vertical.is_some() && matches!(domain, Domain::Surface | Domain::Ground) {
                return Some("sep_m.vertical: a surface or ground type has no layers in height".into());
            }
        }
        match (&self.alt_band_m, domain) {
            (Some(_), d) if d != Domain::Air => return Some("alt_band_m is an aircraft's".into()),
            (Some(RawAltBand::Between([lo, hi])), _) if !(lo.is_finite() && *lo >= 0.0 && hi > lo) => {
                return Some("alt_band_m = [min, max] above the ground, 0 <= min < max".into());
            }
            (Some(RawAltBand::Around { around }), _) if !pos(*around) => {
                return Some("alt_band_m = { around } above 0 m".into());
            }
            _ => {}
        }
        match (self.depth_band_m, domain) {
            (Some(_), d) if d != Domain::Subsurface => return Some("depth_band_m is a sub's".into()),
            (Some([top, sea]), _) if !(top.is_finite() && top >= 0.0 && sea.is_finite() && sea >= 0.0) => {
                return Some("depth_band_m = [below the surface, above the seabed], each 0 m or more".into());
            }
            _ => {}
        }
        None
    }

    /// The figures the entry gives, over the default by autopilot kind for what it leaves out.
    pub fn resolve(&self, p: &PlatformFacts) -> Formation {
        let mut f = Formation::default_for(p);
        if let Some(s) = &self.sep_m {
            f.lateral_m = s.lateral;
            if s.vertical.is_some() {
                f.vertical_m = s.vertical;
            }
            f.guess = s.guess;
        }
        match self.alt_band_m {
            Some(RawAltBand::Between([lo, hi])) => f.band = Band::AboveGround { min_m: lo, max_m: hi },
            Some(RawAltBand::Around { around }) => f.band = Band::AroundOrder { half_m: around },
            None => {}
        }
        if let Some([top, sea]) = self.depth_band_m {
            f.band = Band::Depth { top_m: top, seabed_m: sea };
        }
        if f.loiter_m.is_some()
            && let Some(r) = self.loiter_radius_m
        {
            f.loiter_m = Some(r);
        }
        f.cruise_alt_m = self.cruise_alt_m;
        f
    }
}

/// A platform's figures as a group order reads them: its logged `formation` from rules 34, else
/// the default by autopilot kind.
pub fn figures(p: &PlatformFacts) -> Formation {
    p.formation.unwrap_or_else(|| Formation::default_for(p))
}

// ---- the layout -------------------------------------------------------------------------------

/// One member as the layout reads it: from its logged facts and its platform's figures.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub uid: Uid,
    /// Its last reported position (fresh or not: the assignment pairs it with the slot nearest
    /// where it was last seen); None: never reported, laid out last.
    pub at: Option<LonLat>,
    /// Its last reported altitude above mean sea level, m (a hold's warnings read it).
    pub alt_m: Option<f64>,
    pub domain: Domain,
    pub fixed_wing: bool,
    /// How tight it turns, m: a fixed wing's loiter radius, else its catalog turn radius.
    pub turn_m: Option<f64>,
    pub figures: Formation,
}

impl Member {
    /// One of ours as the layout reads it, from its logged facts (None: no platform facts).
    pub fn from_facts(uid: &Uid, v: &crate::world::VehicleFacts) -> Option<Member> {
        let p = v.platform.as_ref()?;
        let figures = figures(p);
        let fixed_wing = p.autopilot.is_some_and(AutopilotKind::is_fixed_wing);
        Some(Member {
            uid: uid.clone(),
            at: v.position.as_ref().map(|f| f.value),
            alt_m: v.alt_msl_m.as_ref().map(|f| f.value),
            domain: p.domain,
            fixed_wing,
            turn_m: if fixed_wing { figures.loiter_m } else { p.min_turn_radius_m },
            figures,
        })
    }
}

/// A group's layout: each member's share, and the figures it was laid out on (logged in the
/// group's `check.result`, recomputed by replay).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub spread: Option<Spread>,
    /// The members laid out, by uid.
    pub shares: BTreeMap<Uid, Share>,
    /// Per domain laid out: how many, the even spacing along the path, the layers or rings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<DomainLayout>,
    /// What a person must see though the layout holds (overlapping fixed-wing circles in a hold).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Why the members do not fit (more than its layers or rings hold), with the most that fit:
    /// the `separation` rule refuses the group with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// One domain's part of a layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DomainLayout {
    pub domain: Domain,
    pub members: u32,
    /// The even spacing between neighbours along the path, m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacing_m: Option<f64>,
    /// Layers in height (1: one layer).
    pub layers: u32,
    /// On the surface: the rings' radii (concentric rings instead of layers), m.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rings_m: Vec<f64>,
    /// The lateral and vertical minima applied (the largest of its kinds').
    pub lateral_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vertical_m: Option<f64>,
    /// `along` on a two-point route: how far apart the racetrack's legs are, m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub racetrack_m: Option<f64>,
    /// The path's length, m (a ring's circumference, a route's loop or line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_m: Option<f64>,
}

/// Lay out `members` (the members a group's check lays out) over `task` as `group` spreads it:
/// the slots first, per domain (air, surface and subsurface never share a layer), then each
/// member paired with a slot by minimum total distance (integer millimetres, ties by uid), its
/// layer and altitude within its type's band (docs/SWARMS.md section 3). A pure function of its
/// arguments: the engine and replay call it on the same logged facts and get the same layout,
/// whatever order the members come in.
pub fn layout(task: &Task, group: &GroupSpec, members: &[Member]) -> Layout {
    let spread = group.spread.or_else(|| Spread::default_for(task.kind()));
    let mut out = Layout {
        spread,
        ..Layout::default()
    };
    let Some(spread) = spread.filter(|s| s.fits(task.kind())) else {
        out.refused = Some(format!("no spread lays out a group {}", task.kind().as_str()));
        return out;
    };
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_by(|a, b| a.uid.cmp(&b.uid));
    sorted.dedup_by(|a, b| a.uid == b.uid);
    if !spread.laid_out() {
        // a hold or an rtl: each where it is, or home; never refused for its layout
        for (i, m) in sorted.iter().enumerate() {
            out.shares.insert(
                m.uid.clone(),
                Share {
                    slot: i as u32,
                    at: if spread == Spread::Here { m.at } else { None },
                    ..Share::default()
                },
            );
        }
        if spread == Spread::Here {
            out.warnings = hold_warnings(&sorted);
        }
        return out;
    }
    let mut by_domain: BTreeMap<Domain, Vec<&Member>> = BTreeMap::new();
    for m in sorted {
        by_domain.entry(m.domain).or_default().push(m);
    }
    for (domain, ms) in by_domain {
        match lay_domain(task, group, spread, domain, &ms) {
            Ok((d, shares)) => {
                out.domains.push(d);
                out.shares.extend(shares);
            }
            Err(why) => {
                out.refused = Some(why);
                out.shares.clear();
                return out;
            }
        }
    }
    if let Some(why) = separation(&out, members) {
        out.refused = Some(why);
        out.shares.clear();
    }
    out
}

/// A member order's task: the group's, with the member's own geometry (its ring's radius on the
/// surface, its lane, the racetrack a two-point route becomes); its altitude and where it enters
/// are its share's. None when the layout has no share for `uid`.
pub fn member_task(task: &Task, group: &GroupSpec, lay: &Layout, uid: &Uid) -> Option<Task> {
    let share = lay.shares.get(uid)?;
    Some(match (task, lay.spread) {
        (Task::Orbit { center, radius_m }, _) => Task::Orbit {
            center: *center,
            radius_m: share.ring_m.unwrap_or(*radius_m),
        },
        (Task::Patrol { route }, Some(Spread::Lanes)) => {
            let m = group.lanes.unwrap_or(1);
            let spacing = group.lane_spacing_m.unwrap_or(0.0);
            let j = share.lane.unwrap_or(0);
            Task::Patrol {
                route: offset_route(route, lane_offset(j, m, spacing)),
            }
        }
        (Task::Patrol { route }, Some(Spread::Along)) if route.len() == 2 => Task::Patrol {
            route: racetrack(route[0], route[1], share.width_m?),
        },
        // a station: straight there (it enters the route at its own point, as a patrol's member
        // enters at its own; the pairing keeps the members' ways from crossing), held there
        (Task::Transit { route }, Some(Spread::Stations)) => Task::Transit {
            route: vec![station(route, share.entry_m?)],
        },
        // a block's cell: the route moved as the cell sits from its end, so the block keeps its
        // shape along the way and no two members pass through one point, held in its cell
        (Task::Transit { route }, Some(Spread::Block)) => {
            let (Some(end), Some(cell)) = (route.last(), share.at) else { return None };
            let (dlon, dlat) = (cell.lon - end.lon, cell.lat - end.lat);
            let mut r: Vec<LonLat> = route.iter().map(|p| round_ll(LonLat::new(p.lon + dlon, p.lat + dlat))).collect();
            if let Some(last) = r.last_mut() {
                *last = cell;
            }
            Task::Transit { route: r }
        }
        (t, _) => t.clone(),
    })
}

/// A station `d` metres along `route` (open), as the layout placed it: the route's own point when
/// it falls on one, else the point there at MAVLink's resolution.
pub fn station(route: &[LonLat], d: f64) -> LonLat {
    route_to(route, d).last().copied().unwrap_or(LonLat::new(0.0, 0.0))
}

/// `route` (open) as far as `d` metres along it: its points before there, then the point there
/// (at MAVLink's resolution; the route's own point when it falls on one).
pub fn route_to(route: &[LonLat], d: f64) -> Vec<LonLat> {
    let Some(first) = route.first().copied() else {
        return Vec::new();
    };
    let plane = Plane::new(first);
    let pts: Vec<(f64, f64)> = route.iter().map(|p| plane.xy(*p)).collect();
    let mut out = vec![first];
    let mut left = d.max(0.0);
    for (i, w) in pts.windows(2).enumerate() {
        let l = dist(w[0], w[1]);
        if left <= l + 0.05 {
            let at = if l - left <= 0.05 {
                route[i + 1]
            } else {
                let f = if l > 0.0 { left / l } else { 0.0 };
                round_ll(plane.ll((w[0].0 + (w[1].0 - w[0].0) * f, w[0].1 + (w[1].1 - w[0].1) * f)))
            };
            if out.last() != Some(&at) {
                out.push(at);
            }
            return out;
        }
        left -= l;
        out.push(route[i + 1]);
    }
    out
}

// ---- geometry on a local plane -------------------------------------------------------------

/// Metres per degree of latitude (as `crate::geo`'s circles).
const M_PER_DEG: f64 = 111_320.0;

/// Points on each half circle of a racetrack's ends.
const ARC_POINTS: usize = 8;

#[derive(Clone, Copy, Debug)]
struct Plane {
    origin: LonLat,
    cos: f64,
}

impl Plane {
    fn new(origin: LonLat) -> Plane {
        Plane {
            origin,
            cos: origin.lat.to_radians().cos().max(1e-6),
        }
    }
    fn xy(&self, p: LonLat) -> (f64, f64) {
        (
            (p.lon - self.origin.lon) * M_PER_DEG * self.cos,
            (p.lat - self.origin.lat) * M_PER_DEG,
        )
    }
    fn ll(&self, (x, y): (f64, f64)) -> LonLat {
        LonLat::new(self.origin.lon + x / (M_PER_DEG * self.cos), self.origin.lat + y / M_PER_DEG)
    }
}

fn dist((ax, ay): (f64, f64), (bx, by): (f64, f64)) -> f64 {
    (ax - bx).hypot(ay - by)
}

/// A polyline on the plane, closed (back to its first point) or open.
#[derive(Clone, Debug)]
struct Path {
    pts: Vec<(f64, f64)>,
    closed: bool,
}

impl Path {
    fn legs(&self) -> Vec<((f64, f64), (f64, f64))> {
        let n = self.pts.len();
        let mut v: Vec<_> = self.pts.windows(2).map(|w| (w[0], w[1])).collect();
        if self.closed && n > 2 {
            v.push((self.pts[n - 1], self.pts[0]));
        } else if self.closed && n == 2 {
            v.push((self.pts[1], self.pts[0]));
        }
        v
    }
    fn length(&self) -> f64 {
        self.legs().iter().map(|(a, b)| dist(*a, *b)).sum()
    }
    /// The point `d` metres along (wrapped round a closed path, clamped on an open one).
    fn at(&self, d: f64) -> (f64, f64) {
        let len = self.length();
        let mut d = if self.closed && len > 0.0 { d.rem_euclid(len) } else { d.clamp(0.0, len) };
        for (a, b) in self.legs() {
            let l = dist(a, b);
            if d <= l && l > 0.0 {
                let f = d / l;
                return (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f);
            }
            d -= l;
        }
        self.pts.last().copied().unwrap_or((0.0, 0.0))
    }
}

/// A two-point route as a racetrack whose legs are `width_m` apart: out along one side of the
/// line, round the far end, back along the other, round the near end (a closed loop, so members
/// going opposite ways never meet head on).
pub fn racetrack(a: LonLat, b: LonLat, width_m: f64) -> Vec<LonLat> {
    let plane = Plane::new(a);
    let (pa, pb) = (plane.xy(a), plane.xy(b));
    let len = dist(pa, pb).max(1e-9);
    let u = ((pb.0 - pa.0) / len, (pb.1 - pa.1) / len);
    let n = (-u.1, u.0);
    let r = width_m / 2.0;
    let mut pts = Vec::new();
    // the out leg on the right of the line (looking from a to b)
    pts.push((pa.0 - n.0 * r, pa.1 - n.1 * r));
    pts.push((pb.0 - n.0 * r, pb.1 - n.1 * r));
    // round b, from its right through beyond it to its left
    for k in 1..ARC_POINTS {
        let t = std::f64::consts::PI * k as f64 / ARC_POINTS as f64;
        let (c, s) = (t.cos(), t.sin());
        pts.push((pb.0 - n.0 * r * c + u.0 * r * s, pb.1 - n.1 * r * c + u.1 * r * s));
    }
    pts.push((pb.0 + n.0 * r, pb.1 + n.1 * r));
    pts.push((pa.0 + n.0 * r, pa.1 + n.1 * r));
    // round a, from its left through behind it to its right
    for k in 1..ARC_POINTS {
        let t = std::f64::consts::PI * k as f64 / ARC_POINTS as f64;
        let (c, s) = (t.cos(), t.sin());
        pts.push((pa.0 + n.0 * r * c - u.0 * r * s, pa.1 + n.1 * r * c - u.1 * r * s));
    }
    pts.into_iter().map(|p| round_ll(plane.ll(p))).collect()
}

/// A member's route as it flies its share of a patrol: the closed loop of `route` (its task's,
/// the lane or the racetrack it was given) entered `entry_m` along it, the point there first, then
/// the route's points from the next one on, round to the one before it. The task layer flies this
/// (rules 34: no slot arithmetic of its own), the same distance along the same loop the layout
/// measured.
pub fn entered_route(route: &[LonLat], entry_m: f64) -> Vec<LonLat> {
    let Some(first) = route.first().copied() else {
        return Vec::new();
    };
    let n = route.len();
    if n < 2 || !(entry_m.is_finite() && entry_m > 0.0) {
        return route.to_vec();
    }
    let plane = Plane::new(first);
    let path = Path {
        pts: route.iter().map(|p| plane.xy(*p)).collect(),
        closed: true,
    };
    let len = path.length();
    if len <= 0.0 {
        return route.to_vec();
    }
    let mut d = entry_m.rem_euclid(len);
    // the leg it enters on: from point i to point i + 1 (round to the first)
    let mut leg = 0;
    for (i, (a, b)) in path.legs().iter().enumerate() {
        let l = dist(*a, *b);
        if d < l {
            leg = i;
            break;
        }
        d -= l;
        leg = i + 1;
    }
    let leg = leg % n.max(1);
    let at = round_ll(plane.ll(path.at(entry_m)));
    let mut out = vec![at];
    for k in 1..=n {
        let p = route[(leg + k) % n];
        if k == n && p == at {
            break;
        }
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

/// A point at MAVLink's resolution (1e-7 degree), so a route's waypoints read back the same.
fn round_ll(p: LonLat) -> LonLat {
    LonLat::new((p.lon * 1e7).round() / 1e7, (p.lat * 1e7).round() / 1e7)
}

/// Lane `j` of `m`'s sideways offset, m (lanes centred on the route).
fn lane_offset(j: u32, m: u32, spacing_m: f64) -> f64 {
    (f64::from(j) - f64::from(m.saturating_sub(1)) / 2.0) * spacing_m
}

/// `route` moved `off` metres sideways (to the left, looking from its first point to its last).
pub fn offset_route(route: &[LonLat], off: f64) -> Vec<LonLat> {
    let (Some(first), Some(last)) = (route.first(), route.last()) else {
        return route.to_vec();
    };
    let plane = Plane::new(*first);
    let (a, b) = (plane.xy(*first), plane.xy(*last));
    let len = dist(a, b);
    let n = if len > 0.0 { (-(b.1 - a.1) / len, (b.0 - a.0) / len) } else { (0.0, 1.0) };
    route
        .iter()
        .map(|p| {
            let (x, y) = plane.xy(*p);
            round_ll(plane.ll((x + n.0 * off, y + n.1 * off)))
        })
        .collect()
}

// ---- one domain ------------------------------------------------------------------------------

/// A slot: where it is, and what a member there flies.
struct Slot {
    xy: (f64, f64),
    phase_deg: Option<f64>,
    entry_m: Option<f64>,
    ring_m: Option<f64>,
    lane: Option<u32>,
    layer: u32,
}

type DomainShares = (DomainLayout, BTreeMap<Uid, Share>);

fn lay_domain(task: &Task, group: &GroupSpec, spread: Spread, domain: Domain, ms: &[&Member]) -> Result<DomainShares, String> {
    let n = ms.len();
    let lateral = ms.iter().map(|m| m.figures.lateral_m).fold(0.0, f64::max);
    let vertical = ms.iter().filter_map(|m| m.figures.vertical_m).reduce(f64::max);
    let layered = vertical.is_some() && matches!(domain, Domain::Air | Domain::Subsurface);
    let name = domain.as_str();
    // the most layers every member's band allows
    let max_layers = if layered { max_layers(group.alt_m, ms, vertical.unwrap_or(0.0)) } else { 1 };
    if layered && max_layers == 0 {
        return Err(format!(
            "the {name} members' layer base is outside their band (alt_m {}): no layer fits",
            group.alt_m.map_or("their cruise altitude".into(), |a| format!("{a:.0} m"))
        ));
    }
    // `k_need` neighbours in a row must be in different layers (1: all in one)
    let layers_for = |k_need: u32, need: f64, capacity_per_layer: usize, what: &str, closed: bool| -> Result<u32, String> {
        if k_need <= 1 {
            return Ok(1);
        }
        if !layered {
            return Err(format!(
                "{what} holds {capacity_per_layer} {name} members at least {need:.0} m apart; {n} do not fit"
            ));
        }
        let k = k_need.min(n as u32);
        // round a closed path the wrap from the last slot to the first needs one more where k
        // does not divide n
        let k = if closed { layer_pattern(n, k).0 } else { k };
        if k > max_layers {
            let alts = layer_alts(group.alt_m, ms, max_layers, vertical.unwrap_or(0.0));
            return Err(format!(
                "{what} holds {} {name} members in {max_layers} layer(s) ({alts}) at least {need:.0} m apart; {n} do not fit",
                capacity_per_layer * max_layers as usize
            ));
        }
        Ok(k)
    };
    let mut d = DomainLayout {
        domain,
        members: n as u32,
        spacing_m: None,
        layers: 1,
        rings_m: Vec::new(),
        lateral_m: lateral,
        vertical_m: if layered { vertical } else { None },
        racetrack_m: None,
        path_m: None,
    };
    let (plane, slots) = match (task, spread) {
        (Task::Orbit { center, radius_m }, Spread::Ring) => {
            let plane = Plane::new(*center);
            let r = *radius_m;
            let circ = std::f64::consts::TAU * r;
            let s = circ / n as f64;
            d.spacing_m = Some(round1(s));
            d.path_m = Some(round1(circ));
            let surface = !layered && s + 1e-9 < lateral;
            let mut slots = Vec::with_capacity(n);
            if surface {
                // concentric rings two lateral minima apart, centred on the ordered radius, within
                // a quarter of it; neighbours on the ring in different rings (the wrap round from
                // the last slot to the first too: [`layer_pattern`])
                let off = |j: u32, m: u32| lateral * (2.0 * f64::from(j) - f64::from(m - 1));
                let fits = |m: u32| (0..m).all(|j| off(j, m).abs() <= 0.25 * r + 1e-9 && r + off(j, m) > 0.0);
                // members of one ring are at least `m0` slots apart: on the innermost ring too
                // their chord must be the lateral minimum, so `m0` grows until it is (or the
                // rings no longer fit)
                let mut m0 = ((lateral / s) - 1e-9).ceil().max(1.0) as u32;
                let (m, pattern) = loop {
                    let (m, pattern) = layer_pattern(n, m0.min(n as u32));
                    let inner = r + off(0, m);
                    let chord = 2.0 * inner * (std::f64::consts::PI * f64::from(m0) / n as f64).min(std::f64::consts::FRAC_PI_2).sin();
                    if chord + 1e-6 >= lateral || !fits(m) || m0 as usize >= n {
                        break (m, pattern);
                    }
                    m0 += 1;
                };
                if !fits(m) {
                    let mut m_max = 1;
                    while fits(m_max + 1) {
                        m_max += 1;
                    }
                    let cap: usize = (0..m_max).map(|j| (std::f64::consts::TAU * (r + off(j, m_max)) / lateral).floor() as usize).sum();
                    let radii: Vec<String> = (0..m_max).map(|j| format!("{:.0}", r + off(j, m_max))).collect();
                    return Err(format!(
                        "a {r:.0} m ring holds {cap} {name} members on {m_max} ring(s), at {} m; {n} do not fit",
                        radii.join(", ")
                    ));
                }
                d.rings_m = (0..m).map(|j| round1(r + off(j, m))).collect();
                for (i, j) in pattern.iter().copied().enumerate() {
                    let rr = r + off(j, m);
                    let phase = 360.0 * i as f64 / n as f64;
                    slots.push(Slot {
                        xy: on_circle(rr, phase),
                        phase_deg: Some(round3(phase)),
                        entry_m: None,
                        ring_m: (m > 1).then_some(round1(rr)),
                        lane: None,
                        layer: 0,
                    });
                }
            } else {
                // members `j` slots apart are a chord apart, shorter than the arc on a small ring
                let chord = |j: usize| 2.0 * r * (std::f64::consts::PI * j as f64 / n as f64).min(std::f64::consts::FRAC_PI_2).sin();
                let k_need = (1..=n).find(|j| chord(*j) + 1e-6 >= lateral).unwrap_or(n) as u32;
                let per_layer = if lateral <= 2.0 * r { (std::f64::consts::PI / (lateral / (2.0 * r)).asin()).floor() as usize } else { 1 };
                let k = layers_for(k_need, lateral, per_layer, &format!("a {r:.0} m ring"), true)?;
                let pattern = closed_layers(n, k_need);
                d.layers = k;
                for (i, layer) in pattern.iter().copied().enumerate() {
                    let phase = 360.0 * i as f64 / n as f64;
                    slots.push(Slot {
                        xy: on_circle(r, phase),
                        phase_deg: Some(round3(phase)),
                        entry_m: None,
                        ring_m: None,
                        lane: None,
                        layer,
                    });
                }
            }
            (plane, slots)
        }
        (Task::Patrol { route }, Spread::Along) => {
            let first = route.first().copied().unwrap_or(LonLat::new(0.0, 0.0));
            let plane = Plane::new(first);
            let pts: Vec<LonLat> = if route.len() == 2 {
                let turn = ms.iter().filter_map(|m| m.turn_m).fold(0.0, f64::max);
                let w = (2.0 * turn).max(lateral);
                d.racetrack_m = Some(round1(w));
                racetrack(route[0], route[1], w)
            } else {
                route.clone()
            };
            let path = Path {
                pts: pts.iter().map(|p| plane.xy(*p)).collect(),
                closed: true,
            };
            let len = path.length();
            let s = len / n as f64;
            d.spacing_m = Some(round1(s));
            d.path_m = Some(round1(len));
            let xy: Vec<(f64, f64)> = (0..n).map(|i| path.at(len * i as f64 / n as f64)).collect();
            let k_need = apart_in_layers(&xy, need_in_a_row(s, lateral), lateral);
            let k = layers_for(k_need, lateral, (len / lateral).floor() as usize, &format!("a {:.0} m route", len), true)?;
            let pattern = closed_layers(n, k_need);
            d.layers = k;
            let slots = (0..n)
                .map(|i| {
                    let e = len * i as f64 / n as f64;
                    Slot {
                        xy: xy[i],
                        phase_deg: None,
                        entry_m: Some(round1(e)),
                        ring_m: None,
                        lane: None,
                        layer: pattern[i],
                    }
                })
                .collect();
            (plane, slots)
        }
        (Task::Patrol { route }, Spread::Lanes) => {
            let m = group.lanes.unwrap_or(1).max(1);
            let spacing = group.lane_spacing_m.unwrap_or(0.0);
            if m > 1 && spacing + 1e-9 < lateral {
                return Err(format!("lanes {spacing:.0} m apart are closer than the {name} members' {lateral:.0} m"));
            }
            let first = route.first().copied().unwrap_or(LonLat::new(0.0, 0.0));
            let plane = Plane::new(first);
            let mut slots = Vec::with_capacity(n);
            let mut k_all = 1;
            for j in 0..m {
                let lane_n = (0..n).filter(|i| (*i as u32) % m == j).count();
                if lane_n == 0 {
                    continue;
                }
                let lane = offset_route(route, lane_offset(j, m, spacing));
                let path = Path {
                    pts: lane.iter().map(|p| plane.xy(*p)).collect(),
                    closed: true,
                };
                let len = path.length();
                let s = len / lane_n as f64;
                let xy: Vec<(f64, f64)> = (0..lane_n).map(|q| path.at(len * q as f64 / lane_n as f64)).collect();
                let k_need = apart_in_layers(&xy, need_in_a_row(s, lateral), lateral);
                let k = layers_for(k_need, lateral, (len / lateral).floor() as usize, &format!("lane {} ({len:.0} m)", j + 1), true)?;
                let pattern = closed_layers(lane_n, k_need);
                k_all = k_all.max(k);
                d.spacing_m = Some(round1(s));
                d.path_m = Some(round1(len));
                for (q, _) in (0..n).filter(|i| (*i as u32) % m == j).enumerate() {
                    let e = len * q as f64 / lane_n as f64;
                    slots.push((j as usize + q * m as usize, Slot {
                        xy: path.at(e),
                        phase_deg: None,
                        entry_m: Some(round1(e)),
                        ring_m: None,
                        lane: Some(j),
                        layer: pattern[q],
                    }));
                }
            }
            d.layers = k_all;
            slots.sort_by_key(|(i, _)| *i);
            (plane, slots.into_iter().map(|(_, s)| s).collect())
        }
        (Task::Transit { route }, Spread::Stations) => {
            let first = route.first().copied().unwrap_or(LonLat::new(0.0, 0.0));
            let plane = Plane::new(first);
            let path = Path {
                pts: route.iter().map(|p| plane.xy(*p)).collect(),
                closed: false,
            };
            let len = path.length();
            if n > 1 && len < lateral {
                return Err(format!(
                    "stations need a route longer than the {name} members' {lateral:.0} m (this one is {len:.0} m): a transit to one point spreads its members as a block"
                ));
            }
            let s = if n > 1 { len / (n - 1) as f64 } else { f64::INFINITY };
            // a station holds by its type: a copter in place, a fixed wing on its loiter circle
            let need = ms
                .iter()
                .map(|m| if m.fixed_wing { (2.0 * m.figures.loiter_m.unwrap_or(LOITER_DEFAULT_M)).max(lateral) } else { lateral })
                .fold(lateral, f64::max);
            d.spacing_m = Some(round1(s.min(len)));
            d.path_m = Some(round1(len));
            let k = layers_for(need_in_a_row(s, need), need, (len / need).floor() as usize + 1, &format!("a {len:.0} m route"), false)?;
            d.layers = k;
            let slots = (0..n)
                .map(|i| {
                    // one member of a domain holds at the route's end, as a transit does
                    let e = if n > 1 { len * i as f64 / (n - 1) as f64 } else { len };
                    Slot {
                        xy: path.at(e),
                        phase_deg: None,
                        entry_m: Some(round1(e)),
                        ring_m: None,
                        lane: None,
                        layer: (i as u32) % k,
                    }
                })
                .collect();
            (plane, slots)
        }
        (Task::Transit { route }, Spread::Block) => {
            let end = route.last().copied().unwrap_or(LonLat::new(0.0, 0.0));
            let plane = Plane::new(end);
            let g = ms
                .iter()
                .map(|m| if m.fixed_wing { (2.0 * m.figures.loiter_m.unwrap_or(LOITER_DEFAULT_M)).max(lateral) } else { lateral })
                .fold(lateral, f64::max);
            let c = (n as f64).sqrt().ceil().max(1.0) as usize;
            let rows = n.div_ceil(c);
            d.spacing_m = Some(round1(g));
            let slots = (0..n)
                .map(|i| {
                    let (col, row) = (i % c, i / c);
                    let x = (col as f64 - (c as f64 - 1.0) / 2.0) * g;
                    let y = ((rows as f64 - 1.0) / 2.0 - row as f64) * g;
                    Slot {
                        xy: (x, y),
                        phase_deg: None,
                        entry_m: None,
                        ring_m: None,
                        lane: None,
                        layer: 0,
                    }
                })
                .collect();
            (plane, slots)
        }
        (t, s) => return Err(format!("{} does not lay out a {}", s.as_str(), t.kind().as_str())),
    };
    // each member with a slot by minimum total distance (ties by uid: the members come sorted)
    let cost: Vec<Vec<i64>> = ms
        .iter()
        .map(|m| {
            slots
                .iter()
                .map(|sl| m.at.map_or(0, |p| (dist(plane.xy(p), sl.xy) * 1000.0).round() as i64))
                .collect()
        })
        .collect();
    let pick = assign(&cost);
    let mut shares = BTreeMap::new();
    for (mi, m) in ms.iter().enumerate() {
        let si = pick[mi];
        let sl = &slots[si];
        let alt = alt_for(m, group.alt_m, sl.layer, d.layers, d.vertical_m.unwrap_or(0.0));
        let alt = match alt {
            Ok(a) => a,
            Err(why) => return Err(format!("{}: {why}", m.uid)),
        };
        shares.insert(
            m.uid.clone(),
            Share {
                slot: si as u32,
                layer: sl.layer,
                alt_m: alt.map(round1),
                phase_deg: sl.phase_deg,
                entry_m: sl.entry_m,
                at: Some(round_ll(plane.ll(sl.xy))),
                ring_m: sl.ring_m,
                lane: sl.lane,
                width_m: d.racetrack_m,
            },
        );
    }
    Ok((d, shares))
}

/// How many neighbours in a row, `s` apart along a path, must be in different layers to keep
/// `need` apart in each (1: all in one).
fn need_in_a_row(s: f64, need: f64) -> u32 {
    if s + 1e-9 >= need {
        1
    } else {
        ((need / s) - 1e-9).ceil().max(1.0) as u32
    }
}

/// The fewest neighbours in a row (from `k_need` up) that, put in different layers round a closed
/// path ([`closed_layers`]), leave every two slots of one layer at least `lateral` apart in a
/// straight line (round a racetrack's ends the line is shorter than the way along it).
fn apart_in_layers(xy: &[(f64, f64)], k_need: u32, lateral: f64) -> u32 {
    let n = xy.len();
    let mut k = k_need.max(1);
    while (k as usize) < n {
        let pattern = closed_layers(n, k);
        let apart = (0..n).all(|i| (i + 1..n).all(|j| pattern[i] != pattern[j] || dist(xy[i], xy[j]) + 1e-6 >= lateral));
        if apart {
            break;
        }
        k += 1;
    }
    k
}

/// [`layer_pattern`]'s layers for `n` slots round a closed path, `k_need` in a row in different
/// layers (all in one when 1).
fn closed_layers(n: usize, k_need: u32) -> Vec<u32> {
    if k_need <= 1 {
        return vec![0; n];
    }
    layer_pattern(n, k_need.min(n as u32)).1
}

/// The layer of each of `n` slots round a closed path, where any `k` neighbours in a row must be
/// in different layers: `i mod k` when `k` divides `n`; else blocks of `k` and `k + 1` slots (the
/// `k + 1` ones using one more layer), so the wrap round from the last slot to the first keeps
/// them apart too; else the smallest number of layers that divides `n`. Returns the layers used.
fn layer_pattern(n: usize, k: u32) -> (u32, Vec<u32>) {
    let k = k.max(1) as usize;
    if k == 1 || n.is_multiple_of(k) {
        return (k as u32, (0..n).map(|i| (i % k) as u32).collect());
    }
    let (q, r) = (n / k, n % k);
    if q >= r {
        let mut v = Vec::with_capacity(n);
        for b in 0..q {
            let size = if b < r { k + 1 } else { k };
            v.extend((0..size).map(|j| j as u32));
        }
        return ((k + 1) as u32, v);
    }
    let k2 = (k + 1..=n).find(|c| n.is_multiple_of(*c)).unwrap_or(n);
    (k2 as u32, (0..n).map(|i| (i % k2) as u32).collect())
}

/// The point `phase` degrees clockwise from north on a circle of `r` about the plane's origin.
fn on_circle(r: f64, phase_deg: f64) -> (f64, f64) {
    let a = phase_deg.to_radians();
    (r * a.sin(), r * a.cos())
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// A member's altitude on `layer` of `k`, `v` m apart, as the task layer flies it (negative below
/// the surface), or why it is outside its band. None on the surface.
fn alt_for(m: &Member, alt_m: Option<f64>, layer: u32, k: u32, v: f64) -> Result<Option<f64>, String> {
    let f = &m.figures;
    let l = f64::from(layer);
    match f.band {
        Band::Surface => Ok(None),
        Band::AboveGround { min_m, max_m } => {
            let base = alt_m.or(f.cruise_alt_m.filter(|c| *c > 0.0)).unwrap_or(min_m);
            let a = base + l * v;
            if a + 1e-6 < min_m || a - 1e-6 > max_m {
                return Err(format!("{a:.0} m is outside its band, {min_m:.0} to {max_m:.0} m above the ground"));
            }
            Ok(Some(a))
        }
        Band::AroundOrder { half_m } => {
            let base = alt_m.or(f.cruise_alt_m.filter(|c| *c > 0.0)).unwrap_or(half_m);
            let a = base + (l - f64::from(k.saturating_sub(1)) / 2.0) * v;
            if (a - base).abs() - 1e-6 > half_m {
                return Err(format!("{a:.0} m is outside its band, {base:.0} m plus or minus {half_m:.0}"));
            }
            Ok(Some(a))
        }
        Band::Depth { top_m, .. } => {
            let base = alt_m.or(f.cruise_alt_m.filter(|c| *c < 0.0).map(|c| -c)).unwrap_or(top_m);
            let depth = base + l * v;
            if depth + 1e-6 < top_m {
                return Err(format!("{depth:.0} m deep is above its band, {top_m:.0} m below the surface"));
            }
            Ok(Some(-depth))
        }
    }
}

/// The most layers, `v` m apart, in which every member stays inside its band.
fn max_layers(alt_m: Option<f64>, ms: &[&Member], v: f64) -> u32 {
    const CAP: u32 = 64;
    let mut best = 0;
    for k in 1..=CAP {
        let ok = ms.iter().all(|m| (0..k).all(|l| alt_for(m, alt_m, l, k, v).is_ok()));
        if ok {
            best = k;
        } else if best > 0 {
            break;
        }
    }
    best
}

/// The layers' altitudes in words (the first member's kind), for a refusal.
fn layer_alts(alt_m: Option<f64>, ms: &[&Member], k: u32, v: f64) -> String {
    let Some(m) = ms.first() else { return String::new() };
    let alts: Vec<String> = (0..k)
        .filter_map(|l| alt_for(m, alt_m, l, k, v).ok().flatten())
        .map(|a| if a < 0.0 { format!("{:.0} m deep", -a) } else { format!("{a:.0} m") })
        .collect();
    alts.join(", ")
}

/// The minimum-total-cost pairing of rows (members) with columns (slots) of a square matrix
/// (the Hungarian method with potentials, O(n^3) on integer costs: exact, and the same answer
/// for the same matrix). Returns each row's column.
fn assign(cost: &[Vec<i64>]) -> Vec<usize> {
    let n = cost.len();
    if n == 0 {
        return Vec::new();
    }
    const INF: i64 = i64::MAX / 4;
    let mut u = vec![0i64; n + 1];
    let mut v = vec![0i64; n + 1];
    let mut p = vec![0usize; n + 1];
    let mut way = vec![0usize; n + 1];
    for i in 1..=n {
        p[0] = i;
        let mut j0 = 0;
        let mut minv = vec![INF; n + 1];
        let mut used = vec![false; n + 1];
        loop {
            used[j0] = true;
            let i0 = p[j0];
            let mut delta = INF;
            let mut j1 = 0;
            for j in 1..=n {
                if !used[j] {
                    let cur = cost[i0 - 1].get(j - 1).copied().unwrap_or(INF) - u[i0] - v[j];
                    if cur < minv[j] {
                        minv[j] = cur;
                        way[j] = j0;
                    }
                    if minv[j] < delta {
                        delta = minv[j];
                        j1 = j;
                    }
                }
            }
            for j in 0..=n {
                if used[j] {
                    u[p[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if p[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            p[j0] = p[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut out = vec![0; n];
    for j in 1..=n {
        if p[j] > 0 {
            out[p[j] - 1] = j - 1;
        }
    }
    out
}

/// The separation rule's own judgement of a layout: no two members of one domain within both
/// minima at their slots (laterally closer than the domain's lateral minimum, at heights less
/// than its vertical minimum apart). None when they are all apart.
pub fn separation(lay: &Layout, members: &[Member]) -> Option<String> {
    let domain_of: BTreeMap<&Uid, Domain> = members.iter().map(|m| (&m.uid, m.domain)).collect();
    for d in &lay.domains {
        let ours: Vec<(&Uid, &Share)> = lay.shares.iter().filter(|(u, _)| domain_of.get(u) == Some(&d.domain)).collect();
        let Some(origin) = ours.iter().find_map(|(_, s)| s.at) else { continue };
        let plane = Plane::new(origin);
        let pts: Vec<(&Uid, (f64, f64), Option<f64>)> = ours
            .iter()
            .filter_map(|(u, s)| Some((*u, plane.xy(s.at?), s.alt_m)))
            .collect();
        for (i, a) in pts.iter().enumerate() {
            for b in &pts[i + 1..] {
                let apart_v = match (a.2, b.2, d.vertical_m) {
                    (Some(x), Some(y), Some(v)) => (x - y).abs() + 1e-6 >= v,
                    _ => false,
                };
                let lat = dist(a.1, b.1);
                if !apart_v && lat + 0.05 < d.lateral_m {
                    return Some(format!(
                        "{} and {} would be {lat:.0} m apart at one height (the {} minimum is {:.0} m)",
                        a.0,
                        b.0,
                        d.domain.as_str(),
                        d.lateral_m
                    ));
                }
            }
        }
    }
    None
}

/// A hold's warnings: fixed wings whose loiter circles overlap at one height (each holds where it
/// is, by its type; never a refusal).
fn hold_warnings(ms: &[&Member]) -> Vec<String> {
    let wings: Vec<&&Member> = ms.iter().filter(|m| m.fixed_wing && m.at.is_some()).collect();
    let Some(origin) = wings.first().and_then(|m| m.at) else { return Vec::new() };
    let plane = Plane::new(origin);
    let mut pairs = 0;
    let mut first: Option<String> = None;
    for (i, a) in wings.iter().enumerate() {
        for b in &wings[i + 1..] {
            let (Some(pa), Some(pb)) = (a.at, b.at) else { continue };
            let r = a.figures.loiter_m.unwrap_or(LOITER_DEFAULT_M) + b.figures.loiter_m.unwrap_or(LOITER_DEFAULT_M);
            let v = a.figures.vertical_m.unwrap_or(0.0).max(b.figures.vertical_m.unwrap_or(0.0));
            let one_height = match (a.alt_m, b.alt_m) {
                (Some(x), Some(y)) => (x - y).abs() < v,
                _ => true,
            };
            let d = dist(plane.xy(pa), plane.xy(pb));
            if one_height && d < r {
                pairs += 1;
                first.get_or_insert_with(|| format!("{} and {}, {d:.0} m apart", a.uid, b.uid));
            }
        }
    }
    match first {
        None => Vec::new(),
        Some(eg) => vec![format!(
            "{pairs} pair(s) of fixed wings hold on loiter circles that overlap at one height (first: {eg}); a hold holds each where it is"
        )],
    }
}

// ---- re-forming ------------------------------------------------------------------------------

/// A re-form's layout (docs/SWARMS.md section 4.3): the members on their shares and those back,
/// spread back over the same task, moving as few as it can.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reform {
    pub layout: Layout,
    /// The members given a new share (each back member, and those whose slot moved more than half
    /// the even spacing).
    pub moved: BTreeSet<Uid>,
    /// The largest gap between neighbours along the path now, and the even spacing, m.
    pub largest_gap_m: f64,
    pub even_spacing_m: f64,
}

/// How much larger than the even spacing the largest gap may stay (section 4.3, a GUESS).
pub const GAP_FACTOR: f64 = 1.5;

/// A gap must be under [`GAP_FACTOR`] times the spacing by at least this, m (a gap exactly at the
/// bound, as rounding leaves one, is not under it).
const GAP_MARGIN_M: f64 = 0.01;

/// Lay out a re-form of `prev` (a ring or a route of one domain): `members` are those on their
/// shares (with their share in `prev`) and those back (without one); `pinned` keep their slot
/// whatever (a link not healthy now: no upload goes into it). The largest gap is closed by
/// spreading its neighbours into it, the window growing until it is under [`GAP_FACTOR`] times the
/// even spacing; a member whose slot moves by no more than half the spacing keeps its old one.
/// A pure function: replay recomputes it from the logged facts.
pub fn reform(task: &Task, group: &GroupSpec, prev: &Layout, members: &[Member], pinned: &BTreeSet<Uid>) -> Reform {
    let mut out = Reform::default();
    let refuse = |out: &mut Reform, why: String| out.layout.refused = Some(why);
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_by(|a, b| a.uid.cmp(&b.uid));
    sorted.dedup_by(|a, b| a.uid == b.uid);
    let n = sorted.len();
    let (Some(d0), true) = (prev.domains.first(), prev.domains.len() == 1) else {
        refuse(&mut out, "a re-form lays out a ring or a route of one domain".into());
        return out;
    };
    if !matches!(prev.spread, Some(Spread::Ring) | Some(Spread::Along)) || d0.rings_m.len() > 1 || n == 0 {
        refuse(&mut out, "a re-form lays out one ring or one route (not concentric rings, lanes or stations)".into());
        return out;
    }
    let Some(len) = d0.path_m.filter(|l| *l > 0.0) else {
        refuse(&mut out, "the layout has no path length".into());
        return out;
    };
    // positions along the path, m
    let pos_of = |s: &Share| -> Option<f64> {
        match prev.spread {
            Some(Spread::Ring) => s.phase_deg.map(|p| p / 360.0 * len),
            _ => s.entry_m,
        }
    };
    let mut placed: Vec<(f64, &Member)> = Vec::new();
    let mut back: Vec<&Member> = Vec::new();
    for m in &sorted {
        match prev.shares.get(&m.uid).and_then(pos_of) {
            Some(p) => placed.push((p.rem_euclid(len), m)),
            None => back.push(m),
        }
    }
    let s_even = len / n as f64;
    out.even_spacing_m = round1(s_even);
    let old: BTreeMap<Uid, f64> = placed.iter().map(|(p, m)| (m.uid.clone(), *p)).collect();
    placed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.uid.cmp(&b.1.uid)));
    // the back members into the largest gaps, one at a time
    for m in back {
        let at = match largest_gap(&placed, len) {
            Some((i, g)) => (placed[i].0 + g / 2.0).rem_euclid(len),
            None => 0.0,
        };
        placed.push((at, m));
        placed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.uid.cmp(&b.1.uid)));
    }
    // close each gap too large by spreading a window of its neighbours over it; a member whose
    // link is not healthy is never in a window (it anchors one), and a gap no window can close (a
    // pinned member on each side, or pinned ones all round) is left while the others are closed
    let mut stuck: BTreeSet<Uid> = BTreeSet::new();
    let mut guard = 0;
    loop {
        guard += 1;
        let k = placed.len();
        if k < 2 || guard > 4 * n + 4 {
            break;
        }
        let gap_after = |i: usize| -> f64 {
            let g = (placed[(i + 1) % k].0 - placed[i].0).rem_euclid(len);
            if g == 0.0 && i == k - 1 { len } else { g }
        };
        let Some((i, g)) = (0..k)
            .filter(|i| !stuck.contains(&placed[*i].1.uid))
            .map(|i| (i, gap_after(i)))
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
        else {
            break;
        };
        if g + GAP_MARGIN_M < GAP_FACTOR * s_even {
            break;
        }
        let pin = |q: usize| pinned.contains(&placed[q].1.uid);
        let (a, b) = (i, (i + 1) % k);
        // the window: the movable members round the gap, lo..=hi cyclically, anchored outside it
        let (mut lo, mut hi) = match (pin(a), pin(b)) {
            (true, true) => {
                stuck.insert(placed[a].1.uid.clone());
                continue;
            }
            (true, false) => (b, b),
            (false, true) => (a, a),
            (false, false) => (a, b),
        };
        let mut fixed = false;
        loop {
            let size = (hi + k - lo) % k + 1;
            if size >= k {
                break;
            }
            let left = (lo + k - 1) % k;
            let right = (hi + 1) % k;
            let span = (placed[right].0 - placed[left].0).rem_euclid(len);
            let span = if span == 0.0 { len } else { span };
            if span / (size as f64 + 1.0) + GAP_MARGIN_M < GAP_FACTOR * s_even {
                for q in 0..size {
                    let idx = (lo + q) % k;
                    placed[idx].0 = (placed[left].0 + span * (q as f64 + 1.0) / (size as f64 + 1.0)).rem_euclid(len);
                }
                fixed = true;
                break;
            }
            // grow on the side not pinned, alternately
            if !pin(left) && (size.is_multiple_of(2) || pin(right)) {
                lo = left;
            } else if !pin(right) {
                hi = right;
            } else {
                break;
            }
        }
        if !fixed {
            stuck.insert(placed[a].1.uid.clone());
            continue;
        }
        placed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.uid.cmp(&b.1.uid)));
    }
    // a member moved by no more than half the spacing keeps its old slot, where that leaves no gap
    // too large (the smallest shifts first); a pinned one always does
    let shift_of = |p: f64, o: f64| {
        let d = (p - o).rem_euclid(len);
        d.min(len - d)
    };
    let mut small: Vec<(f64, Uid)> = placed
        .iter()
        .filter_map(|(p, m)| old.get(&m.uid).map(|o| (shift_of(*p, *o), m.uid.clone())))
        .filter(|(d, u)| *d > 0.0 && (*d <= s_even / 2.0 || pinned.contains(u)))
        .collect();
    small.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, uid) in small {
        let Some(i) = placed.iter().position(|(_, m)| m.uid == uid) else { continue };
        let Some(o) = old.get(&uid).copied() else { continue };
        let was = placed[i].0;
        placed[i].0 = o;
        let mut trial = placed.clone();
        trial.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.uid.cmp(&b.1.uid)));
        let ok = largest_gap(&trial, len).is_none_or(|(_, g)| g + GAP_MARGIN_M < GAP_FACTOR * s_even);
        if !ok && !pinned.contains(&uid) {
            placed[i].0 = was;
        }
    }
    placed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.uid.cmp(&b.1.uid)));
    out.largest_gap_m = round1(largest_gap(&placed, len).map_or(0.0, |(_, g)| g));
    // the shares: a kept member's as it was; a moved or back one's at its new place, in a layer
    // the separation holds in
    let geometry = match (task, prev.spread) {
        (Task::Orbit { center, radius_m }, _) => Some((Plane::new(*center), None, *radius_m)),
        (Task::Patrol { route }, _) => {
            let first = route.first().copied().unwrap_or(LonLat::new(0.0, 0.0));
            let plane = Plane::new(first);
            let pts: Vec<LonLat> = match d0.racetrack_m {
                Some(w) if route.len() == 2 => racetrack(route[0], route[1], w),
                _ => route.clone(),
            };
            Some((plane, Some(Path { pts: pts.iter().map(|p| plane.xy(*p)).collect(), closed: true }), 0.0))
        }
        _ => None,
    };
    let Some((plane, path, r)) = geometry else {
        refuse(&mut out, "a re-form lays out a ring or a route".into());
        return out;
    };
    let k = d0.layers.max(1);
    let v = d0.vertical_m.unwrap_or(0.0);
    let mut shares: BTreeMap<Uid, Share> = BTreeMap::new();
    for (slot, (p, m)) in placed.iter().enumerate() {
        let kept = old.get(&m.uid).is_some_and(|o| o == p);
        let mut share = match (kept, prev.shares.get(&m.uid)) {
            (true, Some(s)) => s.clone(),
            _ => {
                out.moved.insert(m.uid.clone());
                let xy = match &path {
                    Some(path) => path.at(*p),
                    None => on_circle(r, p / len * 360.0),
                };
                Share {
                    slot: slot as u32,
                    layer: 0,
                    alt_m: None,
                    phase_deg: path.is_none().then_some(round3(p / len * 360.0)),
                    entry_m: path.is_some().then_some(round1(*p)),
                    at: Some(round_ll(plane.ll(xy))),
                    ring_m: None,
                    lane: None,
                    width_m: d0.racetrack_m,
                }
            }
        };
        share.slot = slot as u32;
        shares.insert(m.uid.clone(), share);
    }
    // each moved member in the first layer where its neighbours along the path leave it room
    let order: Vec<(f64, Uid)> = placed.iter().map(|(p, m)| (*p, m.uid.clone())).collect();
    for (idx, (p, uid)) in order.iter().enumerate() {
        if !out.moved.contains(uid) {
            continue;
        }
        let Some(m) = sorted.iter().find(|m| &m.uid == uid) else { continue };
        let lateral = d0.lateral_m;
        let mut chosen = None;
        for layer in 0..k {
            let clear = order.iter().enumerate().all(|(j, (q, other))| {
                if j == idx || shares.get(other).is_none_or(|s| s.layer != layer) {
                    return true;
                }
                let gap = (q - p).rem_euclid(len);
                gap.min(len - gap) + 1e-6 >= lateral
            });
            if clear {
                chosen = Some(layer);
                break;
            }
        }
        let layer = chosen.unwrap_or(0);
        let alt = alt_for(m, group.alt_m, layer, k, v);
        match alt {
            Ok(a) => {
                if let Some(s) = shares.get_mut(uid) {
                    s.layer = layer;
                    s.alt_m = a.map(round1);
                }
            }
            Err(why) => {
                refuse(&mut out, format!("{uid}: {why}"));
                return out;
            }
        }
    }
    out.layout = Layout {
        spread: prev.spread,
        shares,
        domains: vec![DomainLayout {
            members: n as u32,
            spacing_m: Some(round1(s_even)),
            ..d0.clone()
        }],
        warnings: Vec::new(),
        refused: None,
    };
    if let Some(why) = separation(&out.layout, members) {
        out.layout.refused = Some(why);
    }
    out
}

/// The largest gap between neighbours along a path of `len` (each `placed` sorted by position):
/// the index of the member it starts after, and its length.
fn largest_gap(placed: &[(f64, &Member)], len: f64) -> Option<(usize, f64)> {
    let k = placed.len();
    if k == 0 {
        return None;
    }
    if k == 1 {
        return Some((0, len));
    }
    (0..k)
        .map(|i| {
            let next = placed[(i + 1) % k].0;
            let g = (next - placed[i].0).rem_euclid(len);
            (i, if g == 0.0 && i == k - 1 { len } else { g })
        })
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)))
}

/// The members a re-form may move (docs/SWARMS.md section 4.3): those on their shares and those
/// back, but a member whose link is not healthy now keeps its slot.
pub fn movable(on_share: &BTreeSet<Uid>, back: &BTreeSet<Uid>, unhealthy: &BTreeSet<Uid>) -> BTreeSet<Uid> {
    on_share.union(back).filter(|u| !unhealthy.contains(*u)).cloned().collect()
}

// ---- in force: re-forming on drops (stage 4) ---------------------------------------------------

/// A silent member drops this long after its position went stale, s: longer than Saturation's
/// 10 s lost-link timer, so a member its failsafe sends home has gone before it counts, and a
/// brief gap in its link does not count (docs/SWARMS.md section 4.3; an estimate).
pub const DROP_AFTER_S: i64 = 20;
/// A member a re-form left out is back once heard this long running, s (an estimate).
pub const BACK_AFTER_S: i64 = 30;
/// A re-form waits until no member dropped or came back for this long, s, so a jam that takes a
/// swarm's links over half a minute makes one re-form, not ten (an estimate).
pub const SETTLE_S: i64 = 15;
/// At most one re-form, or one refusal, per group in this long, s (an estimate).
pub const REFORM_EVERY_S: i64 = 60;
/// Material: at least this many members dropped or back since the last layout...
pub const MATERIAL_MIN: usize = 2;
/// ...or this share of the selection, whichever is larger; fewer leave gaps (an estimate).
pub const MATERIAL_SHARE: f64 = 0.05;

/// A group order in force (docs/SWARMS.md sections 2.2 and 4.3): dispatched or done, re-forming,
/// not suspended, some member still on an order of its own. The engine keeps one per group it
/// dispatched, and replay one per group it saw dispatched, each changed only by the events both
/// read (`group.dispatched`, `group.reformed`, `group.reform_refused`, `group.suspended`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Force {
    /// The layout in force: its dispatch's, then its last re-form's.
    pub layout: Layout,
    /// Each member's member order in force, by uid: its dispatch's, then a re-form's.
    pub members: BTreeMap<Uid, IntentId>,
    /// When it was dispatched (a member that never reported is silent from then).
    pub dispatched_t: Time,
    /// Its re-forms so far (each `group.reformed`'s `reform`).
    pub seq: u32,
    /// When it last re-formed, was refused one or was suspended (the rate).
    pub last_attempt: Option<Time>,
    /// The members whose drops and returns its last refusal was on: the same cause is not tried
    /// again (a new drop or return is); and why it was refused (the console's).
    pub refused: Option<BTreeSet<Uid>>,
    pub refused_why: Option<String>,
    /// Members back whose new share failed its checks: left out for good (the person re-forms).
    pub out: BTreeSet<Uid>,
    /// When a re-form left each dropped member out of the layout (its return is after it).
    pub left: BTreeMap<Uid, Time>,
    /// Below its minimum: it re-forms no more (decision 4: the person re-forms it).
    pub suspended: bool,
}

impl Force {
    /// A group just dispatched on `layout` with `members`.
    pub fn new(layout: Layout, members: BTreeMap<Uid, IntentId>, t: Time) -> Force {
        Force {
            layout,
            members,
            dispatched_t: t,
            ..Force::default()
        }
    }
}

/// One member's order as the group step reads it (the engine's and replay's own intents).
#[derive(Clone, Debug, PartialEq)]
pub struct MemberView {
    pub state: crate::intent::State,
    /// When it ended, if it did.
    pub ended_t: Option<Time>,
    /// A newer order that is not its group's took its vehicle.
    pub taken: bool,
}

/// Why a member counts as dropped, and since when (logged in a re-form's cause; replay
/// recomputes each from the facts and its own intents).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Drop {
    pub why: String,
    pub at: Time,
}

/// What a re-form, a refusal or a suspension is on: each member on the layout that dropped, and
/// each member a re-form left out that is back (heard since `since`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cause {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped: BTreeMap<Uid, Drop>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub back: BTreeMap<Uid, Time>,
}

impl Cause {
    pub fn is_empty(&self) -> bool {
        self.dropped.is_empty() && self.back.is_empty()
    }

    /// The members it is on, by uid.
    pub fn uids(&self) -> BTreeSet<Uid> {
        self.dropped.keys().chain(self.back.keys()).cloned().collect()
    }

    /// The latest change in it: a drop's time, or a return's (heard `BACK_AFTER_S` running).
    pub fn latest(&self) -> Option<Time> {
        self.dropped
            .values()
            .map(|d| d.at)
            .chain(self.back.values().map(|t| t.plus_ms(BACK_AFTER_S * 1000)))
            .max()
    }

    /// In words, for a refusal, a suspension and the console.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.dropped.is_empty() {
            let mut by: BTreeMap<&str, u32> = BTreeMap::new();
            for d in self.dropped.values() {
                let k = d.why.split(':').next().unwrap_or("");
                *by.entry(k).or_default() += 1;
            }
            let list: Vec<String> = by.iter().map(|(k, v)| format!("{v} {k}")).collect();
            parts.push(format!("{} dropped ({})", self.dropped.len(), list.join(", ")));
        }
        if !self.back.is_empty() {
            parts.push(format!("{} back", self.back.len()));
        }
        parts.join(", ")
    }
}

/// A group in force's members as the facts show them now.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Standing {
    /// On the layout and not dropped (a member silent less than [`DROP_AFTER_S`] past its stale
    /// limit still counts here).
    pub on_share: BTreeSet<Uid>,
    /// On the layout and dropped, with why and since when.
    pub dropped: BTreeMap<Uid, Drop>,
    /// Left out of the layout by a re-form and heard again now (not dropped for any reason): back
    /// once heard [`BACK_AFTER_S`] running.
    pub heard: BTreeSet<Uid>,
    /// Taken by a newer order that is not its group's: neither on its share nor dropped; the
    /// minimum drops by one for each.
    pub taken: BTreeSet<Uid>,
}

/// Why `uid` (member order `view`) counts as dropped at `t`, and since when: its member order
/// ended other than done; confirmed or presumed lost (rules 30's ledger); silent
/// [`DROP_AFTER_S`] past its link's stale limit. Drift alone never drops a member.
pub fn drop_reason(
    uid: &Uid,
    view: &MemberView,
    world: &WorldSnapshot,
    nodes: &crate::nodes::NodeLedger,
    dispatched_t: Time,
    cfg: &RulesConfig,
) -> Option<Drop> {
    use crate::intent::State;
    if view.state.is_terminal() && view.state != State::Done {
        let name = serde_json::to_value(view.state).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        return Some(Drop {
            why: format!("ended: its member order {name}"),
            at: view.ended_t.unwrap_or(world.t),
        });
    }
    if let Some(m) = nodes.lost_mark(uid) {
        return Some(Drop {
            why: "lost: confirmed lost".into(),
            at: m.t,
        });
    }
    if let Some(p) = nodes.presumption(uid) {
        return Some(Drop {
            why: "lost: presumed lost".into(),
            at: p.t,
        });
    }
    let v = world.vehicles.get(uid);
    let (limit, _) = cfg.fact_age_limit_ms(v.and_then(|v| v.report_interval_ms));
    let stale_at = match v.and_then(|v| v.position.as_ref()) {
        Some(p) => p.t.plus_ms(limit),
        None => dispatched_t.plus_ms(limit),
    };
    let at = stale_at.plus_ms(DROP_AFTER_S * 1000);
    if world.t >= at {
        return Some(Drop {
            why: format!("silent: no position for {:.0} s", world.t.ms_since(stale_at.plus_ms(-limit)) as f64 / 1000.0),
            at,
        });
    }
    None
}

/// The members of a group in force as the facts at `world` show them (`views`: each member
/// order in force; members a re-form left out for good are neither).
pub fn standing(
    force: &Force,
    views: &BTreeMap<Uid, MemberView>,
    world: &WorldSnapshot,
    nodes: &crate::nodes::NodeLedger,
    cfg: &RulesConfig,
) -> Standing {
    let mut s = Standing::default();
    for uid in force.members.keys() {
        if force.out.contains(uid) {
            continue;
        }
        let Some(view) = views.get(uid) else { continue };
        if view.taken {
            s.taken.insert(uid.clone());
            continue;
        }
        let drop = drop_reason(uid, view, world, nodes, force.dispatched_t, cfg);
        let silent_now = world
            .vehicles
            .get(uid)
            .is_none_or(|v| crate::nodes::silent(v, world.t, cfg));
        if force.layout.shares.contains_key(uid) {
            match drop {
                Some(d) => {
                    s.dropped.insert(uid.clone(), d);
                }
                None => {
                    s.on_share.insert(uid.clone());
                }
            }
        } else if drop.is_none() && !silent_now {
            s.heard.insert(uid.clone());
        }
    }
    s
}

/// What a group in force's step does now.
#[derive(Clone, Debug, PartialEq)]
pub enum Due {
    Nothing,
    /// Below its minimum once settled: `group.suspended`.
    Suspend { strength: u32, min: u32 },
    /// A re-form is due (material, settled, not within the rate, not the cause last refused).
    Reform,
}

/// The minimum a group in force holds to: its own, less one for each member a newer order took.
pub fn min_in_force(group: &GroupSpec, taken: usize) -> u32 {
    group.min_members().saturating_sub(taken as u32)
}

/// Whether a group in force re-forms, is suspended or does nothing at `t` on `cause` (its drops,
/// and its members back: those heard [`BACK_AFTER_S`] running), `on_share` members on their
/// shares, `n` the selection (docs/SWARMS.md section 4.3): suspended below its minimum once
/// settled; else a re-form when the drops and returns are material (at least the larger of
/// [`MATERIAL_MIN`] and [`MATERIAL_SHARE`] of the selection), settled (none in the last
/// [`SETTLE_S`]), at least [`REFORM_EVERY_S`] after the last attempt, and not the cause last
/// refused. A pure function: replay asks it of every logged step.
pub fn due(force: &Force, group: &GroupSpec, cause: &Cause, on_share: usize, taken: usize, n: usize, t: Time) -> Due {
    if force.suspended || group.reform != Some(true) {
        return Due::Nothing;
    }
    let settled = cause.latest().is_none_or(|l| t.ms_since(l) >= SETTLE_S * 1000);
    let min = min_in_force(group, taken);
    let strength = (on_share + cause.back.len()) as u32;
    if strength < min && settled && !cause.dropped.is_empty() {
        return Due::Suspend { strength, min };
    }
    let changes = cause.dropped.len() + cause.back.len();
    let material = changes >= MATERIAL_MIN.max((n as f64 * MATERIAL_SHARE).ceil() as usize);
    if changes == 0 || !material || !settled {
        return Due::Nothing;
    }
    if force.last_attempt.is_some_and(|l| t.ms_since(l) < REFORM_EVERY_S * 1000) {
        return Due::Nothing;
    }
    if force.refused.as_ref() == Some(&cause.uids()) {
        return Due::Nothing;
    }
    Due::Reform
}

/// The members a re-form lays out (those on their shares and those back), from their facts.
pub fn reform_members(on_share: &BTreeSet<Uid>, back: &BTreeSet<Uid>, world: &WorldSnapshot) -> Vec<Member> {
    on_share
        .iter()
        .chain(back.iter())
        .filter_map(|u| world.vehicles.get(u).and_then(|v| Member::from_facts(u, v)))
        .collect()
}

/// A member's link not healthy for a mission upload now (rules 5's reach): a re-form keeps its
/// slot, so no upload goes into a link that just failed.
pub fn pinned(group: &IntentSpec, on_share: &BTreeSet<Uid>, world: &WorldSnapshot, queue: &crate::queue::QueueConfig, cfg: &RulesConfig) -> BTreeSet<Uid> {
    on_share
        .iter()
        .filter(|u| {
            let one = alone(group, group.task.clone(), u);
            crate::queue::reach(&one, world, queue, cfg).iter().any(|r| !r.reachable)
        })
        .cloned()
        .collect()
}

/// A re-form as the engine makes it and replay recomputes it: the new layout, each member moved
/// or back with its member order's spec (in uid order) and their checks, and the members back
/// left out because their share failed its checks.
#[derive(Clone, Debug, PartialEq)]
pub struct ReformPlan {
    pub reform: Reform,
    pub specs: Vec<IntentSpec>,
    pub results: Vec<CheckResult>,
    pub left_out: BTreeMap<Uid, String>,
}

/// Plan a re-form of group order `spec` (its normalized `group`) in force as `force`, on `cause`
/// (its drops and returns) with `on_share` members on their shares, on `inputs` (every member's
/// facts). The engine passes `sample` (the world and its terrain source): each moved member's
/// new share is sampled into `inputs` before its checks read them; replay passes none (the logged
/// inputs hold those samples). The layout ([`reform`]) keeps the slot of a member whose link is
/// not healthy now, or that an engagement holds; each moved member's share passes every vehicle
/// rule ([`member_checks`]), a member back whose share fails is left out (for good) and the
/// layout made again without it, a member on its share whose new one fails refuses the re-form;
/// and no moved member's own policy decision raises what its group's approval (`decision`)
/// covers ([`crate::queue::raised`]). Err: why the re-form is refused.
#[allow(clippy::too_many_arguments)]
pub fn plan_reform(
    spec: &IntentSpec,
    gs: &GroupSpec,
    force: &Force,
    on_share: &BTreeSet<Uid>,
    cause: &Cause,
    inputs: &mut WorldSnapshot,
    sample: Option<(&WorldSnapshot, &dyn crate::terrain::TerrainOracle)>,
    policy: &crate::policy::Policy,
    decision: Option<&crate::policy::PolicyDecision>,
) -> Result<ReformPlan, String> {
    let cfg = &policy.rules;
    let mut keep = pinned(spec, on_share, inputs, &policy.queue(), cfg);
    keep.extend(on_share.iter().filter(|u| inputs.engaged.contains_key(*u)).cloned());
    let mut left_out: BTreeMap<Uid, String> = BTreeMap::new();
    loop {
        let back: BTreeSet<Uid> = cause.back.keys().filter(|u| !left_out.contains_key(*u)).cloned().collect();
        let members = reform_members(on_share, &back, inputs);
        let rf = reform(&spec.task, gs, &force.layout, &members, &keep);
        if let Some(why) = &rf.layout.refused {
            return Err(format!("the re-formed layout does not hold: {why}"));
        }
        let mut specs = Vec::new();
        for uid in &rf.moved {
            let (Some(task), Some(share)) = (member_task(&spec.task, gs, &rf.layout, uid), rf.layout.shares.get(uid)) else {
                return Err(format!("{uid}: the re-formed layout gives it no share"));
            };
            specs.push(member_spec(spec, task, uid, share.clone()));
        }
        if let Some((world, terrain)) = sample {
            for ms in &specs {
                let mw = world.for_intent_plan(ms, terrain, cfg, None);
                inputs.terrain.merge(mw.terrain);
                inputs.route_plans.extend(mw.route_plans);
            }
        }
        let mut results = Vec::new();
        let mut again = false;
        for ms in &specs {
            let Some(uid) = ms.uids.first() else { continue };
            let own = member_checks(ms, inputs, cfg);
            if let Some(f) = own.iter().find(|r| r.outcome == Outcome::Fail) {
                let why = format!("{}: {}", f.rule.as_str(), f.detail);
                if back.contains(uid) {
                    left_out.insert(uid.clone(), why);
                    again = true;
                } else {
                    return Err(format!("{uid}'s new share fails {why}"));
                }
            }
            results.extend(own);
        }
        if again {
            continue;
        }
        // the tier: what each moved member's own facts would ask of the policy, against its group's
        let Some(group_decision) = decision else {
            return Err("no policy decision of its group's to judge the re-form against".into());
        };
        for ms in &specs {
            let Some(uid) = ms.uids.first() else { continue };
            let mut w = inputs.clone();
            w.engaged.retain(|u, _| u == uid);
            w.waiting.retain(|u, _| u == uid);
            let d = crate::policy::decide(ms, &w, policy);
            let raised = crate::queue::raised(&d, group_decision);
            if !raised.is_empty() {
                return Err(format!("{uid}: {}", raised.join("; ")));
            }
        }
        return Ok(ReformPlan {
            reform: rf,
            specs,
            results,
            left_out,
        });
    }
}

// ---- the group's check (rules 34) ----------------------------------------------------------

/// How one member came out of its group's check.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberOutcome {
    /// Sent (None), waiting or excluded.
    pub class: Option<MemberClass>,
    /// Its member order's task and share, when it was laid out.
    pub task: Option<Task>,
    pub share: Option<Share>,
}

/// A group order's check on one world (docs/SWARMS.md section 2.2): its layout, each member's
/// outcome, and the results in rule order (each member's own results, then the group's
/// `separation` and `strength`). [`crate::rules::evaluate`] returns its results; the engine logs
/// its layout with them, and replay recomputes both from the logged inputs.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct GroupEval {
    pub layout: Layout,
    pub members: BTreeMap<Uid, MemberOutcome>,
    pub results: Vec<CheckResult>,
}

impl GroupEval {
    /// The members that can take it (sent or waiting), in uid order.
    pub fn counted(&self) -> Vec<Uid> {
        self.members
            .iter()
            .filter(|(_, m)| m.class != Some(MemberClass::Excluded))
            .map(|(u, _)| u.clone())
            .collect()
    }

    /// A member's own order's spec as its group's check judged it (`id` and `created_t` the
    /// group's), when it was laid out.
    pub fn member_spec(&self, group: &IntentSpec, uid: &Uid) -> Option<IntentSpec> {
        let m = self.members.get(uid)?;
        Some(member_spec(group, m.task.clone()?, uid, m.share.clone()?))
    }
}

/// A member order of `group` for `uid`: one vehicle, its own task and share, the group's source,
/// constraints and rationale, its `parent` the group.
pub fn member_spec(group: &IntentSpec, task: Task, uid: &Uid, share: Share) -> IntentSpec {
    IntentSpec {
        id: group.id,
        created_t: group.created_t,
        source: group.source.clone(),
        task,
        uids: vec![uid.clone()],
        constraints: group.constraints.clone(),
        rationale: group.rationale.clone(),
        grouping: Grouping {
            group: None,
            parent: Some(group.id),
            share: Some(share),
        },
    }
}

/// Whether `spec` is decided as a group order under `cfg`.
pub fn is_group(spec: &IntentSpec, cfg: &RulesConfig) -> bool {
    cfg.group_orders() && spec.group().is_some()
}

/// One of ours `uid` alone on `task` (the checks of one member that do not depend on its share).
fn alone(spec: &IntentSpec, task: Task, uid: &Uid) -> IntentSpec {
    IntentSpec {
        task,
        uids: vec![uid.clone()],
        grouping: Grouping::default(),
        ..spec.clone()
    }
}

/// Whether a vehicle's navigation status or position is missing or older than the checks trust:
/// what makes a member wait (it may report again) rather than be left out.
fn stale(v: &VehicleFacts, world: &WorldSnapshot, cfg: &RulesConfig) -> bool {
    let (limit, _) = cfg.fact_age_limit_ms(v.report_interval_ms);
    [v.nav_ready.as_ref().map(|f| f.t), v.position.as_ref().map(|f| f.t)]
        .into_iter()
        .any(|t| t.is_none_or(|t| world.t.ms_since(t) > limit))
}

/// The members a group order lays out (the selection less those a hard reason leaves out before
/// any share: no such vehicle, a type that can't do the task, spent, lost, propulsion damaged,
/// held by an engagement), with those reasons' results by uid, and the layout over them.
fn first_pass(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> (Layout, BTreeMap<Uid, Vec<CheckResult>>, Vec<Member>) {
    let mut early: BTreeMap<Uid, Vec<CheckResult>> = BTreeMap::new();
    let mut laid: Vec<Member> = Vec::new();
    let hold = Task::Hold {};
    for uid in &spec.uids {
        let mut out = Vec::new();
        let cap = crate::rules::capability(&alone(spec, spec.task.clone(), uid), world);
        let capable = cap.iter().all(|r| r.outcome == Outcome::Pass);
        out.extend(cap);
        let mut hard = false;
        if capable {
            // spent and lost: readiness's own verdict on a safety command, which judges nothing else
            for r in crate::rules::readiness(&alone(spec, hold.clone(), uid), world, cfg) {
                if r.outcome == Outcome::Fail {
                    out.push(r);
                    hard = true;
                }
            }
            let v = world.vehicles.get(uid);
            if !hard
                && cfg.damage()
                && let Some(d) = v.and_then(|v| v.damage.as_ref()).filter(|d| d.value.mobility)
            {
                out.push(CheckResult::new(
                    RuleId::Readiness,
                    Some(uid),
                    Outcome::Fail,
                    format!("{} (its own report): a member without propulsion is left out of a group order", d.value.describe()),
                ));
                hard = true;
            }
            if !hard && let Some(e) = world.engaged.get(uid) {
                out.push(CheckResult::new(
                    RuleId::PlatformFree,
                    Some(uid),
                    Outcome::Fail,
                    format!("held by engagement {e}: a group order never stops an engagement (order it alone to stop it)"),
                ));
                hard = true;
            }
        }
        if capable && !hard
            && let Some(m) = world.vehicles.get(uid).and_then(|v| Member::from_facts(uid, v))
        {
            laid.push(m);
        }
        early.insert(uid.clone(), out);
    }
    let group = spec.group().cloned().unwrap_or_default();
    let lay = layout(&spec.task, &group, &laid);
    (lay, early, laid)
}

/// Each laid-out member's own order's spec (the engine samples the terrain along each share and
/// plans each one's way before the checks read them).
pub fn member_specs(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<IntentSpec> {
    let (lay, _, laid) = first_pass(spec, world, cfg);
    let group = spec.group().cloned().unwrap_or_default();
    laid.iter()
        .filter_map(|m| {
            let task = member_task(&spec.task, &group, &lay, &m.uid)?;
            Some(member_spec(spec, task, &m.uid, lay.shares.get(&m.uid)?.clone()))
        })
        .collect()
}

/// A group order's check (docs/SWARMS.md sections 2.2 and 4.1): the members a hard reason leaves
/// out first, then the layout over the rest, then every vehicle rule on each member's own share
/// (and `depth` on a sub's), each failing result classing its member waiting (its facts too old
/// or not reported) or excluded; then `separation` (the layout holds) and `strength` (sent and
/// waiting at least the minimum). Results in rule order, each rule's in the selection's order.
pub fn evaluate(spec: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> GroupEval {
    let group = spec.group().cloned().unwrap_or_default();
    let (lay, early, laid) = first_pass(spec, world, cfg);
    let laid_out: BTreeSet<&Uid> = laid.iter().map(|m| &m.uid).collect();
    let mut by_rule: BTreeMap<RuleId, Vec<CheckResult>> = BTreeMap::new();
    let mut members: BTreeMap<Uid, MemberOutcome> = BTreeMap::new();
    for uid in &spec.uids {
        let mut own: Vec<CheckResult> = early.get(uid).cloned().unwrap_or_default();
        let mut task = None;
        let mut share = None;
        if laid_out.contains(uid) && lay.refused.is_none()
            && let (Some(t), Some(sh)) = (member_task(&spec.task, &group, &lay, uid), lay.shares.get(uid))
        {
            let ms = member_spec(spec, t.clone(), uid, sh.clone());
            own.extend(member_checks(&ms, world, cfg));
            task = Some(t);
            share = Some(sh.clone());
        }
        // a member whose facts are too old waits: what its share's checks could not judge on them
        // is judged again when it reports (its member order's re-check); a hard reason before its
        // share leaves it out whatever
        let waits = task.is_some() && world.vehicles.get(uid).is_some_and(|v| stale(v, world, cfg));
        let n_early = early.get(uid).map_or(0, Vec::len);
        for (i, r) in own.iter_mut().enumerate() {
            if r.outcome == Outcome::Fail {
                r.class = Some(if waits && i >= n_early { MemberClass::Waiting } else { MemberClass::Excluded });
            }
        }
        let class = own.iter().filter_map(|r| r.class).max();
        // a member left out before its share has no order; one laid out but excluded neither
        members.insert(
            uid.clone(),
            MemberOutcome {
                class: class.or(if task.is_none() { Some(MemberClass::Excluded) } else { None }),
                task: task.filter(|_| class != Some(MemberClass::Excluded)),
                share: share.filter(|_| class != Some(MemberClass::Excluded)),
            },
        );
        for r in own {
            by_rule.entry(r.rule).or_default().push(r);
        }
    }
    let mut results: Vec<CheckResult> = Vec::new();
    for rule in [
        RuleId::Capability,
        RuleId::Readiness,
        RuleId::PlatformLimits,
        RuleId::Domain,
        RuleId::Geofence,
        RuleId::Reserves,
        RuleId::PlatformFree,
        RuleId::Depth,
    ] {
        results.extend(by_rule.remove(&rule).unwrap_or_default());
    }
    // the layout
    let sep = match &lay.refused {
        Some(why) => CheckResult::new(RuleId::Separation, None, Outcome::Fail, why.clone()),
        None => {
            let mut c = CheckResult::new(RuleId::Separation, None, Outcome::Pass, describe(&lay, &group));
            if let Some(w) = lay.warnings.first() {
                c.warning = Some(w.clone());
            }
            c
        }
    };
    results.push(sep);
    // the strength
    let min = group.min_members();
    let n = spec.uids.len();
    let waiting: Vec<&Uid> = members.iter().filter(|(_, m)| m.class == Some(MemberClass::Waiting)).map(|(u, _)| u).collect();
    let sent = members.values().filter(|m| m.class.is_none()).count();
    let counted = sent + waiting.len();
    let mut reasons: BTreeMap<&str, u32> = BTreeMap::new();
    for r in results.iter().filter(|r| r.class == Some(MemberClass::Excluded)) {
        *reasons.entry(r.rule.as_str()).or_default() += 1;
    }
    let excluded = n - counted;
    let why_out = if excluded == 0 {
        String::new()
    } else {
        let list: Vec<String> = reasons.iter().map(|(k, v)| format!("{k} {v}")).collect();
        format!(", {excluded} excluded ({})", list.join(", "))
    };
    let why_wait = if waiting.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = waiting.iter().map(|u| u.to_string()).collect();
        format!(", {} waiting for fresh facts ({})", waiting.len(), list.join(", "))
    };
    let detail = format!("{counted} of {n} can take it ({sent} sent{why_wait}{why_out}); the minimum is {min}");
    let ok = counted as u32 >= min && lay.refused.is_none();
    results.push(CheckResult::new(RuleId::Strength, None, if ok { Outcome::Pass } else { Outcome::Fail }, detail));
    GroupEval {
        layout: lay,
        members,
        results,
    }
}

/// Every vehicle rule on one member's own share (`ms`, a member order's spec): readiness, its
/// platform's limits, the domain, the geofence, its reserves, and `depth` on a sub's layer. The
/// group's check and a re-form's both judge a share here.
pub fn member_checks(ms: &IntentSpec, world: &WorldSnapshot, cfg: &RulesConfig) -> Vec<CheckResult> {
    let mut own = Vec::new();
    own.extend(crate::rules::readiness(ms, world, cfg));
    own.extend(crate::rules::platform_limits(ms, world, cfg));
    own.extend(crate::rules::domain(ms, world, cfg));
    own.extend(crate::rules::geofence(ms, world, cfg));
    own.extend(crate::rules::reserves(ms, world, cfg));
    if let Some(d) = ms.share().and_then(|sh| depth(ms, world, sh)) {
        own.push(d);
    }
    own
}

/// The layout in words (the `separation` rule's detail).
fn describe(lay: &Layout, group: &GroupSpec) -> String {
    let spread = lay.spread.or(group.spread).map_or("", Spread::as_str);
    if lay.domains.is_empty() {
        return format!("{} member(s), {spread}: each by its type", lay.shares.len());
    }
    let parts: Vec<String> = lay
        .domains
        .iter()
        .map(|d| {
            let mut p = format!("{} {}", d.members, d.domain.as_str());
            if let Some(s) = d.spacing_m {
                p.push_str(&format!(" {s:.0} m apart"));
            }
            if !d.rings_m.is_empty() {
                let r: Vec<String> = d.rings_m.iter().map(|r| format!("{r:.0}")).collect();
                p.push_str(&format!(" on rings of {} m", r.join(", ")));
            }
            if d.layers > 1 {
                p.push_str(&format!(" in {} layers {:.0} m apart", d.layers, d.vertical_m.unwrap_or(0.0)));
            }
            if let Some(w) = d.racetrack_m {
                p.push_str(&format!(", a racetrack {w:.0} m wide"));
            }
            p
        })
        .collect();
    format!("{spread}: {}; at least {} apart", parts.join("; "), lay.domains.iter().map(|d| format!("{:.0} m", d.lateral_m)).collect::<Vec<_>>().join(" and "))
}

/// The `depth` rule on a sub member's share: its layer clears the seabed by its band's margin at
/// every waypoint of its task, on the terrain's samples there (None for anything but a sub with a
/// layer); not evaluated where a waypoint has no sample.
fn depth(ms: &IntentSpec, world: &WorldSnapshot, share: &Share) -> Option<CheckResult> {
    let uid = ms.uids.first()?;
    let p = world.vehicles.get(uid)?.platform.as_ref()?;
    let Band::Depth { seabed_m, .. } = figures(p).band else { return None };
    let deep = -share.alt_m?;
    let path = ms.task.path()?;
    let mut shallowest: Option<f64> = None;
    for (i, pt) in path.points.iter().enumerate() {
        match world.terrain.lookup(*pt) {
            Some(crate::terrain::Surface::Water { depth_m }) => {
                shallowest = Some(shallowest.map_or(*depth_m, |s: f64| s.min(*depth_m)));
                if *depth_m < deep + seabed_m {
                    return Some(CheckResult::new(
                        RuleId::Depth,
                        Some(uid),
                        Outcome::Fail,
                        format!(
                            "at waypoint {} the seabed is {depth_m:.0} m down: {deep:.0} m deep leaves {:.0} m above it (it needs {seabed_m:.0} m)",
                            i + 1,
                            depth_m - deep
                        ),
                    ));
                }
            }
            Some(_) => {}
            None => {
                return Some(CheckResult::new(
                    RuleId::Depth,
                    Some(uid),
                    Outcome::NotEvaluated,
                    format!("not evaluated: no depth at waypoint {} (no terrain answer there)", i + 1),
                ));
            }
        }
    }
    Some(CheckResult::new(
        RuleId::Depth,
        Some(uid),
        Outcome::Pass,
        format!(
            "{deep:.0} m deep clears the seabed by {seabed_m:.0} m at every waypoint (the shallowest {:.0} m)",
            shallowest.unwrap_or(0.0)
        ),
    ))
}

/// Rules 34: what a member order's approval rests on: its group's (when and by whom it was
/// approved, the group's policy decision, which a member's own re-check is judged against).
pub fn member_basis(group: &crate::queue::Basis, decision: Option<&crate::policy::PolicyDecision>) -> crate::queue::Basis {
    crate::queue::Basis {
        t: group.t,
        by_human: group.by_human,
        checks: Vec::new(),
        policy: decision.cloned(),
        engaged: BTreeMap::new(),
    }
}

/// Rules 34: a dispatched group order's outcome once every member order it sent has ended
/// (`ends`: each one's state, and whether a newer order took its vehicle): done when at least the
/// minimum reached their shares (done), the minimum less one for each member a newer order took
/// (which counts neither way), else failed; the reason counts each end. The engine and replay
/// both decide it here.
pub fn outcome(min: u32, ends: &[(crate::intent::State, bool)]) -> (bool, String) {
    use crate::intent::State;
    let done = ends.iter().filter(|(s, _)| *s == State::Done).count();
    let taken = ends.iter().filter(|(s, t)| *s != State::Done && *t).count();
    let mut others: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (s, t) in ends {
        if *s != State::Done && !*t {
            let name = match s {
                State::Failed => "failed",
                State::Voided => "voided",
                State::Expired => "never sent (its wait ran out)",
                State::Aborted => "aborted",
                State::Rejected => "rejected",
                _ => "ended",
            };
            *others.entry(name).or_default() += 1;
        }
    }
    let need = (min as usize).saturating_sub(taken);
    let mut parts = vec![format!("{done} reached their shares")];
    parts.extend(others.iter().map(|(k, v)| format!("{v} {k}")));
    if taken > 0 {
        parts.push(format!("{taken} taken by a newer order"));
    }
    let ok = done >= need;
    let verdict = if ok { "done" } else { "failed" };
    let min_text = if taken > 0 { format!("{need} ({min} less {taken} taken)") } else { min.to_string() };
    (ok, format!("{verdict}: {}; the minimum is {min_text}", parts.join(", ")))
}

// ---- the policy's figures ---------------------------------------------------------------------

/// `[group]` (rules 34, policy schema 9, [`crate::rules::GROUP_ORDER_SCHEMA_SINCE`]): the figures a
/// group order takes when its proposal leaves them out. Every one an estimate (docs/SWARMS.md
/// decision 3 and section 14, question 2). Logged with the policy from rules 34 only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupPolicy {
    /// A group order's minimum strength when its proposal names none: this share of its
    /// selection, rounded up (a hold or an rtl: one member). An estimate: 0.8.
    pub min_share: f64,
}

/// [`GroupPolicy::min_share`]'s standing value (docs/SWARMS.md decision 3, a GUESS).
pub const MIN_SHARE: f64 = 0.8;

impl Default for GroupPolicy {
    fn default() -> Self {
        GroupPolicy { min_share: MIN_SHARE }
    }
}

impl GroupPolicy {
    /// Why the figures can't be used, if they can't.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.min_share.is_finite() && self.min_share > 0.0 && self.min_share <= 1.0) {
            return Err(format!("group.min_share is a share of the selection, above 0 and at most 1, got {}", self.min_share));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn a_group_spec_is_bounded_and_normalized() {
        let g = GroupSpec::default();
        assert!(g.invalid(TaskKind::Orbit, 1).is_some(), "one vehicle is no group");
        assert!(g.invalid(TaskKind::Track, 3).is_some(), "a tracker's track has no group meaning");
        assert!(g.invalid(TaskKind::Engage, 3).is_some());
        assert_eq!(g.invalid(TaskKind::Orbit, 10), None);
        let ring_patrol = GroupSpec { spread: Some(Spread::Ring), ..GroupSpec::default() };
        assert!(ring_patrol.invalid(TaskKind::Patrol, 10).is_some());
        let lanes = GroupSpec { spread: Some(Spread::Lanes), ..GroupSpec::default() };
        assert!(lanes.invalid(TaskKind::Patrol, 10).is_some(), "lanes needs its count and spacing");
        let too_many = GroupSpec { min_members: Some(11), ..GroupSpec::default() };
        assert!(too_many.invalid(TaskKind::Orbit, 10).is_some());
        let n = GroupSpec::default().normalized(TaskKind::Orbit, 100, MIN_SHARE);
        assert_eq!((n.spread, n.min_members, n.reform), (Some(Spread::Ring), Some(80), Some(true)));
        let n = GroupSpec::default().normalized(TaskKind::Orbit, 7, MIN_SHARE);
        assert_eq!(n.min_members, Some(6), "rounded up: 5.6 is 6");
        let n = GroupSpec::default().normalized(TaskKind::Hold, 100, MIN_SHARE);
        assert_eq!((n.spread, n.min_members, n.reform), (Some(Spread::Here), Some(1), Some(false)));
    }

    /// The catalog's figures for Saturation's types (docs/SWARMS.md section 3.3), each marked an
    /// estimate, and only on the facts of a run under rules 34.
    #[test]
    fn the_catalog_figures_are_section_3_3s() {
        let c = crate::Catalog::load(&root().join("sim/platforms.toml")).unwrap();
        assert!(c.get("x10").unwrap().formation.is_none(), "not on the facts as loaded");
        let f = c.formed();
        let x10 = f.get("x10").unwrap().formation.unwrap();
        assert_eq!((x10.lateral_m, x10.vertical_m), (15.0, Some(10.0)));
        assert_eq!(x10.band, Band::AboveGround { min_m: 30.0, max_m: 120.0 });
        let puma = f.get("puma").unwrap().formation.unwrap();
        assert_eq!((puma.lateral_m, puma.vertical_m, puma.loiter_m), (100.0, Some(30.0), Some(LOITER_DEFAULT_M)));
        assert_eq!(puma.band, Band::AroundOrder { half_m: 150.0 });
        let sail = f.get("saildrone").unwrap().formation.unwrap();
        assert_eq!((sail.lateral_m, sail.vertical_m, sail.band), (50.0, None, Band::Surface));
        let corsair = f.get("corsair").unwrap().formation.unwrap();
        assert_eq!(corsair.lateral_m, 30.0);
        let remus = f.get("remus").unwrap().formation.unwrap();
        assert_eq!((remus.lateral_m, remus.vertical_m), (50.0, Some(5.0)));
        assert_eq!(remus.band, Band::Depth { top_m: 10.0, seabed_m: 10.0 });
        assert_eq!(remus.cruise_alt_m, Some(-40.0));
        for p in [x10, puma, sail, corsair, remus] {
            assert!(p.guess, "every figure an estimate");
        }
        // an entry that says nothing has its autopilot kind's default
        let mq9 = f.get("mq9b").unwrap().formation.unwrap();
        assert_eq!((mq9.lateral_m, mq9.band), (100.0, Band::AroundOrder { half_m: 150.0 }));
    }

    #[test]
    fn the_policy_logs_group_figures_from_rules_34_only() {
        let mut p = crate::Policy::load(&root().join("sim/policy.toml")).unwrap();
        assert!(p.group.is_none(), "sim/policy.toml has no [group]: the standing values");
        p.rules.rules_version = crate::rules::GROUP_ORDER_SINCE - 1;
        assert!(p.clone().as_logged().group.is_none());
        assert_eq!(p.rules.policy_schema(), crate::rules::SCENARIO_SETTINGS_SCHEMA_SINCE);
        p.group = Some(GroupPolicy { min_share: 0.5 });
        assert!(p.fits_rules().is_err(), "a run of older rules refuses figures it would not read");
        p.rules.rules_version = crate::rules::GROUP_ORDER_SINCE;
        assert!(p.fits_rules().is_ok());
        p.group = None;
        assert_eq!(p.clone().as_logged().group, Some(GroupPolicy::default()));
        assert_eq!(p.rules.policy_schema(), crate::rules::GROUP_ORDER_SCHEMA_SINCE);
        assert!(GroupPolicy { min_share: 0.0 }.validate().is_err());
        assert!(GroupPolicy { min_share: 1.2 }.validate().is_err());
    }
}
