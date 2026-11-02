//! Platform facts from the catalog (`sim/platforms.toml`) and the capability table.
//!
//! The bridge copies a vehicle's [`PlatformFacts`] into each [`crate::WorldSnapshot`], so the
//! facts a rule read (armed, turn radius, endurance, a sensor's reach) are logged with the
//! decision and replay needs no catalog file. A sensor's reach is the one of the sensing model
//! the run is on ([`Reaches`], [`Catalog::reaching`]).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::intent::TaskKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Air,
    Ground,
    Surface,
    Subsurface,
}

impl Domain {
    pub fn as_str(self) -> &'static str {
        match self {
            Domain::Air => "air",
            Domain::Ground => "ground",
            Domain::Surface => "surface",
            Domain::Subsurface => "subsurface",
        }
    }
}

/// Which autopilot a vehicle runs (or imitates), which sets what it can be tasked with.
/// Mirrors `KINDS` in the Python bridge's `tasks.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutopilotKind {
    Plane,
    Vtol,
    Copter,
    Blimp,
    Boat,
    Ugv,
    Sub,
    Tracker,
    /// A drifting buoy: telemetry only, it rejects every task.
    Drifter,
}

impl AutopilotKind {
    pub fn tasks(self) -> &'static [TaskKind] {
        use TaskKind::*;
        match self {
            AutopilotKind::Plane
            | AutopilotKind::Vtol
            | AutopilotKind::Copter
            | AutopilotKind::Blimp
            | AutopilotKind::Boat
            | AutopilotKind::Ugv
            | AutopilotKind::Sub => &[Orbit, Patrol, Hold, Rtl, Engage, Identify],
            AutopilotKind::Tracker => &[Track, Hold],
            AutopilotKind::Drifter => &[],
        }
    }

    pub fn supports(self, kind: TaskKind) -> bool {
        self.tasks().contains(&kind)
    }

    /// Fixed-wing kinds fly orbits as a loiter circle with a physical minimum radius.
    pub fn is_fixed_wing(self) -> bool {
        matches!(self, AutopilotKind::Plane | AutopilotKind::Vtol)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AutopilotKind::Plane => "plane",
            AutopilotKind::Vtol => "vtol",
            AutopilotKind::Copter => "copter",
            AutopilotKind::Blimp => "blimp",
            AutopilotKind::Boat => "boat",
            AutopilotKind::Ugv => "ugv",
            AutopilotKind::Sub => "sub",
            AutopilotKind::Tracker => "tracker",
            AutopilotKind::Drifter => "drifter",
        }
    }

    /// From a catalog entry: SITL `vehicle` or stand-in `behavior` (as `SITL_KIND` /
    /// `STANDIN_KIND` in the Python bridge).
    pub fn from_catalog(sim: &str, vehicle_or_behavior: &str) -> Option<Self> {
        Some(match (sim, vehicle_or_behavior) {
            (_, "plane") => AutopilotKind::Plane,
            ("sitl", "vtol") => AutopilotKind::Vtol,
            ("sitl", "copter") => AutopilotKind::Copter,
            ("sitl", "blimp") => AutopilotKind::Blimp,
            ("sitl", "tracker") => AutopilotKind::Tracker,
            (_, "boat") | ("sitl", "sailboat") => AutopilotKind::Boat,
            ("sitl", "ugv") => AutopilotKind::Ugv,
            (_, "sub") => AutopilotKind::Sub,
            ("standin", "drifter") => AutopilotKind::Drifter,
            _ => return None,
        })
    }
}

/// What the decision layer needs to know about a platform type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlatformFacts {
    /// Catalog id, e.g. `"switchblade"`.
    pub id: String,
    pub domain: Domain,
    /// `armed = true` in the catalog: any task needs a human (policy).
    pub armed: bool,
    pub cruise_mps: Option<f64>,
    /// cruise speed / turn rate, rounded to metres (as the Python bridge computes it).
    pub min_turn_radius_m: Option<f64>,
    pub endurance_h: Option<f64>,
    /// The autopilot the catalog says this platform runs. The live kind comes from the
    /// vehicle's heartbeat ([`crate::VehicleFacts::autopilot`]).
    pub autopilot: Option<AutopilotKind>,
    /// The platform's sensors (`sensors = [...]` in the catalog): which of our vehicles can
    /// cover a point, for an engagement's assessment. Absent in logs written before it
    /// existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensors: Vec<SensorFact>,
    /// The catalog's climb rate, m/s (`climb_mps`): rules 6 count one of our vehicles as able
    /// to change altitude at least this fast (or `vertical_rate_mps`, the larger) when they
    /// read a keep-out in 3D. Absent in logs written before it existed and for platforms
    /// without the figure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub climb_mps: Option<f64>,
    /// The catalog's top speed, m/s (`max_mps`): rules 6 count a fixed wing of ours as able
    /// to dive at it down `FIXED_WING_DIVE_DEG`. Absent where the catalog has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mps: Option<f64>,
    /// The catalog's `expended_by_effect = true`: the platform is spent by its own effect (a
    /// loitering munition, a one-way strike drone). Rules 12 close what is owed back to it
    /// right after its engagement's release (`restore.closed`, `expended`): there is no
    /// platform left to put its own lost-link values back on. Absent (false) elsewhere and in
    /// older logs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expended_by_effect: bool,
    /// Rules 20: the munition the platform launches at an engagement's release (the catalog's
    /// `munition = { name, speed_mps, range_m }`), which flies to the effect point and strikes
    /// there a time of flight later ([`crate::engage::shot`]). Absent for a platform that
    /// carries none and for one spent by its own effect (a loitering munition is its own), and
    /// in older logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub munition: Option<MunitionFacts>,
    /// Rules 24 ([`crate::rules::LOADOUT_SINCE`]): the weapons the platform carries (the
    /// catalog's `loadout`), in the catalog's order, which is the order the engine picks one in
    /// when a nomination names none ([`crate::loadout::choose`]). From rules 24 the loadout
    /// replaces `munition`: the weapon a nomination uses is one of these, its rounds are counted,
    /// and a missile, bomb or torpedo flies as rules 20's munition did, with its own figures.
    /// Absent for a platform that carries none, and in older logs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loadout: Vec<WeaponFacts>,
}

/// Rules 24: what kind of weapon a loadout entry is. Missiles, bombs and torpedoes fly from the
/// platform to the effect point (rules 20's straight slant line at a constant speed, with the
/// weapon's own speed and ranges); `self` is the platform itself, a one-way strike (a loitering
/// munition, an FPV drone, an explosive boat), spent by its own effect as rules 12 already
/// counts it. Guns (bd murmur-lbw8.3) are usable from rules 26 ([`crate::rules::GUN_SINCE`]):
/// a gun fires bursts from its counted magazine through a firing window (`crate::engage`).
/// Mines (bd murmur-lbw8.4) are usable from rules 27 ([`crate::rules::MINE_SINCE`]): a lay order
/// lays them ([`crate::mines`]); an engagement never releases one at a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeaponKind {
    Missile,
    Bomb,
    Torpedo,
    #[serde(rename = "self")]
    SelfStrike,
    Gun,
    Mine,
}

impl WeaponKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WeaponKind::Missile => "missile",
            WeaponKind::Bomb => "bomb",
            WeaponKind::Torpedo => "torpedo",
            WeaponKind::SelfStrike => "self",
            WeaponKind::Gun => "gun",
            WeaponKind::Mine => "mine",
        }
    }

    /// Whether it can be used under the current rules ([`crate::rules::RULES_VERSION`]):
    /// missiles, bombs, torpedoes, the platform itself, (rules 26) guns and (rules 27) mines, which
    /// a lay order lays.
    pub fn usable(self) -> bool {
        self.usable_at(
            crate::rules::RULES_VERSION >= crate::rules::GUN_SINCE,
            crate::rules::RULES_VERSION >= crate::rules::MINE_SINCE,
        )
    }

    /// Whether it can be used under rules that do (`guns`: rules 26 on; `mines`: rules 27 on) or
    /// do not take guns and mines.
    pub fn usable_at(self, guns: bool, mines: bool) -> bool {
        self.inert_why(guns, mines).is_none()
    }

    /// Whether it flies from the platform to the effect point (rules 20's flight): a missile, a
    /// bomb or a torpedo. The platform itself goes there (`self`: released within `arrival_m`,
    /// as a platform without a munition always was).
    pub fn flies(self) -> bool {
        matches!(self, WeaponKind::Missile | WeaponKind::Bomb | WeaponKind::Torpedo)
    }

    /// Whether its rounds are counted per release ([`crate::loadout`]): every kind but `self`,
    /// whose one round is the platform (spent and un-spent with it, rules 12 and 15).
    pub fn counted(self) -> bool {
        self != WeaponKind::SelfStrike
    }

    /// Why it can't be used yet, if it can't (`guns`: under rules 26 on, which take guns;
    /// `mines`: under rules 27 on, which take mines).
    pub fn inert_why(self, guns: bool, mines: bool) -> Option<&'static str> {
        match self {
            WeaponKind::Gun if !guns => Some("guns are listed in the loadout but not usable before their stage (bd murmur-lbw8.3)"),
            WeaponKind::Mine if !mines => Some("mines are listed in the loadout but not usable before their stage (bd murmur-lbw8.4)"),
            _ => None,
        }
    }
}

/// Rules 24: how a weapon flies to its target, as the public record describes it. Logged with
/// the weapon; stage 1 flies every one the same way (rules 20's straight slant line at the
/// weapon's average speed), so the profile is a fact for the reader and for later stages, not
/// yet a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlightProfile {
    /// A direct line to the target (a missile's direct-attack mode, an FPV's run in).
    Direct,
    /// Climbs after launch and dives on the target (a lofted or top-attack mode).
    Lofted,
    /// A guided bomb's unpowered glide.
    Glide,
    /// An unguided fall, or a gun's round.
    Ballistic,
    /// A loitering munition's terminal dive.
    Dive,
    /// An anti-ship missile low over the water.
    SeaSkimming,
    /// A surface or underwater run (an explosive boat, a torpedo).
    Waterborne,
    /// A mine, laid and waiting.
    Laid,
}

/// Rules 24: a warhead's class (blast-fragmentation, shaped charge, ...) and its mass, kg, as
/// published. Logged and shown; stage 2 (bd murmur-lbw8.2) reads it for damage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Warhead {
    pub class: String,
    pub mass_kg: f64,
}

/// A weapon's figures against one kind of target (the catalog's `effects`, keyed by a contact
/// kind such as `small_fast_boat`; [`target_kind`] maps the other kinds onto these): the chance of
/// a hit on one shot in a valid engagement (day, clear, sea state 3 or less), and given a hit the
/// chance of each damage, `p_kill` destroyed, `p_mobility` its propulsion lost, `p_firepower` its
/// weapons out of action (armed targets only); the rest of a hit leaves it undamaged. Public,
/// game-level figures (docs/research/weapons.md section 10, each [E] or sourced in the catalog).
/// Rules 24 listed the key, inert; from stage 2 (bd murmur-lbw8.2) the truth side draws on it
/// (crates/murmur-contacts `damage`) and, from rules 25 ([`crate::rules::DAMAGE_SINCE`]), the
/// weapon check shows it. The engine decides nothing on it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponEffect {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_hit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_kill: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_mobility: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_firepower: Option<f64>,
}

/// Contact kinds the catalog's `effects` do not name, and the kind whose figures stand for each
/// (docs/research/weapons.md 10.3 and 10.4 give rows for a small fast boat, a fishing boat, a
/// sailboat, a cargo ship, a car, a truck, a small drone, a light aircraft, a UUV and a person):
/// each an estimate [E] by size and speed. The truth side keeps the same table
/// (crates/murmur-contacts `damage::KIND_ALIASES`; crates/murmur-headless tests that they agree).
pub const KIND_ALIASES: &[(&str, &str)] = &[
    // a tanker is a cargo ship's size and speed
    ("tanker", "cargo_ship"),
    // a bay ferry of hundreds of tonnes is nearer a ship than a 20 m fishing boat for a 9 kg warhead
    ("ferry", "cargo_ship"),
    // small, fast and armed
    ("patrol_boat", "small_fast_boat"),
    ("helicopter", "light_aircraft"),
    ("unknown_uas", "small_drone"),
    // no row for a crewed submarine: the UUV's (only torpedoes reach either)
    ("submarine", "uuv"),
    ("periscope", "uuv"),
    ("diver", "person"),
];

/// The kind whose figures stand for a contact of `kind` ([`KIND_ALIASES`]; itself otherwise).
pub fn target_kind(kind: &str) -> &str {
    KIND_ALIASES.iter().find(|(k, _)| *k == kind).map_or(kind, |(_, to)| to)
}

/// Rules 24 ([`crate::rules::LOADOUT_SINCE`]): one weapon of a platform's loadout, as the
/// catalog gives it and every decision logs it (a platform fact): its name (the one a nomination
/// names), kind, how many rounds the platform carries, its minimum and maximum range (the slant
/// distance from the launch to the effect point), its average speed, flight profile, guidance,
/// warhead and the kinds of target it is for (domains). The optional fields are the later
/// stages' and inert now: `effects` (stage 2: hit and kill chances by target kind),
/// `burst_rounds` and `rate_rpm` (stage 3: a gun's bursts from its round count), `persist_h` and
/// `trigger` (stage 4: a mine laid and waiting).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponFacts {
    pub name: String,
    pub kind: WeaponKind,
    pub count: u32,
    pub min_range_m: f64,
    pub max_range_m: f64,
    pub speed_mps: f64,
    pub profile: FlightProfile,
    pub guidance: String,
    pub warhead: Warhead,
    pub targets: Vec<Domain>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, WeaponEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_rpm: Option<f64>,
    /// Rules 26: a gun's seconds from one burst to the next (the rate it holds through a firing
    /// window, not its cyclic rate, `rate_rpm`, which a burst fires at).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_interval_s: Option<f64>,
    /// Rules 27: a mine's longest live period, hours: a lay order sets its own period, at most
    /// this (the hours after which every mine of it is inert).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_h: Option<f64>,
    /// Rules 27: what sets a mine off, in words (`"pressure 150-550 kg"`, `"acoustic"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Rules 27: how far from a mine a contact of its target domains sets it off, m (a pressure
    /// mine's the wheel or track over it; an acoustic one's how far it hears a target to launch
    /// at). Absent: its `max_range_m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_m: Option<f64>,
    /// Rules 27: how long after it is laid a mine arms, s (its layer is clear of it by then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm_s: Option<f64>,
}

impl WeaponFacts {
    /// Whether it is meant for a target in `domain`.
    pub fn for_target(&self, domain: Domain) -> bool {
        self.targets.contains(&domain)
    }

    /// The facts rules 20's flight reads ([`crate::engage::shot`]): its name, speed and maximum
    /// range.
    pub fn munition(&self) -> MunitionFacts {
        MunitionFacts {
            name: self.name.clone(),
            speed_mps: self.speed_mps,
            range_m: self.max_range_m,
        }
    }

    /// Why the catalog's entry can't stand, if it can't.
    fn invalid(&self) -> Option<String> {
        let n = &self.name;
        if n.trim().is_empty() {
            return Some("a weapon has no name".into());
        }
        if self.count == 0 {
            return Some(format!("{n}: count is at least 1"));
        }
        if !(self.min_range_m.is_finite()
            && self.min_range_m >= 0.0
            && self.max_range_m.is_finite()
            && self.max_range_m > self.min_range_m)
        {
            return Some(format!(
                "{n}: ranges are 0 <= min_range_m < max_range_m (got {} m, {} m)",
                self.min_range_m, self.max_range_m
            ));
        }
        let still = self.kind == WeaponKind::Mine;
        if !(self.speed_mps.is_finite() && (self.speed_mps > 0.0 || (still && self.speed_mps == 0.0))) {
            return Some(format!("{n}: speed_mps is above 0 (0 for a mine), got {}", self.speed_mps));
        }
        if self.kind == WeaponKind::SelfStrike && self.count != 1 {
            return Some(format!("{n}: a self strike is the platform itself, count 1"));
        }
        if self.targets.is_empty() {
            return Some(format!("{n}: targets names at least one domain"));
        }
        // rules 26: a gun fires bursts: their rounds, its cyclic rate and the time between them
        if self.kind == WeaponKind::Gun {
            let rounds_ok = self.burst_rounds.is_some_and(|b| b >= 1 && b <= self.count);
            let rate_ok = self.rate_rpm.is_some_and(|r| r.is_finite() && r > 0.0);
            let every_ok = self.burst_interval_s.is_some_and(|i| i.is_finite() && i >= 1.0);
            if !(rounds_ok && rate_ok && every_ok) {
                return Some(format!(
                    "{n}: a gun names burst_rounds (1 to its count), rate_rpm (above 0) and burst_interval_s (1 s or more)"
                ));
            }
        }
        // rules 27: a mine names its longest live period, how soon it arms, what sets it off and
        // how near
        if self.kind == WeaponKind::Mine {
            let persist_ok = self.persist_h.is_some_and(|h| h.is_finite() && h > 0.0);
            let arm_ok = self.arm_s.is_some_and(|a| a.is_finite() && a >= 0.0);
            let trigger_ok = self.trigger.as_ref().is_some_and(|t| !t.trim().is_empty());
            let reach_ok = self.trigger_m.is_none_or(|r| r.is_finite() && r > 0.0 && r <= self.max_range_m);
            if !(persist_ok && arm_ok && trigger_ok && reach_ok) {
                return Some(format!(
                    "{n}: a mine names persist_h (above 0), arm_s (0 or more), trigger, and trigger_m (above 0, at most max_range_m) if any"
                ));
            }
        }
        if self.guidance.trim().is_empty() || self.warhead.class.trim().is_empty() {
            return Some(format!("{n}: guidance and warhead.class are named"));
        }
        if !(self.warhead.mass_kg.is_finite() && self.warhead.mass_kg >= 0.0) {
            return Some(format!("{n}: warhead.mass_kg is 0 or more"));
        }
        for (k, e) in &self.effects {
            for p in [e.p_hit, e.p_kill, e.p_mobility, e.p_firepower].into_iter().flatten() {
                if !(0.0..=1.0).contains(&p) {
                    return Some(format!("{n}: effects.{k}: a chance is between 0 and 1, got {p}"));
                }
            }
            let given_hit: f64 = [e.p_kill, e.p_mobility, e.p_firepower].into_iter().flatten().sum();
            if given_hit > 1.0 + 1e-9 {
                return Some(format!(
                    "{n}: effects.{k}: p_kill, p_mobility and p_firepower are shares of a hit, summing to 1 at most (got {given_hit})"
                ));
            }
        }
        None
    }

    /// Rules 26: the rounds a gun's next burst fires with `left` rounds in its magazine: its
    /// `burst_rounds`, or what is left for the last; none for any other kind.
    pub fn burst_of(&self, left: u32) -> Option<u32> {
        (self.kind == WeaponKind::Gun).then(|| self.burst_rounds.unwrap_or(1).max(1).min(left))
    }

    /// Rules 26: how long a gun's burst of `rounds` lasts at its cyclic rate, s.
    pub fn burst_duration_s(&self, rounds: u32) -> f64 {
        self.rate_rpm.filter(|r| *r > 0.0).map_or(0.0, |r| f64::from(rounds) * 60.0 / r)
    }

    /// Rules 26: a gun's seconds between bursts (1 s at least, its catalog figure).
    pub fn burst_every_s(&self) -> f64 {
        self.burst_interval_s.unwrap_or(1.0).max(1.0)
    }

    /// Rules 27: how near a mine a contact of its target domains sets it off, m (`trigger_m`, else
    /// `max_range_m`).
    pub fn trigger_reach_m(&self) -> f64 {
        self.trigger_m.unwrap_or(self.max_range_m)
    }

    /// Rules 27: how long after it is laid a mine arms, ms.
    pub fn arm_ms(&self) -> i64 {
        (self.arm_s.unwrap_or(0.0).max(0.0) * 1000.0).round() as i64
    }

    /// Rules 25: its figures against a target of `kind` (a contact kind, as the tracker estimates
    /// it), through [`target_kind`]: the key they are listed under and the figures, if listed.
    pub fn effect_for(&self, kind: &str) -> Option<(&str, &WeaponEffect)> {
        let k = target_kind(kind);
        self.effects.get_key_value(k).map(|(k, e)| (k.as_str(), e))
    }
}

/// Rules 20: a munition from the catalog: what it is called, how fast it flies (a constant
/// speed along a straight slant line from the platform to the effect point) and how far, the
/// slant distance from launch to the effect point it reaches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MunitionFacts {
    pub name: String,
    pub speed_mps: f64,
    pub range_m: f64,
}

/// One sensor from the catalog: its type (`radar`, `eo_ir`, `ais`, `sonar`, `esm`) and its
/// nominal range (where a nominal contact is detected half the time).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SensorFact {
    #[serde(rename = "type")]
    pub kind: String,
    /// The legacy sensing model's reach (and the engine's in a run on that model).
    pub range_m: f64,
    /// In a run on the physical sensing model, the engine's reach by the domain of what it
    /// looks for: the catalog's `physical_reach_m`, derived from the physical model
    /// (docs/SENSING.md, "The decision engine's reaches"; [`Catalog::reaching`]). Absent in a
    /// run on the legacy model, for a sensor the catalog derives none for, and in every log
    /// before it: the engine reads `range_m` then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reach_m: Option<SensorReach>,
}

impl SensorFact {
    /// How far this sensor reaches a contact in `domain`, m: its physical reach for the domain
    /// when it carries them (none: it does not reach one there at all), else `range_m`.
    pub fn reach_for(&self, domain: Domain) -> Option<f64> {
        match &self.reach_m {
            Some(r) => r.get(domain),
            None => Some(self.range_m),
        }
    }
}

/// A sensor's reach by the domain of the contact, m (none: it does not reach one there).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SensorReach {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ground: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsurface: Option<f64>,
}

impl SensorReach {
    pub fn get(&self, domain: Domain) -> Option<f64> {
        match domain {
            Domain::Air => self.air,
            Domain::Ground => self.ground,
            Domain::Surface => self.surface,
            Domain::Subsurface => self.subsurface,
        }
    }
}

/// Which reach of each sensor the decision facts carry: the sensing model the run is on
/// (bd murmur-1u4). The engine reads whatever the facts carry, and the facts are logged with
/// each decision, so replay needs neither the catalog nor the model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reaches {
    /// `range_m` alone, as every record before world v3 (v1, v2, the Phase G stacks).
    #[default]
    Legacy,
    /// `range_m` and the physical model's reaches by domain (`physical_reach_m`).
    Physical,
}

#[derive(Deserialize)]
struct RawPlatform {
    domain: Domain,
    sim: String,
    vehicle: Option<String>,
    behavior: Option<String>,
    #[serde(default)]
    armed: bool,
    cruise_mps: Option<f64>,
    turn_dps: Option<f64>,
    endurance_h: Option<f64>,
    climb_mps: Option<f64>,
    max_mps: Option<f64>,
    #[serde(default)]
    expended_by_effect: bool,
    munition: Option<MunitionFacts>,
    #[serde(default)]
    loadout: Vec<WeaponFacts>,
    #[serde(default)]
    sensors: Vec<RawSensor>,
    /// Each sensor's reach under the physical model, by its type and the contact's domain
    /// (generated: docs/SENSING.md, "The decision engine's reaches").
    #[serde(default)]
    physical_reach_m: BTreeMap<String, SensorReach>,
}

#[derive(Deserialize)]
struct RawSensor {
    #[serde(rename = "type")]
    kind: String,
    range_m: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("platform catalog: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("platform catalog {0}: {1}")]
    Io(String, std::io::Error),
    #[error("platform catalog: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    /// Each platform's facts with `range_m` alone ([`Reaches::Legacy`]).
    pub platforms: BTreeMap<String, PlatformFacts>,
    /// Each platform's `physical_reach_m`, by sensor type: what [`Catalog::reaching`] puts
    /// on its sensors for a run on the physical model.
    physical_reach: BTreeMap<String, BTreeMap<String, SensorReach>>,
}

impl Catalog {
    pub fn from_toml(text: &str) -> Result<Self, CatalogError> {
        let mut raw: BTreeMap<String, RawPlatform> = toml::from_str(text)?;
        let mut physical_reach = BTreeMap::new();
        for (id, r) in &mut raw {
            for (kind, reach) in &r.physical_reach_m {
                if !r.sensors.iter().any(|s| s.kind == *kind) {
                    return Err(CatalogError::Invalid(format!(
                        "[{id}] physical_reach_m.{kind}: the platform carries no {kind}"
                    )));
                }
                for d in [Domain::Air, Domain::Ground, Domain::Surface, Domain::Subsurface] {
                    if let Some(m) = reach.get(d)
                        && !(m.is_finite() && m > 0.0)
                    {
                        return Err(CatalogError::Invalid(format!(
                            "[{id}] physical_reach_m.{kind}.{}: {m} m (a reach is above 0; leave out a domain it does not reach)",
                            format!("{d:?}").to_lowercase()
                        )));
                    }
                }
            }
            if let Some(m) = &r.munition {
                if !(m.speed_mps.is_finite() && m.speed_mps > 0.0 && m.range_m.is_finite() && m.range_m > 0.0) {
                    return Err(CatalogError::Invalid(format!(
                        "[{id}] munition: speed_mps and range_m are above 0 (got {} m/s, {} m)",
                        m.speed_mps, m.range_m
                    )));
                }
                if r.expended_by_effect {
                    return Err(CatalogError::Invalid(format!(
                        "[{id}] munition: a platform spent by its own effect is its own munition"
                    )));
                }
            }
            // rules 24: each weapon whole and named once; the platform itself only on a platform
            // spent by its own effect, and once
            let mut names = std::collections::BTreeSet::new();
            for w in &r.loadout {
                if let Some(why) = w.invalid() {
                    return Err(CatalogError::Invalid(format!("[{id}] loadout: {why}")));
                }
                if !names.insert(w.name.as_str()) {
                    return Err(CatalogError::Invalid(format!("[{id}] loadout: {} is named twice", w.name)));
                }
            }
            let selves = r.loadout.iter().filter(|w| w.kind == WeaponKind::SelfStrike).count();
            if selves > 1 || (selves == 1 && !r.expended_by_effect) {
                return Err(CatalogError::Invalid(format!(
                    "[{id}] loadout: a self strike is one, on a platform spent by its own effect (expended_by_effect)"
                )));
            }
            let reach = std::mem::take(&mut r.physical_reach_m);
            if !reach.is_empty() {
                physical_reach.insert(id.clone(), reach);
            }
        }
        let platforms = raw
            .into_iter()
            .map(|(id, r)| {
                let min_turn_radius_m = match (r.cruise_mps, r.turn_dps) {
                    (Some(v), Some(dps)) if v > 0.0 && dps > 0.0 => {
                        Some((v / dps.to_radians()).round())
                    }
                    _ => None,
                };
                let which = r.vehicle.as_deref().or(r.behavior.as_deref()).unwrap_or("");
                let facts = PlatformFacts {
                    id: id.clone(),
                    domain: r.domain,
                    armed: r.armed,
                    cruise_mps: r.cruise_mps,
                    min_turn_radius_m,
                    endurance_h: r.endurance_h,
                    autopilot: AutopilotKind::from_catalog(&r.sim, which),
                    sensors: r
                        .sensors
                        .into_iter()
                        .map(|s| SensorFact {
                            kind: s.kind,
                            range_m: s.range_m,
                            reach_m: None,
                        })
                        .collect(),
                    climb_mps: r.climb_mps.filter(|c| c.is_finite() && *c > 0.0),
                    max_mps: r.max_mps.filter(|c| c.is_finite() && *c > 0.0),
                    expended_by_effect: r.expended_by_effect,
                    munition: r.munition,
                    loadout: r.loadout,
                };
                (id, facts)
            })
            .collect();
        Ok(Catalog {
            platforms,
            physical_reach,
        })
    }

    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| CatalogError::Io(path.display().to_string(), e))?;
        Self::from_toml(&text)
    }

    pub fn get(&self, id: &str) -> Option<&PlatformFacts> {
        self.platforms.get(id)
    }

    /// This catalog with its sensors' facts for a run on `reaches`' sensing model: on the
    /// legacy one `range_m` alone, as the catalog is loaded (its facts, and so every decision
    /// logged with them, as before the physical reaches existed); on the physical one each
    /// sensor the catalog derives reaches for carries them too (`reach_m`), which the engine
    /// then reads instead of `range_m` ([`SensorFact::reach_for`]).
    pub fn reaching(&self, reaches: Reaches) -> Catalog {
        let mut c = self.clone();
        for (id, p) in &mut c.platforms {
            for s in &mut p.sensors {
                s.reach_m = match reaches {
                    Reaches::Legacy => None,
                    Reaches::Physical => self
                        .physical_reach
                        .get(id)
                        .and_then(|by_kind| by_kind.get(&s.kind))
                        .cloned(),
                };
            }
        }
        c
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const TEXT: &str = r#"
[usv]
domain = "surface"
sim = "standin"
behavior = "boat"
sensors = [{ type = "radar", range_m = 40000 }, { type = "ais", range_m = 50000 }, { type = "sonar", range_m = 10000 }]
physical_reach_m = { radar = { air = 7530, surface = 9770 }, sonar = {} }
"#;

    /// Loaded, the facts carry `range_m` alone and serialize as every log before the physical
    /// reaches; on the physical model each sensor with derived reaches carries them, and one
    /// without (AIS) none.
    #[test]
    fn the_legacy_facts_are_as_before_and_the_physical_ones_carry_the_reaches() {
        let c = Catalog::from_toml(TEXT).unwrap();
        assert_eq!(c, c.reaching(Reaches::Legacy));
        let radar = &c.get("usv").unwrap().sensors[0];
        assert_eq!(serde_json::to_string(radar).unwrap(), r#"{"type":"radar","range_m":40000.0}"#);
        assert_eq!(radar.reach_for(Domain::Surface), Some(40_000.0));
        assert_eq!(radar.reach_for(Domain::Ground), Some(40_000.0));

        let p = c.reaching(Reaches::Physical);
        let s = &p.get("usv").unwrap().sensors;
        assert_eq!(s[0].reach_for(Domain::Surface), Some(9_770.0));
        assert_eq!(s[0].reach_for(Domain::Air), Some(7_530.0));
        // a domain it is given no reach in: it does not reach a contact there
        assert_eq!(s[0].reach_for(Domain::Ground), None);
        assert_eq!(s[1].reach_m, None);
        assert_eq!(s[1].reach_for(Domain::Surface), Some(50_000.0));
        assert_eq!(s[2].reach_m, Some(SensorReach::default()));
        assert_eq!(s[2].reach_for(Domain::Subsurface), None);
        // logged and read back as it was
        let json = serde_json::to_string(&s[0]).unwrap();
        assert_eq!(json, r#"{"type":"radar","range_m":40000.0,"reach_m":{"air":7530.0,"surface":9770.0}}"#);
        assert_eq!(serde_json::from_str::<SensorFact>(&json).unwrap(), s[0]);
        // and back to the legacy facts
        assert_eq!(p.reaching(Reaches::Legacy), c);
    }

    /// A reach for a sensor the platform does not carry, a reach of 0 and a misspelt domain are
    /// refused.
    #[test]
    fn a_bad_physical_reach_is_refused() {
        for (bad, why) in [
            ("physical_reach_m = { eo_ir = { air = 10 } }", "carries no eo_ir"),
            ("physical_reach_m = { radar = { air = 0 } }", "a reach is above 0"),
            ("physical_reach_m = { radar = { surfce = 10 } }", "surfce"),
        ] {
            let text = TEXT.replace(
                "physical_reach_m = { radar = { air = 7530, surface = 9770 }, sonar = {} }",
                bad,
            );
            let e = Catalog::from_toml(&text).unwrap_err().to_string();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }
}
