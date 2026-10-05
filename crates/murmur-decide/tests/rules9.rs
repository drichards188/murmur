//! Decision rules 9 (`RULE_ID_SINCE`): rule-based identification with human review. The
//! engine declares a track hostile by its rule only on an observed hostile act attributed to
//! it at the policy's threshold or more with nothing conflicting; what it is unsure of it flags
//! (never declares); proximity to a declared hostile is weak evidence (a flag and a suspect
//! raise, never a declaration); any operator confirms or revokes, and a revocation stops the
//! engagements on the track first. Off, the engine decides as rules 8 did. Replay recomputes
//! every attribution, declaration and flag, and catches a forged one.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::catalog::Domain;
use murmur_decide::ident::{self, ActVerdict, Against, FlagKind, IdentConfig, Ledger};
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str, replay_str_with};
use murmur_decide::scenario::{self, IdentOverride, ManualProxy, ManualStandard, RunOptions};
use murmur_decide::*;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const AT: LonLat = LonLat::new(-122.4950, 37.8120);

fn rule_on() -> IdentConfig {
    IdentConfig {
        rule_declaration: true,
        ..IdentConfig::default()
    }
}

fn track(at: LonLat, aff: Affiliation, sigma: f64) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, T0),
        alt_m: None,
        alt_t: None,
        velocity_mps: Some([0.0, 0.0]),
        pos_sigma_m: sigma,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(aff, T0),
        affiliation_confidence: 0.8,
        identity: Some("small_fast_boat".into()),
        identity_confidence: 0.9,
        sources: vec!["usv-1:radar".into()],
        source_t: BTreeMap::new(),
        domain: Domain::Surface,
        last_seen_t: T0,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
    }
}

fn report(at: LonLat, sigma: f64, against: Against) -> ident::ActReport {
    ident::ActReport {
        act: "A-1".into(),
        against,
        observed_t: T0,
        sensors: vec!["usv-1:radar".into()],
        location: at,
        sigma_m: sigma,
    }
}

fn ours() -> Against {
    Against::Vehicle { uid: Uid::new("usv-1") }
}

/// `m` metres east of `p`.
fn east(p: LonLat, m: f64) -> LonLat {
    LonLat::new(p.lon + m / (111_320.0 * p.lat.to_radians().cos()), p.lat)
}

fn decide(tracks: &BTreeMap<String, TrackFacts>, r: &ident::ActReport, cfg: &IdentConfig) -> ActVerdict {
    let c = ident::attribute(r, tracks, T0, cfg);
    ident::verdict(r, &c, tracks, &Ledger::default(), cfg)
}

// ---- the rule ---------------------------------------------------------------------------------

#[test]
fn a_clear_act_on_the_only_track_there_is_declared() {
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into();
    let v = decide(&tracks, &report(AT, 20.0, ours()), &rule_on());
    assert!(matches!(&v, ActVerdict::Declare { track, attribution } if track == "T-1" && *attribution >= 0.9), "{v:?}");
    // off: recorded, whatever the evidence (rules 8's behaviour: an operator declares)
    let v = decide(&tracks, &report(AT, 20.0, ours()), &IdentConfig::default());
    assert!(matches!(v, ActVerdict::Recorded { .. }), "{v:?}");
}

#[test]
fn two_tracks_close_together_share_the_attribution_and_the_act_is_flagged() {
    let tracks: BTreeMap<_, _> = [
        ("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0)),
        ("T-2".to_string(), track(east(AT, 15.0), Affiliation::Unknown, 15.0)),
    ]
    .into();
    let r = report(east(AT, 7.5), 60.0, ours());
    let c = ident::attribute(&r, &tracks, T0, &rule_on());
    assert_eq!(c.len(), 2);
    assert!((c[0].confidence - c[1].confidence).abs() < 0.1, "{c:?}");
    let v = ident::verdict(&r, &c, &tracks, &Ledger::default(), &rule_on());
    assert!(matches!(&v, ActVerdict::Flag { kind: FlagKind::Attribution, tracks, .. } if tracks.len() == 2), "{v:?}");
}

#[test]
fn an_act_far_from_every_track_or_on_no_victim_of_ours_is_flagged() {
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into();
    // 2 km away at 20 m: nobody we track
    let v = decide(&tracks, &report(east(AT, 2_000.0), 20.0, ours()), &rule_on());
    assert!(matches!(v, ActVerdict::Flag { kind: FlagKind::Untracked, .. }), "{v:?}");
    // against something we hold no track of: it can't be said to be ours
    let v = decide(&tracks, &report(AT, 20.0, Against::Unknown), &rule_on());
    assert!(matches!(v, ActVerdict::Flag { kind: FlagKind::Victim, .. }), "{v:?}");
    // against a track not in the picture
    let v = decide(&tracks, &report(AT, 20.0, Against::Track { track: "T-9".into() }), &rule_on());
    assert!(matches!(v, ActVerdict::Flag { kind: FlagKind::Victim, .. }), "{v:?}");
}

#[test]
fn a_track_shown_neutral_with_confidence_conflicts_and_is_flagged_not_declared() {
    let mut ferry = track(AT, Affiliation::Neutral, 15.0);
    ferry.affiliation_confidence = 0.95;
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), ferry)].into();
    let v = decide(&tracks, &report(AT, 20.0, ours()), &rule_on());
    assert!(matches!(v, ActVerdict::Flag { kind: FlagKind::Conflict, .. }), "{v:?}");
    // a weakly held neutral estimate does not conflict
    let mut weak = track(AT, Affiliation::Neutral, 15.0);
    weak.affiliation_confidence = 0.4;
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), weak)].into();
    let v = decide(&tracks, &report(AT, 20.0, ours()), &rule_on());
    assert!(matches!(v, ActVerdict::Declare { .. }), "{v:?}");
}

#[test]
fn the_threshold_and_every_key_are_bounded_and_unknown_keys_fail() {
    let bad = |toml: &str| toml::from_str::<IdentConfig>(toml).map_err(|e| e.to_string()).and_then(|c| c.validate());
    assert!(bad("rule_declaration = true\nattribution_threshold = 0.9").is_ok());
    for t in [
        "attribution_threshold = 0.3",
        "attribution_threshold = 1.0",
        "conflict_confidence = 0.1",
        "untracked_per_km2 = 0",
        "escort_m = 5000",
        "escort_speed_mps = 0",
        "rule_declaration = true\nthreshold = 0.9",
        "declare = true",
    ] {
        assert!(bad(t).is_err(), "{t}");
    }
    // a higher threshold flags what a lower one declares
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into();
    let r = report(east(AT, 40.0), 25.0, ours());
    let lo = IdentConfig { attribution_threshold: Some(0.6), ..rule_on() };
    let hi = IdentConfig { attribution_threshold: Some(0.99), ..rule_on() };
    assert!(matches!(decide(&tracks, &r, &lo), ActVerdict::Declare { .. }));
    assert!(matches!(decide(&tracks, &r, &hi), ActVerdict::Flag { kind: FlagKind::Attribution, .. }));
}

// ---- proximity: weak evidence -----------------------------------------------------------------

#[test]
fn keeping_company_with_a_declared_hostile_flags_and_raises_but_never_declares() {
    let clock = ManualClock::new(T0);
    let mut p = policy();
    p.identification = Some(rule_on());
    p.sync();
    let log = MemorySink::new();
    let mut e = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let mut w = world();
    let mut hostile = track(AT, Affiliation::Suspect, 15.0);
    hostile.declared_by = Some("op-1".into());
    hostile.affiliation = Fact::new(Affiliation::Hostile, T0);
    let mut escort = track(east(AT, 150.0), Affiliation::Unknown, 15.0);
    escort.velocity_mps = Some([0.5, 0.0]);
    let mut plane = track(east(AT, 100.0), Affiliation::Unknown, 15.0);
    plane.domain = Domain::Air;
    w.tracks = [("T-1".to_string(), hostile), ("T-2".to_string(), escort), ("T-3".to_string(), plane)].into();
    w.t = T0;
    declare_shown(&mut e, &w);
    let new = e.step_identification(&w).unwrap();
    assert_eq!(new.len(), 1, "one flag, for the surface track alongside only");
    let led = e.identification();
    let (f, _) = led.flags().next().unwrap();
    assert_eq!(f.kind, FlagKind::Proximity);
    assert_eq!(f.tracks, vec!["T-2".to_string()]);
    assert!(led.declaration("T-2").is_none(), "proximity never declares");
    // the engine's picture raises it to suspect; the next step flags nothing new
    let seen = e.identified(&w);
    assert_eq!(seen.tracks["T-2"].affiliation.value, Affiliation::Suspect);
    assert!(seen.tracks["T-2"].declared_by.is_none());
    assert!(e.step_identification(&w).unwrap().is_empty());
    let rep = replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((rep.flags, rep.rule_declarations), (1, 0));
}

// ---- off is rules 8 ---------------------------------------------------------------------------

/// Every scenario written before rules 9 (no act in it) ends the same with rule declaration
/// off under rules 9 as under rules 8: the same final state, the same failed rules.
#[test]
fn rule_declaration_off_decides_as_rules_8_did() {
    let dir = root().join("sim/scenarios/engage");
    let mut n = 0;
    for f in scenario::files(&dir).unwrap() {
        if std::fs::read_to_string(&f).unwrap().contains("action = \"act\"") {
            continue;
        }
        let off = RunOptions {
            identification: IdentOverride { rule_declaration: Some(false), ..IdentOverride::default() },
            ..RunOptions::default()
        };
        let v8 = RunOptions { rules_version: Some(OPERATOR_AUTH_SINCE), ..RunOptions::default() };
        let a = scenario::run_file(&f, &root(), &off).unwrap();
        let b = scenario::run_file(&f, &root(), &v8).unwrap();
        assert_eq!(a.final_state, b.final_state, "{}", f.display());
        assert_eq!(a.failed_rules, b.failed_rules, "{}", f.display());
        assert!(a.replay_ok && b.replay_ok, "{}", f.display());
        n += 1;
    }
    assert!(n >= 60, "{n} scenarios");
}

// ---- forgeries ----------------------------------------------------------------------------------

fn scenario_log(name: &str) -> String {
    let mut sc = scenario::load(&root().join("sim/scenarios/engage").join(name)).unwrap();
    sc.tamper = None;
    let r = scenario::run(&sc, name, &root(), &RunOptions::default()).unwrap();
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    r.log
}

#[test]
fn a_rule_declaration_without_its_act_is_caught() {
    let text = scenario_log("72-hostile-act-declared-by-rule.toml");
    assert!(replay_str(&text).unwrap().ok());
    // the act's event cut: the declaration names an act the log never observed
    let forged = forge(&text, |ev| ev.retain(|e| !matches!(e.body, EventBody::IdentAct { .. })));
    assert!(!replay_str(&forged).unwrap().ok());
    // the evidence names another act
    let forged = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IdentDeclared { declaration } = &mut e.body
                && let ident::Declarer::Rule { evidence, .. } = &mut declaration.by
            {
                evidence.act = "A-9".into();
            }
        }
    });
    assert!(!replay_str(&forged).unwrap().ok());
}

#[test]
fn a_rule_declaration_above_its_recomputed_attribution_is_caught() {
    let text = scenario_log("72-hostile-act-declared-by-rule.toml");
    // the act moved 300 m off in the log: the recomputed attribution no longer declares
    let forged = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IdentAct { act, .. } = &mut e.body {
                act.location = east(act.location, 300.0);
            }
        }
    });
    let r = replay_str(&forged).unwrap();
    assert!(!r.ok());
}

#[test]
fn a_declaration_claimed_for_an_operator_who_did_not_make_it_is_caught() {
    let text = scenario_log("74-neutral-alongside-hostile-flagged.toml");
    let forged = forge(&text, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IdentDeclared { declaration } = &mut e.body
                && let ident::Declarer::Operator { operator, .. } = &mut declaration.by
            {
                *operator = OperatorId::new("op-3");
            }
        }
    });
    assert!(!replay_str(&forged).unwrap().ok());
}

#[test]
fn identification_in_a_log_claiming_older_rules_is_caught() {
    let text = scenario_log("72-hostile-act-declared-by-rule.toml");
    let claim8 = text.replacen("\"rules_version\":9", "\"rules_version\":8", 1);
    let whats: Vec<String> = replay_str_with(
        &forge(&claim8, |_| {}),
        &ReplayOptions { rules: RulesAccepted::AtLeast(1), ..ReplayOptions::default() },
    )
    .unwrap()
    .mismatches
    .into_iter()
    .map(|m| m.what)
    .collect();
    assert!(!whats.is_empty(), "a rules 8 claim with rules 9's events must not replay clean");
}

// ---- the revocation, pending ---------------------------------------------------------------------

/// A revocation while an engagement on the track waits for its signatures voids it, before
/// anything else in that step (scenario 75 has one under way: aborted with its fallback).
#[test]
fn a_revocation_voids_an_engagement_still_waiting_for_signatures() {
    let mut sc = scenario::load(&root().join("sim/scenarios/engage/75-rule-declaration-revoked-mid-engagement.toml")).unwrap();
    sc.tamper = None;
    // the revocation before any signature
    for st in &mut sc.steps {
        if st.at_s == 30 {
            st.at_s = 8;
        }
    }
    sc.steps.retain(|s| !matches!(s.action, scenario::Action::Approve { .. } | scenario::Action::Expect { .. }));
    sc.expect = "voided".into();
    sc.expect_stop = None;
    sc.expect_ident.clear();
    sc.end_s = Some(20);
    let r = scenario::run(&sc, "75-pending", &root(), &RunOptions::default()).unwrap();
    assert_eq!(r.final_state.as_deref(), Some("voided"), "{:?} {:?}", r.final_reason, r.failures);
    assert!(r.ok, "{:?}", r.failures);
    assert!(r.final_reason.unwrap_or_default().contains("revoked by op-3"));
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
}

// ---- the manual proxy (the evaluation's) --------------------------------------------------------

#[test]
fn the_manual_proxy_declares_as_an_operator_after_its_delay() {
    let f = root().join("sim/scenarios/engage/72-hostile-act-declared-by-rule.toml");
    let opts = RunOptions {
        seed: Some(3),
        identification: IdentOverride { rule_declaration: Some(false), ..IdentOverride::default() },
        manual: Some(ManualProxy { delay_s: 10.0, standard: ManualStandard::Rule }),
        ..RunOptions::default()
    };
    let r = scenario::run_file(&f, &root(), &opts).unwrap();
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    let decl: Vec<_> = r.ident.iter().filter(|x| x.kind.ends_with("declaration")).collect();
    assert_eq!(decl.len(), 1, "{:?}", r.ident);
    assert_eq!(decl[0].kind, "operator_declaration");
    assert_eq!(decl[0].act.as_deref(), Some("A-1"));
    assert!(decl[0].t_s > 3.0, "after the act");
    // the ambiguous one: by the rule's standard the proxy declares nothing, the best guess does
    let f = root().join("sim/scenarios/engage/73-ambiguous-attribution-flagged.toml");
    let r = scenario::run_file(&f, &root(), &opts).unwrap();
    assert!(r.ident.iter().all(|x| !x.kind.ends_with("declaration")), "{:?}", r.ident);
    let best = RunOptions { manual: Some(ManualProxy { delay_s: 10.0, standard: ManualStandard::Best }), ..opts };
    let r = scenario::run_file(&f, &root(), &best).unwrap();
    assert!(r.ident.iter().any(|x| x.kind == "operator_declaration"), "{:?}", r.ident);
}

// ---- policies -------------------------------------------------------------------------------------

/// Every policy file that ships loads under the current rules (an unknown key fails closed),
/// and the minimal-approval profile says what its header says.
#[test]
fn every_shipped_policy_loads() {
    let mut files = vec![root().join("sim/policy.toml")];
    for dir in ["eval/policy", "sim/scenarios/engage/policies"] {
        for e in std::fs::read_dir(root().join(dir)).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "toml") {
                files.push(p);
            }
        }
    }
    for f in &files {
        Policy::load(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
    }
    let m = Policy::load(&root().join("eval/policy/minimal.toml")).unwrap();
    let i = m.identification();
    assert!(i.rule_declaration && i.nominate_unconfirmed == Some(true));
    assert_eq!(m.approval.step_up_acts.as_deref(), Some(&[][..]));
    // an unknown identification key fails to load
    let text = std::fs::read_to_string(root().join("sim/policy.toml")).unwrap();
    let bad = text.replace("[identification]\n", "[identification]\nauto_declare = true\n");
    assert!(Policy::from_toml(&bad).is_err());
}
