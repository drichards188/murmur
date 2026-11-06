//! The damage model on the truth side (bd murmur-lbw8.2, DECISIONS.md "Warheads and damage"):
//! what a weapon's strike does to what it strikes. Game-level, in the manner of Command: Modern
//! Operations for the draw and War Thunder for the outcome (docs/research/weapons.md section 10):
//!
//! - **A hit draw.** The weapon's chance of a hit against the target's kind (its figures, the
//!   catalog's `effects` for our weapons or the scenario's for a contact's), times the
//!   conditions' multipliers where the research gives a basis (night and fog by guidance, a
//!   rough sea against a small boat, the missile's range over the ground), less points for a
//!   fast target
//!   ([`hit_chance`]); each factor is recorded with the draw.
//! - **A damage draw**, given a hit: destroyed, its propulsion lost (`mobility`), its weapons
//!   out of action (`firepower`, armed targets only) or undamaged, by the figures' shares
//!   ([`outcome`]).
//! - **Collateral.** Anything else within the warhead's blast radius ([`blast_radius_m`]) draws
//!   too: caught with a chance falling linearly from 1 at the point to 0 at the radius, then the
//!   damage draw as for a hit.
//!
//! Every draw is a uniform from the run's seed and a stream named by the strike (the intent of
//! our effect, the act of a contact's weapon) and what it is for ([`uniform`]): the same scenario
//! and seed give the same outcomes, live and headless, whatever else happens in the run. The
//! record of each draw goes to the evaluation truth log only; the C2 learns of a result only
//! through its sensors (a contact that stops, or is no longer seen) and our vehicles' own reports.
//!
//! A contact whose scenario says `strike_outcome` (a training aid, the tutorial's boat only:
//! DECISIONS.md 11-03) draws nothing when a strike is aimed at it: the outcome its scenario names
//! is recorded, marked `pinned` ([`pinned`]). Collateral around it is drawn as ever.
//!
//! A weapon without figures (`effects` empty) does none of this: its effect stays abstract, as
//! before (crates/murmur-contacts `effect::apply`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::scenario;

/// What a strike left of what it struck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DamageState {
    Undamaged,
    /// Its propulsion is lost: a boat drifts, a ground vehicle stands, an aircraft is slowed
    /// (a contact) or comes down (ours).
    Mobility,
    /// Its weapons are out of action (an armed target only).
    Firepower,
    /// Out of play: sunk, burnt out, shot down.
    Destroyed,
}

impl DamageState {
    pub fn as_str(self) -> &'static str {
        match self {
            DamageState::Undamaged => "undamaged",
            DamageState::Mobility => "mobility",
            DamageState::Firepower => "firepower",
            DamageState::Destroyed => "destroyed",
        }
    }
}

/// A weapon's figures against one kind of target (the catalog's `WeaponEffect`): the chance of
/// a hit, and given a hit the shares destroyed, propulsion lost and weapons out; the rest of a
/// hit does no damage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Figures {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_hit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_kill: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_mobility: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_firepower: Option<f64>,
}

impl Figures {
    fn check(&self) -> Result<(), String> {
        for p in [self.p_hit, self.p_kill, self.p_mobility, self.p_firepower].into_iter().flatten() {
            if !(0.0..=1.0).contains(&p) {
                return Err(format!("a chance is between 0 and 1, got {p}"));
            }
        }
        let given: f64 = [self.p_kill, self.p_mobility, self.p_firepower].into_iter().flatten().sum();
        if given > 1.0 + 1e-9 {
            return Err(format!("p_kill, p_mobility and p_firepower sum to 1 at most, got {given}"));
        }
        Ok(())
    }
}

/// A warhead as the catalog gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Warhead {
    #[serde(default)]
    pub class: String,
    pub mass_kg: f64,
}

/// A weapon as the truth side reads it: what the bridge hands over with our effect (the catalog's
/// `WeaponFacts`, whose other keys are ignored here) or what a contact's weapon spec says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeaponDamage {
    pub name: String,
    /// missile, bomb, torpedo, self (the platform itself), or a contact weapon's kind (`atgm`).
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub guidance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warhead: Option<Warhead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_range_m: Option<f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, Figures>,
}

impl WeaponDamage {
    /// Whether its strike is drawn and does damage (it lists figures); else it stays abstract.
    pub fn modelled(&self) -> bool {
        !self.effects.is_empty()
    }

    /// Its figures against a target of contact kind `kind` ([`target_kind`]): the key they are
    /// listed under and the figures, if any.
    pub fn figures_for(&self, kind: &str) -> Option<(&str, &Figures)> {
        let k = target_kind(kind);
        self.effects.get_key_value(k).map(|(k, f)| (k.as_str(), f))
    }

    /// A missile-like weapon (the range and speed rules apply): a missile, or a contact's ATGM.
    fn missile(&self) -> bool {
        matches!(self.kind.as_str(), "missile" | "atgm")
    }

    /// Rules 26: a gun (each burst a shot).
    fn gun(&self) -> bool {
        self.kind == "gun"
    }

    /// Its figures' errors, if any.
    pub fn check(&self) -> Result<(), String> {
        for (k, f) in &self.effects {
            f.check().map_err(|e| format!("{}: effects.{k}: {e}", self.name))?;
        }
        Ok(())
    }
}

/// Contact kinds the figures do not name, and the kind whose figures stand for each: the decision
/// core's `catalog::KIND_ALIASES`, the same table (crates/murmur-headless tests that they agree).
pub const KIND_ALIASES: &[(&str, &str)] = &[
    ("tanker", "cargo_ship"),
    ("ferry", "cargo_ship"),
    ("patrol_boat", "small_fast_boat"),
    ("helicopter", "light_aircraft"),
    ("unknown_uas", "small_drone"),
    ("submarine", "uuv"),
    ("periscope", "uuv"),
    ("diver", "person"),
];

/// The kind whose figures stand for a contact of `kind`.
pub fn target_kind(kind: &str) -> &str {
    KIND_ALIASES.iter().find(|(k, _)| *k == kind).map_or(kind, |(_, to)| to)
}

/// The kind whose figures stand for one of our vehicles, by its catalog platform and domain [E,
/// by size and speed]: the large aircraft a light aircraft, the rest of the air a small drone; the
/// Sea Hunter (40 m, some 145 t) a fishing boat, the Saildrone and a buoy a sailboat, the other
/// boats a small fast boat; a ground vehicle a car (a UGV of one or two tonnes); under water a UUV.
pub fn ours_kind(platform: Option<&str>, domain: Option<&str>) -> &'static str {
    let p = platform.unwrap_or("");
    if p.starts_with("mq9b") || matches!(p, "rq4" | "cca" | "fvr90" | "lta") {
        return "light_aircraft";
    }
    match p {
        "seahunter" => return "fishing_vessel",
        "saildrone" | "buoy" => return "sailboat",
        _ => {}
    }
    match domain.unwrap_or("") {
        "air" => "small_drone",
        "surface" => "small_fast_boat",
        "ground" => "vehicle",
        "subsurface" => "uuv",
        _ => "small_drone",
    }
}

/// Small surface targets, which a rough sea masks (docs/research/weapons.md 10.5).
fn small_boat(kind: &str) -> bool {
    matches!(target_kind(kind), "small_fast_boat" | "sailboat" | "fishing_vessel")
}

/// The blast radius of a warhead of `kg`, m, inside which anything else draws for collateral
/// damage: 4 m/kg^(1/3) [E: blast scales with the cube root of the charge (Hopkinson-Cranz, a
/// textbook law); the constant is chosen so a Hellfire's ~9 kg damages within about 8 m and a
/// Magura's 300 kg within about 27 m, the sizes of a small boat's and a ship's danger zones].
pub fn blast_radius_m(kg: f64) -> f64 {
    4.0 * kg.max(0.0).cbrt()
}

/// The conditions a strike happens in, as the truth side has them: the scenario's
/// `[conditions]` (a clear day, sea state 2 where it says nothing, as the bridge's sensing model
/// fills them) and the sun from the clock and the place.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Conditions {
    pub visibility_m: f64,
    pub sea_state: f64,
    pub sun_elevation_deg: f64,
}

impl Conditions {
    /// The conditions at unix time `t` at (`lat`, `lon`) under the scenario's `c`.
    pub fn at(c: Option<&scenario::Conditions>, t: f64, lat: f64, lon: f64) -> Conditions {
        Conditions {
            visibility_m: c.and_then(|c| c.visibility_m).unwrap_or(20_000.0),
            sea_state: c.and_then(|c| c.sea_state).unwrap_or(2.0),
            sun_elevation_deg: c
                .and_then(|c| c.sun_elevation_deg)
                .unwrap_or_else(|| sun_elevation_deg(t, lat, lon)),
        }
    }
}

/// The sun's elevation above the horizon, degrees, at unix time `t` and a place: the
/// Astronomical Almanac's low-precision solar position, the bridge's own
/// (`tracks::physical::conditions::sun_elevation_deg`, the same formula).
pub fn sun_elevation_deg(t: f64, lat: f64, lon: f64) -> f64 {
    let d = t / 86_400.0 + 2_440_587.5 - 2_451_545.0;
    let g = (357.528 + 0.985_600_3 * d).to_radians();
    let l = 280.460 + 0.985_647_4 * d;
    let lambda = (l + 1.915 * g.sin() + 0.020 * (2.0 * g).sin()).to_radians();
    let eps = (23.439 - 0.000_000_4 * d).to_radians();
    let ra = (eps.cos() * lambda.sin()).atan2(lambda.cos());
    let dec = (eps.sin() * lambda.sin()).asin();
    let gmst_deg = (280.460_618_37 + 360.985_647_366_29 * d).rem_euclid(360.0);
    let h = (gmst_deg + lon).to_radians() - ra;
    let phi = lat.to_radians();
    (phi.sin() * dec.sin() + phi.cos() * dec.cos() * h.cos()).asin().to_degrees()
}

/// Night: the sun more than 6 degrees below the horizon (civil twilight over).
const NIGHT_SUN_DEG: f64 = -6.0;
/// Fog: visibility under 1 km (the meteorological definition).
const FOG_M: f64 = 1000.0;

/// The hit chance's factor by night, by guidance (docs/research/weapons.md 10.5): laser, radar
/// and thermal seekers lose nothing (the MQ-9's MWIR turret designates by night [4,23]); an
/// operator's EO/IR video 0.9; a day camera 0.5; a boat's low EO camera 0.3; all [E].
fn night_factor(guidance: &str) -> f64 {
    match guidance {
        "operator_video_eo_ir" => 0.9,
        "operator_video_day" | "operator_video" => 0.5,
        "operator_video_eo_tracker" => 0.3,
        _ => 1.0,
    }
}

/// The hit chance's factor in fog, by guidance (10.5): a laser spot is lost in it (0.3 [48; E]),
/// an IR seeker or an operator's video sees half as well (0.5 [E]), a gun's stabilised sight 0.6
/// (10.5's guns [E]), radar and GPS not at all.
fn fog_factor(guidance: &str) -> f64 {
    match guidance {
        "semi_active_laser" => 0.3,
        "stabilised_sight" => 0.6,
        "imaging_infrared" | "operator_video_eo_ir" | "operator_video_day" | "operator_video" | "operator_video_eo_tracker" => 0.5,
        _ => 1.0,
    }
}

/// Points off the hit chance for each 5 m/s band of the target's speed over 15 m/s, by guidance
/// (10.5, Command's rule [52,53]: 10 for laser missiles and loitering munitions, 20 for FPVs and
/// (rules 26) a gun's stabilised sight; the band's width and the rest [E]). Bombs and radar
/// missiles: none (their figures already price a mover, or they are built for one).
fn speed_points(guidance: &str) -> f64 {
    match guidance {
        "semi_active_laser" | "imaging_infrared" | "operator_video_eo_ir" => 0.10,
        "operator_video_day" | "operator_video" | "operator_video_eo_tracker" | "stabilised_sight" => 0.20,
        _ => 0.0,
    }
}

/// One factor of a hit chance, as recorded: a multiplier (`x`) or points off (`minus`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Factor {
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minus: Option<f64>,
}

/// What a hit chance reads of the target and the shot.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// Its kind (a contact kind, or [`ours_kind`]).
    pub kind: &'a str,
    pub speed_mps: f64,
    pub armed: bool,
    /// On the water.
    pub surface: bool,
}

/// The chance that one shot of `w` hits `target` in conditions `c`, from a launch `out_m` away
/// over the ground (none: not known): the figure for its kind (none listed: it can't engage it,
/// 0), less the speed points, times the factors. Returns the figures' kind, the base chance, the
/// factors and the chance. The range rule (Command's for rocket missiles [52]: full chance to half
/// the maximum range, falling linearly to half at the maximum) reads the distance over the
/// ground, not the slant [E]: a missile released from an aircraft's height trades the height for
/// energy, so a Hellfire from 7,600 m with the point 1.5 km off is not a shot at its limit.
pub fn hit_chance(w: &WeaponDamage, target: &Target, c: &Conditions, out_m: Option<f64>) -> (Option<String>, f64, Vec<Factor>, f64) {
    let Some((key, figs)) = w.figures_for(target.kind) else {
        return (None, 0.0, Vec::new(), 0.0);
    };
    let base = figs.p_hit.unwrap_or(0.0);
    let mut factors = Vec::new();
    let mut p = base;
    // rules 26: a gun's burst too (10.5: 20 points a band for guns)
    let guided_run = w.missile() || w.kind == "self" || w.gun();
    if guided_run && target.speed_mps > 15.0 {
        let per = speed_points(&w.guidance);
        if per > 0.0 {
            let bands = ((target.speed_mps - 15.0) / 5.0).ceil();
            let minus = per * bands;
            factors.push(Factor {
                what: format!("target at {:.1} m/s, {bands:.0} band(s) of 5 m/s over 15", target.speed_mps),
                x: None,
                minus: Some(round3(minus)),
            });
            p -= minus;
        }
    }
    let mut times = |what: String, x: f64| {
        if (x - 1.0).abs() > 1e-12 {
            factors.push(Factor { what, x: Some(round3(x)), minus: None });
            p *= x;
        }
    };
    if c.sun_elevation_deg < NIGHT_SUN_DEG {
        times(format!("night (the sun {:.0} degrees below the horizon), {}", -c.sun_elevation_deg, w.guidance), night_factor(&w.guidance));
    }
    if c.visibility_m < FOG_M {
        times(format!("fog ({:.0} m visibility), {}", c.visibility_m, w.guidance), fog_factor(&w.guidance));
    }
    if target.surface && small_boat(target.kind) && c.sea_state >= 4.0 {
        let x = if c.sea_state > 5.0 {
            0.0
        } else if w.gun() {
            // 10.5: guns from a vessel or against a boat
            0.6
        } else if w.kind == "self" {
            0.8
        } else {
            0.85
        };
        times(format!("sea state {:.1} against a small boat", c.sea_state), x);
    }
    if w.missile()
        && let (Some(d), Some(max)) = (out_m, w.max_range_m)
        && max > 0.0
        && d > 0.5 * max
    {
        let f = (d / max).min(1.0);
        times(format!("launched {d:.0} m out over the ground, past half its {max:.0} m range"), 1.0 - (f - 0.5));
    }
    (Some(key.to_string()), base, factors, p.clamp(0.0, 1.0))
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The damage a hit does by figures `figs` (none: the weapon lists none for the kind, so the
/// generic split of a blast [E]: destroyed 0.3, propulsion lost 0.4, the rest undamaged) for a
/// draw `u` in [0, 1); `firepower` only for an armed target (else that share does no damage).
pub fn outcome(figs: Option<&Figures>, armed: bool, u: f64) -> DamageState {
    let (kill, mob, fp) = match figs {
        Some(f) => (f.p_kill.unwrap_or(0.0), f.p_mobility.unwrap_or(0.0), f.p_firepower.unwrap_or(0.0)),
        None => (0.3, 0.4, 0.0),
    };
    let fp = if armed { fp } else { 0.0 };
    if u < kill {
        DamageState::Destroyed
    } else if u < kill + mob {
        DamageState::Mobility
    } else if u < kill + mob + fp {
        DamageState::Firepower
    } else {
        DamageState::Undamaged
    }
}

/// A uniform in [0, 1) from the run's `seed` and a `stream` naming the draw (splitmix64 over
/// the seed and the stream's FNV-1a hash): every draw depends on nothing but the run's seed and
/// what it is for.
pub fn uniform(seed: u64, stream: &str) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in stream.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut z = seed ^ h;
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// One draw's record, for the evaluation truth log (never the C2's).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DamageRecord {
    /// A contact id, or `ours:<uid>`.
    pub who: String,
    /// `target` (the shot was at it) or `collateral` (within the blast radius).
    pub role: String,
    pub kind: String,
    /// The kind whose figures were read (none: the weapon lists none for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub figures: Option<String>,
    /// From the effect point (3D, m).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distance_m: Option<f64>,
    /// The chance it was hit (a target: the hit chance with its factors; collateral: caught in
    /// the blast) and the draw against it (a pinned record's is no draw: 0 for its hit, 1 for its
    /// miss).
    pub p_hit: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub factors: Vec<Factor>,
    pub u_hit: f64,
    pub hit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub u_damage: Option<f64>,
    pub state: DamageState,
    /// Whether the state was put on it (a contact whose scenario says `persists` keeps going;
    /// a state no worse than it had changes nothing).
    pub applied: bool,
    /// The outcome was pinned by the target's scenario (`strike_outcome`, a training aid), not
    /// drawn ([`pinned`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A shot at a target whose scenario pins every strike on it (`strike_outcome`, [`crate::scenario::StrikeOutcome`]):
/// a hit with `state`, or a miss (none), drawing nothing, and saying so (`pinned`, a factor and the
/// note) so no one reads it as a draw.
pub fn pinned(who: &str, w: &WeaponDamage, target: &Target, outcome: crate::scenario::StrikeOutcome) -> DamageRecord {
    let state = outcome.state();
    let hit = state.is_some();
    DamageRecord {
        who: who.into(),
        role: "target".into(),
        kind: target.kind.into(),
        figures: w.figures_for(target.kind).map(|(k, _)| k.to_string()),
        distance_m: None,
        p_hit: if hit { 1.0 } else { 0.0 },
        factors: vec![Factor {
            what: format!("pinned by the scenario: strike_outcome = \"{}\", not drawn", outcome.as_str()),
            x: None,
            minus: None,
        }],
        u_hit: if hit { 0.0 } else { 1.0 },
        hit,
        u_damage: None,
        state: state.unwrap_or(DamageState::Undamaged),
        applied: false,
        pinned: true,
        note: Some("its scenario pins every strike on it (a training aid): no hit or damage draw".into()),
    }
}

/// A strike on one of ours an earlier strike destroyed (bd murmur-o72k): recorded, nothing drawn
/// (`u_hit` 1, as a pinned miss's), its state still `destroyed` and nothing applied.
pub fn already_destroyed(who: &str, kind: &str, w: &WeaponDamage) -> DamageRecord {
    DamageRecord {
        who: who.into(),
        role: "target".into(),
        kind: kind.into(),
        figures: w.figures_for(kind).map(|(k, _)| k.to_string()),
        distance_m: None,
        p_hit: 0.0,
        factors: Vec::new(),
        u_hit: 1.0,
        hit: false,
        u_damage: None,
        state: DamageState::Destroyed,
        applied: false,
        pinned: false,
        note: Some("already destroyed by an earlier strike: not drawn again".into()),
    }
}

/// A shot at a target: the hit draw, then (on a hit) the damage draw, on `stream`.
pub fn shot(seed: u64, stream: &str, who: &str, w: &WeaponDamage, target: &Target, c: &Conditions, out_m: Option<f64>) -> DamageRecord {
    shot_with(seed, stream, who, w, target, c, out_m, None)
}

/// [`shot`], with the hit chance an evaluation sets in place of the figures' (`over`; the damage
/// given a hit is still the figures').
#[allow(clippy::too_many_arguments)]
pub fn shot_with(
    seed: u64,
    stream: &str,
    who: &str,
    w: &WeaponDamage,
    target: &Target,
    c: &Conditions,
    out_m: Option<f64>,
    over: Option<f64>,
) -> DamageRecord {
    let (figures, _, factors, p) = match over {
        Some(p) => (
            w.figures_for(target.kind).map(|(k, _)| k.to_string()),
            p,
            vec![Factor { what: "the evaluation's hit chance, in place of the figures'".into(), x: None, minus: None }],
            p.clamp(0.0, 1.0),
        ),
        None => hit_chance(w, target, c, out_m),
    };
    let u_hit = uniform(seed, &format!("{stream}:hit"));
    let hit = u_hit < p;
    let (u_damage, state) = if hit {
        let u = uniform(seed, &format!("{stream}:damage"));
        (Some(u), outcome(w.figures_for(target.kind).map(|(_, f)| f), target.armed, u))
    } else {
        (None, DamageState::Undamaged)
    };
    DamageRecord {
        who: who.into(),
        role: "target".into(),
        kind: target.kind.into(),
        note: figures.is_none().then(|| format!("{} lists no figures against a {}: it can't engage one", w.name, target.kind)),
        figures,
        distance_m: None,
        p_hit: round3(p),
        factors,
        u_hit,
        hit,
        u_damage,
        state,
        applied: false,
        pinned: false,
    }
}

/// Something else `d` m from a blast of `w` (collateral), on `stream`: caught with a chance
/// falling from 1 at the point to 0 at the blast radius, then the damage draw. None beyond the
/// radius.
pub fn near(seed: u64, stream: &str, who: &str, w: &WeaponDamage, kind: &str, armed: bool, d: f64) -> Option<DamageRecord> {
    let r = blast_radius_m(w.warhead.as_ref().map_or(0.0, |h| h.mass_kg));
    if d > r {
        return None;
    }
    let p = if r > 0.0 { (1.0 - d / r).clamp(0.0, 1.0) } else { 0.0 };
    let u_hit = uniform(seed, &format!("{stream}:hit"));
    let hit = u_hit < p;
    let figs = w.figures_for(kind);
    let (u_damage, state) = if hit {
        let u = uniform(seed, &format!("{stream}:damage"));
        (Some(u), outcome(figs.map(|(_, f)| f), armed, u))
    } else {
        (None, DamageState::Undamaged)
    };
    Some(DamageRecord {
        who: who.into(),
        role: "collateral".into(),
        kind: kind.into(),
        figures: figs.map(|(k, _)| k.to_string()),
        distance_m: Some((d * 10.0).round() / 10.0),
        p_hit: round3(p),
        factors: vec![Factor { what: format!("within the {r:.1} m blast radius"), x: None, minus: None }],
        u_hit,
        hit,
        u_damage,
        state,
        applied: false,
        pinned: false,
        note: figs.is_none().then(|| "no figures for its kind: a blast's generic split [E]".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hellfire() -> WeaponDamage {
        serde_json::from_value(serde_json::json!({
            "name": "AGM-114R Hellfire II", "kind": "missile", "count": 4, "min_range_m": 500.0,
            "max_range_m": 8000.0, "speed_mps": 425.0, "profile": "direct", "guidance": "semi_active_laser",
            "warhead": { "class": "multi_function", "mass_kg": 9.0 }, "targets": ["surface", "ground"],
            "effects": {
                "small_fast_boat": { "p_hit": 0.75, "p_kill": 0.85, "p_mobility": 0.10, "p_firepower": 0.05 },
                "cargo_ship": { "p_hit": 0.95, "p_mobility": 0.45 },
            },
        }))
        .expect("the catalog's facts read as a weapon")
    }

    const DAY: Conditions = Conditions { visibility_m: 20_000.0, sea_state: 2.0, sun_elevation_deg: 40.0 };

    fn boat(speed: f64) -> Target<'static> {
        Target { kind: "small_fast_boat", speed_mps: speed, armed: true, surface: true }
    }

    /// The figure stands in good conditions; a fast boat costs points per band, night nothing for
    /// a laser seeker, fog, a rough sea and a long shot multiply, and every factor is recorded.
    #[test]
    fn the_hit_chance_reads_the_figures_and_the_conditions() {
        let w = hellfire();
        let (k, base, f, p) = hit_chance(&w, &boat(10.0), &DAY, Some(3000.0));
        assert_eq!((k.as_deref(), base, p), (Some("small_fast_boat"), 0.75, 0.75));
        assert!(f.is_empty());
        // 22 m/s: two bands over 15, 0.20 off
        let (_, _, f, p) = hit_chance(&w, &boat(22.0), &DAY, None);
        assert!((p - 0.55).abs() < 1e-9 && f[0].minus == Some(0.2), "{p} {f:?}");
        let night = Conditions { sun_elevation_deg: -30.0, ..DAY };
        assert_eq!(hit_chance(&w, &boat(10.0), &night, None).3, 0.75);
        let fog = Conditions { visibility_m: 400.0, ..DAY };
        assert!((hit_chance(&w, &boat(10.0), &fog, None).3 - 0.225).abs() < 1e-9);
        let rough = Conditions { sea_state: 4.5, ..DAY };
        assert!((hit_chance(&w, &boat(10.0), &rough, None).3 - 0.6375).abs() < 1e-9);
        assert_eq!(hit_chance(&w, &boat(10.0), &Conditions { sea_state: 6.0, ..DAY }, None).3, 0.0);
        // at its full 8 km: half the chance
        let (_, _, f, p) = hit_chance(&w, &boat(10.0), &DAY, Some(8000.0));
        assert!((p - 0.375).abs() < 1e-9 && f.len() == 1, "{p} {f:?}");
        // a ferry reads the cargo ship's figures; a kind it lists none for, nothing
        assert_eq!(hit_chance(&w, &Target { kind: "ferry", speed_mps: 5.0, armed: false, surface: true }, &DAY, None).0.as_deref(), Some("cargo_ship"));
        assert_eq!(hit_chance(&w, &Target { kind: "small_drone", speed_mps: 5.0, armed: false, surface: false }, &DAY, None), (None, 0.0, Vec::new(), 0.0));
    }

    /// The damage shares split a hit; firepower only on an armed target.
    #[test]
    fn a_hit_splits_into_the_damage_states() {
        let w = hellfire();
        let f = w.figures_for("small_fast_boat").map(|(_, f)| f);
        assert_eq!(outcome(f, true, 0.10), DamageState::Destroyed);
        assert_eq!(outcome(f, true, 0.90), DamageState::Mobility);
        assert_eq!(outcome(f, true, 0.97), DamageState::Firepower);
        assert_eq!(outcome(f, false, 0.97), DamageState::Undamaged);
        let ship = w.figures_for("tanker").map(|(_, f)| f);
        assert_eq!(outcome(ship, false, 0.30), DamageState::Mobility);
        assert_eq!(outcome(ship, false, 0.60), DamageState::Undamaged);
        assert_eq!(outcome(None, false, 0.2), DamageState::Destroyed);
    }

    /// A draw depends on the seed and its stream only; streams and seeds differ.
    #[test]
    fn draws_are_seeded_by_stream() {
        let a = uniform(17, "effect:4:hit");
        assert_eq!(a, uniform(17, "effect:4:hit"));
        assert_ne!(a, uniform(17, "effect:5:hit"));
        assert_ne!(a, uniform(18, "effect:4:hit"));
        assert!((0.0..1.0).contains(&a));
        // spread: a thousand streams average near a half
        let mean = (0..1000).map(|i| uniform(1, &format!("s{i}"))).sum::<f64>() / 1000.0;
        assert!((mean - 0.5).abs() < 0.05, "{mean}");
        let r1 = shot(17, "effect:4", "boat", &hellfire(), &boat(10.0), &DAY, None);
        let r2 = shot(17, "effect:4", "boat", &hellfire(), &boat(10.0), &DAY, None);
        assert_eq!(r1, r2);
    }

    /// Collateral: certain at the point, never past the blast radius (4 m/kg^(1/3): about 8.3 m
    /// for 9 kg).
    #[test]
    fn collateral_falls_off_to_the_blast_radius() {
        let w = hellfire();
        assert!((blast_radius_m(9.0) - 8.32).abs() < 0.01);
        assert!(near(1, "x", "c", &w, "sailboat", false, 9.0).is_none());
        let at = near(1, "x", "c", &w, "sailboat", false, 0.0).expect("inside");
        assert!(at.hit && at.p_hit == 1.0 && at.state != DamageState::Firepower);
        assert_eq!(at.note.as_deref(), Some("no figures for its kind: a blast's generic split [E]"));
        let edge = near(1, "x", "c", &w, "sailboat", false, 8.0).expect("inside");
        assert!(edge.p_hit < 0.05);
    }

    /// The sun at the Gate: high at local noon, below the horizon at midnight (the bridge's own
    /// figures, conditions.rs's tests).
    #[test]
    fn the_sun_is_the_bridges() {
        // 2026-06-14 19:00Z, local noon at the Gate
        assert!(sun_elevation_deg(1_781_463_600.0, 37.82, -122.48) > 60.0);
        assert!(sun_elevation_deg(1_781_463_600.0 + 12.0 * 3600.0, 37.82, -122.48) < -20.0);
    }
}
