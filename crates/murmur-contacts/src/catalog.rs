//! The contact catalog (`sim/contacts.toml`, bd murmur-y85j.1, docs/AUTONOMOUS-NODES.md 2.1):
//! what each kind of contact that is not ours is and what it carries, in the shape of our own
//! catalog (`sim/platforms.toml`). Its figures and their sources are in
//! docs/research/opponent-weapons.md.
//!
//! It replaces the `KINDS` table that `scenario.rs` held: a scenario's contact takes its kind's
//! defaults (signature, emits, turn rate, height) from here ([`crate::scenario::kind_defaults`]).
//! An armed variant names the kind it passes for (`looks_like`, decision D3): the contact then
//! publishes the look-alike as its truth `kind` and its own kind as the truth-only `variant`, so
//! the bridge's sensing, tracker and identification see exactly what they saw before.
//!
//! The loadouts are what a node's fire control fires (stage 2, bd murmur-y85j.2, [`crate::node`]),
//! its `sensors` what it perceives a target by, its speeds how it closes and flees. Each loadout
//! entry is the decision core's `WeaponFacts` (crates/murmur-decide catalog.rs) plus two
//! truth-side keys, `reload_s` and `sense`; crates/murmur-headless tests that the decision core
//! reads every entry as its own.
//!
//! The file is compiled in ([`Catalog::shipped`]), as the table it replaces was: the contacts
//! process, the headless simulator and every test read the same catalog without a path.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::damage::{Figures, Warhead, WeaponDamage};
use crate::scenario::Domain;

/// The shipped catalog's text (`sim/contacts.toml`).
pub const SHIPPED: &str = include_str!("../../../sim/contacts.toml");

/// The weapon kinds of a loadout entry (the decision core's `WeaponKind`).
pub const WEAPON_KINDS: &[&str] = &["missile", "bomb", "torpedo", "self", "gun", "mine"];

/// The flight profiles of a loadout entry (the decision core's `FlightProfile`).
pub const PROFILES: &[&str] = &["direct", "lofted", "glide", "ballistic", "dive", "sea_skimming", "waterborne", "laid"];

/// The weapon signatures a loadout entry may be sensed by (`sense`): `atgm` has the bridge's
/// figures today (tracks/physical/weapon.rs `weapon_signature`); stage 3 (bd murmur-y85j.3)
/// gives the others theirs.
pub const SENSES: &[&str] = &["atgm", "rocket", "air_missile", "gun", "torpedo"];

/// What a node itself sees (decision D2): our vehicles within these reaches, m.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sensors {
    #[serde(default)]
    pub eye_m: f64,
    #[serde(default)]
    pub radar_m: f64,
    #[serde(default)]
    pub sonar_m: f64,
}

/// One weapon of a kind's loadout: the decision core's `WeaponFacts`, field for field, plus the
/// truth side's `reload_s` and `sense`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weapon {
    pub name: String,
    pub kind: String,
    pub count: u32,
    pub min_range_m: f64,
    pub max_range_m: f64,
    pub speed_mps: f64,
    pub profile: String,
    pub guidance: String,
    pub warhead: Warhead,
    pub targets: Vec<Domain>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub effects: BTreeMap<String, Figures>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_rpm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_interval_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist_h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm_s: Option<f64>,
    /// Seconds from one shot of a missile, rocket or torpedo to the next (truth side only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_s: Option<f64>,
    /// The weapon signature our sensors see it by ([`SENSES`]); none for `self` (truth side only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sense: Option<String>,
}

impl Weapon {
    /// The weapon as the damage model reads it.
    pub fn damage(&self) -> WeaponDamage {
        WeaponDamage {
            name: self.name.clone(),
            kind: self.kind.clone(),
            guidance: self.guidance.clone(),
            warhead: Some(self.warhead.clone()),
            max_range_m: Some(self.max_range_m),
            effects: self.effects.clone(),
        }
    }

    /// The figures it lists, by the name `sources` keys them under: each must carry a tag.
    pub fn figures(&self) -> Vec<&'static str> {
        let mut f = vec!["count", "min_range_m", "max_range_m", "speed_mps", "guidance", "warhead"];
        let optional = [
            ("effects", !self.effects.is_empty()),
            ("burst_rounds", self.burst_rounds.is_some()),
            ("rate_rpm", self.rate_rpm.is_some()),
            ("burst_interval_s", self.burst_interval_s.is_some()),
            ("persist_h", self.persist_h.is_some()),
            ("trigger_m", self.trigger_m.is_some()),
            ("arm_s", self.arm_s.is_some()),
            ("reload_s", self.reload_s.is_some()),
        ];
        f.extend(optional.into_iter().filter(|(_, has)| *has).map(|(k, _)| k));
        f
    }

    /// Why the entry can't stand, if it can't: the decision core's rules for a `WeaponFacts`,
    /// and the truth side's for `reload_s` and `sense`.
    fn invalid(&self) -> Option<String> {
        let n = &self.name;
        let finite_pos = |v: Option<f64>| v.is_some_and(|v| v.is_finite() && v > 0.0);
        if n.trim().is_empty() {
            return Some("a weapon has no name".into());
        }
        if !WEAPON_KINDS.contains(&self.kind.as_str()) {
            return Some(format!("{n}: kind {:?} is none of {}", self.kind, WEAPON_KINDS.join(", ")));
        }
        if !PROFILES.contains(&self.profile.as_str()) {
            return Some(format!("{n}: profile {:?} is none of {}", self.profile, PROFILES.join(", ")));
        }
        if self.count == 0 {
            return Some(format!("{n}: count is at least 1"));
        }
        if !(self.min_range_m.is_finite() && self.min_range_m >= 0.0 && self.max_range_m.is_finite() && self.max_range_m > self.min_range_m) {
            return Some(format!("{n}: ranges are 0 <= min_range_m < max_range_m"));
        }
        let mine = self.kind == "mine";
        if !(self.speed_mps.is_finite() && (self.speed_mps > 0.0 || (mine && self.speed_mps == 0.0))) {
            return Some(format!("{n}: speed_mps is above 0 (0 for a mine)"));
        }
        if self.kind == "self" && self.count != 1 {
            return Some(format!("{n}: a self strike is the node itself, count 1"));
        }
        if self.targets.is_empty() {
            return Some(format!("{n}: targets names at least one domain"));
        }
        if self.kind == "gun"
            && !(self.burst_rounds.is_some_and(|b| b >= 1 && b <= self.count)
                && finite_pos(self.rate_rpm)
                && self.burst_interval_s.is_some_and(|i| i.is_finite() && i >= 1.0))
        {
            return Some(format!("{n}: a gun names burst_rounds (1 to its count), rate_rpm (above 0) and burst_interval_s (1 s or more)"));
        }
        if mine
            && !(finite_pos(self.persist_h)
                && self.arm_s.is_some_and(|a| a.is_finite() && a >= 0.0)
                && self.trigger.as_ref().is_some_and(|t| !t.trim().is_empty())
                && self.trigger_m.is_none_or(|r| r.is_finite() && r > 0.0 && r <= self.max_range_m))
        {
            return Some(format!("{n}: a mine names persist_h, arm_s, trigger, and trigger_m within max_range_m if any"));
        }
        if self.guidance.trim().is_empty() || self.warhead.class.trim().is_empty() {
            return Some(format!("{n}: guidance and warhead.class are named"));
        }
        if !(self.warhead.mass_kg.is_finite() && self.warhead.mass_kg >= 0.0) {
            return Some(format!("{n}: warhead.mass_kg is 0 or more"));
        }
        if let Err(e) = self.damage().check() {
            return Some(e);
        }
        // the truth side's keys: a missile, rocket or torpedo waits its reload between shots; a
        // gun its burst interval; the node itself, once
        let shoots = matches!(self.kind.as_str(), "missile" | "bomb" | "torpedo");
        if shoots != finite_pos(self.reload_s) || (!shoots && self.reload_s.is_some()) {
            return Some(format!("{n}: reload_s (above 0) for a missile, bomb or torpedo, and for nothing else"));
        }
        let sensed = !matches!(self.kind.as_str(), "self" | "mine");
        match &self.sense {
            Some(s) if sensed && SENSES.contains(&s.as_str()) => {}
            None if !sensed => {}
            _ => return Some(format!("{n}: sense, one of {}, for any kind but self and mine (and none for those)", SENSES.join(", "))),
        }
        None
    }
}

/// What a node does once it has no round left ([`Kind::on_empty`]): away along its way in at its
/// top speed (the default), stop where it is, or back to its file's motion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnEmpty {
    #[default]
    Flee,
    Hold,
    Resume,
}

/// A source tag (`"[3]"`, `"[2, 3; E]"`, `"[E]"`) or, for a weapon, its figures' tags by name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Source {
    Tag(String),
    Weapon(BTreeMap<String, String>),
}

/// One kind of contact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Kind {
    pub name: String,
    pub domain: Domain,
    /// The kind our sensors and the C2 see (decision D3); none: itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub looks_like: Option<String>,
    pub signature: f64,
    pub emits: bool,
    pub turn_dps: f64,
    pub alt_m: f64,
    pub cruise_mps: f64,
    pub max_mps: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensors: Option<Sensors>,
    #[serde(default)]
    pub armed: bool,
    #[serde(default)]
    pub expended_by_effect: bool,
    /// What a node of the kind does once an order has spent its last round (stage 2,
    /// docs/AUTONOMOUS-NODES.md 2.3): none is [`OnEmpty::Flee`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_empty: Option<OnEmpty>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loadout: Vec<Weapon>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sources: BTreeMap<String, Source>,
}

/// The catalog: every kind by its name.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub kinds: BTreeMap<String, Kind>,
}

impl Catalog {
    /// Parse and validate a catalog.
    pub fn from_toml(text: &str) -> Result<Catalog, String> {
        let kinds: BTreeMap<String, Kind> = toml::from_str(text).map_err(|e| format!("contact catalog: {e}"))?;
        let c = Catalog { kinds };
        c.check()?;
        Ok(c)
    }

    /// The shipped catalog (`sim/contacts.toml`, compiled in). A broken file fails its tests
    /// first; here it stops the process with the reason.
    pub fn shipped() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(|| Catalog::from_toml(SHIPPED).unwrap_or_else(|e| panic!("sim/contacts.toml: {e}")))
    }

    pub fn get(&self, kind: &str) -> Option<&Kind> {
        self.kinds.get(kind)
    }

    /// The kind our sensors see a contact of `kind` as: its `looks_like`, or itself.
    pub fn look<'a>(&'a self, kind: &'a str) -> &'a str {
        self.get(kind).and_then(|k| k.looks_like.as_deref()).unwrap_or(kind)
    }

    fn check(&self) -> Result<(), String> {
        for (id, k) in &self.kinds {
            let bad = |m: String| Err(format!("contact catalog [{id}]: {m}"));
            if k.name.trim().is_empty() {
                return bad("name is empty".into());
            }
            if !(k.signature.is_finite() && k.signature > 0.0 && k.turn_dps.is_finite() && k.turn_dps > 0.0 && k.alt_m.is_finite()) {
                return bad("signature and turn_dps are above 0, alt_m a number".into());
            }
            if !(k.cruise_mps.is_finite() && k.cruise_mps > 0.0 && k.max_mps.is_finite() && k.max_mps >= k.cruise_mps) {
                return bad("0 < cruise_mps <= max_mps".into());
            }
            if let Some(look) = &k.looks_like {
                match self.kinds.get(look) {
                    Some(l) if l.looks_like.is_none() && l.domain == k.domain && look != id => {}
                    _ => return bad(format!("looks_like {look:?} is no other kind of its domain that is itself")),
                }
            }
            if let Some(s) = &k.sensors
                && [s.eye_m, s.radar_m, s.sonar_m].iter().any(|m| !(m.is_finite() && *m >= 0.0))
            {
                return bad("sensors' reaches are 0 or more".into());
            }
            if k.armed != !k.loadout.is_empty() {
                return bad("armed is true exactly when it has a loadout".into());
            }
            if k.armed && k.sensors.is_none() {
                return bad("an armed kind names its sensors (what it perceives a target by)".into());
            }
            let mut names = std::collections::BTreeSet::new();
            for w in &k.loadout {
                if let Some(why) = w.invalid() {
                    return bad(format!("loadout: {why}"));
                }
                if !names.insert(w.name.as_str()) {
                    return bad(format!("loadout: {} is named twice", w.name));
                }
                if let Some(t) = w.effects.keys().find(|t| !self.kinds.contains_key(t.as_str())) {
                    return bad(format!("loadout: {}: effects.{t} is no kind of the catalog", w.name));
                }
            }
            let selves = k.loadout.iter().filter(|w| w.kind == "self").count();
            if selves > 1 || (selves == 1) != k.expended_by_effect {
                return bad("expended_by_effect exactly when it is its own weapon (one self entry)".into());
            }
        }
        Ok(())
    }

    /// Every figure without a well-formed source tag, as `kind: figure` (the catalog's
    /// `sources`; docs/research/opponent-weapons.md holds the references and the estimates'
    /// bases), and every reference number the tags cite, for a test to find in that note.
    pub fn sources(&self) -> (Vec<String>, std::collections::BTreeSet<u32>) {
        let mut missing = Vec::new();
        let mut cited = std::collections::BTreeSet::new();
        let mut tag = |what: String, t: Option<&String>| match t.and_then(|t| parse_tag(t)) {
            Some(ns) => cited.extend(ns),
            None => missing.push(what),
        };
        for (id, k) in &self.kinds {
            let kind_tag = |f: &str| match k.sources.get(f) {
                Some(Source::Tag(t)) => Some(t),
                _ => None,
            };
            for f in ["cruise_mps", "max_mps"].into_iter().chain(k.sensors.is_some().then_some("sensors")) {
                tag(format!("{id}: {f}"), kind_tag(f));
            }
            for w in &k.loadout {
                let tags = match k.sources.get(&w.name) {
                    Some(Source::Weapon(m)) => Some(m),
                    _ => None,
                };
                for f in w.figures() {
                    tag(format!("{id}: {}: {f}", w.name), tags.and_then(|m| m.get(f)));
                }
            }
        }
        (missing, cited)
    }
}

/// A tag's reference numbers (`"[2, 3; E]"` is 2 and 3), if it is a tag: `[...]` holding numbers
/// and `E`, separated by commas or semicolons.
fn parse_tag(t: &str) -> Option<Vec<u32>> {
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    let mut ns = Vec::new();
    for part in inner.split([',', ';']).map(str::trim) {
        match part {
            "E" => {}
            p => ns.push(p.parse::<u32>().ok()?),
        }
    }
    Some(ns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// Every kind a shipped contacts file names (sim/scenarios, the situations' contacts and
    /// Saturation's generated file among them) and every kind of the table the catalog replaced
    /// (which the frozen v1 and v2 worlds' generators draw on) has an entry, and every figure of
    /// the catalog carries a source tag whose references the research note lists.
    #[test]
    fn every_shipped_kind_is_catalogued_and_every_figure_sourced() {
        let cat = Catalog::shipped();
        // the `KINDS` table as it was (bd murmur-y85j.1 moved it here)
        let replaced = [
            "cargo_ship", "tanker", "ferry", "patrol_boat", "fishing_vessel", "sailboat", "small_fast_boat", "light_aircraft",
            "helicopter", "unknown_uas", "submarine", "uuv", "vehicle", "truck", "small_drone", "periscope", "diver", "person",
        ];
        let mut named: Vec<(String, String)> = replaced.iter().map(|k| ("KINDS".to_string(), k.to_string())).collect();
        let mut files: Vec<_> = std::fs::read_dir(repo().join("sim/scenarios"))
            .expect("sim/scenarios")
            .map(|e| e.expect("entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        assert!(files.len() > 10, "the shipped contacts files");
        for p in &files {
            let s = crate::scenario::Scenario::from_toml(&std::fs::read_to_string(p).expect("read")).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            named.extend(s.contacts.iter().map(|c| (p.display().to_string(), c.kind.clone())));
        }
        let missing: Vec<_> = named.iter().filter(|(_, k)| cat.get(k).is_none()).collect();
        assert!(missing.is_empty(), "kinds with no catalog entry: {missing:?}");

        let (unsourced, cited) = cat.sources();
        assert!(unsourced.is_empty(), "figures without a source tag: {unsourced:?}");
        let note = std::fs::read_to_string(repo().join("docs/research/opponent-weapons.md")).expect("the research note");
        let refs = note.split("\n## Sources").nth(1).expect("its Sources section");
        let listed: std::collections::BTreeSet<u32> = refs
            .lines()
            .filter_map(|l| l.split_once(". ").and_then(|(n, _)| n.trim().parse().ok()))
            .collect();
        let unlisted: Vec<_> = cited.difference(&listed).collect();
        assert!(unlisted.is_empty(), "references the catalog cites that the note does not list: {unlisted:?}");
        // the armed variants are what the migration (docs/AUTONOMOUS-NODES.md section 4) names
        for (k, look) in [("armed_fast_boat", "small_fast_boat"), ("armed_coaster", "cargo_ship"), ("naval_auxiliary", "tanker"), ("armed_uas", "unknown_uas"), ("oneway_drone", "small_drone"), ("cutter", "patrol_boat"), ("technical", "truck")] {
            assert_eq!(cat.look(k), look, "{k}");
            assert!(cat.get(k).is_some_and(|e| e.armed), "{k} is armed");
        }
        assert!(cat.get("submarine").is_some_and(|e| e.armed && e.looks_like.is_none()));
        // a contact of an armed variant is its look-alike to the bridge, its variant in truth only
        let s = crate::scenario::Scenario::from_toml(
            "title = \"t\"\nseed = 1\n[[contact]]\nid = \"raider\"\naffiliation = \"hostile\"\ndomain = \"surface\"\nkind = \"armed_fast_boat\"\nmotion = \"station\"\nat = [-122.5, 37.8]\n",
        )
        .expect("scenario");
        let c = &s.contacts().expect("contacts")[0];
        assert_eq!((c.kind.as_str(), c.variant.as_deref()), ("small_fast_boat", Some("armed_fast_boat")));
    }
}
