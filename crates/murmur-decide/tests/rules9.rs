//! Decision rules 9 (`RULE_ID_SINCE`): rule-based identification with human review. The
//! engine declares a track hostile by its rule only on an observed hostile act attributed to
//! it at the policy's threshold or more with nothing conflicting; what it is unsure of it flags
//! (never declares); proximity to a declared hostile is weak evidence (a flag and a suspect
//! raise, never a declaration); any operator confirms or revokes, and a revocation stops the
//! engagements on the track first. Off, the engine decides as rules 8 did. Replay recomputes
//! every attribution, declaration and flag, and catches a forged one.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::catalog::Domain;
use murmur_decide::ident::{self, ActVerdict, Against, Corroboration, FlagKind, FlagResolution, IdentConfig, Ledger};
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str, replay_str_with};
use murmur_decide::scenario::{self, IdentOverride, Pick, ReviewProxy, RunOptions};
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
        manoeuvre: None,
        mean_velocity_mps: None,
        first_seen_t: None,
        velocity_sigma_mps: None,
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
        fixes: Vec::new(),
        channel: None,
    }
}

/// Our fleet as the tests' acts name it.
fn fleet() -> BTreeSet<Uid> {
    [Uid::new("usv-1")].into()
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
    ident::verdict(r, &c, tracks, &fleet(), &Ledger::default(), T0, cfg)
}

// ---- the rule ---------------------------------------------------------------------------------

#[test]
fn a_clear_act_on_the_only_track_there_is_declared() {
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into();
    let v = decide(&tracks, &report(AT, 20.0, ours()), &rule_on());
    assert!(matches!(&v, ActVerdict::Declare { track, attribution, .. } if track == "T-1" && *attribution >= 0.9), "{v:?}");
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
    let v = ident::verdict(&r, &c, &tracks, &fleet(), &Ledger::default(), T0, &rule_on());
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
    // a fishing boat without AIS shown neutral at 0.56 (classified at 0.7): conflicts at the
    // default 0.5 (it did not at the first default, 0.7)
    let mut boat = track(AT, Affiliation::Neutral, 15.0);
    boat.affiliation_confidence = 0.56;
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), boat)].into();
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
        "corroboration = \"twice\"",
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
    // decisions read its own evidence (the flag shows it suspect to readers only, and names it
    // in its review flags); the next step flags nothing new
    let seen = e.identified(&w);
    assert_eq!(seen.tracks["T-2"].affiliation.value, Affiliation::Unknown);
    assert_eq!(seen.tracks["T-2"].review_flags, vec![1]);
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
        // (and none of rules 12's: young tracks counted as possibly manoeuvring, a platform
        // spent by its effect taking no order, by design; nor rules 14's: an aircraft counted
        // anywhere it could turn or speed up to; nor rules 15's: a nomination a failed carry may
        // owe refused, an un-spend; nor rules 16's: a dropped contact still counted)
        let text = std::fs::read_to_string(&f).unwrap();
        if text.contains("action = \"act\"")
            || text.contains("tracked_s")
            || text.starts_with("# Rules 12")
            || text.starts_with("# Rules 14")
            || text.starts_with("# Rules 15")
            || text.starts_with("# Rules 16")
        {
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
    let claim8 = text.replacen(
        &format!("\"rules_version\":{RULES_VERSION}"),
        &format!("\"rules_version\":{}", RULE_ID_SINCE - 1),
        1,
    );
    assert_ne!(claim8, text, "the claim was changed");
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

// ---- the review proxy (the evaluation's) -------------------------------------------------

/// The evaluation's proxy works from what the console shows, by its own procedure: one track
/// at the fix and not shown neutral is the author; a second track there, or only a neutral,
/// and it can't say. Never the engine's rule.
#[test]
fn the_proxys_procedure_reads_the_console_not_the_rule() {
    let one: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Unknown, 15.0))].into();
    let r = report(east(AT, 30.0), 20.0, ours());
    assert_eq!(scenario::console_pick(&r, &one), Pick::Author("T-1".into()));
    // 30 m from it at 20 m and 15 m: within two combined sigmas (50 m); the rule's attribution
    // against an untracked contact per km2 is 0.99 too, but at 80 m the proxy can't say while
    // the rule would still weigh it
    let far = report(east(AT, 80.0), 20.0, ours());
    assert_eq!(scenario::console_pick(&far, &one), Pick::CantSay("no track at the fix"));
    let two: BTreeMap<_, _> = [
        ("T-1".to_string(), track(AT, Affiliation::Unknown, 15.0)),
        ("T-2".to_string(), track(east(AT, 40.0), Affiliation::Unknown, 15.0)),
    ]
    .into();
    assert_eq!(scenario::console_pick(&r, &two), Pick::CantSay("several tracks at the fix"));
    let mut ferry = track(AT, Affiliation::Neutral, 15.0);
    ferry.affiliation_confidence = 0.8;
    let neutral: BTreeMap<_, _> = [("T-1".to_string(), ferry)].into();
    assert_eq!(scenario::console_pick(&r, &neutral), Pick::CantSay("only a neutral or friend at the fix"));
    // the victim is never its author
    let on_victim = report(AT, 20.0, Against::Track { track: "T-1".into() });
    assert_eq!(scenario::console_pick(&on_victim, &one), Pick::CantSay("no track at the fix"));
}

/// With rule declaration off the proxy declares from the acts itself, under its own
/// session, naming the act; with it on it confirms or revokes the rule's declarations and
/// answers flags. Every log replays.
#[test]
fn the_review_proxy_declares_confirms_and_answers_flags() {
    let f = root().join("sim/scenarios/engage/72-hostile-act-declared-by-rule.toml");
    let manual = RunOptions {
        seed: Some(3),
        identification: IdentOverride { rule_declaration: Some(false), ..IdentOverride::default() },
        proxy: Some(ReviewProxy { diligence: 1.0, misread: 0.0, ..ReviewProxy::default() }),
        ..RunOptions::default()
    };
    let r = scenario::run_file(&f, &root(), &manual).unwrap();
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    let decl: Vec<_> = r.ident.iter().filter(|x| x.kind.ends_with("declaration")).collect();
    assert_eq!(decl.len(), 1, "{:?}", r.ident);
    assert_eq!(decl[0].kind, "operator_declaration");
    assert_eq!(decl[0].act.as_deref(), Some("A-1"));
    assert!(decl[0].t_s > 3.0, "after the act");
    // the ambiguous one: the proxy can't say either (two boats at the fix): nothing declared
    let f = root().join("sim/scenarios/engage/73-ambiguous-attribution-flagged.toml");
    let r = scenario::run_file(&f, &root(), &manual).unwrap();
    assert!(r.ident.iter().all(|x| !x.kind.ends_with("declaration")), "{:?}", r.ident);
    // rule on: the false declaration of 75 (an untracked author beside a fishing boat), which
    // the proxy, reading, can't pin on the boat... only if the boat is alone at the fix: it is,
    // so it confirms (the picture is wrong, not the reading)
    let f = root().join("sim/scenarios/engage/75-rule-declaration-revoked-mid-engagement.toml");
    let rule = RunOptions {
        seed: Some(3),
        proxy: Some(ReviewProxy { diligence: 1.0, misread: 0.0, ..ReviewProxy::default() }),
        ..RunOptions::default()
    };
    let r = scenario::run_file(&f, &root(), &rule).unwrap();
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    assert!(r.ident.iter().any(|x| x.kind == "confirmed" && x.by.as_deref() == Some("op-1")), "{:?}", r.ident);
}

/// The nominator nominates a declared track as soon as it may: with the shipped policy a rule
/// declaration is refused until confirmed (and retried), with `nominate_unconfirmed` at once.
#[test]
fn the_nominator_waits_for_a_confirmation_only_when_the_policy_does() {
    let f = root().join("sim/scenarios/engage/72-hostile-act-declared-by-rule.toml");
    let mut sc = scenario::load(&f).unwrap();
    sc.tamper = None;
    // the file's own nomination and signatures out: the nominator's only
    sc.steps.retain(|st| {
        !matches!(
            st.action,
            scenario::Action::Nominate { .. } | scenario::Action::Approve { .. } | scenario::Action::ConfirmDeclaration { .. } | scenario::Action::Expect { .. }
        )
    });
    sc.end_s = Some(200);
    let run = |nominate: bool, proxy: bool| {
        let o = RunOptions {
            identification: IdentOverride { nominate_unconfirmed: Some(nominate), ..IdentOverride::default() },
            proxy: proxy.then(|| ReviewProxy { diligence: 1.0, misread: 0.0, ..ReviewProxy::default() }),
            nominator: true,
            ..RunOptions::default()
        };
        scenario::run(&sc, "72-nominator", &root(), &o).unwrap()
    };
    // shipped, nobody to confirm: never nominated past the confirmation
    let r = run(false, false);
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    assert_ne!(r.final_state.as_deref(), Some("done"), "{:?}", r.final_reason);
    assert!(r.final_reason.as_deref().unwrap_or("").contains("confirmation"), "{:?}", r.final_reason);
    // shipped, the proxy confirms: released after the confirmation
    let r = run(false, true);
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    assert_eq!(r.final_state.as_deref(), Some("done"), "{:?} {:?}", r.final_reason, r.refusals);
    // nominate_unconfirmed: released without anyone confirming
    let r = run(true, false);
    assert!(r.replay_ok, "{:?}", r.replay_mismatches);
    assert_eq!(r.final_state.as_deref(), Some("done"), "{:?} {:?}", r.final_reason, r.refusals);
}

// ---- the review of rules 9 ------------------------------------------------------------------

/// A clock that returns `base` until its `at`-th read, `base + 1 ms` from then on: the wall
/// clock crossing a millisecond boundary at one particular read (the review's probe).
#[derive(Clone)]
struct StepClock {
    calls: Arc<AtomicI64>,
    at: i64,
    base: i64,
}

impl Clock for StepClock {
    fn now(&self) -> Time {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Time(self.base + i64::from(n >= self.at))
    }
}

fn rule_policy() -> Policy {
    let mut p = policy();
    p.identification = Some(rule_on());
    p.sync();
    p
}

fn rv_track(at: LonLat, aff: Affiliation, conf: f64) -> TrackFacts {
    let mut t = track(at, aff, 10.0);
    t.affiliation_confidence = conf;
    t.identity = Some("fishing_vessel".into());
    t
}

fn rv_report(id: &str, at: LonLat, sigma: f64) -> ident::ActReport {
    let mut r = report(at, sigma, Against::Vehicle { uid: Uid::new("usv-1") });
    r.act = id.into();
    r
}

/// One identification sequence: an operator declares T-1 hostile; an act on T-2 (alone
/// there) is declared by rule; an act between T-3 and T-4 is flagged; the weak-evidence rule
/// flags T-5 beside T-1. Returns the log and the clock reads.
fn sequence(clock: StepClock) -> (String, i64) {
    let log = MemorySink::new();
    let calls = clock.calls.clone();
    let mut e = Engine::new(rule_policy(), roster(), Box::new(clock), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.vehicles.insert(Uid::new("usv-1"), vehicle("seahunter", ON_WATER));
    let far = east(AT, 3_000.0);
    let mid = east(AT, 6_000.0);
    w.tracks = [
        ("T-1".to_string(), rv_track(AT, Affiliation::Suspect, 0.8)),
        ("T-2".to_string(), rv_track(far, Affiliation::Unknown, 0.3)),
        ("T-3".to_string(), rv_track(mid, Affiliation::Unknown, 0.3)),
        ("T-4".to_string(), rv_track(east(mid, 15.0), Affiliation::Unknown, 0.3)),
        ("T-5".to_string(), rv_track(east(AT, 100.0), Affiliation::Unknown, 0.3)),
    ]
    .into();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.declare_track(s.authority(), None, "T-1", Affiliation::Hostile, None, None, &w).unwrap();
    let o = e.record_act(rv_report("A-1", far, 20.0), &w).unwrap();
    assert!(matches!(o.verdict, ActVerdict::Declare { .. }), "{:?}", o.verdict);
    let o = e.record_act(rv_report("A-2", east(mid, 7.5), 60.0), &w).unwrap();
    assert!(matches!(o.verdict, ActVerdict::Flag { .. }), "{:?}", o.verdict);
    let f = e.step_identification(&w).unwrap();
    assert!(!f.is_empty());
    (log.text(), calls.load(Ordering::SeqCst))
}

/// Review, fix 4: a declaration's or flag's own time and its event's time are one clock read,
/// so a millisecond boundary between two reads never makes a genuine log fail replay (before:
/// 4 of 12 positions did).
#[test]
fn a_millisecond_boundary_between_clock_reads_never_breaks_replay() {
    let base = T0.0;
    let (text, reads) = sequence(StepClock { calls: Arc::new(AtomicI64::new(0)), at: i64::MAX, base });
    assert!(replay_str(&text).unwrap().ok(), "no tick: replays");
    let mut bad = Vec::new();
    for k in 0..reads {
        let (text, _) = sequence(StepClock { calls: Arc::new(AtomicI64::new(0)), at: k, base });
        let r = replay_str(&text).unwrap();
        if !r.ok() {
            bad.push((k, r.mismatches.iter().map(|m| m.what.clone()).collect::<Vec<_>>()));
        }
    }
    assert!(reads >= 6, "{reads} reads");
    assert!(bad.is_empty(), "{} of {reads} tick positions produce a genuine log replay rejects: {bad:?}", bad.len());
}

/// Review, fix 2: a dismissed proximity flag leaves no residue. T-2 (unclassified) keeps
/// company with the declared T-1 and is flagged; an operator dismisses it; later T-2 reads
/// neutral at 0.95 (AIS heard). An act whose fix sits on T-2 conflicts, as it would with no
/// flag history (before: declared at 0.997).
#[test]
fn a_dismissed_proximity_flag_does_not_switch_off_the_conflict_check() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(rule_policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.vehicles.insert(Uid::new("usv-1"), vehicle("seahunter", ON_WATER));
    let mut t2 = rv_track(east(AT, 250.0), Affiliation::Unknown, 0.0);
    t2.identity = None;
    t2.identity_confidence = 0.0;
    w.tracks = [("T-1".to_string(), rv_track(AT, Affiliation::Suspect, 0.8)), ("T-2".to_string(), t2)].into();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.declare_track(s.authority(), None, "T-1", Affiliation::Hostile, None, None, &w).unwrap();
    let f = e.step_identification(&w).unwrap();
    assert_eq!(f.len(), 1, "a proximity flag on T-2");
    e.resolve_flag(s.authority(), f[0], FlagResolution::Dismissed, Some("a ferry, not an escort".into()))
        .unwrap();
    clock.advance_ms(30_000);
    w.t = Time(T0.0 + 30_000);
    let t = w.tracks.get_mut("T-2").unwrap();
    t.affiliation = Fact::new(Affiliation::Neutral, w.t);
    t.affiliation_confidence = 0.95;
    t.identity = Some("ferry".into());
    t.position = Fact::new(east(AT, 250.0), w.t);
    w.tracks.get_mut("T-1").unwrap().position = Fact::new(AT, w.t);
    let mut r = rv_report("A-1", east(AT, 250.0), 20.0);
    r.observed_t = w.t;
    let o = e.record_act(r, &w).unwrap();
    assert!(replay_str(&log.text()).unwrap().ok());
    assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Conflict, .. }), "{:?}", o.verdict);
}

/// Review, fix 2: an open proximity flag never hides later neutral evidence from the check
/// (before: the decision picture showed T-2 suspect and declared it).
#[test]
fn an_open_proximity_flag_never_hides_neutral_evidence() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(rule_policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.vehicles.insert(Uid::new("usv-1"), vehicle("seahunter", ON_WATER));
    let mut t2 = rv_track(east(AT, 250.0), Affiliation::Unknown, 0.0);
    t2.identity = None;
    w.tracks = [("T-1".to_string(), rv_track(AT, Affiliation::Suspect, 0.8)), ("T-2".to_string(), t2)].into();
    let s = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    e.declare_track(s.authority(), None, "T-1", Affiliation::Hostile, None, None, &w).unwrap();
    assert_eq!(e.step_identification(&w).unwrap().len(), 1);
    clock.advance_ms(30_000);
    w.t = Time(T0.0 + 30_000);
    let t = w.tracks.get_mut("T-2").unwrap();
    t.affiliation = Fact::new(Affiliation::Neutral, w.t);
    t.affiliation_confidence = 0.95;
    let mut r = rv_report("A-1", east(AT, 250.0), 20.0);
    r.observed_t = w.t;
    let o = e.record_act(r, &w).unwrap();
    let seen = e.identified(&w);
    assert_eq!(seen.tracks["T-2"].affiliation.value, Affiliation::Neutral, "decisions read its own evidence");
    assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Conflict, .. }), "{:?}", o.verdict);
    assert!(replay_str(&log.text()).unwrap().ok());
}

/// Review, fix 1: a victim "vehicle" that is not one of our fleet is not ours (before: any
/// uid counted).
#[test]
fn an_act_against_a_vehicle_not_in_our_fleet_is_flagged() {
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Unknown, 15.0))].into();
    let r = report(AT, 20.0, Against::Vehicle { uid: Uid::new("no-such-vehicle") });
    let v = decide(&tracks, &r, &rule_on());
    assert!(matches!(v, ActVerdict::Flag { kind: FlagKind::Victim, .. }), "{v:?}");
    // through the engine: the fleet is the world's vehicles, logged with the act
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(rule_policy(), roster(), Box::new(clock), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.tracks = tracks;
    let o = e.record_act(r, &w).unwrap();
    assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Victim, .. }), "{:?}", o.verdict);
    let rep = replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
}

/// Review, fix 3: the victim is never its own attacker. An untracked author 20 m from a
/// tracked fishing boat attacks that boat (before: the boat was declared at 0.995).
#[test]
fn the_victim_of_an_act_is_never_declared_its_author() {
    let tracks: BTreeMap<_, _> = [("T-9".to_string(), rv_track(AT, Affiliation::Unknown, 0.3))].into();
    let r = report(east(AT, 20.0), 20.0, Against::Track { track: "T-9".into() });
    let c = ident::attribute(&r, &tracks, T0, &rule_on());
    assert!(c.is_empty(), "{c:?}");
    let v = decide(&tracks, &r, &rule_on());
    assert!(matches!(v, ActVerdict::Flag { kind: FlagKind::Untracked, .. }), "{v:?}");
}

/// Decided (review, design point 8): an act against an undeclared suspect is an act against
/// a contact we protect, so its author may be declared; an unclassified author is declarable
/// (the act is the evidence).
#[test]
fn an_act_against_a_suspect_declares_its_author() {
    let mut victim = rv_track(east(AT, 2_000.0), Affiliation::Suspect, 0.9);
    victim.identity = None;
    let mut author = rv_track(AT, Affiliation::Unknown, 0.0);
    author.identity = None;
    author.identity_confidence = 0.0;
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), author), ("T-9".to_string(), victim)].into();
    let r = report(AT, 20.0, Against::Track { track: "T-9".into() });
    let v = decide(&tracks, &r, &rule_on());
    assert!(matches!(&v, ActVerdict::Declare { track, .. } if track == "T-1"), "{v:?}");
}

/// Corroboration (user decision): with `second_observation` one fix flags, a second
/// observer's agreeing fix declares, and so does a second act; replay recomputes both.
#[test]
fn corroboration_waits_for_a_second_observation() {
    let cfg = IdentConfig { corroboration: Some(Corroboration::SecondObservation), ..rule_on() };
    let tracks: BTreeMap<_, _> = [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into();
    let one = report(AT, 20.0, ours());
    assert!(matches!(decide(&tracks, &one, &cfg), ActVerdict::Flag { kind: FlagKind::Uncorroborated, .. }));
    let mut two = one.clone();
    two.fixes.push(ident::Fix {
        observer: "blade-1".into(),
        sensors: vec!["blade-1:eo_ir".into()],
        location: east(AT, 5.0),
        sigma_m: 25.0,
    });
    let v = decide(&tracks, &two, &cfg);
    assert!(
        matches!(&v, ActVerdict::Declare { corroborated_by: Some(c), .. } if c.observer.as_deref() == Some("blade-1")),
        "{v:?}"
    );
    // a second act, through the engine, replayed
    let mut p = policy();
    p.identification = Some(cfg);
    p.sync();
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.vehicles.insert(Uid::new("usv-1"), vehicle("seahunter", ON_WATER));
    w.tracks = tracks;
    let o = e.record_act(one.clone(), &w).unwrap();
    assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Uncorroborated, .. }), "{:?}", o.verdict);
    clock.advance_ms(20_000);
    let mut again = one;
    again.act = "A-2".into();
    let o = e.record_act(again, &w).unwrap();
    assert!(
        matches!(&o.verdict, ActVerdict::Declare { corroborated_by: Some(c), .. } if c.act == "A-1"),
        "{:?}",
        o.verdict
    );
    let rep = replay_str(&log.text()).unwrap();
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert!(o.declaration.unwrap().describe().contains("corroborated by act A-1"));
}

/// An engine with corroboration on, its world holding `tracks`, our usv-1 on the water.
fn corroborating_engine(window_s: Option<f64>, tracks: BTreeMap<String, TrackFacts>) -> (Engine, WorldSnapshot, ManualClock, MemorySink) {
    let cfg = IdentConfig {
        corroboration: Some(Corroboration::SecondObservation),
        corroboration_window_s: window_s,
        ..rule_on()
    };
    let mut p = policy();
    p.identification = Some(cfg);
    p.sync();
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let e = Engine::new(p, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.vehicles.insert(Uid::new("usv-1"), vehicle("seahunter", ON_WATER));
    w.tracks = tracks;
    (e, w, clock, log)
}

/// Review of 10-05 (fix 3): an earlier act corroborates only if it would itself have counted:
/// one flagged because its victim was not ours, or because the track's evidence conflicted,
/// never does; and only within `corroboration_window_s`. Replay recomputes every verdict.
#[test]
fn corroboration_counts_only_eligible_earlier_acts_within_the_window() {
    let suspect = || -> BTreeMap<String, TrackFacts> { [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into() };
    // a victim not ours: flagged, and no corroboration for the next act
    let (mut e, w, clock, log) = corroborating_engine(None, suspect());
    let mut first = report(AT, 20.0, Against::Vehicle { uid: Uid::new("usv-9") });
    first.act = "A-1".into();
    let o = e.record_act(first, &w).unwrap();
    assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Victim, .. }), "{:?}", o.verdict);
    clock.advance_ms(20_000);
    let mut second = report(AT, 20.0, ours());
    second.act = "A-2".into();
    let o = e.record_act(second, &w).unwrap();
    assert!(
        matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Uncorroborated, .. }),
        "an act flagged for its victim corroborated: {:?}",
        o.verdict
    );
    assert!(replay_str(&log.text()).unwrap().ok());

    // conflicting evidence then: flagged, and no corroboration once the evidence changes
    let mut neutral = suspect();
    neutral.get_mut("T-1").unwrap().affiliation = Fact::new(Affiliation::Neutral, T0);
    let (mut e, mut w, clock, log) = corroborating_engine(None, neutral);
    let o = e.record_act(report(AT, 20.0, ours()), &w).unwrap();
    assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Conflict, .. }), "{:?}", o.verdict);
    clock.advance_ms(20_000);
    w.tracks = suspect();
    let mut second = report(AT, 20.0, ours());
    second.act = "A-2".into();
    let o = e.record_act(second, &w).unwrap();
    assert!(
        matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Uncorroborated, .. }),
        "an act flagged for conflicting evidence corroborated: {:?}",
        o.verdict
    );
    assert!(replay_str(&log.text()).unwrap().ok());

    // an eligible act: within the window it corroborates, past it it does not
    for (gap_ms, declares) in [(500_000, true), (700_000, false)] {
        let (mut e, mut w, clock, log) = corroborating_engine(Some(600.0), suspect());
        let o = e.record_act(report(AT, 20.0, ours()), &w).unwrap();
        assert!(matches!(o.verdict, ActVerdict::Flag { kind: FlagKind::Uncorroborated, .. }));
        clock.advance_ms(gap_ms);
        // the tracker kept it fresh meanwhile
        let then = T0.plus_ms(gap_ms);
        let t1 = w.tracks.get_mut("T-1").unwrap();
        t1.position.t = then;
        t1.last_seen_t = then;
        w.t = then;
        let mut second = report(AT, 20.0, ours());
        second.act = "A-2".into();
        second.observed_t = T0.plus_ms(gap_ms);
        let o = e.record_act(second, &w).unwrap();
        assert_eq!(
            matches!(&o.verdict, ActVerdict::Declare { corroborated_by: Some(c), .. } if c.act == "A-1"),
            declares,
            "{gap_ms} ms apart: {:?}",
            o.verdict
        );
        let rep = replay_str(&log.text()).unwrap();
        assert!(rep.ok(), "{:?}", rep.mismatches);
    }
    // the window is bounded
    let bad = IdentConfig { corroboration_window_s: Some(5.0), ..rule_on() };
    assert!(bad.validate().is_err());
}

/// Revocation notices (user decision): the ledger keeps who revoked whose declaration, so the
/// declarer hears of it.
#[test]
fn a_revocation_is_kept_for_the_declarers_notice() {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut e = Engine::new(rule_policy(), roster(), Box::new(clock), Box::new(log.clone())).unwrap();
    let mut w = world();
    w.tracks = [("T-1".to_string(), track(AT, Affiliation::Suspect, 15.0))].into();
    let a = e.open_operator_session(OperatorId::new("op-1")).unwrap();
    let b = e.open_operator_session(OperatorId::new("op-2")).unwrap();
    e.declare_track(a.authority(), None, "T-1", Affiliation::Hostile, None, None, &w).unwrap();
    e.revoke_declaration(b.authority(), "T-1", Some("a ferry".into())).unwrap();
    let rv = e.identification().revocations();
    assert_eq!(rv.len(), 1);
    assert_eq!(rv[0].by.as_str(), "op-2");
    assert_eq!(rv[0].note.as_deref(), Some("a ferry"));
    assert_eq!(rv[0].declaration.label(), "op-1");
    assert!(replay_str(&log.text()).unwrap().ok());
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

// ---- the keep-out of a manoeuvring track (review of 10-05: the tracker) -----------------------

/// A protected track the tracker saw manoeuvre counts in a keep-out anywhere it can reach at
/// the fastest it went, and every track along its average velocity of the last minute too;
/// rules 8 count only its velocity of the moment. Through the keep-out check itself.
#[test]
fn a_manoeuvring_track_is_kept_out_wherever_it_could_reach() {
    use murmur_decide::engage::{keep_out_tracks, track_reach};
    use murmur_decide::geo::haversine_m;
    let effect_t = T0.plus_ms(60_000);
    let ec = EngageRules::default();
    let ec8 = EngageRules {
        rules_version: 8,
        ..EngageRules::default()
    };
    let mut t = track(AT, Affiliation::Neutral, 15.0);
    // a fishing boat on an eastward leg of its zigzag, 8 m/s now
    t.velocity_mps = Some([8.0, 0.0]);
    let inside = |t: &TrackFacts, ec: &EngageRules, p: LonLat| {
        track_reach(t, effect_t, ec)
            .iter()
            .any(|r| haversine_m(r.center, p) <= r.radius_m)
    };
    let check = |t: &TrackFacts, ec: &EngageRules, p: LonLat| {
        let mut w = world();
        w.tracks = [("T-7".to_string(), t.clone())].into();
        keep_out_tracks(p, effect_t, 50.0, "T-0001", &w, ec, None)
    };
    // had it turned back, it would be 300 m west in a minute: its velocity alone says 480 m east
    let west = east(AT, -300.0);
    assert!(!inside(&t, &ec, west));
    assert_eq!(check(&t, &ec, west).outcome, Outcome::Pass);
    // the tracker saw it manoeuvre: anywhere it reaches at 8.5 m/s in a minute
    t.manoeuvre = Some(Manoeuvre {
        t: T0,
        change_mps: 11.3,
        speed_mps: 8.5,
    });
    assert!(inside(&t, &ec, west));
    let c = check(&t, &ec, west);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("manoeuvring"), "{}", c.detail);
    // rules 8: its velocity of the moment only
    assert!(!inside(&t, &ec8, west));
    assert_eq!(check(&t, &ec8, west).outcome, Outcome::Pass);
    // its average velocity of the last minute: north-east, as the zigzag goes
    t.manoeuvre = None;
    t.mean_velocity_mps = Some([4.0, 4.0]);
    let ne = LonLat::new(east(AT, 240.0).lon, AT.lat + 240.0 / 111_320.0);
    assert!(inside(&t, &ec, ne));
    let c = check(&t, &ec, ne);
    assert_eq!(c.outcome, Outcome::Fail, "{}", c.detail);
    assert!(c.detail.contains("average velocity"), "{}", c.detail);
    assert!(!inside(&t, &ec8, ne));
}
