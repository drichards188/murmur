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
use crate::intent::{Task, TaskKind};

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
    /// A transit (built in a later stage): n points evenly along the route, each member flying
    /// to one and holding there.
    Stations,
    /// A transit: a grid round the route's end, ceil(sqrt(n)) columns the lateral minimum apart.
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
            // the transit kind is a later stage's (docs/SWARMS.md section 13, stage 7)
            Spread::Stations | Spread::Block => false,
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
    /// re-forms filled in.
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
            reform: Some(self.reform.unwrap_or(!safety)),
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
    pub domain: Domain,
    pub fixed_wing: bool,
    pub figures: Formation,
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
}

/// Lay out `members` (the members a group's check lays out, in uid order) over `task` as `group`
/// spreads it: the slots first, then each member paired with a slot by minimum total distance
/// (ties by uid), its layer and altitude within its type's band. A pure function of its
/// arguments: the engine and replay call it on the same logged facts and get the same layout.
pub fn layout(task: &Task, group: &GroupSpec, members: &[Member]) -> Layout {
    let _ = (task, members);
    Layout {
        spread: group.spread,
        refused: Some("the layout is not built in this stage".into()),
        ..Layout::default()
    }
}

/// The members a re-form may move (docs/SWARMS.md section 4.3): those on their shares and those
/// back, but a member whose link is not healthy now keeps its slot.
pub fn movable(on_share: &BTreeSet<Uid>, back: &BTreeSet<Uid>, unhealthy: &BTreeSet<Uid>) -> BTreeSet<Uid> {
    on_share.union(back).filter(|u| !unhealthy.contains(*u)).cloned().collect()
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
