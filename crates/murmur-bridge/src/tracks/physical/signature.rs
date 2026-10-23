//! What each kind of contact presents to our sensors (WORLD-V3-PLAN 2.1): its critical
//! dimension and height above the surface (EO/IR, hull-down, the two-ray model), its X-band
//! radar cross-section, its thermal contrast, its underwater source level and target strength,
//! what it emits and which AIS class it carries. Values from the target table of
//! `docs/research/sensors.md` ([n] cited there, [E] estimated there); a figure the research does
//! not give is marked here `[E here]` with its basis. A scenario overrides any of them per
//! contact (`sensing.signatures`, keyed by contact id): the truth structs carry no signature
//! field, so the tracker's comparison copies (`tests/before_oho`) and the decision facts are
//! untouched.

use serde::{Deserialize, Serialize};

use crate::tracks::truth::Domain;

/// AIS carriage: class A (SOLAS ships, passenger ships), class B (small craft), or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AisClass {
    A,
    B,
    #[default]
    None,
}

/// A transmitter an ESM receiver can hear: peak power (dBm), antenna gain (dBi), frequency.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Emitter {
    pub power_dbm: f64,
    pub gain_dbi: f64,
    pub freq_ghz: f64,
}

/// A ship's X-band navigation radar, 25 kW peak, 30 dBi [E here: a typical commercial set].
const NAV_RADAR: Emitter = Emitter { power_dbm: 74.0, gain_dbi: 30.0, freq_ghz: 9.4 };
/// A small craft's radar, 4 kW, 25 dBi [E here].
const SMALL_RADAR: Emitter = Emitter { power_dbm: 66.0, gain_dbi: 25.0, freq_ghz: 9.4 };
/// An aircraft's transponder, 250 W at 1090 MHz, omnidirectional [E here].
const TRANSPONDER: Emitter = Emitter { power_dbm: 54.0, gain_dbi: 0.0, freq_ghz: 1.09 };
/// A small drone's datalink, 0.5 W at 2.4 GHz (the research's 0.1 to 1 W [E]).
const DATALINK: Emitter = Emitter { power_dbm: 27.0, gain_dbi: 2.0, freq_ghz: 2.4 };

/// One contact's signature. A zero `dc_m` is nothing above the water for a camera to see, a
/// zero `rcs_m2` nothing for a radar; `None` levels nothing for a sonar to hear or ping.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Signature {
    /// Critical dimension, sqrt(width x height) of the silhouette, m.
    pub dc_m: f64,
    /// Height above the surface (water or ground), m: what the horizon, hull-down and the
    /// two-ray model read.
    pub height_m: f64,
    /// X-band radar cross-section, m^2.
    pub rcs_m2: f64,
    /// Thermal contrast against the background, K.
    pub thermal_k: f64,
    /// Underwater radiated source level, dB re 1 uPa at 1 m.
    pub source_db: Option<f64>,
    /// Active sonar target strength, dB.
    pub target_db: Option<f64>,
    pub emitter: Option<Emitter>,
    pub ais_class: AisClass,
}

/// A scenario's per-contact changes to its kind's signature (any field it names).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dc_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rcs_m2: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal_k: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ais_class: Option<AisClass>,
    /// The emitter's power, dBm (a contact without an emitter in its kind gets a datalink's
    /// gain and frequency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emitter_dbm: Option<f64>,
}

#[allow(clippy::too_many_arguments)]
const fn sig(
    dc_m: f64,
    height_m: f64,
    rcs_m2: f64,
    thermal_k: f64,
    source_db: Option<f64>,
    target_db: Option<f64>,
    emitter: Option<Emitter>,
    ais_class: AisClass,
) -> Signature {
    Signature { dc_m, height_m, rcs_m2, thermal_k, source_db, target_db, emitter, ais_class }
}

/// The kinds the physical model knows, by name: the contacts process's kinds
/// (`murmur-contacts` `KINDS`), the new ones (`diver`, `periscope`, `small_drone`, `person`).
/// Heights of the surface kinds are the legacy model's horizon heights.
pub fn kind_signature(kind: &str, domain: Domain) -> Signature {
    use AisClass::{A, B, None as NoAis};
    match kind {
        // D_c 84 [E], RCS >= 10,000 [19], SL 177-188 [54]; TS [E here: a large hull]
        "cargo_ship" | "tanker" => sig(84.0, 25.0, 10_000.0, 2.0, Some(182.0), Some(40.0), Some(NAV_RADAR), A),
        // D_c 27 [E], RCS 1,000-5,000 [19,53], SL 165-180 [E]
        "ferry" => sig(27.0, 10.0, 2_000.0, 2.0, Some(172.0), Some(35.0), Some(NAV_RADAR), A),
        // [E here: a 25 m patrol craft, between the fishing vessel and the ferry]
        "patrol_boat" => sig(8.0, 6.0, 100.0, 3.0, Some(160.0), Some(25.0), Some(NAV_RADAR), A),
        // D_c 11 [E], RCS 100-500 (Skolnik about 450 for 200 t) [53], SL 150-170 [E]
        "fishing_vessel" => sig(11.0, 5.0, 300.0, 3.0, Some(160.0), Some(25.0), Some(SMALL_RADAR), B),
        // D_c 4-6 [E], RCS 2-10 [E], quiet under sail [E]
        "sailboat" => sig(5.0, 5.0, 5.0, 1.0, Some(120.0), Some(10.0), None, B),
        // D_c 3.5 [E], RCS 2-2.5 [16,19], SL by speed [52] (`source_level`); 2 m high [19]
        "small_fast_boat" => sig(3.5, 2.0, 2.5, 3.0, Some(150.0), Some(10.0), None, NoAis),
        // D_c 3-5 [E], RCS 1 [16]; engine contrast [E here]
        "light_aircraft" => sig(4.0, 2.0, 1.0, 10.0, None, None, Some(TRANSPONDER), NoAis),
        // [E here: a light helicopter, a little larger than a light aircraft]
        "helicopter" => sig(5.0, 3.0, 5.0, 10.0, None, None, Some(TRANSPONDER), NoAis),
        // small drone: D_c 0.35 [E], RCS 0.01-0.03 [55]
        "unknown_uas" | "small_drone" => sig(0.35, 0.3, 0.02, 3.0, None, None, Some(DATALINK), NoAis),
        // on battery, submerged: SL about 100 (units unclear) [57], TS 15-25 [25]
        "submarine" => sig(0.0, 0.0, 0.0, 0.0, Some(100.0), Some(20.0), None, NoAis),
        // a submarine at periscope depth: D_c 0.5 [E], RCS 0.1-1 [E, 56]; snorting SL 140 [57]
        "periscope" => sig(0.5, 1.5, 0.3, 1.0, Some(140.0), Some(20.0), None, NoAis),
        // AUV SL about 124 [58]; TS [E here: a 3 m body]
        "uuv" => sig(0.0, 0.0, 0.0, 0.0, Some(124.0), Some(0.0), None, NoAis),
        // a swimmer under water: TS -20 to -10 [E]; open-circuit SL [E here: bubbles, heard
        // by a passive array at about 120 m, research [51]]
        "diver" => sig(0.0, 0.0, 0.0, 0.0, Some(110.0), Some(-15.0), None, NoAis),
        // NATO 2.3 m, dT 2 K [5]; RCS 100 [16]
        "vehicle" => sig(2.3, 2.0, 100.0, 2.0, None, None, None, NoAis),
        // RCS 200 [16]; D_c [E here: a box truck]
        "truck" => sig(3.0, 3.0, 200.0, 2.0, None, None, None, NoAis),
        // 0.75 m, dT 5 K [5,10]; RCS 1 on land [16]
        "person" => sig(0.75, 1.8, 1.0, 5.0, None, None, None, NoAis),
        // an unknown kind: by its domain [E here]
        _ => match domain {
            Domain::Surface => sig(5.0, 3.0, 10.0, 2.0, Some(140.0), Some(10.0), None, NoAis),
            Domain::Air => sig(3.0, 2.0, 1.0, 5.0, None, None, None, NoAis),
            Domain::Ground => sig(2.3, 2.0, 100.0, 2.0, None, None, None, NoAis),
            Domain::Subsurface => sig(0.0, 0.0, 0.0, 0.0, Some(120.0), Some(10.0), None, NoAis),
        },
    }
}

/// A small fast boat's source level at its speed: 134 dB at 3 m/s to 171 dB at 16 m/s,
/// straight between (research [52]).
fn fast_boat_source_db(speed_mps: f64) -> f64 {
    134.0 + 37.0 * ((speed_mps - 3.0) / 13.0).clamp(0.0, 1.0)
}

/// A contact's signature now: its kind's, its speed's effect where the research gives one,
/// and the scenario's override.
pub fn signature(kind: &str, domain: Domain, speed_mps: f64, over: Option<&SignatureOverride>) -> Signature {
    let mut s = kind_signature(kind, domain);
    if kind == "small_fast_boat" {
        s.source_db = Some(fast_boat_source_db(speed_mps));
    }
    if let Some(o) = over {
        s.dc_m = o.dc_m.unwrap_or(s.dc_m).max(0.0);
        s.height_m = o.height_m.unwrap_or(s.height_m).max(0.0);
        s.rcs_m2 = o.rcs_m2.unwrap_or(s.rcs_m2).max(0.0);
        s.thermal_k = o.thermal_k.unwrap_or(s.thermal_k).max(0.0);
        s.source_db = o.source_db.or(s.source_db);
        s.target_db = o.target_db.or(s.target_db);
        s.ais_class = o.ais_class.unwrap_or(s.ais_class);
        if let Some(p) = o.emitter_dbm {
            let e = s.emitter.unwrap_or(DATALINK);
            s.emitter = Some(Emitter { power_dbm: p, ..e });
        }
    }
    s
}

/// What a detection says (WORLD-V3-PLAN 2.3): the contact's domain and its size class.
pub fn size_class(sig: &Signature, domain: Domain) -> &'static str {
    match domain {
        Domain::Subsurface => "subsurface",
        Domain::Surface if sig.dc_m >= 8.0 || sig.rcs_m2 >= 100.0 => "large_surface",
        Domain::Surface => "small_surface",
        Domain::Air if sig.dc_m >= 2.0 || sig.rcs_m2 >= 0.5 => "large_air",
        Domain::Air => "small_air",
        Domain::Ground if sig.dc_m >= 1.5 => "large_ground",
        Domain::Ground => "small_ground",
    }
}

/// The size class of a label: a kind's (its table signature), a family's, or a size class's
/// own.
pub fn size_class_of(label: &str) -> &'static str {
    match label {
        "large_surface" | "large_vessel" => "large_surface",
        "small_surface" | "small_craft" => "small_surface",
        "large_air" | "aircraft" => "large_air",
        "small_air" | "small_uas" => "small_air",
        "large_ground" | "ground_vehicle" => "large_ground",
        "small_ground" => "small_ground",
        "subsurface" | "submerged" => "subsurface",
        kind => {
            let domain = match crate::tracks::sensing::family(kind) {
                "large_vessel" | "small_craft" => Domain::Surface,
                "aircraft" | "small_uas" => Domain::Air,
                "ground_vehicle" | "person" => Domain::Ground,
                "submerged" => Domain::Subsurface,
                _ => return "unknown",
            };
            // a diver is a person in the water
            let domain = if kind == "diver" { Domain::Subsurface } else { domain };
            size_class(&kind_signature(kind, domain), domain)
        }
    }
}

/// The wrong family a recognition makes (WORLD-V3-PLAN 2.3: likelier than a wrong kind): the
/// nearest family a look at that resolution confuses it with.
pub fn family_confusion(family: &str) -> &'static str {
    match family {
        "large_vessel" => "small_craft",
        "small_craft" => "large_vessel",
        "aircraft" => "small_uas",
        "small_uas" => "aircraft",
        "ground_vehicle" => "person",
        "person" => "ground_vehicle",
        "submerged" => "small_craft",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_signature_its_sensors_can_use() {
        for (k, d) in [
            ("cargo_ship", Domain::Surface),
            ("ferry", Domain::Surface),
            ("small_fast_boat", Domain::Surface),
            ("light_aircraft", Domain::Air),
            ("small_drone", Domain::Air),
            ("periscope", Domain::Subsurface),
            ("vehicle", Domain::Ground),
        ] {
            let s = kind_signature(k, d);
            assert!(s.dc_m > 0.0 && s.rcs_m2 > 0.0, "{k}");
        }
        for k in ["submarine", "uuv", "diver"] {
            let s = kind_signature(k, Domain::Subsurface);
            assert_eq!((s.dc_m, s.rcs_m2), (0.0, 0.0), "{k}: nothing above the water");
            assert!(s.source_db.is_some() && s.target_db.is_some(), "{k}");
        }
    }

    #[test]
    fn a_fast_boat_is_louder_faster_and_an_override_wins() {
        let slow = signature("small_fast_boat", Domain::Surface, 3.0, None);
        let fast = signature("small_fast_boat", Domain::Surface, 16.0, None);
        assert_eq!((slow.source_db, fast.source_db), (Some(134.0), Some(171.0)));
        let o = SignatureOverride { rcs_m2: Some(0.5), ais_class: Some(AisClass::B), ..Default::default() };
        let s = signature("small_fast_boat", Domain::Surface, 8.0, Some(&o));
        assert_eq!((s.rcs_m2, s.ais_class, s.dc_m), (0.5, AisClass::B, 3.5));
    }

    #[test]
    fn size_classes_follow_size_and_domain() {
        assert_eq!(size_class_of("cargo_ship"), "large_surface");
        assert_eq!(size_class_of("small_fast_boat"), "small_surface");
        assert_eq!(size_class_of("light_aircraft"), "large_air");
        assert_eq!(size_class_of("unknown_uas"), "small_air");
        assert_eq!(size_class_of("diver"), "subsurface");
        assert_eq!(size_class_of("small_craft"), "small_surface");
    }
}
