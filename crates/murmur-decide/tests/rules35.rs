//! Rules 35 (`SALVO_SINCE`, docs/SALVO.md): the salvo's policy keys. The engine's own behaviour is
//! held by scenarios 174 to 180 (`sim/scenarios/engage/`, run by `tests/engage.rs`); here the
//! policy: a ceiling above one round a plan and a spacing under 5 s are loosenings a file must
//! name, and older rules read neither.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A policy file extending the shipped one, with `body` after the `extends` line.
fn write(name: &str, body: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("murmur-rules35-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.toml");
    std::fs::write(&p, format!("extends = {:?}\n{body}", root().join("sim/policy.toml"))).unwrap();
    p
}

#[test]
fn a_ceiling_above_one_round_is_a_loosening_the_file_must_name() {
    let unnamed = write("unnamed", "[rules.engage]\nmax_rounds_per_plan = 6\n");
    let err = Policy::load(&unnamed).unwrap_err().to_string();
    assert!(err.contains("max_rounds_per_plan"), "{err}");
    let named = write(
        "named",
        "loosened = { \"rules.engage.max_rounds_per_plan\" = \"a drill\" }\n[rules.engage]\nmax_rounds_per_plan = 6\n",
    );
    let p = Policy::load(&named).unwrap();
    assert_eq!(p.rules.engage_rules().max_rounds(), 6);
    assert!(p.unnamed_loosenings().is_empty());
}

#[test]
fn a_spacing_under_the_standing_five_seconds_is_one_too() {
    let unnamed = write("sp-unnamed", "[rules.engage]\nsalvo_spacing_s = 2\n");
    let err = Policy::load(&unnamed).unwrap_err().to_string();
    assert!(err.contains("salvo_spacing_s"), "{err}");
    let named = write("sp-named", "loosened = { \"rules.engage.salvo_spacing_s\" = \"a drill\" }\n[rules.engage]\nsalvo_spacing_s = 2\n");
    assert_eq!(Policy::load(&named).unwrap().rules.engage_rules().salvo_spacing(), 2.0);
    // a longer spacing is no loosening
    let longer = write("sp-longer", "[rules.engage]\nsalvo_spacing_s = 30\n");
    assert_eq!(Policy::load(&longer).unwrap().rules.engage_rules().salvo_spacing(), 30.0);
}

#[test]
fn older_rules_read_neither_key_and_say_so() {
    let named = write(
        "old",
        "loosened = { \"rules.engage.max_rounds_per_plan\" = \"a drill\" }\n[rules.engage]\nmax_rounds_per_plan = 6\n",
    );
    let mut p = Policy::load(&named).unwrap();
    p.rules.rules_version = SCENARIO_SETTINGS_SINCE;
    p.sync();
    let err = p.fits_rules().unwrap_err().to_string();
    assert!(err.contains("max_rounds_per_plan") && err.contains("rules 35"), "{err}");
    // the standing policy fits any rules, and logs neither key before rules 35
    let mut std = Policy::load(&root().join("sim/policy.toml")).unwrap();
    std.rules.rules_version = SCENARIO_SETTINGS_SINCE;
    std.sync();
    std.fits_rules().unwrap();
    let logged = std.as_logged();
    assert!(logged.rules.engage_rules().max_rounds_per_plan.is_none() && logged.rules.engage_rules().salvo_spacing_s.is_none());
}
