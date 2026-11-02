//! Rules 24 ([`crate::rules::LOADOUT_SINCE`], bd murmur-lbw8.1): an armed platform's loadout and
//! its rounds. The catalog lists the weapons a platform carries
//! ([`crate::catalog::WeaponFacts`]); an engagement uses one of them, the one its nomination names
//! or the one the engine picks ([`choose`]), and each release of a counted weapon spends a round.
//! What a decision reads of the rounds is a fact the engine fills from its ledger (the `rounds`
//! fact, [`PlatformRounds`]): the rounds released in this run, and those released in earlier runs
//! carried over at the start ([`RoundsSpent`], `run.started`'s `carried_from`) onto the same
//! vehicle only, as rules 15 carries a spent platform. Replay recomputes each from the log.
//!
//! Every function here is pure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{Domain, PlatformFacts, WeaponFacts, WeaponKind};
use crate::engage::VehicleIdentity;
use crate::ids::Uid;
use crate::time::Time;
use crate::world::{Fact, VehicleDamage};

/// Rules 24: a platform's rounds as a decision reads them (the engine's `rounds` fact, for an
/// engagement's platform): the rounds of each weapon released in this run, those released in
/// earlier runs and carried over at the start (each with the vehicle it was released from), and
/// the vehicle under the uid now (the bridge's identity, which a release logs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlatformRounds {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub used: BTreeMap<String, u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<CarriedRounds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<VehicleIdentity>,
}

impl PlatformRounds {
    pub fn is_empty(&self) -> bool {
        self.used.is_empty() && self.carried.is_empty() && self.now.is_none()
    }

    /// The rounds of `weapon` spent: this run's, and the carried ones that hold on the vehicle
    /// under the uid now.
    pub fn spent(&self, weapon: &str) -> u32 {
        self.used.get(weapon).copied().unwrap_or(0)
            + self
                .carried
                .iter()
                .filter(|c| c.weapon == weapon && c.holds())
                .map(|c| c.used)
                .sum::<u32>()
    }
}

/// Rules 24: rounds of one weapon released in an earlier run (`from`, its log), carried over at
/// the start: the vehicle they were released from, and why the vehicle under the uid now is
/// known to be another (they then do not count).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarriedRounds {
    pub from: String,
    pub weapon: String,
    pub used: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_on: Option<VehicleIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_vehicle: Option<String>,
}

impl CarriedRounds {
    /// Whether they count against the vehicle under the uid now: unless it is known to be another.
    pub fn holds(&self) -> bool {
        self.other_vehicle.is_none()
    }
}

/// Rules 24: rounds of one weapon a log released (its own, `from` none) or carried into it (`from`:
/// the log that released them), as a log's debts and `run.started`'s `carried_from` (`rounds`)
/// carry them; the vehicle they were released from (a later run counts them on that vehicle only).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoundsSpent {
    pub uid: Uid,
    pub weapon: String,
    pub used: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle: Option<VehicleIdentity>,
}

/// Rules 24: the weapon an `engage.effect` released (and the engagement's record, the console's):
/// its name and kind, the rounds of it left on the platform after it (none for the platform
/// itself, whose one round is the platform), and the vehicle it was released from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeaponRelease {
    pub name: String,
    pub kind: WeaponKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle: Option<VehicleIdentity>,
}

/// Rules 24: the rounds of `w` left on a platform whose rounds are `rounds`; none for the platform
/// itself (`self`: its one round is the platform, spent with it, rules 12 and 15).
pub fn left(w: &WeaponFacts, rounds: Option<&PlatformRounds>) -> Option<u32> {
    w.kind
        .counted()
        .then(|| w.count.saturating_sub(rounds.map_or(0, |r| r.spent(&w.name))))
}

/// Rules 24: the weapon an engagement uses, and why ([`choose`]).
#[derive(Clone, Debug, PartialEq)]
pub struct WeaponChoice {
    pub weapon: WeaponFacts,
    /// The engine picked it (the nomination named none).
    pub picked: bool,
    /// Its rounds left before this engagement (none for the platform itself).
    pub left: Option<u32>,
    /// The check's detail.
    pub detail: String,
}

/// Rules 25 ([`crate::rules::DAMAGE_SINCE`]): what a choice reads beside rules 24's: the damage
/// the platform's vehicle reported of itself (its weapons out of action refuse every weapon; its
/// propulsion lost refuses a platform that is its own weapon), the target's estimated kind (the
/// detail shows the weapon's figures against it) and the decision's time. Under older rules it is
/// [`Rules25::default`]: nothing read.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rules25<'a> {
    pub damage: Option<&'a Fact<VehicleDamage>>,
    pub kind: Option<&'a str>,
    pub now: Option<Time>,
    /// Show the weapon's figures (rules 25 on).
    pub figures: bool,
}

impl Rules25<'_> {
    fn reported(&self) -> String {
        match (self.damage, self.now) {
            (Some(d), Some(now)) => format!(" (reported {:.0} s ago)", (now.ms_since(d.t) as f64 / 1000.0).max(0.0)),
            _ => String::new(),
        }
    }
}

/// Rules 25: why no weapon of the platform can be used, its vehicle having reported its weapons
/// out of action, if it did.
fn disarmed(id: &str, r: &Rules25) -> Option<String> {
    r.damage.filter(|d| d.value.firepower).map(|_| {
        format!(
            "the vehicle reported its weapons out of action{}: no weapon of {id}'s loadout can be used",
            r.reported()
        )
    })
}

/// Rules 25: why `w` can't be used by a platform whose vehicle reported its propulsion lost, if
/// it can't: a platform that is its own weapon can't reach a target.
fn immobile(w: &WeaponFacts, r: &Rules25) -> Option<String> {
    let d = &r.damage?.value;
    (d.mobility && w.kind == WeaponKind::SelfStrike).then(|| {
        format!(
            "{} is the platform itself, whose vehicle reported its propulsion lost{}: it can't reach a target",
            w.name,
            r.reported()
        )
    })
}

/// Rules 25: the weapon's catalog figures against the target's estimated kind, for the check's
/// detail (empty for a weapon that lists none).
pub fn figures(w: &WeaponFacts, kind: Option<&str>) -> String {
    if w.effects.is_empty() {
        return String::new();
    }
    let Some(kind) = kind else {
        return "; the target's kind is not estimated, so no hit or damage figures are shown".into();
    };
    let Some((key, e)) = w.effect_for(kind) else {
        return format!("; no hit or damage figures against a {kind} (the weapon can't engage one)");
    };
    let mut given = Vec::new();
    for (p, what) in [(e.p_kill, "destroyed"), (e.p_mobility, "propulsion lost"), (e.p_firepower, "weapons out")] {
        if let Some(p) = p.filter(|p| *p > 0.0) {
            given.push(format!("{what} {p:.2}"));
        }
    }
    let as_ = if key == kind { String::new() } else { format!(" (the {key} figures)") };
    format!(
        "; against a {kind}{as_}, as estimated: a hit {:.2} in good conditions, and given a hit {}",
        e.p_hit.unwrap_or(0.0),
        if given.is_empty() { "no damage".to_string() } else { given.join(", ") }
    )
}

/// Why `w` can't be used against a target in `domain` now, if it can't.
fn unusable(w: &WeaponFacts, rounds: Option<&PlatformRounds>, domain: Option<Domain>) -> Option<String> {
    if let Some(why) = w.kind.inert_why() {
        return Some(format!("{} is a {}: {why}", w.name, w.kind.as_str()));
    }
    if let Some(d) = domain
        && !w.for_target(d)
    {
        return Some(format!(
            "{} is for {} targets, not {}",
            w.name,
            names(&w.targets),
            d.as_str()
        ));
    }
    if left(w, rounds) == Some(0) {
        return Some(format!("{} has no round left ({} of {} released)", w.name, w.count, w.count));
    }
    None
}

fn names(d: &[Domain]) -> String {
    d.iter().map(|d| d.as_str()).collect::<Vec<_>>().join(" and ")
}

/// What `w` is, in words, and its rounds left.
fn describe(w: &WeaponFacts, left: Option<u32>) -> String {
    let what = match w.kind {
        WeaponKind::SelfStrike => format!("{}, the platform itself (spent by its own effect)", w.name),
        k => format!(
            "{}, a {} for {} targets, {:.0} to {:.0} m at {:.0} m/s",
            w.name,
            k.as_str(),
            names(&w.targets),
            w.min_range_m,
            w.max_range_m,
            w.speed_mps
        ),
    };
    match left {
        Some(n) => format!("{what}; {n} of {} rounds left", w.count),
        None => what,
    }
}

/// Rules 24: the weapon of `platform`'s loadout an engagement against a target in `domain` uses:
/// the one `named` (it must be in the loadout, usable now, meant for the target's domain and have
/// a round left), or, none named, the first of the loadout in the catalog's order that is (the
/// catalog lists a platform's weapons in the order it prefers them). `Err` says why there is none,
/// each weapon passed over with its reason. `domain` none (no track in the picture): the target's
/// domain is not judged (the target checks fail on their own).
pub fn choose(
    named: Option<&str>,
    platform: &PlatformFacts,
    rounds: Option<&PlatformRounds>,
    domain: Option<Domain>,
) -> Result<WeaponChoice, String> {
    choose_with(named, platform, rounds, domain, &Rules25::default())
}

/// [`choose`], and from rules 25 what [`Rules25`] reads: a weapon the platform's reported damage
/// puts out of action is refused (named) or passed over (picked), and the detail of the one used
/// shows its figures against the target's estimated kind.
pub fn choose_with(
    named: Option<&str>,
    platform: &PlatformFacts,
    rounds: Option<&PlatformRounds>,
    domain: Option<Domain>,
    r25: &Rules25,
) -> Result<WeaponChoice, String> {
    let id = &platform.id;
    if let Some(why) = disarmed(id, r25) {
        return Err(match named {
            Some(n) => format!("{why}: the nomination names {n}"),
            None => why,
        });
    }
    let out_of_use = |w: &WeaponFacts| immobile(w, r25).or_else(|| unusable(w, rounds, domain));
    let shown = |w: &WeaponFacts| if r25.figures { figures(w, r25.kind) } else { String::new() };
    if let Some(n) = named {
        let Some(w) = platform.loadout.iter().find(|w| w.name == n) else {
            return Err(if platform.loadout.is_empty() {
                format!("{id} carries no loadout: no weapon named {n}")
            } else {
                format!(
                    "{id}'s loadout has no weapon named {n} (it carries {})",
                    platform.loadout.iter().map(|w| w.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            });
        };
        if let Some(why) = out_of_use(w) {
            return Err(format!("{why}: the nomination names it"));
        }
        let left = left(w, rounds);
        return Ok(WeaponChoice {
            weapon: w.clone(),
            picked: false,
            left,
            detail: format!("named: {}{}", describe(w, left), shown(w)),
        });
    }
    let mut passed = Vec::new();
    for w in &platform.loadout {
        match out_of_use(w) {
            Some(why) => passed.push(why),
            None => {
                let left = left(w, rounds);
                let target = domain.map_or(String::new(), |d| format!(" against a {} target", d.as_str()));
                let over = if passed.is_empty() {
                    String::new()
                } else {
                    format!(" (passed over: {})", passed.join("; "))
                };
                return Ok(WeaponChoice {
                    weapon: w.clone(),
                    picked: true,
                    left,
                    detail: format!(
                        "picked (none named): {}; the first of {id}'s loadout usable{target} with a round left{over}{}",
                        describe(w, left),
                        shown(w)
                    ),
                });
            }
        }
    }
    Err(if passed.is_empty() {
        format!("{id} carries no loadout")
    } else {
        format!("no weapon of {id}'s loadout can be used: {}", passed.join("; "))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::catalog::{FlightProfile, Warhead};

    fn weapon(name: &str, kind: WeaponKind, count: u32, targets: &[Domain]) -> WeaponFacts {
        WeaponFacts {
            name: name.into(),
            kind,
            count,
            min_range_m: 500.0,
            max_range_m: 8000.0,
            speed_mps: 400.0,
            profile: FlightProfile::Direct,
            guidance: "semi_active_laser".into(),
            warhead: Warhead {
                class: "shaped_charge".into(),
                mass_kg: 9.0,
            },
            targets: targets.to_vec(),
            effects: BTreeMap::new(),
            burst_rounds: None,
            rate_rpm: None,
            persist_h: None,
            trigger: None,
        }
    }

    fn platform(loadout: Vec<WeaponFacts>) -> PlatformFacts {
        PlatformFacts {
            id: "test_armed".into(),
            domain: Domain::Air,
            armed: true,
            cruise_mps: Some(90.0),
            min_turn_radius_m: None,
            endurance_h: None,
            autopilot: None,
            sensors: Vec::new(),
            climb_mps: None,
            max_mps: None,
            expended_by_effect: false,
            munition: None,
            loadout,
        }
    }

    /// Named: used if usable, refused saying why if not; none named: the first usable in order,
    /// the ones passed over said; none usable: refused, each reason said.
    #[test]
    fn a_weapon_is_named_or_picked_in_the_catalogs_order() {
        let p = platform(vec![
            weapon("gun", WeaponKind::Gun, 500, &[Domain::Ground]),
            weapon("aam", WeaponKind::Missile, 2, &[Domain::Air]),
            weapon("agm", WeaponKind::Missile, 4, &[Domain::Surface, Domain::Ground]),
        ]);
        let c = choose(None, &p, None, Some(Domain::Surface)).unwrap();
        assert_eq!((c.weapon.name.as_str(), c.picked, c.left), ("agm", true, Some(4)));
        assert!(c.detail.contains("passed over: gun is a gun"), "{}", c.detail);
        assert!(c.detail.contains("aam is for air targets, not surface"), "{}", c.detail);
        let c = choose(Some("agm"), &p, None, Some(Domain::Ground)).unwrap();
        assert!(!c.picked && c.detail.starts_with("named: agm"), "{}", c.detail);
        let e = choose(Some("aam"), &p, None, Some(Domain::Surface)).unwrap_err();
        assert!(e.contains("for air targets, not surface"), "{e}");
        let e = choose(Some("gun"), &p, None, Some(Domain::Ground)).unwrap_err();
        assert!(e.contains("murmur-lbw8.3"), "{e}");
        let e = choose(Some("hellfire"), &p, None, None).unwrap_err();
        assert!(e.contains("no weapon named hellfire (it carries gun, aam, agm)"), "{e}");
        // rounds spent: this run's and carried ones that hold; another vehicle's do not count
        let mut r = PlatformRounds::default();
        r.used.insert("agm".into(), 3);
        r.carried.push(CarriedRounds {
            from: "a.jsonl".into(),
            weapon: "agm".into(),
            used: 1,
            released_on: None,
            other_vehicle: None,
        });
        let e = choose(Some("agm"), &p, Some(&r), Some(Domain::Surface)).unwrap_err();
        assert!(e.contains("agm has no round left (4 of 4 released)"), "{e}");
        let e = choose(None, &p, Some(&r), Some(Domain::Surface)).unwrap_err();
        assert!(e.starts_with("no weapon of test_armed's loadout can be used"), "{e}");
        r.carried[0].other_vehicle = Some("its callsign differs".into());
        let c = choose(None, &p, Some(&r), Some(Domain::Surface)).unwrap();
        assert_eq!(c.left, Some(1));
        assert!(choose(None, &platform(Vec::new()), None, None).unwrap_err().contains("carries no loadout"));
    }
}
