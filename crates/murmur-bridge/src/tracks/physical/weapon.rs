//! Opponent weapons under the physical model (docs/OPPONENT-WEAPONS.md): what our sensors make of
//! a weapon's launch and of the weapon in flight. The strike needs no sensor: the victim knows it
//! (the bridge reports it, `tracks::weapons`).
//!
//! - **The launch** ([`Engine::observe_launch`]): every EO/IR channel with the shooter in its
//!   field this tick (a panoramic ring always; a gimbal's field as its payload manager points it,
//!   not while it slews) and in optical line of sight sees the flash and plume with the channel's
//!   steady-state detection probability ([`eo::channel_p`]): a transient is caught by change
//!   detection without the search time a still contact needs. It places the author as an EO/IR
//!   detection would. Radar and sonar do not observe a launch.
//! - **The weapon in flight** ([`Engine::sense_weapons`]): each truth message covers the second
//!   since the last, and the weapon's position at any instant is known from its flight, so each
//!   radar's scan passes inside the second are one Swerling 1 draw each at that instant's
//!   geometry, against the sea clutter in the weapon's cell (it flies a few metres up; a Doppler
//!   radar sees it clear of the clutter's band); each camera with it in its field accrues
//!   detection as a look does (`P^2 / tau` a second, in 0.1 s steps), head-on figures for an
//!   observer it flies at and side-on ones for any other, and once detected gives a fix each
//!   second it stays in view.
//!
//! It reads the engine's state (the payload managers' pointing, the terrain) and never writes it:
//! no cache entry, no cue, no look. A camera's accrual on a weapon is kept by the caller. Its
//! draws are salted apart from every contact's, so the contacts' detections are the same with or
//! without a weapon in the truth.

use std::collections::HashMap;

use super::geometry::{K_OPTICAL, K_RADIO, M_PER_DEG, en, visible_height_m};
use super::spec::{EoSensor, Resolved, resolve};
use super::{Engine, Step, Target, eo, in_field, pair, radar, salt, sensor_height_m, sight};
use crate::geo::haversine_m;
use crate::tracks::sensing::{ActFix, ActObservation, Observer, SensorType, normal, uniform};
use crate::tracks::truth::{ContactTruth, WeaponTruth};

/// A weapon kind's figures (docs/OPPONENT-WEAPONS.md, every one [E here]): its radar
/// cross-section, its critical dimension and thermal contrast head-on (as its victim sees it)
/// and side-on, and its launch's flash, plume and smoke as one target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeaponSig {
    pub rcs_m2: f64,
    pub head_on: (f64, f64),
    pub side_on: (f64, f64),
    pub plume: (f64, f64),
}

/// The kinds the contacts process may launch (`murmur-contacts` `WEAPON_KINDS`).
pub fn weapon_signature(kind: &str) -> Option<WeaponSig> {
    match kind {
        // a 150 mm body 1.2 m long: nose-on 0.01 m^2; sqrt(0.15 x 1.2) side-on, its motor's plume
        // in view; a boost motor's flash and plume
        "atgm" => Some(WeaponSig { rcs_m2: 0.01, head_on: (0.15, 30.0), side_on: (0.4, 100.0), plume: (2.0, 300.0) }),
        _ => None,
    }
}

/// One observer's best fix of a launch (lat, lon, its 1-sigma, m) and its cameras that saw it.
type ObserverFix = ((f64, f64, f64), Vec<String>);

/// An observer counts as one a weapon flies at when its line of sight is within this of the
/// weapon's nose, radians (head-on figures).
const HEAD_ON_RAD: f64 = 0.35;
/// A camera's accrual on a weapon is stepped this finely, s.
const EO_STEP_S: f64 = 0.1;

/// One fix of a weapon in flight: the weapon (its truth id: the association, never served),
/// who and which sensor, when (the truth message's clock) and where, with its 1-sigma, m.
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
/// ground and slant range, bearing and elevation.
fn geometry(o: &Observer, h_s: f64, lat: f64, lon: f64, top: f64) -> (f64, f64, f64, f64, f64, f64) {
    let ground = haversine_m(o.lat, o.lon, lat, lon);
    let (de, dn) = en(o.lat, o.lon, lat, lon);
    let slant = ground.hypot(top - h_s);
    (ground, slant, de.atan2(dn), (top - h_s).atan2(ground.max(1.0)), de, dn)
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
            self.gimbals.get(&(o.uid, si)).map(|g| (g.az, g.el, g.narrow, s < g.busy_until))
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

    /// The launch of act `act` by `shooter` (where the truth says it is), as our cameras see it
    /// this tick: which of them saw the flash and where the best of them placed its author (each
    /// other observer's best fix beside it), or None.
    pub fn observe_launch(&self, st: &Step, observers: &[Observer], shooter: &ContactTruth, act: &str, kind: &str) -> Option<ActObservation> {
        let sig = weapon_signature(kind)?;
        let cond = st.sensing.conditions.unwrap_or_default().sane();
        let (lat0, lon0) = observers.first().map(|o| (o.lat, o.lon)).unwrap_or((shooter.lat, shooter.lon));
        let sun = cond.sun_at(st.t, lat0, lon0);
        let terrain = self.terrain.grid.clone();
        let t = Target::new(shooter, st.sensing.signatures.get(&shooter.id), terrain.as_deref());
        let key = format!("{act}/launch");
        let mut sensors: Vec<String> = Vec::new();
        let mut per: std::collections::BTreeMap<u64, ObserverFix> = std::collections::BTreeMap::new();
        for o in observers.iter().filter(|o| !o.submerged) {
            let h_s = sensor_height_m(o);
            for (si, spec) in o.sensors.iter().enumerate() {
                let Resolved::EoIr(cam) = resolve(spec, h_s > 20.0) else { continue };
                let p = pair(o, h_s, &t);
                let obstruction = self.weapon_terrain(o, h_s, shooter.lat, shooter.lon, t.top_msl).map(|x| x[0]);
                if sight(h_s, &t, &p, K_OPTICAL, obstruction).is_none() {
                    continue;
                }
                let pd = self
                    .weapon_view(o, si, &cam, st.s, p.bearing, p.el)
                    .into_iter()
                    .map(|(ci, narrow)| eo::channel_p(&cam.channels[ci], narrow, p.slant_m, [h_s, t.top_msl], sig.plume.0, sig.plume.1, &cond, sun)[0])
                    .fold(0.0, f64::max);
                if pd <= 0.0 || uniform(st.seed, st.tick, o.uid, &key, salt(si, SensorType::EoIr, 20)) >= pd {
                    continue;
                }
                let sigma = 15.0 + 0.005 * p.slant_m;
                let (ne, nn) = (
                    normal(st.seed, st.tick, o.uid, &key, salt(si, SensorType::EoIr, 21)),
                    normal(st.seed, st.tick, o.uid, &key, salt(si, SensorType::EoIr, 22)),
                );
                let (lat, lon) = o.geolocate(shooter.lat + sigma * nn / M_PER_DEG, shooter.lon + sigma * ne / (M_PER_DEG * shooter.lat.to_radians().cos()));
                let name = format!("{}:{}", o.uid, SensorType::EoIr.as_str());
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
            }
        }
        let (&uid, &((lat, lon, sigma), _)) = per.iter().min_by(|a, b| a.1.0.2.total_cmp(&b.1.0.2))?;
        let fixes = per
            .into_iter()
            .filter(|(o, _)| *o != uid)
            .map(|(o, ((lat, lon, s), ss))| ActFix { observer: o, sensors: ss, lat, lon, sigma_m: (s * 10.0).round() / 10.0 })
            .collect();
        Some(ActObservation { sensors, lat, lon, sigma_m: (sigma * 10.0).round() / 10.0, fixes })
    }

    /// The weapons in flight during this message's second (`prev_t` to `st.t`, the truth's clock),
    /// as our radars and cameras fix them. `cams`: each camera's evidence on each weapon, by
    /// (weapon, uid, sensor index), kept by the caller.
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
            let (lat0, lon0) = (w.from[1], w.from[0]);
            let sun = cond.sun_at(st.t, lat0, lon0);
            let (ve, vn) = w.velocity();
            let speed = ve.hypot(vn).max(1e-6);
            // scenario seconds of a truth instant (the scan phases are in scenario seconds)
            let s_of = |t: f64| st.s - (st.t - t);
            for o in observers.iter().filter(|o| !o.submerged) {
                let h_s = sensor_height_m(o);
                for (si, spec) in o.sensors.iter().enumerate() {
                    match resolve(spec, h_s > 20.0) {
                        Resolved::Radar(r) => {
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
                                let (ground, slant, bearing, _, de, dn) = geometry(o, h_s, p[1], p[0], p[2]);
                                let h_t = visible_height_m(p[2], p[2], ground, h_s, K_RADIO);
                                if h_t <= 0.0 || self.weapon_terrain(o, h_s, p[1], p[0], p[2]).is_some_and(|x| x[1] > 0.0) {
                                    continue;
                                }
                                let radial = (de * ve + dn * vn) / ground.max(1.0) * (ground / slant.max(1.0));
                                let sea = radar::Sea { state: cond.sea_state, wind_mps: cond.wind_mps, radial_mps: radial };
                                let pd = radar::pd(&r, sig.rcs_m2, slant, ground, h_s, h_t, Some(sea));
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
                                let range = (ground + sig_r * n_r).max(0.0);
                                let b = bearing + sig_x * n_x / slant.max(1.0);
                                let (lat, lon) = (o.lat + range * b.cos() / M_PER_DEG, o.lon + range * b.sin() / (M_PER_DEG * o.lat.to_radians().cos()));
                                let (lat, lon) = o.geolocate(lat, lon);
                                out.push(WeaponFix { weapon: w.id.clone(), uid: o.uid, si, sensor: SensorType::Radar, t: tau, lat, lon, sigma_m: sig_r.max(sig_x) });
                            }
                        }
                        Resolved::EoIr(cam) => {
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
                                let clear = visible_height_m(p[2], p[2], ground, h_s, K_OPTICAL) > 0.0
                                    && !self.weapon_terrain(o, h_s, p[1], p[0], p[2]).is_some_and(|x| x[0] > 0.0);
                                let views = if clear { self.weapon_view(o, si, &cam, st.s, bearing, el) } else { Vec::new() };
                                if !views.is_empty() {
                                    // head-on when the observer is near the weapon's nose
                                    let nose = (-de * ve - dn * vn) / (ground.max(1.0) * speed);
                                    let (dc, k) = if nose >= HEAD_ON_RAD.cos() { sig.head_on } else { sig.side_on };
                                    let pd = views
                                        .iter()
                                        .map(|&(ci, narrow)| eo::channel_p(&cam.channels[ci], narrow, slant, [h_s, p[2]], dc, k, &cond, sun)[0])
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
                                out.push(WeaponFix { weapon: w.id.clone(), uid: o.uid, si, sensor: SensorType::EoIr, t: tau, lat, lon, sigma_m: sigma });
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
}
