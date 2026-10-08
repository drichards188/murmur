//! The real-tracker option: each protected contact's recent history is played second by
//! second through the bridge's own sensing model (`sensing::detect`, the catalog's sensors on
//! our observing vehicles) and its IMM tracker (`model::Track`), and the decision facts are
//! what that tracker reports at the nomination (`Track::facts`), as the bridge would hand them
//! to the engine. The motion is the generator's (eval/keepout.py), given here as a spec and
//! evaluated with the same formulas.
//!
//! Modes: `radar` (the observers' radars only, contacts without AIS: the pessimistic sensor
//! case), `all` (every sensor the observers carry, contacts without AIS) and `ais` (every sensor,
//! every contact broadcasting AIS: the tracker's best case). A contact no
//! sensor detected has no track, so the keep-out does not protect it, as on the bridge.
//!
//! Rules 16 (murmur-533): a contact may go out of view (`masked_from_s`: no detection from
//! then on, whatever the sensors' range: a radar shadow, clutter, a dark small boat), and a
//! track unseen for the bridge's drop time is dropped as the bridge drops it, at the first
//! second it has been unseen longer; its facts then are what the engine keeps as a lost
//! contact (`Simulated::lost`).

use std::collections::BTreeMap;

use murmur_decide::world::TrackFacts;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tracks::model::Track;
use crate::tracks::sensing::{self, Observer, Params, SensorCatalog, SensorType};
use crate::tracks::truth::{Ais, ContactTruth, Domain, TruthAffiliation};

/// Metres per degree of latitude (the generator's and the engine's flat approximation).
const M_PER_DEG: f64 = 111_320.0;
/// The bridge's default `MURMUR_TRACK_DROP_S`: a track silent this long is dropped.
const DROP_S: f64 = 180.0;
/// The bridge's young-track threshold: no average velocity before this, s.
const MEAN_MIN_S: f64 = 45.0;

/// How a contact moves, in metres east and north of the situation's origin, `t` seconds from
/// the nomination (eval/keepout.py's `Hold`, `Line`, `Circle`, `Legs`).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Motion {
    Hold {
        p: [f64; 2],
    },
    Line {
        p: [f64; 2],
        v: [f64; 2],
    },
    /// Round `c` at `r`, `w` rad/s (positive clockwise), at bearing `phase` from the centre at
    /// t = 0.
    Circle {
        c: [f64; 2],
        r: f64,
        w: f64,
        phase: f64,
    },
    /// Piecewise constant velocity: leg `k` from `starts[k]` at `v[k]`, where it is at
    /// `at[k]` when the leg starts.
    Legs {
        starts: Vec<f64>,
        v: Vec<[f64; 2]>,
        at: Vec<[f64; 2]>,
    },
    /// Rules 16, the out-of-view variants: `base` until `from_s`, then from `p` (where `base`
    /// was then) at `v`: a contact that turns and speeds up after it went out of view.
    Then {
        base: Box<Motion>,
        from_s: f64,
        p: [f64; 2],
        v: [f64; 2],
    },
}

impl Motion {
    fn leg(starts: &[f64], t: f64) -> usize {
        starts.partition_point(|s| *s <= t).saturating_sub(1)
    }

    pub fn pos(&self, t: f64) -> [f64; 2] {
        match self {
            Motion::Hold { p } => *p,
            Motion::Line { p, v } => [p[0] + v[0] * t, p[1] + v[1] * t],
            Motion::Circle { c, r, w, phase } => {
                let a = phase + w * t;
                [c[0] + r * a.sin(), c[1] + r * a.cos()]
            }
            Motion::Legs { starts, v, at } => {
                let k = Self::leg(starts, t);
                let (s, vk, a) = (starts.get(k).copied().unwrap_or(0.0), v.get(k).copied().unwrap_or([0.0; 2]), at.get(k).copied().unwrap_or([0.0; 2]));
                [a[0] + vk[0] * (t - s), a[1] + vk[1] * (t - s)]
            }
            Motion::Then { base, from_s, p, v } => {
                if t < *from_s {
                    base.pos(t)
                } else {
                    [p[0] + v[0] * (t - from_s), p[1] + v[1] * (t - from_s)]
                }
            }
        }
    }

    pub fn vel(&self, t: f64) -> [f64; 2] {
        match self {
            Motion::Hold { .. } => [0.0, 0.0],
            Motion::Line { v, .. } => *v,
            Motion::Circle { r, w, phase, .. } => {
                let a = phase + w * t;
                [r * w * a.cos(), -r * w * a.sin()]
            }
            Motion::Legs { starts, v, .. } => v.get(Self::leg(starts, t)).copied().unwrap_or([0.0; 2]),
            Motion::Then { base, from_s, v, .. } => {
                if t < *from_s {
                    base.vel(t)
                } else {
                    *v
                }
            }
        }
    }
}

/// `origin` moved `e` east and `n` north: [lon, lat].
pub fn to_lonlat(origin: [f64; 2], e: f64, n: f64) -> [f64; 2] {
    [origin[0] + e / (M_PER_DEG * origin[1].to_radians().cos()), origin[1] + n / M_PER_DEG]
}

#[derive(Clone, Debug, Deserialize)]
pub struct ContactIn {
    pub motion: Motion,
    /// how long the tracker could have followed it (its sensors pick it up from then)
    pub tracked_s: f64,
    #[serde(default)]
    pub behaviour: String,
    /// `air` for an aircraft (the sensing model's horizon, the tracker's process noise and
    /// initial velocity uncertainty, the keep-out's top speed are the domain's); else surface
    #[serde(default)]
    pub domain: Option<String>,
    /// its altitude, m (an aircraft's)
    #[serde(default)]
    pub alt_m: f64,
    /// what it is (`light_aircraft`; default a fishing vessel)
    #[serde(default)]
    pub kind: Option<String>,
    /// rules 16: out of view from this second on (relative to the nomination; no detection by
    /// any sensor from then), absent: in view throughout
    #[serde(default)]
    pub masked_from_s: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObserverIn {
    pub uid: u64,
    pub platform: String,
    pub at: [f64; 2],
}

#[derive(Clone, Debug, Deserialize)]
pub struct TrackerIn {
    /// `radar`, `all` or `ais`
    pub mode: String,
    /// [lon, lat] of the motions' origin
    pub origin: [f64; 2],
    pub seed: u64,
    pub observers: Vec<ObserverIn>,
    pub contacts: BTreeMap<String, ContactIn>,
    /// at most this much of a contact's history is played (its manoeuvre memory is 300 s
    /// and compares samples up to 300 s apart)
    pub max_history_s: f64,
    /// report each contact's facts against its truth (for the tracker-quality table)
    #[serde(default)]
    pub stats: bool,
}

/// What the observers are as sensor carriers in this mode.
pub fn observers(cat: &SensorCatalog, input: &TrackerIn) -> Result<Vec<Observer>, String> {
    input
        .observers
        .iter()
        .map(|o| {
            let p = cat
                .get(&o.platform)
                .ok_or_else(|| format!("observer platform {} has no sensors in the catalog", o.platform))?;
            let sensors = p
                .sensors
                .iter()
                .filter(|s| match input.mode.as_str() {
                    "radar" => s.kind == SensorType::Radar,
                    _ => true,
                })
                .cloned()
                .collect();
            Ok(Observer {
                uid: o.uid,
                lat: o.at[1],
                lon: o.at[0],
                alt_msl: 0.0,
                mast_m: p.mast_m,
                submerged: false,
                sensors,
            })
        })
        .collect()
}

/// What [`simulate`] gives: the facts at `now_s` of the contacts with a track, those the
/// tracker dropped (rules 16: each with its facts at the drop and when, wall s), and the
/// tracker's account.
pub struct Simulated {
    pub facts: BTreeMap<String, TrackFacts>,
    pub lost: BTreeMap<String, (TrackFacts, f64)>,
    pub stats: Value,
}

/// The facts of `tr` as the bridge hands them over when it drops it: at the first whole second
/// it has been unseen longer than the drop time.
fn dropped(tr: &Track) -> (TrackFacts, f64) {
    let at = (tr.last_seen_t + DROP_S).floor() + 1.0;
    (tr.facts(at), at)
}

/// Every contact's history through the sensing model and the tracker; the facts at `now_s`
/// of those with a track, and (with `stats`) each contact's facts against its truth.
pub fn simulate(cat: &SensorCatalog, input: &TrackerIn, now_s: f64) -> Result<Simulated, String> {
    if !matches!(input.mode.as_str(), "radar" | "all" | "ais") {
        return Err(format!("tracker mode {:?}: radar, all or ais", input.mode));
    }
    let obs = observers(cat, input)?;
    let mut facts = BTreeMap::new();
    let mut lost: BTreeMap<String, (TrackFacts, f64)> = BTreeMap::new();
    let mut stats = Vec::new();
    let mut tracked = 0usize;
    for (n, (cid, c)) in input.contacts.iter().enumerate() {
        let ais = (input.mode == "ais").then(|| Ais {
            mmsi: format!("3{:08}", n + 1),
            name: cid.clone(),
            ship_type: "fishing".into(),
            spoofed: false,
        });
        let domain = match c.domain.as_deref() {
            Some("air") => Domain::Air,
            _ => Domain::Surface,
        };
        let h = c.tracked_s.min(input.max_history_s).max(0.0).floor() as i64;
        let mut track: Option<Track> = None;
        for s in -h..=0 {
            let t = s as f64;
            let [e, nn] = c.motion.pos(t);
            let [ve, vn] = c.motion.vel(t);
            let ll = to_lonlat(input.origin, e, nn);
            let truth = ContactTruth {
                id: cid.clone(),
                name: String::new(),
                affiliation: TruthAffiliation::Neutral,
                domain,
                kind: c.kind.clone().unwrap_or_else(|| "fishing_vessel".into()),
                lat: ll[1],
                lon: ll[0],
                alt_m: c.alt_m,
                speed_mps: ve.hypot(vn),
                course_deg: ve.atan2(vn).to_degrees().rem_euclid(360.0),
                emits: false,
                signature: 1.0,
                ais: ais.clone(),
            };
            let p = Params {
                seed: input.seed,
                tick: s,
                misclassification_rate: 0.0,
            };
            let mut dets = sensing::detect(p, &obs, std::slice::from_ref(&truth), &|_, _| false);
            // rules 16: out of view: nothing detects it
            if c.masked_from_s.is_some_and(|m| t >= m) {
                dets.clear();
            }
            let wall = now_s + t;
            if let Some(tr) = track.as_ref().filter(|tr| tr.age_s(wall) > DROP_S) {
                // dropped (seen again later: a new track, as on the bridge; the drop stays lost)
                let k = lost.keys().filter(|k| k.starts_with(cid.as_str())).count();
                lost.insert(if k == 0 { cid.clone() } else { format!("{cid}#{k}") }, dropped(tr));
                track = None;
            }
            for d in &mut dets {
                d.measured_t = Some(wall);
                match &mut track {
                    Some(tr) => tr.update(d, wall),
                    None => track = Some(Track::new(cid.clone(), n as u64 + 1, d, wall)),
                }
            }
        }
        let f = track.as_ref().filter(|tr| tr.age_s(now_s) <= DROP_S).map(|tr| tr.facts(now_s));
        if let Some(tr) = track.as_ref().filter(|tr| tr.age_s(now_s) > DROP_S) {
            let k = lost.keys().filter(|k| k.starts_with(cid.as_str())).count();
            lost.insert(if k == 0 { cid.clone() } else { format!("{cid}#{k}") }, dropped(tr));
        }
        if input.stats {
            let v = c.motion.vel(0.0);
            stats.push(match &f {
                Some(f) => {
                    let seen = (f.position.t.0 as f64) / 1000.0 - now_s;
                    let [e, nn] = c.motion.pos(seen);
                    let ll = to_lonlat(input.origin, e, nn);
                    let pos_err = murmur_decide::geo::haversine_m(f.position.value, murmur_decide::geo::LonLat::new(ll[0], ll[1]));
                    let vel_err = f.velocity_mps.map(|[a, b]| (a - v[0]).hypot(b - v[1]));
                    let tracked_for = f.first_seen_t.map(|t0| now_s - t0.0 as f64 / 1000.0);
                    json!({
                        "behaviour": c.behaviour, "tracked": true, "vel": f.velocity_mps.is_some(), "alt": f.alt_m,
                        "vel_err": vel_err, "pos_err": pos_err, "sigma": f.pos_sigma_m,
                        "mean": f.mean_velocity_mps.is_some(), "manoeuvre": f.manoeuvre.is_some(),
                        "young": f.mean_velocity_mps.is_none() && tracked_for.is_some_and(|x| x < MEAN_MIN_S),
                        "tracked_s": tracked_for, "true_speed": v[0].hypot(v[1]),
                    })
                }
                None => json!({"behaviour": c.behaviour, "tracked": false}),
            });
        }
        if let Some(f) = f {
            tracked += 1;
            facts.insert(cid.clone(), f);
        }
    }
    let mut out = json!({"contacts": input.contacts.len(), "tracked": tracked, "lost": lost.keys().collect::<Vec<_>>()});
    if input.stats {
        out["per_contact"] = Value::Array(stats);
    }
    Ok(Simulated { facts, lost, stats: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legs_are_continuous_and_circles_keep_their_radius() {
        let m = Motion::Legs {
            starts: vec![-100.0, 0.0, 50.0],
            v: vec![[1.0, 0.0], [0.0, 2.0], [0.0, 0.0]],
            at: vec![[-100.0, 0.0], [0.0, 0.0], [0.0, 100.0]],
        };
        assert_eq!(m.pos(-150.0), [-150.0, 0.0]);
        assert_eq!(m.pos(-50.0), [-50.0, 0.0]);
        assert_eq!(m.pos(25.0), [0.0, 50.0]);
        assert_eq!(m.pos(80.0), [0.0, 100.0]);
        assert_eq!(m.vel(10.0), [0.0, 2.0]);
        let c = Motion::Circle {
            c: [10.0, -5.0],
            r: 200.0,
            w: 0.02,
            phase: 1.0,
        };
        for t in [-300.0, 0.0, 77.0] {
            let p = c.pos(t);
            assert!(((p[0] - 10.0).hypot(p[1] + 5.0) - 200.0).abs() < 1e-9);
            let v = c.vel(t);
            assert!((v[0].hypot(v[1]) - 4.0).abs() < 1e-9);
        }
    }
}
