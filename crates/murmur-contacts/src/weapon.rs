//! Opponent weapons on the truth side (docs/OPPONENT-WEAPONS.md). A contact may carry a weapon
//! (`scenario::WeaponSpec`); every act it commits launches it at the act's second, from where it
//! is, at a lead point on its victim: the victim's position plus its velocity times the time of
//! flight. The weapon flies a straight line at constant speed and strikes the lead point at a
//! time fixed at launch. No guidance and no seeker: the strike is a time and a place. A weapon
//! that lists figures (`effects`) is drawn at its strike by the damage model ([`strike`], bd
//! murmur-lbw8.2): a miss, or the victim's damage (one of ours: handed back for the vehicle
//! simulation; a contact: put on it). A shooter whose weapon is out of action (a firepower
//! kill) holds its fire.
//!
//! The contacts process does not know where our vehicles are: a truth message names the ones
//! its weapons may be aimed at (`aim_at`), and the bridge's answer carries where they are
//! (`ours`, [`OursAt`]). A launch at a victim beyond the weapon's range, or with no position
//! for it yet, is held: the act does not happen (`ActTruth::held` says why).
//!
//! A node's own shots (docs/AUTONOMOUS-NODES.md 2.3, [`crate::node`]) fly here too: its fire
//! control hands each flight to the [`Armory`] with what the damage model reads at its strike
//! ([`Strike`]: the catalog weapon's figures, the draw's stream, the launch's distance), and
//! [`strike_by`] draws every strike, the scripted ones as before.
//!
//! Shared by the contacts process and (refused for now) the headless simulator.

use std::collections::BTreeMap;

use murmur_standins::geo::{M_PER_DEG, distance_m};
use serde::{Deserialize, Serialize};

use crate::damage::{self, Conditions, DamageState, Target, WeaponDamage};
use crate::effect::{DamageCtx, OursDamaged};
use crate::motion::Mover;
use crate::scenario::Scenario;
use crate::truth::{ActTruth, ImpactTruth, WeaponTruth};

/// One of our vehicles as the bridge last heard it (its answer's `ours`): where it is and its
/// velocity over the ground, m/s east and north; and what the damage model reads of it (its
/// catalog platform, domain and whether it is armed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub armed: bool,
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

/// The lead point in three dimensions: from `from` (`[lat, lon, metres above the sea]`) at
/// `speed`, on a victim at `at` (`[lat, lon, alt]`) moving (`ve`, `vn`) m/s over the ground, its
/// height held: the time of flight from the slant distance, solved by three fixed-point steps as
/// [`lead`]. Returns the lead point (`[lat, lon, alt]`) and the time of flight, s.
pub fn lead3(from: [f64; 3], at: [f64; 3], v: (f64, f64), speed: f64) -> ([f64; 3], f64) {
    let slant = |p: [f64; 3]| distance_m(from[0], from[1], p[0], p[1]).hypot(p[2] - from[2]);
    let mut tof = slant(at) / speed;
    let mut p = at;
    for _ in 0..3 {
        p = [at[0] + v.1 * tof / M_PER_DEG, at[1] + v.0 * tof / (M_PER_DEG * at[0].to_radians().cos()), at[2]];
        tof = slant(p) / speed;
    }
    (p, tof)
}

/// What the damage model reads of one strike: the weapon's figures, the stream its draws are
/// named by (`act:<id>`, a gun's burst `act:<id>:<burst>`), and the launch's distance over the
/// ground for the range rule (none: not known, as a scripted act's).
#[derive(Debug, Clone, PartialEq)]
pub struct Strike {
    pub weapon: WeaponDamage,
    pub stream: String,
    pub out_m: Option<f64>,
}

/// The weapons launched and not yet struck, and what the damage model reads of a node's
/// strikes (by weapon id) until they are drawn.
#[derive(Debug, Default)]
pub struct Armory {
    flights: Vec<WeaponTruth>,
    strikes: BTreeMap<String, Strike>,
}

impl Armory {
    /// A node's shot: its flight, struck like any other ([`Armory::impacts`]), and what the
    /// damage model reads at its strike.
    pub fn fly(&mut self, flight: WeaponTruth, strike: Strike) {
        self.strikes.insert(flight.id.clone(), strike);
        self.flights.push(flight);
    }

    /// What the damage model reads of a strike with no flight (a node's `self` strike).
    pub fn note(&mut self, weapon: &str, strike: Strike) {
        self.strikes.insert(weapon.to_string(), strike);
    }

    /// What the damage model reads of a node's strike by weapon `id`, once.
    pub fn take(&mut self, id: &str) -> Option<Strike> {
        self.strikes.remove(id)
    }

    /// Launch the weapon of each act in `acts` whose author carries one (an abstract act is left
    /// as it is), at the act's own time: sets the act's `weapon`, or `held` with why. `ours`: our
    /// vehicles where the bridge last said they were.
    pub fn launch(&mut self, scenario: &Scenario, acts: &mut [ActTruth], movers: &[Mover], ours: &BTreeMap<u64, OursAt>) {
        for a in acts.iter_mut() {
            let Some(w) = scenario.contacts.iter().find(|c| c.id == a.by).and_then(|c| c.weapon.as_ref()) else {
                continue;
            };
            if movers.iter().any(|m| m.contact.id == a.by && m.disarmed) {
                a.held = Some(format!("its {} is out of action (a firepower kill)", w.name));
                continue;
            }
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
                rounds: None,
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
                damage: None,
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.flights.is_empty()
    }
}

/// The damage model on each strike in `impacts` whose weapon lists figures (bd murmur-lbw8.2):
/// a hit draw against the victim's kind in the conditions, then the damage draw, on the act's own
/// stream (`act:<id>`), recorded in the impact (`damage`, truth only). A contact victim takes its
/// state here; ours are returned (each hurt one) for the vehicle simulation. `ours`: where the
/// bridge last said our vehicles were and what they are.
pub fn strike(
    scenario: &Scenario,
    impacts: &mut [ImpactTruth],
    movers: &mut [Mover],
    ours: &BTreeMap<u64, OursAt>,
    ctx: &DamageCtx,
) -> Vec<OursDamaged> {
    strike_by(impacts, movers, ours, ctx, |i| scripted(scenario, i))
}

/// What the damage model reads of a scripted act's strike (its author's `weapon`, when it lists
/// figures), on the act's stream.
pub fn scripted(scenario: &Scenario, i: &ImpactTruth) -> Option<Strike> {
    let by = scenario.acts.iter().find(|a| a.id == i.act).map(|a| a.by.as_str())?;
    let w = scenario.contacts.iter().find(|c| c.id == by).and_then(|c| c.weapon.as_ref()).and_then(|w| w.damage())?;
    Some(Strike { weapon: w, stream: format!("act:{}", i.act), out_m: None })
}

/// [`strike`] with each strike's weapon, stream and range from `lookup` (none: not drawn): a
/// node's from the [`Armory`] ([`Armory::take`]), a scripted act's by [`scripted`].
pub fn strike_by(
    impacts: &mut [ImpactTruth],
    movers: &mut [Mover],
    ours: &BTreeMap<u64, OursAt>,
    ctx: &DamageCtx,
    mut lookup: impl FnMut(&ImpactTruth) -> Option<Strike>,
) -> Vec<OursDamaged> {
    let mut hurt = Vec::new();
    for i in impacts.iter_mut() {
        let Some(Strike { weapon: w, stream, out_m }) = lookup(i) else {
            continue;
        };
        let c = Conditions::at(ctx.conditions.as_ref(), i.t, i.lat, i.lon);
        match (i.against_vehicle, &i.against_contact) {
            (Some(uid), _) => {
                let o = ours.get(&uid);
                let kind = damage::ours_kind(o.and_then(|o| o.platform.as_deref()), o.and_then(|o| o.domain.as_deref()));
                let target = Target {
                    kind,
                    speed_mps: o.map_or(0.0, |o| o.ve.hypot(o.vn)),
                    armed: o.is_some_and(|o| o.armed),
                    surface: o.and_then(|o| o.domain.as_deref()) == Some("surface"),
                };
                let mut r = damage::shot(ctx.seed, &stream, &format!("ours:{uid}"), &w, &target, &c, out_m);
                if r.state != DamageState::Undamaged {
                    r.applied = true;
                    hurt.push(OursDamaged { uid: uid.to_string(), state: r.state, cause: format!("struck by {} ({})", w.name, i.act) });
                }
                i.damage = Some(r);
            }
            (None, Some(id)) => {
                let Some(m) = movers.iter_mut().find(|m| m.contact.id == *id && m.st.exists) else {
                    continue;
                };
                let target = crate::effect::target_of(m);
                let mut r = damage::shot(ctx.seed, &stream, id, &w, &target, &c, out_m);
                crate::effect::put(m, &mut r, i.t);
                i.damage = Some(r);
            }
            _ => {}
        }
    }
    hurt
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
        let ours = BTreeMap::from([(1, OursAt { uid: 1, lat: vlat, lon: vlon, alt_m: 0.0, ve: 0.0, vn: 5.0, platform: None, domain: None, armed: false })]);
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
        let far = BTreeMap::from([(1, OursAt { uid: 1, lat: 37.8120 + 6000.0 / M_PER_DEG, lon: -122.4950, alt_m: 0.0, ve: 0.0, vn: 0.0, platform: None, domain: None, armed: false })]);
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
        for k in ["aim_at", "weapons", "impacts", "weapon", "held", "look", "node_events", "rounds"] {
            assert!(!json.contains(k), "{k} in {json}");
        }
        assert!(Scenario::from_toml(&format!("{ARMED}[[act]]\nid = \"a\"\nat_s = 5\nby = \"boat\"\nagainst_contact = \"ferry\"\n")).is_ok());
        let far = ARMED.replace("[-122.4836, 37.8120]", "[-122.40, 37.8120]");
        let e = Scenario::from_toml(&format!("{far}[[act]]\nid = \"a\"\nat_s = 5\nby = \"boat\"\nagainst_contact = \"ferry\"\n")).expect_err("out of range");
        assert!(e.contains("beyond"), "{e}");
        assert!(Scenario::from_toml(&ARMED.replace("\"atgm\"", "\"railgun\"")).and_then(|s| s.contacts()).is_err());
        assert!(Scenario::from_toml(&format!("{ARMED}[sensing]\nmodel = \"legacy\"\n")).is_err());
    }

    /// With figures, a strike on one of ours is drawn on the act's stream: a certain hit with a
    /// certain mobility kill hands HUNTER 1 back as stopped; a certain miss hands nothing back.
    /// A shooter whose weapon a strike put out of action holds its next act's fire. Without
    /// figures the impact carries no damage (the bytes as before).
    #[test]
    fn a_strike_with_figures_damages_its_victim() {
        let with = |figs: &str| {
            ARMED.replace(
                "range_m = 5000 }",
                &format!("range_m = 5000, warhead_kg = 9, guidance = \"semi_active_laser\", effects = {{ small_fast_boat = {figs} }} }}"),
            )
        };
        let acts = "[[act]]\nid = \"a1\"\nat_s = 10\nby = \"boat\"\nagainst_vehicle = 1\n";
        let ours = BTreeMap::from([(
            1,
            OursAt { uid: 1, lat: 37.8120, lon: -122.4836, alt_m: 0.0, ve: 0.0, vn: 0.0, platform: Some("seahunter".into()), domain: Some("surface".into()), armed: false },
        )]);
        let ctx = DamageCtx { seed: 17, conditions: None, hit_override: None };
        let run = |toml: &str| {
            let s = Scenario::from_toml(&format!("{toml}{acts}")).expect("parses");
            let mut movers: Vec<Mover> = s.contacts().expect("valid").into_iter().enumerate().map(|(i, c)| Mover::new(c, 1, i as u64)).collect();
            for m in &mut movers {
                m.step(1.0, 1.0, None);
            }
            let mut armory = Armory::default();
            let mut a = vec![act("a1", 1000.0, 1)];
            armory.launch(&s, &mut a, &movers, &ours);
            let mut hits = armory.impacts(1010.0);
            let hurt = strike(&s, &mut hits, &mut movers, &ours, &ctx);
            (hits, hurt)
        };
        // the Sea Hunter counts as a fishing boat: figures for small_fast_boat only, so no hit
        let (hits, hurt) = run(&with("{ p_hit = 1.0, p_mobility = 1.0 }"));
        assert!(hurt.is_empty() && hits[0].damage.as_ref().is_some_and(|d| !d.hit && d.figures.is_none()));
        let fishing = |figs: &str| with(figs).replace("small_fast_boat = {", "fishing_vessel = {");
        let (hits, hurt) = run(&fishing("{ p_hit = 1.0, p_mobility = 1.0 }"));
        assert_eq!(hurt, vec![OursDamaged { uid: "1".into(), state: DamageState::Mobility, cause: "struck by ATGM-class missile (a1)".into() }]);
        assert!(hits[0].damage.as_ref().is_some_and(|d| d.hit && d.applied && d.who == "ours:1"));
        let (_, hurt) = run(&fishing("{ p_hit = 0.0, p_kill = 1.0 }"));
        assert!(hurt.is_empty());
        let (hits, hurt) = run(ARMED);
        assert!(hurt.is_empty() && hits[0].damage.is_none());
        assert!(!serde_json::to_string(&hits[0]).expect("json").contains("damage"));
        // a disarmed shooter holds its fire
        let s = Scenario::from_toml(&format!("{ARMED}{acts}")).expect("parses");
        let mut movers: Vec<Mover> = s.contacts().expect("valid").into_iter().enumerate().map(|(i, c)| Mover::new(c, 1, i as u64)).collect();
        movers[0].damage(DamageState::Firepower, 900.0);
        let mut armory = Armory::default();
        let mut a = vec![act("a1", 1000.0, 1)];
        armory.launch(&s, &mut a, &movers, &ours);
        assert!(a[0].held.as_deref().is_some_and(|h| h.contains("out of action")), "{:?}", a[0].held);
        assert!(armory.is_empty());
    }
}
