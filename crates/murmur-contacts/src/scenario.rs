//! `sim/scenarios/*.toml`: the contacts of a scenario and how they move.
//!
//! ```toml
//! title = "Golden Gate traffic"
//! seed = 7                      # motion jitter and the bridge's sensing draws
//!
//! [sensing]
//! misclassification_rate = 0.05 # injected classifier error, forwarded to the bridge
//!
//! [[contact]]
//! id = "ferry-sausalito"        # unique; the evaluation log keys on it
//! name = "Sausalito ferry"      # truth-side label (the C2 never sees it)
//! affiliation = "neutral"       # hostile | suspect | neutral | unknown | assumed_friend
//! domain = "surface"            # air | surface | subsurface | ground
//! kind = "ferry"                # see KINDS for the defaults each kind carries
//! speed_mps = 11
//! motion = "route"              # route | transit | loiter | station | drift
//! route = [[-122.3920, 37.7960], [-122.4790, 37.8560]]   # [lon, lat] (GeoJSON order)
//! reverse = true                # back and forth (default: loop back to the first point)
//! dwell_s = 300                 # wait at each end
//! ais = { mmsi = "366999001", name = "MV SAUSALITO", ship_type = "passenger" }
//! ```
//!
//! Optional per contact: `alt_m` (negative = depth; defaults per domain), `emits` (has a
//! radar or transponder an ESM receiver can hear; default per kind), `signature` (detection
//! range multiplier; default per kind), `turn_dps`, `start_s` and `end_s` (scenario seconds
//! the contact exists), `ais.spoofed = true` (the AIS identity is false: a suspect boat
//! claiming to be a fishing vessel), `on_effect` (what an engagement's abstract effect does
//! to it: `"out_of_play"`, the default, removes it from the scenario, so our sensors stop
//! detecting it; `"persists"` keeps it moving, flagged). `transit` moves once from `route[0]` to the last point
//! and holds there; `loiter` needs `center` and `radius_m`; `station` and `drift` need
//! `at`. Drift follows the tidewater current when the contacts process has conditions.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affiliation {
    Hostile,
    Suspect,
    Neutral,
    Unknown,
    AssumedFriend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Air,
    Surface,
    Subsurface,
    Ground,
}

/// A cooperative identity broadcast on AIS (ships) or, by analogy, a transponder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ais {
    pub mmsi: String,
    pub name: String,
    /// passenger | cargo | fishing | tanker | tug | pleasure | law_enforcement | military | other
    pub ship_type: String,
    /// The broadcast identity is false (truth knows; the C2 has to notice).
    #[serde(default)]
    pub spoofed: bool,
}

/// What a kind of contact is like by default: radar/EO signature relative to a nominal
/// contact, whether it emits (navigation radar, transponder) and how fast it turns.
pub struct KindDefaults {
    pub kind: &'static str,
    pub domain: Domain,
    pub signature: f64,
    pub emits: bool,
    pub turn_dps: f64,
    pub alt_m: f64,
}

/// Rounded, indicative figures: a cargo ship is a big radar target, a small fast boat or a
/// small UAS a poor one, a submarine quiet. Not models of specific vessels.
pub const KINDS: &[KindDefaults] = &[
    KindDefaults { kind: "cargo_ship", domain: Domain::Surface, signature: 1.6, emits: true, turn_dps: 1.0, alt_m: 0.0 },
    KindDefaults { kind: "tanker", domain: Domain::Surface, signature: 1.6, emits: true, turn_dps: 0.8, alt_m: 0.0 },
    KindDefaults { kind: "ferry", domain: Domain::Surface, signature: 1.2, emits: true, turn_dps: 3.0, alt_m: 0.0 },
    KindDefaults { kind: "patrol_boat", domain: Domain::Surface, signature: 0.9, emits: true, turn_dps: 5.0, alt_m: 0.0 },
    KindDefaults { kind: "fishing_vessel", domain: Domain::Surface, signature: 0.7, emits: true, turn_dps: 4.0, alt_m: 0.0 },
    KindDefaults { kind: "sailboat", domain: Domain::Surface, signature: 0.5, emits: false, turn_dps: 6.0, alt_m: 0.0 },
    KindDefaults { kind: "small_fast_boat", domain: Domain::Surface, signature: 0.35, emits: false, turn_dps: 12.0, alt_m: 0.0 },
    KindDefaults { kind: "light_aircraft", domain: Domain::Air, signature: 0.6, emits: true, turn_dps: 6.0, alt_m: 300.0 },
    KindDefaults { kind: "helicopter", domain: Domain::Air, signature: 0.7, emits: true, turn_dps: 10.0, alt_m: 200.0 },
    KindDefaults { kind: "unknown_uas", domain: Domain::Air, signature: 0.15, emits: false, turn_dps: 20.0, alt_m: 120.0 },
    KindDefaults { kind: "submarine", domain: Domain::Subsurface, signature: 0.3, emits: false, turn_dps: 2.0, alt_m: -50.0 },
    KindDefaults { kind: "uuv", domain: Domain::Subsurface, signature: 0.15, emits: false, turn_dps: 8.0, alt_m: -20.0 },
    KindDefaults { kind: "vehicle", domain: Domain::Ground, signature: 0.5, emits: false, turn_dps: 15.0, alt_m: 0.0 },
    KindDefaults { kind: "truck", domain: Domain::Ground, signature: 0.8, emits: false, turn_dps: 10.0, alt_m: 0.0 },
];

pub fn kind_defaults(kind: &str) -> Option<&'static KindDefaults> {
    KINDS.iter().find(|k| k.kind == kind)
}

/// What an engagement's abstract effect (Phase E) does to a contact inside its radius: a
/// flag and a time, and either out of play (no longer in the truth, so our sensors stop
/// detecting it) or unchanged. No damage, blast or lethality is modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnEffect {
    #[default]
    OutOfPlay,
    Persists,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Sensing {
    /// Probability, per track, that a classifying sensor reports the wrong kind.
    #[serde(default)]
    pub misclassification_rate: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    pub title: String,
    #[serde(default)]
    pub seed: u64,
    #[serde(default)]
    pub sensing: Sensing,
    #[serde(default, rename = "contact")]
    pub contacts: Vec<ContactSpec>,
    /// Rules 9: scripted hostile acts (`[[act]]`), truth only.
    #[serde(default, rename = "act")]
    pub acts: Vec<ActSpec>,
}

/// Rules 9: an abstract hostile act a contact commits at a scenario second, against one of
/// our vehicles (`against_vehicle`, its fleet uid) or another contact (`against_contact`).
/// No weapon of any kind is modelled: an act is a time, an author and a victim. It is truth:
/// the C2 learns of it only through its sensors (the bridge's sensing model decides which of
/// ours observe it, and where they place its author).
///
/// ```toml
/// [[act]]
/// id = "act-1"
/// at_s = 90
/// by = "fast-boat"            # a contact id
/// against_vehicle = 3         # or against_contact = "ferry-sausalito"
/// label = "fires on the patrol USV"   # truth-side note, never seen by the C2
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActSpec {
    pub id: String,
    pub at_s: f64,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_vehicle: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub against_contact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One contact as written in the file (flat fields; [`Motion`] is derived and validated).
#[derive(Debug, Clone, Deserialize)]
pub struct ContactSpec {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub affiliation: Affiliation,
    pub domain: Domain,
    pub kind: String,
    #[serde(default)]
    pub ais: Option<Ais>,
    #[serde(default)]
    pub emits: Option<bool>,
    #[serde(default)]
    pub signature: Option<f64>,
    #[serde(default)]
    pub speed_mps: f64,
    #[serde(default)]
    pub alt_m: Option<f64>,
    #[serde(default)]
    pub turn_dps: Option<f64>,
    pub motion: String,
    #[serde(default)]
    pub route: Vec<[f64; 2]>,
    #[serde(default)]
    pub reverse: bool,
    #[serde(default)]
    pub dwell_s: f64,
    #[serde(default)]
    pub center: Option<[f64; 2]>,
    #[serde(default)]
    pub radius_m: Option<f64>,
    #[serde(default)]
    pub at: Option<[f64; 2]>,
    #[serde(default)]
    pub start_s: f64,
    #[serde(default)]
    pub end_s: Option<f64>,
    #[serde(default)]
    pub on_effect: OnEffect,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Motion {
    /// Waypoints `[lon, lat]`; back to the first point, or back and forth with `reverse`.
    Route {
        points: Vec<[f64; 2]>,
        reverse: bool,
        dwell_s: f64,
    },
    /// Once along the points, then hold at the last one.
    Transit { points: Vec<[f64; 2]> },
    Loiter { center: [f64; 2], radius_m: f64 },
    Station { at: [f64; 2] },
    /// With the current (tidewater) plus a little jitter; still water without conditions.
    Drift { at: [f64; 2] },
}

/// A validated contact with every default filled in.
#[derive(Debug, Clone)]
pub struct Contact {
    pub id: String,
    pub name: String,
    pub affiliation: Affiliation,
    pub domain: Domain,
    pub kind: String,
    pub ais: Option<Ais>,
    pub emits: bool,
    pub signature: f64,
    pub speed_mps: f64,
    pub alt_m: f64,
    pub turn_dps: f64,
    pub motion: Motion,
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub on_effect: OnEffect,
}

impl ContactSpec {
    pub fn validate(&self) -> Result<Contact, String> {
        let bad = |m: String| format!("contact {:?}: {m}", self.id);
        if self.id.trim().is_empty() {
            return Err("a contact has an empty id".into());
        }
        let valid = |p: &[f64; 2]| p[0].abs() <= 180.0 && p[1].abs() <= 90.0;
        let points = |min: usize| -> Result<Vec<[f64; 2]>, String> {
            if self.route.len() < min {
                return Err(bad(format!(
                    "motion {:?} needs at least {min} route points",
                    self.motion
                )));
            }
            if let Some(p) = self.route.iter().find(|p| !valid(p)) {
                return Err(bad(format!("route point {p:?} is not [lon, lat]")));
            }
            Ok(self.route.clone())
        };
        let point = |p: Option<[f64; 2]>, name: &str| -> Result<[f64; 2], String> {
            let p = p.ok_or_else(|| bad(format!("motion {:?} needs `{name}`", self.motion)))?;
            if !valid(&p) {
                return Err(bad(format!("{name} {p:?} is not [lon, lat]")));
            }
            Ok(p)
        };
        let motion = match self.motion.as_str() {
            "route" => Motion::Route {
                points: points(2)?,
                reverse: self.reverse,
                dwell_s: self.dwell_s.max(0.0),
            },
            "transit" => Motion::Transit { points: points(2)? },
            "loiter" => Motion::Loiter {
                center: point(self.center, "center")?,
                radius_m: self
                    .radius_m
                    .filter(|r| *r > 0.0)
                    .ok_or_else(|| bad("loiter needs radius_m > 0".into()))?,
            },
            "station" => Motion::Station {
                at: point(self.at, "at")?,
            },
            "drift" => Motion::Drift {
                at: point(self.at, "at")?,
            },
            other => {
                return Err(bad(format!(
                    "unknown motion {other:?} (route, transit, loiter, station, drift)"
                )));
            }
        };
        let moving = !matches!(motion, Motion::Station { .. } | Motion::Drift { .. });
        if moving && (self.speed_mps <= 0.0 || !self.speed_mps.is_finite()) {
            return Err(bad(format!("motion {:?} needs speed_mps > 0", self.motion)));
        }
        if self.ais.is_some() && self.domain != Domain::Surface {
            return Err(bad("ais is for surface contacts".into()));
        }
        let d = kind_defaults(&self.kind);
        if let Some(d) = d
            && d.domain != self.domain
        {
            return Err(bad(format!(
                "kind {:?} is a {:?} contact, not {:?}",
                self.kind, d.domain, self.domain
            )));
        }
        let default_alt = match self.domain {
            Domain::Air => 300.0,
            Domain::Subsurface => -30.0,
            Domain::Surface | Domain::Ground => 0.0,
        };
        Ok(Contact {
            id: self.id.clone(),
            name: self.name.clone().unwrap_or_else(|| self.id.clone()),
            affiliation: self.affiliation,
            domain: self.domain,
            kind: self.kind.clone(),
            ais: self.ais.clone(),
            emits: self.emits.unwrap_or(d.is_some_and(|d| d.emits)),
            signature: self
                .signature
                .filter(|s| *s > 0.0)
                .unwrap_or(d.map_or(1.0, |d| d.signature)),
            speed_mps: self.speed_mps.max(0.0),
            alt_m: self.alt_m.unwrap_or(d.map_or(default_alt, |d| d.alt_m)),
            turn_dps: self
                .turn_dps
                .filter(|t| *t > 0.0)
                .unwrap_or(d.map_or(5.0, |d| d.turn_dps)),
            motion,
            start_s: self.start_s.max(0.0),
            end_s: self.end_s,
            on_effect: self.on_effect,
        })
    }
}

impl Scenario {
    pub fn from_toml(text: &str) -> Result<Scenario, String> {
        let s: Scenario = toml::from_str(text).map_err(|e| e.to_string())?;
        if !(0.0..=1.0).contains(&s.sensing.misclassification_rate) {
            return Err("sensing.misclassification_rate must be within 0..1".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for c in &s.contacts {
            if !seen.insert(c.id.as_str()) {
                return Err(format!("duplicate contact id {:?}", c.id));
            }
        }
        let mut acts = std::collections::BTreeSet::new();
        for a in &s.acts {
            if a.id.trim().is_empty() || !acts.insert(a.id.as_str()) {
                return Err(format!("act {:?}: an empty or duplicate id", a.id));
            }
            if !seen.contains(a.by.as_str()) {
                return Err(format!("act {:?}: by {:?} is no contact of the scenario", a.id, a.by));
            }
            if a.against_vehicle.is_some() == a.against_contact.is_some() {
                return Err(format!("act {:?}: name one of against_vehicle and against_contact", a.id));
            }
            if let Some(c) = &a.against_contact
                && (!seen.contains(c.as_str()) || *c == a.by)
            {
                return Err(format!("act {:?}: against_contact {c:?} is no other contact of the scenario", a.id));
            }
            if !(a.at_s.is_finite() && a.at_s >= 0.0) {
                return Err(format!("act {:?}: at_s must be a scenario second", a.id));
            }
        }
        Ok(s)
    }

    pub fn load(path: &Path) -> Result<Scenario, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Scenario::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every contact validated, in file order.
    pub fn contacts(&self) -> Result<Vec<Contact>, String> {
        self.contacts.iter().map(ContactSpec::validate).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_fills_defaults() {
        let s = Scenario::from_toml(
            r#"
            title = "t"
            seed = 3
            [sensing]
            misclassification_rate = 0.1
            [[contact]]
            id = "f1"
            affiliation = "neutral"
            domain = "surface"
            kind = "ferry"
            speed_mps = 11
            motion = "route"
            route = [[-122.39, 37.79], [-122.47, 37.85]]
            reverse = true
            dwell_s = 60
            ais = { mmsi = "1", name = "MV X", ship_type = "passenger" }
            [[contact]]
            id = "u1"
            affiliation = "unknown"
            domain = "air"
            kind = "unknown_uas"
            speed_mps = 15
            motion = "loiter"
            center = [-122.48, 37.83]
            radius_m = 400
            "#,
        )
        .expect("parses");
        assert_eq!(s.seed, 3);
        assert_eq!(s.sensing.misclassification_rate, 0.1);
        let cs = s.contacts().expect("valid");
        assert_eq!(cs.len(), 2);
        let f = &cs[0];
        assert_eq!(f.name, "f1");
        assert!(f.emits);
        assert_eq!(f.signature, 1.2);
        assert_eq!(f.turn_dps, 3.0);
        assert!(matches!(&f.motion, Motion::Route { reverse: true, dwell_s, .. } if *dwell_s == 60.0));
        let u = &cs[1];
        assert!(!u.emits);
        assert_eq!(u.alt_m, 120.0);
        assert_eq!(u.signature, 0.15);
    }

    #[test]
    fn refuses_bad_contacts() {
        let base = |body: &str| {
            Scenario::from_toml(&format!(
                "title = \"t\"\n[[contact]]\nid = \"c\"\naffiliation = \"neutral\"\n{body}"
            ))
            .and_then(|s| s.contacts())
        };
        assert!(base("domain = \"surface\"\nkind = \"ferry\"\nmotion = \"route\"\nroute = [[0,0]]\nspeed_mps = 1").is_err());
        assert!(base("domain = \"surface\"\nkind = \"ferry\"\nmotion = \"loiter\"\ncenter = [0,0]\nspeed_mps = 1").is_err());
        assert!(base("domain = \"surface\"\nkind = \"ferry\"\nmotion = \"route\"\nroute = [[0,0],[1,1]]").is_err());
        assert!(base("domain = \"air\"\nkind = \"ferry\"\nmotion = \"station\"\nat = [0,0]").is_err());
        assert!(base("domain = \"air\"\nkind = \"light_aircraft\"\nmotion = \"hover\"\nat = [0,0]").is_err());
        assert!(base("domain = \"surface\"\nkind = \"fishing_vessel\"\nmotion = \"drift\"\nat = [0,0]").is_ok());
        assert!(
            Scenario::from_toml("title = \"t\"\n[[contact]]\nid = \"a\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat=[0,0]\n[[contact]]\nid = \"a\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat=[0,0]\n").is_err()
        );
    }

    /// Every contacts scenario that ships loads, its contacts and acts valid.
    #[test]
    fn the_shipped_scenarios_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sim/scenarios");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).expect("sim/scenarios") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "toml") {
                let s = Scenario::load(&p).expect("loads");
                s.contacts().unwrap_or_else(|e| panic!("{}: {e}", p.display()));
                n += 1;
            }
        }
        assert!(n >= 4, "{n} scenarios");
        let ident = Scenario::load(&dir.join("golden-gate-ident.toml")).expect("loads");
        assert_eq!(ident.acts.len(), 3);
    }

    /// Rules 9: scripted acts name an author and exactly one victim among the scenario's
    /// contacts or our vehicles; anything else fails to load.
    #[test]
    fn acts_name_an_author_and_one_victim() {
        let two = "title = \"t\"\n\
            [[contact]]\nid = \"boat\"\naffiliation = \"hostile\"\ndomain = \"surface\"\nkind = \"small_fast_boat\"\nmotion = \"station\"\nat = [-122.5, 37.8]\n\
            [[contact]]\nid = \"ferry\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"ferry\"\nmotion = \"station\"\nat = [-122.49, 37.8]\n";
        let with = |act: &str| Scenario::from_toml(&format!("{two}[[act]]\n{act}"));
        let s = with("id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3\nlabel = \"fires on the USV\"").expect("parses");
        assert_eq!(s.acts.len(), 1);
        assert_eq!(s.acts[0].against_vehicle, Some(3));
        assert!(with("id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_contact = \"ferry\"").is_ok());
        for bad in [
            "id = \"a1\"\nat_s = 30\nby = \"nobody\"\nagainst_vehicle = 3",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3\nagainst_contact = \"ferry\"",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_contact = \"boat\"",
            "id = \"a1\"\nat_s = -1\nby = \"boat\"\nagainst_vehicle = 3",
            "id = \"\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3",
            "id = \"a1\"\nat_s = 30\nby = \"boat\"\nagainst_vehicle = 3\nweapon = \"gun\"",
        ] {
            assert!(with(bad).is_err(), "{bad}");
        }
        let dup = format!("{two}[[act]]\nid = \"a\"\nat_s = 1\nby = \"boat\"\nagainst_vehicle = 1\n[[act]]\nid = \"a\"\nat_s = 2\nby = \"boat\"\nagainst_vehicle = 1\n");
        assert!(Scenario::from_toml(&dup).is_err());
    }
}
