//! Opponent weapons on the truth side (docs/OPPONENT-WEAPONS.md). A contact may carry a weapon
//! (`scenario::WeaponSpec`); every act it commits launches it at the act's second, from where it
//! is, at a lead point on its victim: the victim's position plus its velocity times the time of
//! flight. The weapon flies a straight line at constant speed and strikes the lead point at a
//! time fixed at launch. Kinematics only: no guidance, no seeker, no miss and no damage; the
//! strike is a time and a place.
//!
//! The contacts process does not know where our vehicles are: a truth message names the ones
//! its weapons may be aimed at (`aim_at`), and the bridge's answer carries where they are
//! (`ours`, [`OursAt`]). A launch at a victim beyond the weapon's range, or with no position
//! for it yet, is held: the act does not happen (`ActTruth::held` says why).
//!
//! Shared by the contacts process and (refused for now) the headless simulator.

use std::collections::BTreeMap;

use murmur_standins::geo::{M_PER_DEG, distance_m};
use serde::{Deserialize, Serialize};

use crate::motion::Mover;
use crate::scenario::Scenario;
use crate::truth::{ActTruth, ImpactTruth, WeaponTruth};

/// One of our vehicles as the bridge last heard it (its answer's `ours`): where it is and its
/// velocity over the ground, m/s east and north.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OursAt {
    pub uid: u64,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt_m: f64,
    #[serde(default)]
    pub ve: f64,
    #[serde(default)]
    pub vn: f64,
}

/// Our vehicles the scenario's weapons may be aimed at (the truth message's `aim_at`): each
/// uid an act by an armed contact names, once, in order.
pub fn aim_at(s: &Scenario) -> Vec<u64> {
    let armed = |by: &str| s.contacts.iter().any(|c| c.id == by && c.weapon.is_some());
    let mut out: Vec<u64> = s.acts.iter().filter(|a| armed(&a.by)).filter_map(|a| a.against_vehicle).collect();
    out.sort_unstable();
    out.dedup();
    out
}

fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

/// The lead point on a victim at (`lat`, `lon`) moving (`ve`, `vn`) m/s, for a weapon from
/// (`flat`, `flon`) at `speed`: the victim's position a time of flight on, the time of flight
/// solved by three fixed-point steps (a victim at a few m/s moves metres in a few seconds).
/// Returns the lead point and the time of flight, s.
pub fn lead(from: (f64, f64), at: (f64, f64), v: (f64, f64), speed: f64) -> ((f64, f64), f64) {
    let mut tof = distance_m(from.0, from.1, at.0, at.1) / speed;
    let mut p = at;
    for _ in 0..3 {
        p = (at.0 + v.1 * tof / M_PER_DEG, at.1 + v.0 * tof / (M_PER_DEG * at.0.to_radians().cos()));
        tof = distance_m(from.0, from.1, p.0, p.1) / speed;
    }
    (p, tof)
}

/// The weapons launched and not yet struck.
#[derive(Debug, Default)]
pub struct Armory {
    flights: Vec<WeaponTruth>,
}

impl Armory {
    /// Launch the weapon of each act in `acts` whose author carries one (an abstract act is left
    /// as it is), at the act's own time: sets the act's `weapon`, or `held` with why. `ours`: our
    /// vehicles where the bridge last said they were.
    pub fn launch(&mut self, scenario: &Scenario, acts: &mut [ActTruth], movers: &[Mover], ours: &BTreeMap<u64, OursAt>) {
        for a in acts.iter_mut() {
            let Some(w) = scenario.contacts.iter().find(|c| c.id == a.by).and_then(|c| c.weapon.as_ref()) else {
                continue;
            };
            let victim = match (a.against_vehicle, &a.against_contact) {
                (Some(uid), _) => ours.get(&uid).map(|o| ((o.lat, o.lon), (o.ve, o.vn))),
                (None, Some(c)) => movers.iter().find(|m| m.contact.id == *c && m.st.exists).map(|m| {
                    let c = m.st.course_deg.to_radians();
                    ((m.st.lat, m.st.lon), (m.st.speed_mps * c.sin(), m.st.speed_mps * c.cos()))
                }),
                _ => None,
            };
            let Some((at, v)) = victim else {
                a.held = Some("no position for its victim yet".into());
                continue;
            };
            let d = distance_m(a.lat, a.lon, at.0, at.1);
            if d > w.range_m {
                a.held = Some(format!("its victim {:.1} km away, beyond its {}'s {:.1} km", d / 1000.0, w.name, w.range_m / 1000.0));
                continue;
            }
            let (p, tof) = lead((a.lat, a.lon), at, v, w.speed_mps);
            let f = WeaponTruth {
                id: format!("{}:weapon", a.id),
                act: a.id.clone(),
                by: a.by.clone(),
                name: w.name.clone(),
                kind: w.kind.clone(),
                speed_mps: w.speed_mps,
                launch_t: a.t,
                impact_t: round(a.t + tof, 3),
                from: [a.lon, a.lat, w.fly_m],
                to: [round(p.1, 7), round(p.0, 7), w.fly_m],
                against_vehicle: a.against_vehicle,
                against_contact: a.against_contact.clone(),
            };
            a.weapon = Some(f.id.clone());
            self.flights.push(f);
        }
    }

    /// The weapons in flight at some time in (`prev_t`, `t`]: launched by `t`, not struck by
    /// `prev_t` (one striking in this second is still here, so the bridge senses its last part).
    pub fn in_flight(&self, prev_t: f64, t: f64) -> Vec<WeaponTruth> {
        self.flights.iter().filter(|f| f.launch_t <= t && f.impact_t > prev_t).cloned().collect()
    }

    /// The strikes due by `t`, each once (their flights end).
    pub fn impacts(&mut self, t: f64) -> Vec<ImpactTruth> {
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.flights).into_iter().partition(|f| f.impact_t <= t);
        self.flights = wait;
        due.into_iter()
            .map(|f| ImpactTruth {
                weapon: f.id,
                act: f.act,
                t: f.impact_t,
                lat: f.to[1],
                lon: f.to[0],
                against_vehicle: f.against_vehicle,
                against_contact: f.against_contact,
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.flights.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::truth::TruthMessage;

    const ARMED: &str = "title = \"t\"\n\
        [[contact]]\nid = \"boat\"\naffiliation = \"hostile\"\ndomain = \"surface\"\nkind = \"small_fast_boat\"\nmotion = \"station\"\nat = [-122.4950, 37.8120]\n\
        weapon = { name = \"ATGM-class missile\", kind = \"atgm\", speed_mps = 250, range_m = 5000 }\n\
        [[contact]]\nid = \"ferry\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat = [-122.4836, 37.8120]\n";

    fn act(id: &str, t: f64, uid: u64) -> ActTruth {
        ActTruth {
            id: id.into(),
            s: t,
            t,
            by: "boat".into(),
            lat: 37.8120,
            lon: -122.4950,
            against_vehicle: Some(uid),
            against_contact: None,
            label: None,
            weapon: None,
            held: None,
        }
    }

    /// A launch flies to a lead point on its victim and strikes once at the time fixed then;
    /// it is in flight in every message whose second it spans, the striking one included.
    #[test]
    fn a_weapon_flies_to_its_lead_point_and_strikes_once() {
        let s = Scenario::from_toml(&format!("{ARMED}[[act]]\nid = \"a1\"\nat_s = 10\nby = \"boat\"\nagainst_vehicle = 1\n")).expect("parses");
        assert_eq!(aim_at(&s), vec![1]);
        let (vlat, vlon) = (37.8120, -122.4950 + 1000.0 / (M_PER_DEG * 37.812f64.to_radians().cos()));
        let ours = BTreeMap::from([(1, OursAt { uid: 1, lat: vlat, lon: vlon, alt_m: 0.0, ve: 0.0, vn: 5.0 })]);
        let mut armory = Armory::default();
        let mut acts = vec![act("a1", 1000.0, 1)];
        armory.launch(&s, &mut acts, &[], &ours);
        assert_eq!(acts[0].weapon.as_deref(), Some("a1:weapon"));
        let f = armory.in_flight(999.0, 1000.0);
        assert_eq!(f.len(), 1);
        let tof = f[0].impact_t - f[0].launch_t;
        assert!((tof - 4.0).abs() < 0.02, "1 km at 250 m/s: {tof}");
        // led 5 m/s north for the flight
        let north = (f[0].to[1] - vlat) * M_PER_DEG;
        assert!((north - 5.0 * tof).abs() < 0.5, "{north}");
        assert!(armory.impacts(1003.0).is_empty());
        assert_eq!(armory.in_flight(1003.0, 1004.0).len(), 1, "the striking second is sensed");
        let hit = armory.impacts(1004.0);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].t, f[0].impact_t);
        assert!(armory.impacts(1005.0).is_empty() && armory.in_flight(1004.0, 1005.0).is_empty());
        let mid = f[0].at(f[0].launch_t + tof / 2.0);
        assert!((distance_m(mid[1], mid[0], 37.8120, -122.4950) - 500.0).abs() < 15.0);
    }

    /// A victim beyond the range, or nowhere known yet, holds the shooter's fire; an act by an
    /// unarmed contact stays abstract.
    #[test]
    fn a_launch_out_of_range_or_blind_is_held() {
        let s = Scenario::from_toml(ARMED).expect("parses");
        let far = BTreeMap::from([(1, OursAt { uid: 1, lat: 37.8120 + 6000.0 / M_PER_DEG, lon: -122.4950, alt_m: 0.0, ve: 0.0, vn: 0.0 })]);
        let mut armory = Armory::default();
        let mut acts = vec![act("a1", 10.0, 1), act("a2", 10.0, 2)];
        armory.launch(&s, &mut acts, &[], &far);
        assert!(acts[0].held.as_deref().is_some_and(|h| h.contains("beyond")), "{:?}", acts[0].held);
        assert!(acts[1].held.as_deref().is_some_and(|h| h.contains("no position")));
        assert!(armory.is_empty());
        let mut abstract_ = vec![ActTruth { by: "ferry".into(), ..act("a3", 10.0, 1) }];
        armory.launch(&s, &mut abstract_, &[], &far);
        assert!(abstract_[0].weapon.is_none() && abstract_[0].held.is_none());
    }

    /// Without a weapon the truth message carries none of the new fields (the same bytes as
    /// before); an act against a contact out of range, a weapon the bridge has no figures for,
    /// or a weapon under the legacy model, fails to load.
    #[test]
    fn weapons_are_opt_in_and_checked_at_load() {
        let plain = Scenario::from_toml("title = \"t\"\n[[contact]]\nid = \"b\"\naffiliation = \"hostile\"\ndomain = \"surface\"\nkind = \"small_fast_boat\"\nmotion = \"station\"\nat = [0, 0]\n").expect("parses");
        assert!(aim_at(&plain).is_empty());
        let movers: Vec<Mover> = plain.contacts().expect("valid").into_iter().map(|c| Mover::new(c, 1, 0)).collect();
        let msg = TruthMessage::build(1.0, 1.0, 1, "t", 1, plain.sensing.clone(), &movers);
        let json = serde_json::to_string(&msg).expect("json");
        for k in ["aim_at", "weapons", "impacts", "weapon", "held"] {
            assert!(!json.contains(k), "{k} in {json}");
        }
        assert!(Scenario::from_toml(&format!("{ARMED}[[act]]\nid = \"a\"\nat_s = 5\nby = \"boat\"\nagainst_contact = \"ferry\"\n")).is_ok());
        let far = ARMED.replace("[-122.4836, 37.8120]", "[-122.40, 37.8120]");
        let e = Scenario::from_toml(&format!("{far}[[act]]\nid = \"a\"\nat_s = 5\nby = \"boat\"\nagainst_contact = \"ferry\"\n")).expect_err("out of range");
        assert!(e.contains("beyond"), "{e}");
        assert!(Scenario::from_toml(&ARMED.replace("\"atgm\"", "\"railgun\"")).and_then(|s| s.contacts()).is_err());
        assert!(Scenario::from_toml(&format!("{ARMED}[sensing]\nmodel = \"legacy\"\n")).is_err());
    }
}
