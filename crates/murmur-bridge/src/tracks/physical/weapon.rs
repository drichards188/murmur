//! Opponent weapons under the physical model (docs/OPPONENT-WEAPONS.md sections 4 and 15): what
//! our sensors make of a weapon's launch and of the weapon in flight, class by class (`atgm`,
//! `rocket`, `air_missile`, `gun`, `torpedo`: the contact catalog's `sense`, sim/contacts.toml).
//! The strike needs no sensor: the victim knows it (the bridge reports it, `tracks::weapons`).
//!
//! The truth's `kind` selects which of these physics applies to what is sensed, and nothing else:
//! the weapon picture never reads it, nor the truth's weapon name, speed or victim
//! (`tracks::weapons`).
//!
//! - **The launch** ([`Engine::observe_launch`]): a camera's look at the launch's flash, plume and
//!   smoke (a missile's or a rocket's motor, a gun's muzzle flash: [`WeaponSig::plume`]) or a
//!   sonar's hearing of a torpedo's launch transient ([`WeaponSig::launch_db`]). Every EO/IR
//!   channel with the shooter in its field this tick (a panoramic ring always; a gimbal's field as
//!   its payload manager points it, not while it slews) and in optical line of sight sees it with
//!   the channel's steady-state detection probability ([`eo::channel_p`]): a transient is caught
//!   by change detection without the search time a still contact needs. The same draw at the
//!   recognition level tells what kind of motor launch it was ([`LaunchLook`]: a missile's plume
//!   that flies off and burns on, a rocket's blast and short burn); below it, it was a launch and
//!   no more. A gun's muzzle flashes are told as soon as caught, by their rhythm (milliseconds
//!   each at the gun's rate, nothing flying off). A sonar that listens (a passive one, or an
//!   active one's passive mode) hears a torpedo's launch transient by the sonar equation
//!   ([`sonar::pd`]), and a transient heard is a launch transient. It places the author as such a
//!   sensor's fix would.
//!   Radar does not observe a launch; a sonar only a torpedo's.
//! - **The weapon in flight** ([`Engine::sense_weapons`]), judged in three dimensions (slant
//!   ranges, both heights, the horizon and the terrain): each truth message covers the second
//!   since the last, and the weapon's position at any instant is known from its flight, so each
//!   radar's scan passes inside the second are one Swerling 1 draw each at that instant's
//!   geometry, against the sea clutter in its cell while it flies within [`SEA_SKIM_M`] of the
//!   sea (a Doppler radar sees it clear of the clutter's band), as an air target above it; a 2D
//!   radar lays its plot off at the ground range the slant range gives on the sea's surface, a
//!   height-finding radar at the ground range with the height it measured. Each camera with it
//!   in its field accrues detection as a look does (`P^2 / tau` a second, in 0.1 s steps),
//!   head-on figures for an observer it flies at and side-on ones for any other, and once
//!   detected gives a fix each second it stays in view. A torpedo under water is heard running
//!   by a listening sonar, a draw a second, a fix with a passive sonar's error. A gun's rounds are
//!   not plotted or fixed (too small for our radars; a tracer is seen, not placed), nor anything
//!   under water by a radar or a camera.
//!
//! It reads the engine's state (the payload managers' pointing, the terrain, the bathymetry) and
//! never writes it: no cache entry, no cue, no look. A camera's accrual on a weapon is kept by the
//! caller. Its draws are salted apart from every contact's, so the contacts' detections are the
//! same with or without a weapon in the truth.

use std::collections::HashMap;

use super::conditions::Conditions;
use super::geometry::{K_OPTICAL, K_RADIO, M_PER_DEG, en, visible_height_m};
use super::signature::{AisClass, Signature};
use super::spec::{EoSensor, RadarSensor, Resolved, SonarMode, SonarSensor, resolve};
use super::{Engine, Step, Target, eo, in_field, pair, radar, salt, sensor_height_m, sight, sonar};
use crate::geo::haversine_m;
use crate::tracks::sensing::{ActFix, ActObservation, Observer, SensorType, normal, uniform};
use crate::tracks::truth::{ContactTruth, Domain, WeaponTruth};

/// What a launch looks (or sounds) like to the sensor that caught it, once told (docs/
/// OPPONENT-WEAPONS.md section 15, every distinction [E]): a guided missile's (an ejection, then a
/// plume that flies off and burns on as it is steered), a rocket's (a booster's blast and smoke
/// at the launcher, a motor that burns out early and leaves a spinning, unguided flight), a gun's
/// (a muzzle flash, repeated round by round in a burst, and no plume flying off), or a torpedo's
/// (a launch transient on a sonar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchLook {
    Missile,
    Rocket,
    Muzzle,
    Transient,
}

impl LaunchLook {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchLook::Missile => "missile",
            LaunchLook::Rocket => "rocket",
            LaunchLook::Muzzle => "muzzle",
            LaunchLook::Transient => "transient",
        }
    }

    /// For a label: "missile launch", "muzzle flash".
    pub fn short(self) -> &'static str {
        match self {
            LaunchLook::Missile => "missile launch",
            LaunchLook::Rocket => "rocket launch",
            LaunchLook::Muzzle => "muzzle flash",
            LaunchLook::Transient => "launch transient",
        }
    }

    /// Told as soon as caught [E]: a gun's flashes by their rhythm (a few milliseconds each, at
    /// the gun's rate, nothing flying off), a launch transient by being heard at all; a motor's
    /// launch, a missile's or a rocket's, only at the recognition level (how its plume flies and
    /// burns).
    pub fn told_when_caught(self) -> bool {
        matches!(self, LaunchLook::Muzzle | LaunchLook::Transient)
    }

    /// As a sentence's object: "a missile's launch, a plume that flew off and burned on".
    pub fn text(self) -> &'static str {
        match self {
            LaunchLook::Missile => "a missile's launch, a plume that flew off and burned on",
            LaunchLook::Rocket => "a rocket's launch, a blast and a short burn",
            LaunchLook::Muzzle => "gunfire, a muzzle flash and no plume",
            LaunchLook::Transient => "a torpedo's launch transient",
        }
    }
}

/// A weapon kind's figures (docs/OPPONENT-WEAPONS.md sections 3 and 15, every one [E here] with
/// its basis at its kind): its radar cross-section (0: nothing our radars plot), its critical
/// dimension and thermal contrast head-on (as its victim sees it) and side-on (dimension 0:
/// nothing our cameras fix), its launch as a camera sees it (flash, plume and smoke as one
/// target; dimension 0: none), its launch transient's and its run's source levels for a sonar
/// (dB re 1 uPa at 1 m; none: nothing heard), and what its launch looks like once told.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponSig {
    pub rcs_m2: f64,
    pub head_on: (f64, f64),
    pub side_on: (f64, f64),
    pub plume: (f64, f64),
    pub launch_db: Option<f64>,
    pub run_db: Option<f64>,
    pub look: LaunchLook,
    /// What our threat library says the class looks like in flight (the weapon picture's class
    /// stage, `tracks::weapons`); the sensing here never reads it.
    pub threat: ClassSig,
}

/// A weapon class as our threat library has it (docs/OPPONENT-WEAPONS.md sections 14 and 15,
/// every figure [E] on the public figures of docs/research/opponent-weapons.md): its name and
/// drawn kind, the speeds it averages over a flight and its nominal one, the heights it flies at
/// (negative: under water), its reach, what it is fired at, what it travels through (the air,
/// however low, or the water) and what its launch looks like. The weapon picture assesses a class
/// only when what our sensors measured fits these (or its shooter is known to carry it); the
/// truth's own weapon figures are never read there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassSig {
    pub class: &'static str,
    /// the console's symbol kind (`missile`, `bomb`, `torpedo`, `gun`)
    pub symbol: &'static str,
    pub speed_mps: (f64, f64),
    pub nominal_mps: f64,
    pub height_m: (f64, f64),
    pub range_m: f64,
    pub targets: &'static [Domain],
    /// `Domain::Air` (it flies, however low) or `Domain::Subsurface` (it runs in the water)
    pub medium: Domain,
    pub launch: LaunchLook,
}

/// The kinds the contacts process may launch (the contact catalog's `sense`), which are also the
/// threat library's classes ([`threat_library`]). The research figures behind each are
/// docs/research/opponent-weapons.md's (its source numbers in brackets).
pub fn weapon_signature(kind: &str) -> Option<WeaponSig> {
    match kind {
        // a 150 mm body 1.2 m long: nose-on 0.01 m^2; sqrt(0.15 x 1.2) side-on, its motor's plume
        // in view; a boost motor's flash and plume. In flight: the Kornet and Konkurs classes
        // average 200 to 275 m/s over a flight (150 to 350 taken, for boost and glide), fly a few
        // metres to tens of metres up, reach 4 to 5.5 km [1], at boats and vehicles
        "atgm" => Some(WeaponSig {
            rcs_m2: 0.01,
            head_on: (0.15, 30.0),
            side_on: (0.4, 100.0),
            plume: (2.0, 300.0),
            launch_db: None,
            run_db: None,
            look: LaunchLook::Missile,
            threat: ClassSig {
                class: "ATGM-class",
                symbol: "missile",
                speed_mps: (150.0, 350.0),
                nominal_mps: 250.0,
                height_m: (0.0, 50.0),
                range_m: 5_500.0,
                targets: &[Domain::Surface, Domain::Ground],
                medium: Domain::Air,
                launch: LaunchLook::Missile,
            },
        }),
        // the RPG-7's PG-7VL (93 mm [5]) and the 107 mm rocket (106.7 mm, 18.8 kg [7]): nose-on
        // under the ATGM's 0.01 m^2 (0.005); head-on 0.1 m at 30 K (at 250 to 300 m/s its nose
        // heats some v^2/2c_p = 30 to 45 K); side-on sqrt(0.1 x 1 m), cooler than the ATGM's
        // (the motor burns early: the RPG's sustainer lights 10 m out [5], the 107 mm's top speed
        // is its burnout's [7], so its plume is in view mostly near the launcher); the launch a
        // booster's blast and smoke cloud (the RPG's gives the shooter away [5]) and a motor's
        // flash, as the ATGM's. In flight: the RPG 115 m/s from the tube to 295 [5] (some 250 on
        // average over 300 m), the 107 mm 385 at burnout [7], slower coasting (180 to 420 taken);
        // laid flat at a ship it flies low, laid at 8 km a 107 mm arcs to some 1 to 2 km; 8,050 m
        // [7]; at boats and vehicles
        "rocket" => Some(WeaponSig {
            rcs_m2: 0.005,
            head_on: (0.1, 30.0),
            side_on: (0.3, 60.0),
            plume: (2.0, 300.0),
            launch_db: None,
            run_db: None,
            look: LaunchLook::Rocket,
            threat: ClassSig {
                class: "Rocket-class",
                symbol: "missile",
                speed_mps: (180.0, 420.0),
                nominal_mps: 300.0,
                height_m: (0.0, 2_000.0),
                range_m: 8_050.0,
                targets: &[Domain::Surface, Domain::Ground],
                medium: Domain::Air,
                launch: LaunchLook::Rocket,
            },
        }),
        // a light missile fired from an aircraft (the LMM: 76 mm, 1.3 m, a two-stage solid motor,
        // over Mach 1.5, 510 m/s [12]): nose-on 0.005 m^2; head-on its 76 mm at 60 K (at 510 m/s
        // its nose heats some 130 K, the body a share of it); side-on sqrt(0.076 x 1.3) with the
        // motor's plume in view, 150 K; the launch a motor lighting off the rail, as the ATGM's.
        // In flight: the LMM's 510, the Hellfire class's 425 (our catalog) (400 to 750 taken);
        // from a UAS at its working height at anything below or level with it (0 to 6,000 m);
        // over 6 km [12], the Hellfire class 8 km; at boats, vehicles and aircraft [12]
        "air_missile" => Some(WeaponSig {
            rcs_m2: 0.005,
            head_on: (0.08, 60.0),
            side_on: (0.31, 150.0),
            plume: (2.0, 300.0),
            launch_db: None,
            run_db: None,
            look: LaunchLook::Missile,
            threat: ClassSig {
                class: "Air missile-class",
                symbol: "missile",
                speed_mps: (400.0, 750.0),
                nominal_mps: 510.0,
                height_m: (0.0, 6_000.0),
                range_m: 8_000.0,
                targets: &[Domain::Surface, Domain::Ground, Domain::Air],
                medium: Domain::Air,
                launch: LaunchLook::Missile,
            },
        }),
        // a machine gun's or a light cannon's burst (7.62 to 23 mm): its rounds (some 1e-4 m^2)
        // are under every radar of ours and a tracer is seen, not placed: no flight to fix. The
        // muzzle flash: some 0.3 to 0.5 m across, each round's lasting about a millisecond, a
        // burst's six to ten in under a second with their smoke: one look a burst at 0.5 m and
        // 200 K (a camera's frame averages each flash down from its thousand kelvin and more). In
        // flight: 825 to 970 m/s at the muzzle [2, 3, 4, 6, 16, 23], slowing to some 500 to 600 at
        // 2 km (600 to 1,000 taken; 800 nominal, about its average over the first kilometre),
        // out to 2 km [2, 3, 4, 16], at boats, vehicles and aircraft
        "gun" => Some(WeaponSig {
            rcs_m2: 0.0,
            head_on: (0.0, 0.0),
            side_on: (0.0, 0.0),
            plume: (0.5, 200.0),
            launch_db: None,
            run_db: None,
            look: LaunchLook::Muzzle,
            threat: ClassSig {
                class: "Gun-class",
                symbol: "gun",
                speed_mps: (600.0, 1_000.0),
                nominal_mps: 800.0,
                height_m: (0.0, 2_000.0),
                range_m: 2_000.0,
                targets: &[Domain::Surface, Domain::Ground, Domain::Air],
                medium: Domain::Air,
                launch: LaunchLook::Muzzle,
            },
        }),
        // a heavyweight torpedo (the 53-65: 45 kn, 19 km, wake homing [14]): under water, nothing
        // for a radar or a camera. Heard: its launch transient (a tube's impulse and the motor's
        // start, a fraction of a second) at 155 dB, louder than its run; running at 45 kn, its
        // turbine and propellers at 145 dB (above a UUV's 124 and a submarine on battery's 100, a
        // little under a fast boat's 150 at speed: tracks/physical/signature.rs). In the water:
        // 20 to 30 m/s for heavyweights (15 to 35 taken), 23 nominal [14]; down to 300 m; 19 km
        // [14]; at ships and submarines (the 53-65 homes on wakes only [14])
        "torpedo" => Some(WeaponSig {
            rcs_m2: 0.0,
            head_on: (0.0, 0.0),
            side_on: (0.0, 0.0),
            plume: (0.0, 0.0),
            launch_db: Some(155.0),
            run_db: Some(145.0),
            look: LaunchLook::Transient,
            threat: ClassSig {
                class: "Torpedo-class",
                symbol: "torpedo",
                speed_mps: (15.0, 35.0),
                nominal_mps: 23.0,
                height_m: (-300.0, 0.0),
                range_m: 19_000.0,
                targets: &[Domain::Surface, Domain::Subsurface],
                medium: Domain::Subsurface,
                launch: LaunchLook::Transient,
            },
        }),
        _ => None,
    }
}

/// The kinds of the threat library: every one the contacts process can launch.
pub const THREAT_KINDS: &[&str] = &["atgm", "rocket", "air_missile", "gun", "torpedo"];

/// Every class of the threat library.
pub fn threat_library() -> Vec<ClassSig> {
    THREAT_KINDS.iter().filter_map(|k| weapon_signature(k)).map(|s| s.threat).collect()
}

/// One observer's best fix of a launch (lat, lon, its 1-sigma, m) and its sensors that caught it.
type ObserverFix = ((f64, f64, f64), Vec<String>);

/// An observer counts as one a weapon flies at when its line of sight is within this of the
/// weapon's nose, radians (head-on figures).
const HEAD_ON_RAD: f64 = 0.35;
/// A camera's accrual on a weapon is stepped this finely, s.
const EO_STEP_S: f64 = 0.1;
/// A weapon within this of the sea is in its clutter's band for a radar, as a boat is (the range
/// cell holds the sea around it); above it, an air target, m [E: an ATGM at 4 m and a rocket laid
/// flat are inside, a missile from an aircraft at height is not].
pub const SEA_SKIM_M: f64 = 15.0;

/// A launch our sensors caught: the act's observation (who, where they placed its author) and,
/// when told, what kind of launch it was.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchSeen {
    pub obs: ActObservation,
    pub look: Option<LaunchLook>,
}

/// One fix of a weapon in flight: the weapon (its truth id: the cameras' accrual and the probe
/// key on it; the weapon picture never reads it, it associates fixes by gating),
/// who and which sensor, when (the truth message's clock) and where, with its 1-sigma, m, and the
/// height a height-finding radar measured (none from any other sensor).
#[derive(Debug, Clone, PartialEq)]
pub struct WeaponFix {
    pub weapon: String,
    pub uid: u64,
    pub si: usize,
    pub sensor: SensorType,
    pub t: f64,
    pub lat: f64,
    pub lon: f64,
    pub sigma_m: f64,
    pub alt_m: Option<f64>,
}

/// A camera's evidence on one weapon (kept by the caller across truth messages): the hazard
/// accrued, what it takes (an exponential drawn once), when it was detected and last fixed.
#[derive(Debug, Clone, PartialEq)]
pub struct EoWeapon {
    pub accrued: f64,
    pub need: f64,
    pub detected_t: Option<f64>,
    pub last_fix_t: Option<f64>,
}

/// The geometry from an observer at `h_s` to a point (lat, lon) `top` metres above the sea:
/// ground and slant range, bearing and elevation, metres east and north.
fn geometry(o: &Observer, h_s: f64, lat: f64, lon: f64, top: f64) -> (f64, f64, f64, f64, f64, f64) {
    let ground = haversine_m(o.lat, o.lon, lat, lon);
    let (de, dn) = en(o.lat, o.lon, lat, lon);
    let slant = ground.hypot(top - h_s);
    (ground, slant, de.atan2(dn), (top - h_s).atan2(ground.max(1.0)), de, dn)
}

/// The mode of a sonar that listens: a passive one, or an active one's passive mode.
fn listening(sn: &SonarSensor) -> Option<&SonarSensor> {
    match sn.mode {
        SonarMode::Passive => Some(sn),
        SonarMode::Active => sn.passive.as_deref(),
    }
}

/// A sound of `db` (dB re 1 uPa at 1 m) as the sonar model's signature.
fn sound(db: f64) -> Signature {
    Signature { dc_m: 0.0, height_m: 0.0, rcs_m2: 0.0, thermal_k: 0.0, source_db: Some(db), target_db: None, emitter: None, ais_class: AisClass::None }
}

/// A passive sonar's fix error at `r` metres, m (as a contact's, `physical::Engine::sonar`).
fn passive_sigma(r: f64) -> f64 {
    50.0 + 0.10 * r
}

/// Which physics a weapon's flight is sensed by this second: its kind's figures and the instants.
struct Flight<'a> {
    w: &'a WeaponTruth,
    sig: WeaponSig,
    t0: f64,
    t1: f64,
    /// m/s east, north, up, and its speed
    v: (f64, f64, f64),
    speed: f64,
}

impl Engine {
    /// The channels of camera `si` of `o` that have the line of sight (bearing, elevation) in
    /// their field this tick, each with whether its field is the narrow one. As `eo` decides it:
    /// a panoramic channel always; a fixed camera along its heading; a gimbal where its payload
    /// manager points, not while it slews or settles.
    fn weapon_view(&self, o: &Observer, si: usize, cam: &EoSensor, s: f64, bearing: f64, el: f64) -> Vec<(usize, bool)> {
        let fixed = cam.slew_dps <= 0.0 || cam.channels.iter().all(|c| c.panoramic);
        let bore = if fixed {
            Some((o.heading_deg.unwrap_or(0.0).to_radians(), -(5f64.to_radians()), false, false))
        } else {
            self.own.get(&o.uid).and_then(|own| own.gimbals.get(&si)).map(|g| (g.az, g.el, g.narrow, s < g.busy_until))
        };
        cam.channels
            .iter()
            .enumerate()
            .filter_map(|(ci, ch)| {
                if ch.panoramic {
                    return Some((ci, false));
                }
                let (az, bel, narrow, blind) = bore?;
                let narrow = !fixed && narrow;
                (!blind && in_field((az, bel), (bearing, el), if narrow { ch.narrow_rad } else { ch.wide_rad })).then_some((ci, narrow))
            })
            .collect()
    }

    /// The terrain's obstruction of the ray from `o` (at `h_s`) to (lat, lon, top) at k = 7/6 and
    /// 4/3, computed afresh (the engine's cache is the contacts'); none without a grid.
    fn weapon_terrain(&self, o: &Observer, h_s: f64, lat: f64, lon: f64, top: f64) -> Option<[f64; 2]> {
        let g = self.terrain.grid.as_ref()?;
        Some(g.obstruction_m((o.lat, o.lon, h_s), (lat, lon, top), [K_OPTICAL, K_RADIO]))
    }

    /// The water's depth between an observer and a point: the bathymetry's at the midpoint, else
    /// the conditions' (as a contact's, `Engine::water_depth_m`).
    fn weapon_water_m(&self, o: &Observer, lat: f64, lon: f64, cond: &Conditions) -> f64 {
        self.bathymetry
            .grid
            .as_ref()
            .and_then(|g| g.height(0.5 * (o.lat + lat), 0.5 * (o.lon + lon)))
            .filter(|h| *h < -1.0)
            .map_or(cond.water_depth_m, |h| -h)
    }

    /// The probability that listening sonar `sn` of `o` hears a sound of `db` at (lat, lon,
    /// `depth` metres above the sea), and the slant range to it.
    fn hear(&self, o: &Observer, sn: &SonarSensor, db: f64, (lat, lon, depth): (f64, f64, f64), cond: &Conditions) -> (f64, f64) {
        let ground = haversine_m(o.lat, o.lon, lat, lon);
        let r = ground.hypot(depth.min(0.0) - o.alt_msl.min(0.0));
        let Some(fom) = sonar::fom_db(sn, "torpedo", &sound(db)) else { return (0.0, r) };
        let dnl = sonar::noise_db(sn.freq_khz, cond.wind_mps, cond.shipping) - sonar::noise_db(sn.freq_khz, sonar::REF_WIND_MPS, sonar::REF_SHIPPING);
        (sonar::pd(sn, fom, r, self.weapon_water_m(o, lat, lon, cond), dnl), r)
    }

    /// The launch by `shooter` (where the truth says it was at the launch), keyed `key` (an act's
    /// id for the weapon its act launched, else the weapon's id: the draws' stream), of a weapon
    /// of `kind`, as our sensors caught it this tick: which of them did and where the best of them
    /// placed its author (each other observer's best fix beside it), and what kind of launch it
    /// was when told; or None.
    pub fn observe_launch(&self, st: &Step, observers: &[Observer], shooter: &ContactTruth, key: &str, kind: &str) -> Option<LaunchSeen> {
        let sig = weapon_signature(kind)?;
        let cond = st.sensing.conditions.unwrap_or_default().sane();
        let (lat0, lon0) = observers.first().map(|o| (o.lat, o.lon)).unwrap_or((shooter.lat, shooter.lon));
        let sun = cond.sun_at(st.t, lat0, lon0);
        let terrain = self.terrain.grid.clone();
        let t = Target::new(shooter, st.sensing.signatures.get(&shooter.id), terrain.as_deref());
        let key = format!("{key}/launch");
        let mut sensors: Vec<String> = Vec::new();
        let mut told = false;
        let mut per: std::collections::BTreeMap<u64, ObserverFix> = std::collections::BTreeMap::new();
        let mut caught = |o: &Observer, sensor: SensorType, sigma: f64, (ne, nn): (f64, f64)| {
            let (lat, lon) = o.geolocate(shooter.lat + sigma * nn / M_PER_DEG, shooter.lon + sigma * ne / (M_PER_DEG * shooter.lat.to_radians().cos()));
            let name = format!("{}:{}", o.uid, sensor.as_str());
            if !sensors.contains(&name) {
                sensors.push(name.clone());
            }
            let e = per.entry(o.uid).or_insert(((lat, lon, sigma), Vec::new()));
            if sigma < e.0.2 {
                e.0 = (lat, lon, sigma);
            }
            if !e.1.contains(&name) {
                e.1.push(name);
            }
        };
        for o in observers {
            let h_s = sensor_height_m(o);
            for (si, spec) in o.sensors.iter().enumerate() {
                if o.submerged && spec.kind != SensorType::Sonar {
                    continue;
                }
                match resolve(spec, h_s > 20.0) {
                    // a camera: the flash, plume and smoke as one look
                    Resolved::EoIr(cam) if sig.plume.0 > 0.0 => {
                        let p = pair(o, h_s, &t);
                        let obstruction = self.weapon_terrain(o, h_s, shooter.lat, shooter.lon, t.top_msl).map(|x| x[0]);
                        if sight(h_s, &t, &p, K_OPTICAL, obstruction).is_none() {
                            continue;
                        }
                        let [pd, pr] = self
                            .weapon_view(o, si, &cam, st.s, p.bearing, p.el)
                            .into_iter()
                            .map(|(ci, narrow)| eo::channel_p(&cam.channels[ci], narrow, p.slant_m, [h_s, t.top_msl], sig.plume.0, sig.plume.1, &cond, sun))
                            .fold([0.0f64; 2], |a, p| [a[0].max(p[0]), a[1].max(p[1])]);
                        // one draw: caught under the detection level, told under the recognition
                        // level (never above the detection's), or at once for a gun's flashes
                        let u = uniform(st.seed, st.tick, o.uid, &key, salt(si, SensorType::EoIr, 20));
                        if pd <= 0.0 || u >= pd {
                            continue;
                        }
                        told |= u < pr || sig.look.told_when_caught();
                        let noise = (normal(st.seed, st.tick, o.uid, &key, salt(si, SensorType::EoIr, 21)), normal(st.seed, st.tick, o.uid, &key, salt(si, SensorType::EoIr, 22)));
                        caught(o, SensorType::EoIr, 15.0 + 0.005 * p.slant_m, noise);
                    }
                    // a sonar that listens: the launch transient
                    Resolved::Sonar(sn) if sig.launch_db.is_some() => {
                        let Some(ls) = listening(&sn) else { continue };
                        let (pd, r) = self.hear(o, ls, sig.launch_db.unwrap_or(0.0), (shooter.lat, shooter.lon, shooter.alt_m), &cond);
                        if pd <= 0.0 || uniform(st.seed, st.tick, o.uid, &key, salt(si, SensorType::Sonar, 43)) >= pd {
                            continue;
                        }
                        told |= sig.look.told_when_caught();
                        let noise = (normal(st.seed, st.tick, o.uid, &key, salt(si, SensorType::Sonar, 44)), normal(st.seed, st.tick, o.uid, &key, salt(si, SensorType::Sonar, 45)));
                        caught(o, SensorType::Sonar, passive_sigma(r), noise);
                    }
                    _ => {}
                }
            }
        }
        let (&uid, &((lat, lon, sigma), _)) = per.iter().min_by(|a, b| a.1.0.2.total_cmp(&b.1.0.2))?;
        let fixes = per
            .into_iter()
            .filter(|(o, _)| *o != uid)
            .map(|(o, ((lat, lon, s), ss))| ActFix { observer: o, sensors: ss, lat, lon, sigma_m: (s * 10.0).round() / 10.0 })
            .collect();
        Some(LaunchSeen {
            obs: ActObservation { sensors, lat, lon, sigma_m: (sigma * 10.0).round() / 10.0, fixes },
            look: told.then_some(sig.look),
        })
    }

    /// The weapons in flight during this message's second (`prev_t` to `st.t`, the truth's clock),
    /// as our radars, cameras and sonars fix them. `cams`: each camera's evidence on each weapon,
    /// by (weapon, uid, sensor index), kept by the caller.
    pub fn sense_weapons(
        &self,
        st: &Step,
        prev_t: f64,
        observers: &[Observer],
        weapons: &[WeaponTruth],
        cams: &mut HashMap<(String, u64, usize), EoWeapon>,
    ) -> Vec<WeaponFix> {
        let cond = st.sensing.conditions.unwrap_or_default().sane();
        let mut out = Vec::new();
        for w in weapons {
            let Some(sig) = weapon_signature(&w.kind) else { continue };
            let (t0, t1) = (prev_t.max(w.launch_t), st.t.min(w.impact_t));
            if t1 <= t0 {
                continue;
            }
            let v = w.velocity3();
            let f = Flight { w, sig, t0, t1, v, speed: (v.0 * v.0 + v.1 * v.1 + v.2 * v.2).sqrt().max(1e-6) };
            for o in observers {
                let h_s = sensor_height_m(o);
                for (si, spec) in o.sensors.iter().enumerate() {
                    if o.submerged && spec.kind != SensorType::Sonar {
                        continue; // nothing but sonar works under water
                    }
                    match resolve(spec, h_s > 20.0) {
                        Resolved::Radar(r) if sig.rcs_m2 > 0.0 => self.radar_weapon(st, &cond, o, h_s, si, &r, &f, &mut out),
                        Resolved::EoIr(cam) if sig.head_on.0 > 0.0 || sig.side_on.0 > 0.0 => self.eo_weapon(st, &cond, o, h_s, si, &cam, &f, cams, &mut out),
                        Resolved::Sonar(sn) if sig.run_db.is_some() => {
                            if let Some(ls) = listening(&sn) {
                                self.sonar_weapon(st, &cond, o, si, ls, &f, &mut out);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        out.sort_by(|a, b| a.t.total_cmp(&b.t).then(a.uid.cmp(&b.uid)));
        out
    }

    /// A radar's scan passes over a weapon in flight: one Swerling 1 draw each, in three
    /// dimensions, against the sea clutter while it skims the sea.
    #[allow(clippy::too_many_arguments)]
    fn radar_weapon(&self, st: &Step, cond: &Conditions, o: &Observer, h_s: f64, si: usize, r: &RadarSensor, f: &Flight, out: &mut Vec<WeaponFix>) {
        let (w, sig, (t0, t1)) = (f.w, f.sig, (f.t0, f.t1));
        let (ve, vn, vu) = f.v;
        // scenario seconds of a truth instant (the scan phases are in scenario seconds)
        let s_of = |t: f64| st.s - (st.t - t);
        let phase = uniform(st.seed, -2, o.uid, "radar", salt(si, SensorType::Radar, 0)) * r.scan_s;
        let mid = w.at(0.5 * (t0 + t1));
        let (_, _, bearing, _, _, _) = geometry(o, h_s, mid[1], mid[0], mid[2]);
        let pass0 = phase + (bearing.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU) * r.scan_s;
        let (s0, s1) = (s_of(t0), s_of(t1));
        let mut k = ((s0 - pass0) / r.scan_s).floor();
        loop {
            let pass = pass0 + k * r.scan_s;
            k += 1.0;
            if pass > s1 {
                break;
            }
            if pass <= s0 && !(t0 == w.launch_t && pass == s0) {
                continue;
            }
            let tau = t0 + (pass - s0);
            let p = w.at(tau);
            if p[2] < 0.0 {
                continue; // under water
            }
            let (ground, slant, bearing, _, de, dn) = geometry(o, h_s, p[1], p[0], p[2]);
            let h_t = visible_height_m(p[2], p[2], ground, h_s, K_RADIO);
            if h_t <= 0.0 || self.weapon_terrain(o, h_s, p[1], p[0], p[2]).is_some_and(|x| x[1] > 0.0) {
                continue;
            }
            // its speed along the line of sight, in three dimensions
            let radial = (de * ve + dn * vn + (p[2] - h_s) * vu) / slant.max(1.0);
            let sea = (p[2] < SEA_SKIM_M).then_some(radar::Sea { state: cond.sea_state, wind_mps: cond.wind_mps, radial_mps: radial });
            let pd = radar::pd(r, sig.rcs_m2, slant, ground, h_s, h_t, sea);
            let scan = (k - 1.0) as i64;
            if pd <= 0.0 || uniform(st.seed, scan, o.uid, &w.id, salt(si, SensorType::Radar, 21)) >= pd {
                continue;
            }
            // 30 m or 1 % of range, and 1 degree in bearing (as a contact's plot)
            let sig_r = (0.01 * slant).max(30.0);
            let sig_x = slant * 1f64.to_radians();
            let (n_r, n_x) = (
                normal(st.seed, scan, o.uid, &w.id, salt(si, SensorType::Radar, 22)),
                normal(st.seed, scan, o.uid, &w.id, salt(si, SensorType::Radar, 23)),
            );
            // a height-finding radar measures its height and so its ground range; a 2D radar
            // lays the slant range off as if it were on the sea's surface (a weapon at height is
            // plotted short by what its height takes off the slant: the plot's own error)
            let (ground_seen, alt_m) = if r.height {
                (ground, Some(p[2] + 30.0 * normal(st.seed, scan, o.uid, &w.id, salt(si, SensorType::Radar, 24))))
            } else {
                ((slant * slant - h_s * h_s).max(0.0).sqrt(), None)
            };
            let range = (ground_seen + sig_r * n_r).max(0.0);
            let b = bearing + sig_x * n_x / slant.max(1.0);
            let (lat, lon) = (o.lat + range * b.cos() / M_PER_DEG, o.lon + range * b.sin() / (M_PER_DEG * o.lat.to_radians().cos()));
            let (lat, lon) = o.geolocate(lat, lon);
            out.push(WeaponFix { weapon: w.id.clone(), uid: o.uid, si, sensor: SensorType::Radar, t: tau, lat, lon, sigma_m: sig_r.max(sig_x), alt_m });
        }
    }

    /// A camera's look at a weapon in flight: its accrual in 0.1 s steps while in the field and
    /// above the horizon, head-on or side-on by the angle off its nose in three dimensions; a fix
    /// at the last instant held this second once detected.
    #[allow(clippy::too_many_arguments)]
    fn eo_weapon(
        &self,
        st: &Step,
        cond: &Conditions,
        o: &Observer,
        h_s: f64,
        si: usize,
        cam: &EoSensor,
        f: &Flight,
        cams: &mut HashMap<(String, u64, usize), EoWeapon>,
        out: &mut Vec<WeaponFix>,
    ) {
        let (w, sig, (t0, t1)) = (f.w, f.sig, (f.t0, f.t1));
        let (ve, vn, vu) = f.v;
        let sun = cond.sun_at(st.t, w.from[1], w.from[0]);
        let ev = cams.entry((w.id.clone(), o.uid, si)).or_insert_with(|| EoWeapon {
            accrued: 0.0,
            need: -(1.0 - uniform(st.seed, -5, o.uid, &w.id, salt(si, SensorType::EoIr, 23))).ln(),
            detected_t: None,
            last_fix_t: None,
        });
        let mut seen: Option<(f64, f64)> = None;
        let mut tau = t0;
        while tau <= t1 + 1e-9 {
            let p = w.at(tau);
            let (ground, slant, bearing, el, de, dn) = geometry(o, h_s, p[1], p[0], p[2]);
            let clear = p[2] >= 0.0
                && visible_height_m(p[2], p[2], ground, h_s, K_OPTICAL) > 0.0
                && !self.weapon_terrain(o, h_s, p[1], p[0], p[2]).is_some_and(|x| x[0] > 0.0);
            let views = if clear { self.weapon_view(o, si, cam, st.s, bearing, el) } else { Vec::new() };
            if !views.is_empty() {
                // head-on when the observer is near the weapon's nose
                let nose = -(de * ve + dn * vn + (p[2] - h_s) * vu) / (slant.max(1.0) * f.speed);
                let (dc, k) = if nose >= HEAD_ON_RAD.cos() { sig.head_on } else { sig.side_on };
                let pd = views
                    .iter()
                    .map(|&(ci, narrow)| eo::channel_p(&cam.channels[ci], narrow, slant, [h_s, p[2]], dc, k, cond, sun)[0])
                    .fold(0.0, f64::max);
                if ev.detected_t.is_none() {
                    ev.accrued += pd * pd / eo::TAU_K_S * EO_STEP_S;
                    if ev.accrued >= ev.need {
                        ev.detected_t = Some(tau);
                    }
                }
                if ev.detected_t.is_some() && pd > 0.0 {
                    seen = Some((tau, slant));
                }
            }
            tau += EO_STEP_S;
        }
        // a fix at the last instant it was held this second
        if let Some((tau, slant)) = seen {
            ev.last_fix_t = Some(tau);
            let p = w.at(tau);
            let sigma = 15.0 + 0.005 * slant;
            let (ne, nn) = (
                normal(st.seed, st.tick, o.uid, &w.id, salt(si, SensorType::EoIr, 24)),
                normal(st.seed, st.tick, o.uid, &w.id, salt(si, SensorType::EoIr, 25)),
            );
            let (lat, lon) = o.geolocate(p[1] + sigma * nn / M_PER_DEG, p[0] + sigma * ne / (M_PER_DEG * p[1].to_radians().cos()));
            out.push(WeaponFix { weapon: w.id.clone(), uid: o.uid, si, sensor: SensorType::EoIr, t: tau, lat, lon, sigma_m: sigma, alt_m: None });
        }
    }

    /// A listening sonar's ear on a torpedo running this second: one draw at the second's last
    /// instant in the water, a fix with a passive sonar's error (no depth).
    #[allow(clippy::too_many_arguments)]
    fn sonar_weapon(&self, st: &Step, cond: &Conditions, o: &Observer, si: usize, sn: &SonarSensor, f: &Flight, out: &mut Vec<WeaponFix>) {
        let (w, sig) = (f.w, f.sig);
        let p = w.at(f.t1);
        if p[2] > 0.0 {
            return; // out of the water (nothing a sonar hears)
        }
        let (pd, r) = self.hear(o, sn, sig.run_db.unwrap_or(0.0), (p[1], p[0], p[2]), cond);
        if pd <= 0.0 || uniform(st.seed, st.tick, o.uid, &w.id, salt(si, SensorType::Sonar, 40)) >= pd {
            return;
        }
        let sigma = passive_sigma(r);
        let (ne, nn) = (
            normal(st.seed, st.tick, o.uid, &w.id, salt(si, SensorType::Sonar, 41)),
            normal(st.seed, st.tick, o.uid, &w.id, salt(si, SensorType::Sonar, 42)),
        );
        let (lat, lon) = o.geolocate(p[1] + sigma * nn / M_PER_DEG, p[0] + sigma * ne / (M_PER_DEG * p[1].to_radians().cos()));
        out.push(WeaponFix { weapon: w.id.clone(), uid: o.uid, si, sensor: SensorType::Sonar, t: f.t1, lat, lon, sigma_m: sigma, alt_m: None });
    }
}
