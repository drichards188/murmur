//! /api/platforms: the capability catalog. Every platform type in sim/platforms.toml with all
//! of its fields, what it can do (from the same table that drives tasking), and which fleet
//! vehicles are that type.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::tasks::{caps, platform_kind};
use crate::vehicle::Uid;
use crate::world::World;

/// Always present (null when the catalog leaves them out), in this order; the console reads
/// them. Any other field passes through as is.
const KEYS: [&str; 13] = [
    "name",
    "short",
    "role",
    "domain",
    "sim",
    "link",
    "sidc",
    "callsign",
    "cruise_mps",
    "max_mps",
    "endurance_h",
    "climb_mps",
    "turn_dps",
];

pub fn catalog(world: &World) -> Result<Value, String> {
    world.load_manifest();
    let text = std::fs::read_to_string(&world.cfg.platforms)
        .map_err(|e| format!("{}: {e}", world.cfg.platforms.display()))?;
    let table: toml::Table = text.parse().map_err(|e| format!("platforms.toml: {e}"))?;

    let mut members: BTreeMap<String, Vec<(Uid, Value)>> = BTreeMap::new();
    {
        let m = world.manifest();
        for (uid, ident) in &m.identities {
            let platform = ident.str("platform").unwrap_or_default().to_string();
            let entry =
                json!({"uid": uid, "sysid": m.sysid_of(*uid), "callsign": ident.0.get("callsign")});
            members.entry(platform).or_default().push((*uid, entry));
        }
    }

    let mut out = Vec::new();
    for (pid, spec) in &table {
        let Some(spec) = spec.as_table() else {
            continue;
        };
        let get = |k: &str| {
            spec.get(k)
                .and_then(|v| serde_json::to_value(v).ok())
                .unwrap_or(Value::Null)
        };
        let num = |k: &str| {
            spec.get(k)
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        };
        let mut p = Map::new();
        p.insert("id".into(), Value::String(pid.clone()));
        for k in KEYS {
            p.insert(k.into(), get(k));
        }
        for k in spec.keys() {
            if !p.contains_key(k) {
                p.insert(k.clone(), get(k));
            }
        }
        p.insert(
            "alt_m".into(),
            if spec.contains_key("alt_m") {
                get("alt_m")
            } else {
                get("typical_alt_m")
            },
        );
        let min_r = match (num("cruise_mps"), num("turn_dps")) {
            (Some(c), Some(t)) if c != 0.0 && t != 0.0 => {
                json!(crate::clock::round_int(
                    c / (t * std::f64::consts::PI / 180.0)
                ))
            }
            _ => Value::Null,
        };
        p.insert("min_turn_radius_m".into(), min_r);
        let str_of = |k| spec.get(k).and_then(toml::Value::as_str);
        let kind = platform_kind(
            str_of("sim").unwrap_or_default(),
            str_of("vehicle"),
            str_of("behavior"),
        );
        let mut tasks: Vec<&str> = kind
            .and_then(caps)
            .map(|c| c.tasks.iter().map(|t| t.name()).collect())
            .unwrap_or_default();
        tasks.sort_unstable();
        p.insert("tasks".into(), json!(tasks));
        let mut fleet = members.remove(pid).unwrap_or_default();
        fleet.sort_by_key(|(uid, _)| *uid);
        p.insert(
            "in_fleet".into(),
            Value::Array(fleet.into_iter().map(|(_, e)| e).collect()),
        );
        out.push(Value::Object(p));
    }
    Ok(json!({ "platforms": out }))
}
