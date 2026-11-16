//! The scenario's settings (docs/SCENARIO-SETTINGS.md, stage S2; rules 33, the default since S7): the policy's
//! `[watch]` and the new keys parsed, bounded and written out; the floor, refused at load and by
//! replay; a loosening only when named; the legacy values; a run of older rules refusing settings
//! other than the standing ones and logging exactly what it did before; a run of rules 33 logging
//! them, with the files the policy was built from, and replaying.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str_with};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy").join(name)
}

/// A directory of policy files for one test.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("murmur-scenario-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A file over the shipped policy, written beside a copy of it.
fn over_shipped(d: &Dir, name: &str, text: &str) -> PathBuf {
    d.file("policy.toml", &std::fs::read_to_string(root().join("sim/policy.toml")).unwrap());
    d.file(name, &format!("extends = \"policy.toml\"\n{text}"))
}

fn refusal(path: &Path) -> String {
    match Policy::load(path) {
        Ok(_) => panic!("{} loaded", path.display()),
        Err(e) => e.to_string(),
    }
}

fn at(p: &Policy, rules: u32) -> Policy {
    let mut p = p.clone();
    p.rules.rules_version = rules;
    p
}

/// The `run.started` line an engine started on `p` under `rules` writes, with `layers`.
fn started(p: &Policy, rules: u32, layers: Vec<Layer>) -> Result<String, EngineError> {
    let log = MemorySink::new();
    Engine::start(
        at(p, rules),
        None,
        roster(),
        StartCarry { policy_layers: layers, ..StartCarry::default() },
        Box::new(ManualClock::new(T0)),
        Box::new(log.clone()),
    )?;
    Ok(log.lines()[0].clone())
}

const NEW_KEYS: [&str; 6] = [
    "\"watch\":",
    "\"loosened\":",
    "\"silent\":",
    "\"min_identity_level\":",
    "\"default_effect_radius_m\":",
    "\"policy_layers\":",
];

// ---- the settings and their values ----------------------------------------------------------------

#[test]
fn the_standing_values_are_sim_policy_tomls_and_every_file_writes_them_out() {
    let p = policy();
    assert_eq!(p.watch.as_ref(), Some(&WatchSettings::default()));
    assert_eq!(p.rules.silent, Some(SilentRules::default()));
    let e = p.rules.engage.as_ref().unwrap();
    assert_eq!((e.min_identity_level, e.default_effect_radius_m), (Some(murmur_decide::identify::Level::Identify), Some(75.0)));
    // today's constants
    let d = SilentRules::default().drift_mps;
    assert_eq!((d.surface, d.subsurface, d.air), (silent::SURFACE_DRIFT_MPS, silent::SUBSURFACE_DRIFT_MPS, silent::AIR_DRIFT_MPS));
    assert_eq!(WatchSettings::default().identify_on_station_s, murmur_decide::identify::ON_STATION_S);
    assert_eq!(WatchSettings::default().stale_s, engage::OUT_OF_VIEW_S);
    // a file that says nothing of them gets the same, written out
    let bare = Policy::load(&root().join("sim/scenarios/engage/policies/orbit-never-queued.toml")).unwrap();
    assert_eq!(bare.watch, p.watch);
    assert_eq!(bare.rules.silent, p.rules.silent);
    // the JSON is the design's schema: what eval/headless/v3/settings.py and agents/roe.py read
    let v = serde_json::to_value(WatchSettings::default()).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "approach_horizon_s",
        "bar",
        "domain",
        "identify_on_station_s",
        "look_m",
        "neutral_after_bar",
        "no_velocity_mps",
        "stale_s",
        "still_mps",
    ];
    let mut got = keys.clone();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want, "no area unless one is set");
    assert_eq!(v["domain"]["surface"], serde_json::json!({"level": "identify", "sensors": ["eo_ir", "ais"], "grace_s": 60.0}));
    assert_eq!(v["domain"]["subsurface"]["level"], "recognise");
    assert_eq!(v["no_velocity_mps"], serde_json::json!({"air": 25.0}));
}

#[test]
fn legacy_is_the_frozen_files_watch_whatever_a_file_says() {
    let text = std::fs::read_to_string(root().join("sim/policy-v0-watch.toml")).unwrap();
    let frozen: toml::Table = toml::from_str(&text).unwrap();
    let want: WatchSettings = frozen["watch"].clone().try_into().unwrap();
    assert_eq!(WatchSettings::legacy(), &want);
    // its JSON is the file's table, key for key
    let as_json = serde_json::to_value(WatchSettings::legacy()).unwrap();
    let file_json = serde_json::to_value(&frozen["watch"]).unwrap();
    for (k, v) in file_json.as_object().unwrap() {
        if let (Some(a), Some(b)) = (v.as_f64(), as_json[k].as_f64()) {
            assert_eq!(a, b, "{k}");
        } else if k != "domain" && k != "no_velocity_mps" {
            assert_eq!(v, &as_json[k], "{k}");
        }
    }
    assert!(!WatchSettings::legacy().neutral_after_bar);
    // what rules below 33 read, however the policy differs
    let mut p = policy();
    p.watch.as_mut().unwrap().bar = 0.8;
    p.sync();
    assert_eq!(at(&p, REACH_NOW_SINCE).rules.watch(), WatchSettings::legacy());
    let mut p33 = at(&p, SCENARIO_SETTINGS_SINCE);
    p33.sync();
    assert_eq!(p33.rules.watch().bar, 0.8);
}

#[test]
fn bounds_are_checked_and_unknown_keys_refused() {
    let d = Dir::new("bounds");
    for (text, why) in [
        ("[watch]\nbar = 0.2\n", "watch.bar must be within"),
        ("[watch]\nstale_s = 0\n", "watch.stale_s must be within"),
        ("[watch]\nhorizon_s = 300\n", "unknown field `horizon_s`"),
        ("[watch.domain.air]\nsensors = [\"eyes\"]\n", "is not a sensor type"),
        ("[watch.domain.air]\nsensors = []\n", "names no sensor"),
        ("[watch.area]\nname = \"x\"\ncenter = [-122.5, 37.8]\nradius_m = 1\n", "watch.area.radius_m must be within"),
        ("[rules.silent]\ndrift_mps = { surface = 40.0 }\n", "rules.silent.drift_mps.surface must be within"),
        ("[rules.silent]\ndrift_mps = { ground = 1.0 }\n", "unknown field `ground`"),
        ("[rules.engage]\ndefault_effect_radius_m = 400\n", "default_effect_radius_m must be within"),
    ] {
        let e = refusal(&over_shipped(&d, "over.toml", text));
        assert!(e.contains(why), "{text}: {e}");
    }
    // a supplement's domain table merges key by key; a whole file's must be whole (a domain it
    // does not name keeps its standing criteria)
    let base = "[[tier]]\nname = \"default\"\ntier = \"human\"\nreason = \"r\"\n";
    let e = Policy::from_toml(&format!("[watch.domain.air]\ngrace_s = 45\n{base}")).unwrap_err().to_string();
    assert!(e.contains("missing field `level`"), "{e}");
    let w = Policy::from_toml(&format!("[watch]\nbar = 0.6\n{base}")).unwrap().watch.unwrap();
    assert_eq!(w, WatchSettings::default());
    let p = Policy::load(&over_shipped(&d, "grace.toml", "[watch.domain.air]\ngrace_s = 45\n")).unwrap();
    assert_eq!(p.watch.unwrap().domain.air, DomainCriteria { grace_s: 45.0, ..WatchSettings::default().domain.air });
    let p = Policy::load(&over_shipped(&d, "air.toml", "[watch.domain.air]\nsensors = [\"eo_ir\", \"radar\"]\ngrace_s = 45\n")).unwrap();
    let w = p.watch.unwrap();
    assert_eq!((w.domain.air.grace_s, w.domain.surface.grace_s), (45.0, 60.0));
}

// ---- the floor -------------------------------------------------------------------------------------

#[test]
fn the_floor_is_refused_at_load_with_no_override() {
    let d = Dir::new("floor");
    for (text, why) in [
        // the engagement bar below the watch's
        ("[watch]\nbar = 0.7\n", "rules.engage.min_identity_confidence = 0.6 is below watch.bar = 0.7"),
        // not even when named as a loosening
        (
            "loosened = { \"rules.engage.min_identity_confidence\" = \"a drill\" }\n[rules.engage]\nmin_identity_confidence = 0.5\n",
            "is below watch.bar = 0.6",
        ),
        // the engagement's level is identify
        ("[rules.engage]\nmin_identity_level = \"recognise\"\n", "min_identity_level = \"recognise\""),
        // no watch domain at detect
        ("[watch.domain.subsurface]\nlevel = \"detect\"\n", "watch.domain.subsurface.level = \"detect\""),
    ] {
        let e = refusal(&over_shipped(&d, "over.toml", text));
        assert!(e.contains(why) && e.contains("over.toml"), "{text}: {e}");
    }
    // the same in a file that extends none, read from text
    let mut text = std::fs::read_to_string(root().join("sim/policy.toml")).unwrap();
    text = text.replace("bar = 0.6                                   #", "bar = 0.65                                  #");
    let e = Policy::from_toml(&text).unwrap_err().to_string();
    assert!(e.contains("is below watch.bar = 0.65") && e.contains("the floor"), "{e}");
    // a bar raised with the engagement's beside it loads
    let ok = over_shipped(&d, "raised.toml", "[watch]\nbar = 0.7\n[rules.engage]\nmin_identity_confidence = 0.7\n");
    assert_eq!(Policy::load(&ok).unwrap().watch.unwrap().bar, 0.7);
}

#[test]
fn replay_checks_the_floor_on_the_logged_copy() {
    let p = Policy::load(&root().join("sim/situations/policies/golden-gate-v3-watch.toml")).unwrap();
    let mut e33 = at(&p, SCENARIO_SETTINGS_SINCE);
    let log = MemorySink::new();
    Engine::new(e33.clone(), roster(), Box::new(ManualClock::new(T0)), Box::new(log.clone())).unwrap();
    let opts = ReplayOptions { rules: RulesAccepted::Exactly(SCENARIO_SETTINGS_SINCE), ..ReplayOptions::default() };
    assert!(replay_str_with(&log.text(), &opts).unwrap().ok());
    // a log whose watch counts a contact identified more surely than its engagements do
    let forged = forge(&log.text(), |ev| {
        if let EventBody::RunStarted { policy, .. } = &mut ev[0].body {
            policy.watch.as_mut().unwrap().bar = 0.8;
        }
    });
    let r = replay_str_with(&forged, &opts).unwrap();
    assert!(r.mismatches.iter().any(|m| m.what.contains("out of bounds") && m.logged.contains("below watch.bar")), "{:?}", r.mismatches);
    // or a domain identified at detect
    e33.watch.as_mut().unwrap().domain.air.level = murmur_decide::identify::Level::Detect;
    let forged = forge(&log.text(), |ev| {
        if let EventBody::RunStarted { policy, .. } = &mut ev[0].body {
            *policy = e33.as_logged();
        }
    });
    let r = replay_str_with(&forged, &opts).unwrap();
    assert!(r.mismatches.iter().any(|m| m.logged.contains("\"detect\"")), "{:?}", r.mismatches);
}

// ---- loosening, only when named -------------------------------------------------------------------

#[test]
fn a_loosening_is_refused_unless_named() {
    let d = Dir::new("loosen");
    // nominating a rule declaration before a confirmation
    let e = refusal(&over_shipped(&d, "nom.toml", "[identification]\nnominate_unconfirmed = true\n"));
    assert!(e.contains("nom.toml") && e.contains("`identification.nominate_unconfirmed` = true is looser than policy.toml (false)"), "{e}");
    let named = over_shipped(
        &d,
        "nom-named.toml",
        "loosened = { \"identification.nominate_unconfirmed\" = \"the drill rehearses the review\" }\n[identification]\nnominate_unconfirmed = true\n",
    );
    let p = Policy::load(&named).unwrap();
    assert_eq!(p.loosened.as_ref().unwrap()["identification.nominate_unconfirmed"], "the drill rehearses the review");
    // a lower engagement bar than the parent's (the parent's raised: the floor is the watch's)
    d.file("high.toml", "extends = \"policy.toml\"\n[rules.engage]\nmin_identity_confidence = 0.8\n");
    let low = d.file("low.toml", "extends = \"high.toml\"\n[rules.engage]\nmin_identity_confidence = 0.7\n");
    let e = refusal(&low);
    assert!(e.contains("low.toml") && e.contains("`rules.engage.min_identity_confidence` = 0.7 is looser than high.toml (0.8)"), "{e}");
    let low = d.file(
        "low-named.toml",
        "extends = \"high.toml\"\nloosened = { \"rules.engage.min_identity_confidence\" = \"a night drill\" }\n[rules.engage]\nmin_identity_confidence = 0.7\n",
    );
    assert_eq!(Policy::load(&low).unwrap().rules.engage.unwrap().min_identity_confidence, 0.7);
    // a raised bar is no loosening, and naming one is refused
    let e = refusal(&d.file(
        "raise-named.toml",
        "extends = \"policy.toml\"\nloosened = { \"rules.engage.min_identity_confidence\" = \"stricter\" }\n[rules.engage]\nmin_identity_confidence = 0.8\n",
    ));
    assert!(e.contains("`loosened` names \"rules.engage.min_identity_confidence\", which this file does not set looser"), "{e}");
    // a reason that names nothing loosened, a key nothing loosens, a reason left empty
    for (text, why) in [
        ("loosened = { \"identification.nominate_unconfirmed\" = \"why\" }\n", "which this file does not set looser"),
        ("loosened = { \"watch.bar\" = \"why\" }\n", "is not a key a policy loosens"),
        ("loosened = { \"identification.nominate_unconfirmed\" = \" \" }\n[identification]\nnominate_unconfirmed = true\n", "the reason"),
        ("loosened = \"everything\"\n", "a table of reasons by key"),
    ] {
        let e = refusal(&over_shipped(&d, "bad.toml", text));
        assert!(e.contains(why), "{text}: {e}");
    }
    // the children of a named loosening inherit it and need not name it again
    let child = d.file("child.toml", "extends = \"nom-named.toml\"\n[approval]\nexpire_s = 90\n");
    assert!(Policy::load(&child).unwrap().loosened.is_some());
}

#[test]
fn a_file_that_extends_none_is_compared_with_the_codes_defaults() {
    let text = std::fs::read_to_string(root().join("sim/policy.toml")).unwrap();
    let loose = text.replace("nominate_unconfirmed = false", "nominate_unconfirmed = true");
    let e = Policy::from_toml(&loose).unwrap_err().to_string();
    assert!(e.contains("= true is looser than the code's default (false)"), "{e}");
    let named = format!("loosened = {{ \"identification.nominate_unconfirmed\" = \"a test\" }}\n{loose}");
    assert!(Policy::from_toml(&named).is_ok());
    let low = text.replace(
        "min_identity_confidence = 0.6               #",
        "min_identity_confidence = 0.55              #",
    );
    let e = Policy::from_toml(&low).unwrap_err().to_string();
    assert!(e.contains("`rules.engage.min_identity_confidence` = 0.55 is looser than the code's default (0.6)"), "{e}");
    // the shipped files that loosen name it
    for f in ["sim/scenarios/engage/policies/nominate-unconfirmed.toml", "eval/policy/minimal.toml"] {
        let p = Policy::load(&root().join(f)).unwrap();
        assert!(p.loosened.as_ref().unwrap().contains_key("identification.nominate_unconfirmed"), "{f}");
    }
    assert!(Policy::load(&fixture("eval-minimal.toml")).unwrap().loosened.is_some());
}

// ---- rules below 33 --------------------------------------------------------------------------------

#[test]
fn under_older_rules_settings_other_than_the_standing_ones_are_refused_the_area_aside() {
    let d = Dir::new("older");
    for (text, keys) in [
        ("[watch]\nbar = 0.7\n[rules.engage]\nmin_identity_confidence = 0.7\n", "watch.bar"),
        ("[watch]\nneutral_after_bar = false\nstale_s = 12\n", "watch.neutral_after_bar, watch.stale_s"),
        ("[watch.domain.air]\ngrace_s = 45\n", "watch.domain.air"),
        ("[rules.silent]\ndrift_mps = { surface = 1.0 }\n", "rules.silent.drift_mps"),
        ("[rules.engage]\ndefault_effect_radius_m = 50\n", "rules.engage.default_effect_radius_m"),
    ] {
        let p = Policy::load(&over_shipped(&d, "over.toml", text)).unwrap();
        let e = started(&p, REACH_NOW_SINCE, Vec::new()).unwrap_err().to_string();
        assert!(e.starts_with(&format!("policy: {keys} differ")) && e.contains("rules 33"), "{text}: {e}");
        // rules 33 read them
        started(&p, SCENARIO_SETTINGS_SINCE, Vec::new()).unwrap();
    }
    // the area is the scenario's either way
    for f in ["golden-gate-v3-watch.toml", "san-diego-watch.toml", "golden-gate-scale-watch.toml"] {
        let p = Policy::load(&root().join("sim/situations/policies").join(f)).unwrap();
        assert!(p.watch.as_ref().unwrap().area.is_some(), "{f}");
        started(&p, REACH_NOW_SINCE, Vec::new()).unwrap();
    }
    // the v1 and v2 worlds' supplement says what rules 32 meant (the neutral call from 0.3), which
    // rules 32 refuse as any setting other than the standing ones
    let p = Policy::load(&root().join("sim/situations/policies/legacy-v1v2.toml")).unwrap();
    assert!(!p.watch.as_ref().unwrap().neutral_after_bar);
    let e = started(&p, REACH_NOW_SINCE, Vec::new()).unwrap_err().to_string();
    assert!(e.starts_with("policy: watch.neutral_after_bar differs"), "{e}");
    started(&p, SCENARIO_SETTINGS_SINCE, Vec::new()).unwrap();
}

#[test]
fn under_older_rules_the_log_is_what_it_was() {
    // the whole copy of the watch policy from before S2 (no [watch], no new key) and a v3 watch's
    // policy now (the watch policy, every new key and an area): the same run.started, byte for byte
    let (p, layers) = Policy::load_layers(&root().join("sim/situations/policies/golden-gate-v3-watch.toml")).unwrap();
    let old = Policy::load(&fixture("situations-watch.toml")).unwrap();
    for rules in [REACH_NOW_SINCE, IDENTIFY_CLASS_SINCE, 8] {
        let now = started(&p, rules, layers.clone()).unwrap();
        let before = started(&old, rules, Vec::new()).unwrap();
        assert_eq!(now, before, "rules {rules}");
        for k in NEW_KEYS {
            assert!(!now.contains(k), "rules {rules}: {k}");
        }
        // the schema they logged, whatever this build's is
        assert!(now.contains(&format!("\"policy_schema\":{PASSKEY_SCHEMA_SINCE},")), "{now}");
    }
    // and a policy that loosens: nothing of it
    let p = Policy::load(&root().join("eval/policy/minimal.toml")).unwrap();
    let now = started(&p, REACH_NOW_SINCE, Vec::new()).unwrap();
    let before = started(&Policy::load(&fixture("eval-minimal.toml")).unwrap(), REACH_NOW_SINCE, Vec::new()).unwrap();
    assert_eq!(now, before);
    assert!(!now.contains("\"loosened\""));
}

// ---- rules 33 --------------------------------------------------------------------------------------

#[test]
fn under_rules_33_the_settings_and_the_layers_are_logged_and_replay() {
    let path = root().join("sim/situations/policies/golden-gate-v3-watch.toml");
    let (p, layers) = Policy::load_layers(&path).unwrap();
    assert_eq!(layers.iter().map(|l| l.path.as_str()).collect::<Vec<_>>()[1..], ["watch.toml", "../../policy.toml"]);
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::start(
        at(&p, SCENARIO_SETTINGS_SINCE),
        Some(areas()),
        roster(),
        StartCarry { policy_layers: layers.clone(), ..StartCarry::default() },
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    let first: serde_json::Value = serde_json::from_str(&log.lines()[0]).unwrap();
    assert_eq!(first["rules_version"], SCENARIO_SETTINGS_SINCE);
    assert_eq!(first["policy_schema"], SCENARIO_SETTINGS_SCHEMA_SINCE);
    let watch = &first["policy"]["watch"];
    assert_eq!(watch["bar"], 0.6);
    assert_eq!(watch["neutral_after_bar"], true);
    assert_eq!(watch["area"], serde_json::json!({"name": "the Golden Gate Bridge", "center": [-122.4783, 37.8199], "radius_m": 3000.0}));
    assert_eq!(first["policy"]["rules"]["silent"]["drift_mps"]["air"], 10.0);
    assert_eq!(first["policy"]["rules"]["engage"]["min_identity_level"], "identify");
    assert_eq!(first["policy"]["rules"]["engage"]["default_effect_radius_m"], 75.0);
    // each layer as written, never absolute, with its sha256
    let logged: Vec<Layer> = serde_json::from_value(first["policy_layers"].clone()).unwrap();
    assert_eq!(logged.len(), 3);
    assert!(logged[0].path.ends_with("sim/situations/policies/golden-gate-v3-watch.toml"), "{:?}", logged[0]);
    assert!(logged.iter().all(|l| !Path::new(&l.path).is_absolute()));
    assert_eq!(
        logged[2].sha256,
        murmur_decide::hash::sha256_hex(&std::fs::read(root().join("sim/policy.toml")).unwrap())
    );
    // a decision, then replay under rules 33
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.submit(s.authority(), hold(&["boat-1"]), &world(), &terrain()).unwrap();
    let opts = ReplayOptions { rules: RulesAccepted::Exactly(SCENARIO_SETTINGS_SINCE), ..ReplayOptions::default() };
    let r = replay_str_with(&log.text(), &opts).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    // a loosening is logged with its reason
    let p = Policy::load(&root().join("eval/policy/minimal.toml")).unwrap();
    let line = started(&p, SCENARIO_SETTINGS_SINCE, Vec::new()).unwrap();
    assert!(line.contains("\"loosened\":{\"identification.nominate_unconfirmed\":\"the minimal-approval profile"), "{line}");
    // the claims fit: a rules 33 log claiming the old schema, or a rules 32 log carrying a watch
    let older = forge(&log.text(), |ev| {
        if let EventBody::RunStarted { policy_schema, .. } = &mut ev[0].body {
            *policy_schema = Some(PASSKEY_SCHEMA_SINCE);
        }
    });
    let w: Vec<String> = replay_str_with(&older, &opts).unwrap().mismatches.iter().map(|m| m.logged.clone()).collect();
    assert!(w.iter().any(|m| m.starts_with("watch (schema 8)")), "{w:?}");
    assert!(w.iter().any(|m| m.starts_with("rules.engage.min_identity_level (schema 8)")), "{w:?}");
    let downgraded = forge(&log.text(), |ev| {
        if let EventBody::RunStarted { rules_version, .. } = &mut ev[0].body {
            *rules_version = Some(REACH_NOW_SINCE);
        }
    });
    let opts32 = ReplayOptions { rules: RulesAccepted::Exactly(REACH_NOW_SINCE), ..ReplayOptions::default() };
    let w: Vec<String> = replay_str_with(&downgraded, &opts32).unwrap().mismatches.iter().map(|m| m.what.clone()).collect();
    assert!(w.iter().any(|m| m.starts_with("the scenario's settings for the watch") && m.contains("rules 33 or later")), "{w:?}");
    assert!(w.iter().any(|m| m.starts_with("the files the policy was built from")), "{w:?}");
}

// ---- what agents and Python read --------------------------------------------------------------------

#[test]
fn the_effective_watch_is_the_legacy_one_with_the_area_under_older_rules() {
    let p = Policy::load(&root().join("sim/situations/policies/san-diego-watch.toml")).unwrap();
    let area = p.watch.as_ref().unwrap().area.clone().unwrap();
    assert_eq!((area.name.as_str(), area.center, area.radius_m), ("the bay entrance off Ballast Point", [-117.229, 32.686], 3000.0));
    let old = at(&p, REACH_NOW_SINCE).effective_watch();
    assert_eq!(WatchSettings { area: None, ..old.clone() }, *WatchSettings::legacy());
    assert_eq!(old.area, Some(area.clone()));
    let new = at(&p, SCENARIO_SETTINGS_SINCE).effective_watch();
    assert_eq!(new, *p.watch.as_ref().unwrap());
    assert!(new.neutral_after_bar);
}

fn show(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_murmur-policy")).args(args).current_dir(root()).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
}

#[test]
fn show_prints_the_effective_watch_for_the_rules_asked() {
    let file = "sim/situations/policies/golden-gate-scale-watch.toml";
    // the default rules (33): the merged [watch], logged in the policy too
    let (code, out, _) = show(&["show", file]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["watch"]["neutral_after_bar"], true);
    assert_eq!(v["watch"]["area"]["name"], "the Golden Gate Bridge");
    assert_eq!(v["watch"], v["policy"]["watch"]);
    // rules 32: the legacy watch with the scenario's circle, the policy as they logged it
    let (code, out, _) = show(&["show", file, "--rules", &REACH_NOW_SINCE.to_string()]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["watch"]["neutral_after_bar"], false);
    assert_eq!(v["watch"]["area"]["name"], "the Golden Gate Bridge");
    assert!(v["policy"]["watch"].is_null());
    // sim/policy.toml has no area
    let (_, out, _) = show(&["show", "sim/policy.toml"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["watch"].get("area").is_none(), "{}", v["watch"]);
    // a file the bridge would refuse under rules 32, refused there, read under the default
    let d = Dir::new("show");
    let f = over_shipped(&d, "over.toml", "[watch]\nstale_s = 12\n");
    let (code, _, err) = show(&["show", f.to_str().unwrap(), "--rules", &REACH_NOW_SINCE.to_string()]);
    assert_eq!(code, 2);
    assert!(err.contains("watch.stale_s"), "{err}");
    let (code, _, _) = show(&["show", f.to_str().unwrap()]);
    assert_eq!(code, 0);
}
