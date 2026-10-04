//! Policy matching against the real `sim/policy.toml`, plus the file's own validation.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::policy::decide;
use murmur_decide::*;

fn decision(source: Source, p: Proposal, w: &WorldSnapshot) -> PolicyDecision {
    let s = spec(source, p);
    decide(&s, &w.subset_for(&s), &policy())
}

#[test]
fn catalog_marks_exactly_the_armed_platforms() {
    let armed: Vec<_> = catalog()
        .platforms
        .values()
        .filter(|p| p.armed)
        .map(|p| p.id.clone())
        .collect();
    assert_eq!(armed, vec!["fpv", "magura", "switchblade", "ugv_armed"]);
}

#[test]
fn hold_and_rtl_are_auto_even_on_armed_platforms() {
    let d = decision(agent(), hold(&["blade-1"]), &world());
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Auto, "safety-commands"));
    let p = Proposal::new(Task::Rtl {}, vec![uid("blade-1")]);
    assert_eq!(decision(human(), p, &world()).tier, Tier::Auto);
}

#[test]
fn any_task_on_an_armed_platform_needs_a_human() {
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["blade-1"]), &world());
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Human, "armed-platform"));
    assert_eq!(d.facts.armed, vec![uid("blade-1")]);
    // one armed vehicle in a group is enough
    let d = decision(
        human(),
        orbit(NEAR_CRISSY, 150.0, &["sky-1", "blade-1"]),
        &world(),
    );
    assert_eq!(d.tier, Tier::Human);
}

#[test]
fn unknown_platform_counts_as_armed() {
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().platform = None;
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Human, "armed-platform"));
}

#[test]
fn leaving_the_approved_area_needs_a_human() {
    let d = decision(
        human(),
        orbit(OUTSIDE_APPROVED, 150.0, &["sky-1"]),
        &world(),
    );
    assert_eq!(
        (d.tier, d.rule.as_str()),
        (Tier::Human, "outside-approved-area")
    );
    assert!(d.facts.area_detail.contains("outside the approved area"));
    let patrol = |route: Vec<LonLat>| Proposal::new(Task::Patrol { route }, vec![uid("sky-1")]);
    let inside = vec![LonLat::new(-122.52, 37.78), LonLat::new(-122.40, 37.78)];
    assert_eq!(decision(human(), patrol(inside), &world()).tier, Tier::Auto);
    let out = vec![LonLat::new(-122.52, 37.78), LonLat::new(-122.38, 37.78)];
    let d = decision(human(), patrol(out), &world());
    assert_eq!(d.tier, Tier::Human);
    assert!(d.facts.area_detail.contains("waypoint 2"));
}

#[test]
fn a_leg_leaving_a_concave_approved_area_needs_a_human() {
    // U-shaped approved area: both points inside, the leg between them crosses the notch.
    let u = r#"{"type":"FeatureCollection","features":[{"type":"Feature",
        "properties":{"id":"u","kind":"approved"},
        "geometry":{"type":"Polygon","coordinates":[[[0,0],[3,0],[3,3],[2,3],[2,1],[1,1],[1,3],[0,3],[0,0]]]}}]}"#;
    let mut w = world();
    w.areas = Some(AreaSet::from_geojson("u", u, T0).unwrap());
    let route = vec![LonLat::new(0.5, 2.5), LonLat::new(2.5, 2.5)];
    let d = decision(
        human(),
        Proposal::new(Task::Patrol { route }, vec![uid("sky-1")]),
        &w,
    );
    assert_eq!(d.tier, Tier::Human);
    assert!(
        d.facts
            .area_detail
            .contains("crosses the approved-area boundary"),
        "{d:?}"
    );
}

#[test]
fn transit_and_return_legs_leaving_the_approved_area_need_a_human() {
    // The task is inside, but sky-1 is outside the approved area now.
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("sky-1")).unwrap();
    v.position = Some(Fact::new(OUTSIDE_APPROVED, T0));
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_eq!(
        (d.tier, d.rule.as_str()),
        (Tier::Human, "outside-approved-area")
    );
    assert_eq!(
        d.facts.area_detail,
        "sky-1: transit leg (position to waypoint 1): position is outside the approved area"
    );

    // Starts inside, but home is outside: the return leg leaves.
    let mut w = world();
    let v = w.vehicles.get_mut(&uid("sky-1")).unwrap();
    v.home = Some(Fact::new(OUTSIDE_APPROVED, T0));
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_eq!(d.tier, Tier::Human);
    assert_eq!(
        d.facts.area_detail,
        "sky-1: return leg (waypoint 1 to home): home is outside the approved area"
    );

    // rtl is only the return leg; it stays auto (safety commands first) but the fact shows.
    let d = decision(human(), rtl(&["sky-1"]), &w);
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Auto, "safety-commands"));
    assert!(d.facts.leaves_approved_area);
    assert_eq!(
        d.facts.area_detail,
        "sky-1: return leg (position to home): home is outside the approved area"
    );

    // Both ends inside a concave area, the transit leg crosses its notch.
    let u = r#"{"type":"FeatureCollection","features":[{"type":"Feature",
        "properties":{"id":"u","kind":"approved"},
        "geometry":{"type":"Polygon","coordinates":[[[0,0],[3,0],[3,3],[2,3],[2,1],[1,1],[1,3],[0,3],[0,0]]]}}]}"#;
    let mut w = world();
    w.areas = Some(AreaSet::from_geojson("u", u, T0).unwrap());
    let v = w.vehicles.get_mut(&uid("sky-1")).unwrap();
    v.position = Some(Fact::new(LonLat::new(0.5, 2.5), T0));
    v.home = Some(Fact::new(LonLat::new(2.5, 0.5), T0));
    let route = vec![LonLat::new(2.5, 2.5), LonLat::new(2.5, 2.0)];
    let d = decision(
        human(),
        Proposal::new(Task::Patrol { route }, vec![uid("sky-1")]),
        &w,
    );
    assert_eq!(
        d.facts.area_detail,
        "sky-1: transit leg (position to waypoint 1) crosses the approved-area boundary"
    );

    // No fresh position: can't tell, counted as leaving.
    let mut w = world();
    w.vehicles.get_mut(&uid("sky-1")).unwrap().position = None;
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert!(d.facts.leaves_approved_area);
    assert!(d.facts.area_detail.contains("counted as leaving"), "{d:?}");
}

#[test]
fn unknown_endurance_routes_to_a_human_with_a_visible_reason() {
    // mule-1 (logistics UGV) has no endurance figure: reserves is unverified.
    for src in [human(), agent()] {
        let d = decision(src, orbit(NEAR_CRISSY, 100.0, &["mule-1"]), &world());
        assert_eq!(
            (d.tier, d.rule.as_str()),
            (Tier::Human, "endurance-unknown")
        );
        assert_eq!(d.facts.unverified.len(), 1);
        assert!(
            d.facts.unverified[0].starts_with("reserves mule-1: not evaluated: endurance unknown"),
            "{d:?}"
        );
    }
    // Hold and rtl stay auto (reserves is not required for them).
    let d = decision(human(), hold(&["mule-1"]), &world());
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Auto, "safety-commands"));
    assert!(d.facts.unverified.is_empty());
    // A platform with an endurance figure is unaffected.
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &world());
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Auto, "default"));
}

#[test]
fn an_unverified_check_is_never_auto_approved_whatever_the_file_says() {
    let p = Policy::from_toml(
        r#"
        [[tier]]
        name = "everything-auto"
        tier = "auto"
        reason = "careless file"
        "#,
    )
    .unwrap();
    let w = world();
    let s = spec(agent(), orbit(NEAR_CRISSY, 100.0, &["mule-1"]));
    let d = decide(&s, &w.subset_for(&s), &p);
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Human, "(unverified)"));
    assert!(d.reason.contains("everything-auto"), "{d:?}");
    // The condition only matches the rules it lists.
    let p = Policy::from_toml(
        r#"
        [[tier]]
        name = "unverified-domain"
        when = { unverified = ["domain"] }
        tier = "human"
        reason = "r"
        [[tier]]
        name = "rest"
        tier = "human"
        reason = "r"
        "#,
    )
    .unwrap();
    assert_eq!(decide(&s, &w.subset_for(&s), &p).rule, "rest");
}

#[test]
fn no_area_loaded_counts_as_leaving() {
    let mut w = world();
    w.areas = None;
    let d = decision(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &w);
    assert_eq!(
        (d.tier, d.rule.as_str()),
        (Tier::Human, "outside-approved-area")
    );
}

#[test]
fn routine_task_inside_is_auto_for_humans_and_agents() {
    for src in [human(), agent()] {
        let d = decision(src, orbit(NEAR_CRISSY, 150.0, &["sky-1"]), &world());
        assert_eq!((d.tier, d.rule.as_str()), (Tier::Auto, "default"));
    }
}

#[test]
fn first_match_wins_and_no_match_fails_closed() {
    let p = Policy::from_toml(
        r#"
        [[tier]]
        name = "agents-ask"
        when = { source = "agent" }
        tier = "human"
        reason = "agents ask first"
        [[tier]]
        name = "humans-orbit"
        when = { source = "human", kind = ["orbit"] }
        tier = "auto"
        reason = "trusted"
        [[tier]]
        name = "never-reached-for-agents"
        when = { source = "agent" }
        tier = "auto"
        reason = "shadowed by the first rule"
        "#,
    )
    .unwrap();
    let w = world();
    let s = spec(agent(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]));
    assert_eq!(decide(&s, &w, &p).rule, "agents-ask");
    let s = spec(human(), orbit(NEAR_CRISSY, 150.0, &["sky-1"]));
    assert_eq!(decide(&s, &w, &p).tier, Tier::Auto);
    let s = spec(human(), hold(&["sky-1"]));
    let d = decide(&s, &w, &p);
    assert_eq!((d.tier, d.rule.as_str()), (Tier::Human, "(none)"));
}

#[test]
fn policy_file_validation() {
    assert!(Policy::from_toml("").is_err(), "no tiers");
    let typo = r#"
        [[tier]]
        name = "x"
        when = { armd = true }
        tier = "auto"
        reason = "typo must not silently match everything"
    "#;
    assert!(Policy::from_toml(typo).is_err());
    let bad_tier = r#"
        [[tier]]
        name = "x"
        tier = "maybe"
        reason = "r"
    "#;
    assert!(Policy::from_toml(bad_tier).is_err());
    let p = policy();
    assert_eq!(p.approval.expire_s, 120);
    assert_eq!(p.tiers.len(), 8);
    assert_eq!(p.rules.space_rules(), SpaceRules::default());
    // the engagement tier: two approvers with engagement authority, never the proposer
    let eng = p.tiers.iter().find(|t| t.name == "engagement").unwrap();
    assert_eq!(
        (eng.approvers, eng.proposer_may_approve, eng.submitter_authorizes),
        (Some(2), Some(false), Some(false))
    );
    p.rules.engage_rules().validate().unwrap();
    let bad_engage = r#"
        [rules.engage]
        min_identity_confidence = 1.5
        [[tier]]
        name = "x"
        tier = "human"
        reason = "r"
    "#;
    assert!(Policy::from_toml(bad_engage).is_err());
    let zero_approvers = r#"
        [[tier]]
        name = "x"
        tier = "human"
        reason = "r"
        approvers = 0
    "#;
    assert!(Policy::from_toml(zero_approvers).is_err());
    let window_on_orbit = r#"
        [approval.kind.orbit]
        until = "window"
        [[tier]]
        name = "x"
        tier = "human"
        reason = "r"
    "#;
    assert!(Policy::from_toml(window_on_orbit).is_err());
    let bad_space = r#"
        [rules.space]
        cover_min_fraction = 1.5
        [[tier]]
        name = "x"
        tier = "human"
        reason = "r"
    "#;
    assert!(Policy::from_toml(bad_space).is_err());
}
