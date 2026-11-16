//! A policy file that extends another (docs/SCENARIO-SETTINGS.md section 3, stage S1): the
//! merge, each refusal (naming the file and the key), the tier insertion, the files read, and
//! the supplements that replaced full copies of the shipped policy, each held to the copy it
//! replaced (kept in `tests/fixtures/policy/`): the same policy, logged the same.
#![allow(clippy::unwrap_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
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
        let d = std::env::temp_dir().join(format!("murmur-policy-layers-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(&p, text).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const BASE: &str = r#"
[approval]
expire_s = 120
step_up_acts = ["declare_hostile"]

[rules]
reserve_pct = 20

[rules.engage]
keep_out_m = 200
track_accel_mps2 = { air = 2.0, ground = 2.0, surface = 0.2, subsurface = 0.2 }

[[tier]]
name = "safety"
when = { kind = ["hold", "rtl"] }
tier = "auto"
reason = "safety commands"

[[tier]]
name = "default"
tier = "auto"
reason = "routine"
"#;

fn refusal(path: &Path) -> String {
    match Policy::load(path) {
        Ok(_) => panic!("{} loaded", path.display()),
        Err(e) => e.to_string(),
    }
}

fn names(p: &Policy) -> Vec<&str> {
    p.tiers.iter().map(|t| t.name.as_str()).collect()
}

/// The JSON `run.started` logs for a policy under the current rules.
fn logged(p: Policy) -> String {
    serde_json::to_string(&p.as_logged()).unwrap()
}

// ---- the merge ----------------------------------------------------------------------------------

#[test]
fn a_supplement_merges_tables_and_replaces_values_and_arrays() {
    let d = Dir::new("merge");
    d.file("base.toml", BASE);
    let f = d.file(
        "over.toml",
        r#"
extends = "base.toml"

[approval]
step_up_acts = ["accept_unknown"]

[rules]
reserve_pct = 25

[rules.engage.track_accel_mps2]
air = 3.0
"#,
    );
    let p = Policy::load(&f).unwrap();
    let base = Policy::load(&d.0.join("base.toml")).unwrap();
    // an array replaces the one inherited, whole
    assert_eq!(p.approval.step_up_acts, Some(vec![StepUpAct::AcceptUnknown]));
    // a value replaces the one inherited
    assert_eq!(p.rules.reserve_pct, 25.0);
    // a table merges key by key: air changed, the other domains and keep_out_m inherited
    let e = p.rules.engage.as_ref().unwrap();
    let accel = e.track_accel_mps2.as_ref().unwrap();
    assert_eq!(accel[&Domain::Air], 3.0);
    assert_eq!(accel[&Domain::Ground], 2.0);
    assert_eq!(accel[&Domain::Surface], 0.2);
    assert_eq!(e.keep_out_m, 200.0);
    // everything else as inherited
    let mut want = base.clone();
    want.approval.step_up_acts = p.approval.step_up_acts.clone();
    want.rules.reserve_pct = 25.0;
    want.rules.engage = p.rules.engage.clone();
    assert_eq!(p, want);
    assert_ne!(base.rules.engage, p.rules.engage);
}

#[test]
fn a_supplement_places_each_tier_before_the_inherited_one_it_names() {
    let d = Dir::new("tiers");
    d.file("base.toml", BASE);
    let f = d.file(
        "over.toml",
        r#"
extends = "base.toml"

[[tier]]
name = "a"
before = "default"
when = { source = "agent" }
tier = "human"
reason = "first added"

[[tier]]
name = "b"
before = "default"
tier = "human"
reason = "second added"

[[tier]]
name = "c"
before = "safety"
when = { armed = true }
tier = "human"
reason = "ahead of everything"
"#,
    );
    let p = Policy::load(&f).unwrap();
    assert_eq!(names(&p), ["c", "safety", "a", "b", "default"]);
    let a = &p.tiers[2];
    assert_eq!((a.tier, a.when.source, a.reason.as_str()), (Tier::Human, Some(policy::SourceKind::Agent), "first added"));
    // `extends` and `before` are no part of the policy: not in what is logged
    let json = logged(p);
    assert!(!json.contains("extends") && !json.contains("before"), "{json}");
}

#[test]
fn a_chain_merges_in_turn_and_lists_the_files_it_read() {
    let d = Dir::new("chain");
    let base = d.file("std/base.toml", BASE);
    d.file("std/mid.toml", "extends = \"base.toml\"\n[rules]\nreserve_pct = 30\n");
    let top = d.file("scenario/top.toml", "extends = \"../std/mid.toml\"\n[approval]\nexpire_s = 90\n");
    let (p, layers) = Policy::load_layers(&top).unwrap();
    assert_eq!((p.rules.reserve_pct, p.approval.expire_s), (30.0, 90));
    // the file given first, as given, then each file it extends, as its `extends` writes it
    let paths: Vec<&str> = layers.iter().map(|l| l.path.as_str()).collect();
    assert_eq!(paths, [top.display().to_string().as_str(), "../std/mid.toml", "base.toml"]);
    let sha = |p: &Path| murmur_decide::hash::sha256_hex(&std::fs::read(p).unwrap());
    assert_eq!(layers[0].sha256, sha(&top));
    assert_eq!(layers[1].sha256, sha(&d.0.join("std/mid.toml")));
    assert_eq!(layers[2].sha256, sha(&base));
    // a supplement may set what the file between set again (it changes it)
    let again = d.file("scenario/again.toml", "extends = \"../std/mid.toml\"\n[rules]\nreserve_pct = 20\n");
    assert_eq!(Policy::load(&again).unwrap().rules.reserve_pct, 20.0);
}

#[test]
fn a_file_that_extends_none_is_read_as_it_always_was() {
    for f in [root().join("sim/policy.toml"), fixture("situations-watch.toml")] {
        let text = std::fs::read_to_string(&f).unwrap();
        let (p, layers) = Policy::load_layers(&f).unwrap();
        assert_eq!(p, Policy::from_toml(&text).unwrap());
        assert_eq!(
            layers,
            [Layer { path: f.display().to_string(), sha256: murmur_decide::hash::sha256_hex(text.as_bytes()) }]
        );
    }
}

// ---- the refusals --------------------------------------------------------------------------------

#[test]
fn a_key_set_to_what_it_inherits_is_refused_naming_it() {
    let d = Dir::new("copy");
    d.file("base.toml", BASE);
    for (body, key) in [
        ("[approval]\nexpire_s = 120\n", "`approval.expire_s`"),
        // the same number written as a float is the same value
        ("[rules]\nreserve_pct = 20.0\n", "`rules.reserve_pct`"),
        ("[approval]\nstep_up_acts = [\"declare_hostile\"]\n", "`approval.step_up_acts`"),
        ("[rules.engage]\ntrack_accel_mps2 = { air = 2.0 }\n", "`rules.engage.track_accel_mps2.air`"),
    ] {
        let f = d.file("over.toml", &format!("extends = \"base.toml\"\n{body}"));
        let e = refusal(&f);
        assert!(e.contains("over.toml") && e.contains(key) && e.contains("sets only what it changes"), "{e}");
    }
    // one key changed beside one copied: still refused
    let f = d.file("over.toml", "extends = \"base.toml\"\n[approval]\nexpire_s = 60\nstep_up_acts = [\"declare_hostile\"]\n");
    assert!(refusal(&f).contains("`approval.step_up_acts`"));
}

#[test]
fn a_tier_out_of_place_is_refused() {
    let d = Dir::new("bad-tiers");
    d.file("base.toml", BASE);
    let tier = |name: &str, before: Option<&str>| {
        let before = before.map(|b| format!("before = \"{b}\"\n")).unwrap_or_default();
        format!("[[tier]]\nname = \"{name}\"\n{before}tier = \"human\"\nreason = \"r\"\n")
    };
    for (body, why) in [
        (tier("a", Some("nowhere")), "tier a: `before` = \"nowhere\" names no tier of base.toml"),
        (tier("a", None), "tier a: a supplement's tier says where it goes"),
        (tier("default", Some("safety")), "tier default: there is a tier of that name already"),
        (format!("{}{}", tier("a", Some("default")), tier("a", Some("safety"))), "tier a: there is a tier of that name already"),
        // only an inherited tier: one this file adds is no place
        (format!("{}{}", tier("a", Some("default")), tier("b", Some("a"))), "tier b: `before` = \"a\" names no tier"),
    ] {
        let f = d.file("over.toml", &format!("extends = \"base.toml\"\n{body}"));
        let e = refusal(&f);
        assert!(e.starts_with(&format!("policy {}: ", f.display())) && e.contains(why), "{e}");
    }
    // `before` in a file that extends none
    let f = d.file("alone.toml", &format!("{BASE}{}", tier("a", Some("default"))));
    let e = refusal(&f);
    assert!(e.contains("alone.toml") && e.contains("tier a: `before`") && e.contains("extends none"), "{e}");
    // nor in a file a supplement extends
    d.file("mid.toml", &format!("{BASE}{}", tier("a", Some("default"))));
    let f = d.file("over.toml", "extends = \"mid.toml\"\n[rules]\nreserve_pct = 30\n");
    let e = refusal(&f);
    assert!(e.starts_with("policy mid.toml: tier a: `before`"), "{e}");
}

#[test]
fn a_cycle_is_refused() {
    let d = Dir::new("cycle");
    let a = d.file("a.toml", "extends = \"b.toml\"\n[rules]\nreserve_pct = 30\n");
    d.file("b.toml", "extends = \"a.toml\"\n[rules]\nreserve_pct = 40\n");
    let e = refusal(&a);
    assert!(e.starts_with("policy b.toml: `extends` = \"a.toml\": a cycle"), "{e}");
    let me = d.file("me.toml", "extends = \"me.toml\"\n");
    let e = refusal(&me);
    assert!(e.contains("me.toml") && e.contains("`extends` = \"me.toml\": a cycle"), "{e}");
}

#[test]
fn a_chain_of_more_than_four_files_is_refused() {
    let d = Dir::new("deep");
    d.file("l1.toml", BASE);
    for (i, pct) in [(2, 21), (3, 22), (4, 23), (5, 24)] {
        d.file(&format!("l{i}.toml"), &format!("extends = \"l{}.toml\"\n[rules]\nreserve_pct = {pct}\n", i - 1));
    }
    let (p, layers) = Policy::load_layers(&d.0.join("l4.toml")).unwrap();
    assert_eq!((p.rules.reserve_pct, layers.len()), (23.0, policy::MAX_LAYERS));
    let e = refusal(&d.0.join("l5.toml"));
    assert!(e.contains("l2.toml") && e.contains("`extends` = \"l1.toml\"") && e.contains("4 files at most"), "{e}");
}

#[test]
fn other_mistakes_name_the_file_and_the_key() {
    let d = Dir::new("other");
    d.file("base.toml", BASE);
    let missing = d.file("missing.toml", "extends = \"nope.toml\"\n");
    let e = refusal(&missing);
    assert!(e.contains("missing.toml") && e.contains("`extends` = \"nope.toml\""), "{e}");
    let number = d.file("number.toml", "extends = 3\n");
    assert!(refusal(&number).contains("`extends` names the file"));
    // an unknown key in a supplement fails to load, as in any policy file
    let typo = d.file("typo.toml", "extends = \"base.toml\"\n[approval]\nexpire = 60\n");
    let e = refusal(&typo);
    assert!(e.contains("typo.toml") && e.contains("unknown field `expire`") && e.contains("approval"), "{e}");
    // and a merged policy is held to every bound a file is
    let zero = d.file("zero.toml", "extends = \"base.toml\"\n[approval]\nexpire_s = 0\n");
    let e = refusal(&zero);
    assert!(e.contains("zero.toml") && e.contains("approval.expire_s must be > 0"), "{e}");
    // `extends` read from text alone has no file to be relative to
    let e = Policy::from_toml(&format!("extends = \"base.toml\"\n{BASE}")).unwrap_err().to_string();
    assert!(e.contains("unknown field `extends`"), "{e}");
}

// ---- the shipped supplements ---------------------------------------------------------------------

/// Each supplement that replaced a full copy of the shipped policy (S1) is that copy: the same
/// policy, logged in the same bytes. The copies are frozen in `tests/fixtures/policy/` as they
/// were: a change to the files they extend that moves one fails here, and updates the fixture
/// with it only on purpose.
#[test]
fn each_supplement_is_the_full_copy_it_replaced() {
    for (file, copy, extends) in [
        ("sim/situations/policies/watch.toml", "situations-watch.toml", "../../policy.toml"),
        ("eval/policy/minimal.toml", "eval-minimal.toml", "../../sim/policy.toml"),
        ("eval/policy/strict.toml", "eval-strict.toml", "base.toml"),
    ] {
        let (p, layers) = Policy::load_layers(&root().join(file)).unwrap();
        let old = Policy::load(&fixture(copy)).unwrap();
        assert_eq!(p, old, "{file}");
        assert_eq!(logged(p), logged(old), "{file}");
        assert_eq!(layers.len(), 2, "{file}");
        assert_eq!(layers[1].path, extends, "{file}");
    }
}

/// The watch policy: the shipped one with `agent-tasking` before the default.
#[test]
fn the_watch_policy_is_the_shipped_one_and_one_tier() {
    let shipped = policy();
    let mut watch = Policy::load(&root().join("sim/situations/policies/watch.toml")).unwrap();
    let at = watch.tiers.iter().position(|t| t.name == "agent-tasking").unwrap();
    assert_eq!(watch.tiers[at + 1].name, "default");
    let added = watch.tiers.remove(at);
    assert_eq!(added.when.kind, Some(vec![TaskKind::Orbit, TaskKind::Patrol, TaskKind::Track, TaskKind::Identify]));
    assert_eq!(watch, shipped);
}

/// What the engine logs in `run.started` is what `as_logged` gives, under the current rules and
/// under rules from before `[identification]` and the step-up factors.
#[test]
fn the_engine_logs_the_policy_as_logged() {
    let file = root().join("sim/situations/policies/watch.toml");
    for rules in [rules::RULES_VERSION, 8] {
        let mut p = Policy::load(&file).unwrap();
        p.rules.rules_version = rules;
        let log = MemorySink::new();
        let clock = ManualClock::new(T0);
        Engine::new(p.clone(), roster(), Box::new(clock), Box::new(log.clone())).unwrap();
        let line: serde_json::Value = serde_json::from_str(&log.lines()[0]).unwrap();
        assert_eq!(line["type"], "run.started");
        assert_eq!(line["policy"], serde_json::to_value(p.as_logged()).unwrap(), "rules {rules}");
        assert_eq!(line["policy"]["identification"].is_null(), rules < rules::RULE_ID_SINCE);
    }
}

// ---- murmur-policy show --------------------------------------------------------------------------

fn show(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_murmur-policy")).args(args).current_dir(root()).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
}

#[test]
fn show_prints_the_policy_as_logged_and_its_layers() {
    let (code, out, _) = show(&["show", "sim/situations/policies/watch.toml"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let p = Policy::load(&root().join("sim/situations/policies/watch.toml")).unwrap();
    // the policy in the very bytes the engine logs it
    assert!(out.starts_with(&format!("{{\"policy\":{},\"layers\":", logged(p))), "{out}");
    let paths: Vec<&str> = v["layers"].as_array().unwrap().iter().map(|l| l["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["sim/situations/policies/watch.toml", "../../policy.toml"]);
    assert_eq!(
        v["layers"][1]["sha256"],
        murmur_decide::hash::sha256_hex(&std::fs::read(root().join("sim/policy.toml")).unwrap())
    );
    assert!(v["watch"].is_null());
    // older rules: no [identification], no step-up factor (no build of theirs logged one)
    let (code, out, _) = show(&["show", "sim/policy.toml", "--rules", "8"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["policy"]["identification"].is_null() && v["policy"]["approval"]["step_up_factor"].is_null());
}

#[test]
fn show_exits_2_with_the_reason_on_a_refusal() {
    let d = Dir::new("show");
    d.file("base.toml", BASE);
    let f = d.file("over.toml", "extends = \"base.toml\"\n[approval]\nexpire_s = 120\n");
    let (code, out, err) = show(&["show", f.to_str().unwrap()]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("over.toml") && err.contains("`approval.expire_s`"), "{err}");
    for args in [&["show"][..], &["list", "sim/policy.toml"], &["show", "sim/policy.toml", "--rules", "0"]] {
        assert_eq!(show(args).0, 2, "{args:?}");
    }
}
