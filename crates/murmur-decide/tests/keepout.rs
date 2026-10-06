//! The keep-out explanation (murmur-12e, `murmur_decide::keepout`): read-only, built on the
//! functions the checks read, and every hint it gives holds when a dry run tries it. On seeded
//! random pictures (tracks that manoeuvre, are young, have no velocity, are out of view, fly
//! with or without a ranged altitude; vehicles of ours on a course, circling, without a
//! velocity, lost), each claim is checked against the dry run itself (`rules::evaluate`):
//!
//! - the keep-out checks it reports are the dry run's, and its blocking items are exactly what
//!   they fail on (the first in id order is the one a check names);
//! - "clears for a nomination at W": a dry run of the nomination made at W, on the picture
//!   carried forward to W ([`keepout::project`]), passes that keep-out for that item, and one
//!   second earlier it does not; the same for every keep-out at once;
//! - "fits until T" and "move within D": the platform placed within the stated distance (so
//!   the effect comes no later than T) passes; where the target holds, placed so the effect
//!   comes one step later, it does not.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::engage::{self, EngageWindow, Fallback};
use murmur_decide::keepout::{self, Explanation, Item, Subject, TrackerMemory, SPAN_STEP_MS, WAIT_STEP_MS};
use murmur_decide::world::NavSource;
use murmur_decide::*;

const TARGET_AT: LonLat = LonLat::new(-122.4950, 37.8120);
const TARGET: &str = "T-0001";

// ---- a seeded generator ----------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.f()
    }

    fn chance(&mut self, p: f64) -> bool {
        self.f() < p
    }
}

/// `m` metres from `p` on `bearing_deg`.
fn toward(p: LonLat, bearing_deg: f64, m: f64) -> LonLat {
    let b = bearing_deg.to_radians();
    engage::offset(p, m * b.sin(), m * b.cos())
}

fn secs(s: f64) -> i64 {
    (s * 1000.0).round() as i64
}

fn velocity(speed: f64, heading_deg: f64) -> [f64; 2] {
    let h = heading_deg.to_radians();
    [speed * h.sin(), speed * h.cos()]
}

/// One of ours, healthy, every fact fresh at `t`, reporting every second.
fn ours(platform: &str, at: LonLat, t: Time) -> VehicleFacts {
    let mut v = vehicle(platform, at);
    v.autopilot = v.autopilot.map(|f| Fact::new(f.value, t));
    v.position = Some(Fact::new(at, t));
    v.nav_ready = Some(Fact::new(true, t));
    v.nav_source = Some(Fact::new(NavSource::Gnss, t));
    v.gps_fix = Some(Fact::new(3, t));
    v.gps_glitch = Some(Fact::new(false, t));
    v.battery_pct = Some(Fact::new(95.0, t));
    v.velocity_mps = Some(Fact::new([0.0, 0.0], t));
    v.report_interval_ms = Some(1000);
    v
}

/// A protected track seen at `t` (AIS), steady and long followed, unless changed.
fn contact(t: Time, at: LonLat, vel: Option<[f64; 2]>, aff: Affiliation, domain: Domain) -> TrackFacts {
    TrackFacts {
        position: Fact::new(at, t),
        alt_m: None,
        alt_t: None,
        velocity_mps: vel,
        pos_sigma_m: 15.0,
        sigma_growth_mps: Some(1.0),
        confidence: 0.9,
        affiliation: Fact::new(aff, t),
        affiliation_confidence: 0.8,
        identity: Some(if domain == Domain::Air { "light_aircraft" } else { "ferry" }.into()),
        identity_confidence: 0.9,
        sources: vec!["9:ais".into()],
        source_t: [("9:ais".to_string(), t)].into_iter().collect(),
        domain,
        last_seen_t: t,
        declared_by: None,
        declaration: None,
        review_flags: Vec::new(),
        manoeuvre: None,
        mean_velocity_mps: vel,
        first_seen_t: Some(Time(t.0 - 900_000)),
    }
}

/// The declared target: a small fast boat at `at`, holding or under way.
fn target(at: LonLat, vel: [f64; 2]) -> TrackFacts {
    let mut t = contact(T0, at, Some(vel), Affiliation::Hostile, Domain::Surface);
    t.identity = Some("small_fast_boat".into());
    t.declared_by = Some("op-1".into());
    t.pos_sigma_m = 10.0;
    t
}

/// A nomination of `platform` on the target, effect radius 75 m, ten minutes' window.
fn nomination(platform: &str) -> IntentSpec {
    IntentSpec {
        id: IntentId(0),
        created_t: T0,
        source: Source::Human {
            id: OperatorId::new("op-1"),
        },
        task: Task::Engage {
            track: TARGET.into(),
            effect_radius_m: 75.0,
            window: Some(EngageWindow {
                from: None,
                until: T0.plus_ms(600_000),
            }),
            fallback: Fallback::Return,
        },
        uids: vec![uid(platform)],
        constraints: Constraints::default(),
        rationale: None,
    }
}

fn cfg() -> RulesConfig {
    policy().rules
}

fn mem() -> TrackerMemory {
    TrackerMemory::default()
}

/// A random picture around a target holding (or under way) west of the bridge: the platform
/// 0.3 to 3.5 km out, one to four protected tracks within 3 km and up to three vehicles of ours.
fn random_world(rng: &mut Rng) -> (WorldSnapshot, IntentSpec) {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let (platform, kind) = if rng.chance(0.5) { ("viper-1", "magura") } else { ("blade-1", "switchblade") };
    let mut p = ours(kind, toward(TARGET_AT, rng.range(0.0, 360.0), rng.range(300.0, 3500.0)), T0);
    if kind == "switchblade" {
        p.autopilot = Some(Fact::new(AutopilotKind::Plane, T0));
    }
    w.vehicles.insert(uid(platform), p);
    let tv = if rng.chance(0.6) {
        velocity(rng.range(0.0, 1.2), rng.range(0.0, 360.0))
    } else {
        velocity(rng.range(2.0, 6.0), rng.range(0.0, 360.0))
    };
    w.tracks.insert(TARGET.into(), target(TARGET_AT, tv));
    for i in 0..(1 + (rng.next() % 4) as usize) {
        let id = format!("T-{:04}", i + 2);
        let air = rng.chance(0.2);
        let domain = if air { Domain::Air } else { Domain::Surface };
        let at = toward(TARGET_AT, rng.range(0.0, 360.0), rng.range(100.0, 3000.0));
        let seen = if rng.chance(0.9) {
            Time(T0.0 - secs(rng.range(0.0, 3.0)))
        } else {
            Time(T0.0 - secs(rng.range(8.0, 40.0)))
        };
        let vel = if rng.chance(0.1) {
            None
        } else if air {
            Some(velocity(rng.range(30.0, 70.0), rng.range(0.0, 360.0)))
        } else {
            Some(velocity(rng.range(0.0, 10.0), rng.range(0.0, 360.0)))
        };
        let aff = [Affiliation::Neutral, Affiliation::Unknown, Affiliation::Suspect][(rng.next() % 3) as usize];
        let mut t = contact(seen, at, vel, aff, domain);
        t.pos_sigma_m = rng.range(5.0, 40.0);
        let speed = vel.map_or(0.0, |[e, n]| e.hypot(n));
        if vel.is_some() && rng.chance(0.4) {
            t.manoeuvre = Some(Manoeuvre {
                // within the tracker's memory, as it reports one
                t: Time(seen.0 - secs(rng.range(0.0, 299.0))),
                change_mps: rng.range(2.0, 8.0),
                speed_mps: speed + rng.range(0.0, 4.0),
            });
        }
        if vel.is_some() && rng.chance(0.4) {
            // young: no average velocity yet (or, now and then, a caller that says nothing)
            t.mean_velocity_mps = None;
            t.first_seen_t = rng.chance(0.8).then(|| Time(seen.0 - secs(rng.range(2.0, 44.0))));
        } else if let Some([e, n]) = vel {
            t.mean_velocity_mps = Some([e + rng.range(-1.0, 1.0), n + rng.range(-1.0, 1.0)]);
        }
        if air && rng.chance(0.5) {
            t.alt_m = Some(rng.range(100.0, 3000.0));
            t.alt_t = Some(seen);
            t.sources.push("4:radar".into());
            t.source_t.insert("4:radar".into(), seen);
        }
        if rng.chance(0.1) {
            // another declared hostile: exempt like the target
            t.affiliation = Fact::new(Affiliation::Hostile, seen);
            t.declared_by = Some("op-2".into());
        }
        w.tracks.insert(id, t);
    }
    for (i, kind) in ["x10", "seahunter", "x10"].iter().enumerate().take((rng.next() % 4) as usize) {
        let at = toward(TARGET_AT, rng.range(0.0, 360.0), rng.range(500.0, 6000.0));
        let mut v = ours(kind, at, T0);
        let cruise = v.platform.as_ref().and_then(|p| p.cruise_mps).unwrap_or(10.0);
        let r = rng.f();
        if r < 0.55 {
            v.velocity_mps = Some(Fact::new(velocity(rng.range(0.0, cruise), rng.range(0.0, 360.0)), T0));
        } else if r < 0.7 {
            v.velocity_mps = None;
        } else if r < 0.85 {
            // lost: its last report 90 s old
            let then = Time(T0.0 - 90_000);
            v.position = Some(Fact::new(at, then));
            v.velocity_mps = Some(Fact::new(velocity(rng.range(0.0, cruise), 0.0), then));
        } else {
            v.circling = Some(Fact::new(
                Circle {
                    center: toward(at, rng.range(0.0, 360.0), 100.0),
                    radius_m: 150.0,
                    source: Some(CircleSource::OrbitTask),
                    center_t: None,
                },
                T0,
            ));
            v.velocity_mps = Some(Fact::new(velocity(cruise, rng.range(0.0, 360.0)), T0));
        }
        if *kind == "x10" {
            v.alt_msl_m = Some(Fact::new(rng.range(20.0, 150.0), T0));
        }
        w.vehicles.insert(uid(&format!("ours-{i}")), v);
    }
    (w, nomination(platform))
}

// ---- the dry run -----------------------------------------------------------------------------

fn outcome(checks: &[CheckResult], rule: RuleId) -> &CheckResult {
    checks.iter().find(|c| c.rule == rule).unwrap()
}

/// The dry run's keep-out checks for `spec` on `w`: (friendly, tracks).
fn dry(spec: &IntentSpec, w: &WorldSnapshot) -> (CheckResult, CheckResult) {
    let checks = rules::evaluate(spec, w, &cfg());
    (
        outcome(&checks, RuleId::KeepOutFriendly).clone(),
        outcome(&checks, RuleId::KeepOutTracks).clone(),
    )
}

fn passes(c: &CheckResult) -> bool {
    c.outcome == Outcome::Pass
}

/// The picture with only `item` among the tracks (and the target), or among ours (and the
/// platform), and whether its keep-out check passes there.
fn item_passes(spec: &IntentSpec, w: &WorldSnapshot, item: &Item) -> bool {
    let mut w = w.clone();
    let platform = spec.uids[0].clone();
    match item.subject {
        Subject::Track => w.tracks.retain(|id, _| id == TARGET || *id == item.id),
        Subject::Vehicle => w.vehicles.retain(|u, _| *u == platform || u.to_string() == item.id),
    }
    let (f, t) = dry(spec, &w);
    passes(match item.subject {
        Subject::Track => &t,
        Subject::Vehicle => &f,
    })
}

/// The picture with the platform moved to `at` (fresh).
fn platform_at(spec: &IntentSpec, w: &WorldSnapshot, at: LonLat) -> WorldSnapshot {
    let mut w = w.clone();
    if let Some(p) = w.vehicles.get_mut(&spec.uids[0])
        && let Some(f) = p.position.as_mut()
    {
        f.value = at;
    }
    w
}

/// The effect time a dry run would plan for `spec` on `w`.
fn effect_t(spec: &IntentSpec, w: &WorldSnapshot) -> Time {
    engage::preview(spec, w, &cfg()).unwrap().effect_t
}

fn holding(w: &WorldSnapshot) -> bool {
    engage::target_velocity(&w.tracks[TARGET], &cfg().engage_rules()) == [0.0, 0.0]
}

/// Where to put the platform for its effect to come exactly at `at` (the target holding: at
/// cruise speed times the time from its start), on `bearing`.
fn platform_for(spec: &IntentSpec, w: &WorldSnapshot, ex: &Explanation, at: Time, bearing: f64) -> WorldSnapshot {
    let pv = engage::preview(spec, w, &cfg()).unwrap();
    let d = pv.cruise_mps * (at.ms_since(pv.start_t) as f64 / 1000.0);
    platform_at(spec, w, toward(ex.effect_point, bearing, d))
}

#[derive(Default, Debug)]
struct Counts {
    worlds: usize,
    explained: usize,
    blocked: usize,
    items: usize,
    item_waits: usize,
    item_fits: usize,
    item_no_fit: usize,
    not_by_waiting: usize,
    waits: usize,
    moves: usize,
    no_moves: usize,
    vice_versa: usize,
}

/// Every claim of `ex` against the dry run.
fn check_claims(spec: &IntentSpec, w: &WorldSnapshot, ex: &Explanation, rng: &mut Rng, n: &mut Counts) {
    let (cfg, mem) = (cfg(), mem());
    // the checks it reports are the dry run's, and it blocks exactly where they fail
    let (kf, kt) = dry(spec, w);
    assert_eq!(ex.checks, vec![kf.clone(), kt.clone()]);
    assert_eq!(ex.blocked, !passes(&kf) || !passes(&kt));
    let first = |s: Subject| {
        let mut b: Vec<&Item> = ex.items.iter().filter(|i| i.blocks && i.subject == s).collect();
        b.sort_by(|a, b| a.id.cmp(&b.id));
        b.first().copied()
    };
    match first(Subject::Track) {
        Some(i) => assert!(kt.detail.starts_with(&format!("track {} ", i.label)), "{} / {}", kt.detail, i.label),
        None => assert!(passes(&kt), "{}", kt.detail),
    }
    match first(Subject::Vehicle) {
        Some(i) => assert_eq!(kf.uid.as_ref().map(|u| u.to_string()), Some(i.id.clone()), "{}", kf.detail),
        None => assert!(passes(&kf), "{}", kf.detail),
    }
    let pv = engage::preview(spec, w, &cfg).unwrap();
    for item in ex.items.iter().filter(|i| i.blocks) {
        n.items += 1;
        assert!(!item_passes(spec, w, item), "{} blocks alone", item.id);
        if let Some(sh) = item.short_m {
            assert!(sh > 0.0 || item.region.is_none(), "{}: short {sh}", item.id);
        }
        // waiting: the nomination made then, on the picture carried forward
        if let Some(wait) = &item.wait {
            n.item_waits += 1;
            let at = |t: Time| (keepout::spec_at(spec, T0, t), keepout::project(w, spec, t, &cfg, &mem));
            let (s, pw) = at(wait.nominate_t);
            assert!(item_passes(&s, &pw, item), "{}: {}", item.id, wait.why);
            assert_eq!(effect_t(&s, &pw), wait.effect_t);
            let (s, pw) = at(wait.nominate_t.plus_ms(-WAIT_STEP_MS));
            assert!(!item_passes(&s, &pw, item), "{}: the first such time: {}", item.id, wait.why);
        } else {
            assert!(item.not_by_waiting.is_some(), "{}", item.id);
            n.not_by_waiting += 1;
        }
        // a shorter span: within the platform's reach of the effect by then
        match item.fits_until_t {
            Some(by) => {
                n.item_fits += 1;
                let reach = pv.cruise_mps * (by.ms_since(pv.start_t) as f64 / 1000.0);
                let point = engage::target_at(&w.tracks[TARGET], by, &cfg.engage_rules());
                for f in [0.0, 0.5, 0.97] {
                    let moved = platform_at(spec, w, toward(point, rng.range(0.0, 360.0), f * reach));
                    assert!(effect_t(spec, &moved) <= by);
                    assert!(item_passes(spec, &moved, item), "{} fits until {by:?}, at {f}", item.id);
                }
                if holding(w) && by < pv.effect_t {
                    let next = by.plus_ms(SPAN_STEP_MS);
                    let moved = platform_for(spec, w, ex, next, rng.range(0.0, 360.0));
                    if effect_t(spec, &moved) == next {
                        n.vice_versa += 1;
                        assert!(!item_passes(spec, &moved, item), "{}: one step later", item.id);
                    }
                }
            }
            None => {
                n.item_no_fit += 1;
                let moved = platform_at(spec, w, engage::target_at(&w.tracks[TARGET], pv.start_t, &cfg.engage_rules()));
                assert!(!item_passes(spec, &moved, item), "{}: not even the earliest", item.id);
            }
        }
    }
    if !ex.blocked {
        assert!(ex.move_within.is_none() && ex.wait.is_none());
        return;
    }
    n.blocked += 1;
    let mw = ex.move_within.as_ref().unwrap();
    if mw.possible {
        n.moves += 1;
        let (by, point, within) = (mw.effect_by_t.unwrap(), mw.point.unwrap(), mw.within_m.unwrap());
        for f in [0.0, 0.3, 0.6, 0.9, 0.99] {
            let moved = platform_at(spec, w, toward(point, rng.range(0.0, 360.0), f * within));
            assert!(effect_t(spec, &moved) <= by);
            let (f2, t2) = dry(spec, &moved);
            assert!(passes(&f2) && passes(&t2), "move within {within:.0} m at {f}: {} / {}", f2.detail, t2.detail);
        }
        if holding(w) {
            let next = by.plus_ms(SPAN_STEP_MS);
            let moved = platform_for(spec, w, ex, next, rng.range(0.0, 360.0));
            if effect_t(spec, &moved) == next {
                n.vice_versa += 1;
                let (f2, t2) = dry(spec, &moved);
                assert!(!passes(&f2) || !passes(&t2), "one step farther");
            }
        }
    } else {
        n.no_moves += 1;
        let moved = platform_at(spec, w, engage::target_at(&w.tracks[TARGET], pv.start_t, &cfg.engage_rules()));
        let (f2, t2) = dry(spec, &moved);
        assert!(!passes(&f2) || !passes(&t2), "{}", mw.why);
    }
    match &ex.wait {
        Some(wait) => {
            n.waits += 1;
            let s = keepout::spec_at(spec, T0, wait.nominate_t);
            let pw = keepout::project(w, spec, wait.nominate_t, &cfg, &mem);
            let (f2, t2) = dry(&s, &pw);
            assert!(passes(&f2) && passes(&t2), "{}: {} / {}", wait.why, f2.detail, t2.detail);
            let before = wait.nominate_t.plus_ms(-WAIT_STEP_MS);
            let (f2, t2) = dry(
                &keepout::spec_at(spec, T0, before),
                &keepout::project(w, spec, before, &cfg, &mem),
            );
            assert!(!passes(&f2) || !passes(&t2), "the first such time");
        }
        None => assert!(ex.wait_none.is_some()),
    }
}

#[test]
fn every_claim_of_the_explanation_holds_in_a_dry_run() {
    let mut n = Counts::default();
    // MURMUR_KO_SEED and MURMUR_KO_WORLDS run it longer or on other pictures
    let env = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<u64>().ok());
    let mut rng = Rng(env("MURMUR_KO_SEED").unwrap_or(0x6b6f_2d31_3265));
    for _ in 0..env("MURMUR_KO_WORLDS").unwrap_or(240) {
        n.worlds += 1;
        let (w, spec) = random_world(&mut rng);
        let before = w.clone();
        let Ok(ex) = keepout::explain(&spec, &w, &cfg(), &mem()) else {
            continue;
        };
        assert_eq!(w, before, "the explanation changes nothing");
        n.explained += 1;
        check_claims(&spec, &w, &ex, &mut rng, &mut n);
    }
    eprintln!("{n:?}");
    // the generator reaches every kind of claim
    assert!(n.explained >= 200, "{n:?}");
    assert!(n.blocked >= 60 && n.item_waits >= 30 && n.not_by_waiting >= 10, "{n:?}");
    assert!(n.item_fits >= 30 && n.item_no_fit >= 5 && n.moves >= 30 && n.no_moves >= 5, "{n:?}");
    assert!(n.waits >= 20 && n.vice_versa >= 20, "{n:?}");
}

// ---- the cases measured live (murmur-12e) ---------------------------------------------------

/// The engagement demo's picture: the target holding west of the bridge, a harbor tour boat
/// circling 500 m around a point 600 m away (6 m/s, the tracker sees it manoeuvre every few
/// seconds), and our Magura-class USV `dist_m` out.
fn tour_boat_world(dist_m: f64) -> (WorldSnapshot, IntentSpec) {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.2, -0.1]));
    let boat_at = toward(TARGET_AT, 300.0, 700.0);
    let mut boat = contact(T0, boat_at, Some(velocity(6.0, 30.0)), Affiliation::Neutral, Domain::Surface);
    boat.manoeuvre = Some(Manoeuvre {
        t: Time(T0.0 - 3_000),
        change_mps: 8.4,
        speed_mps: 7.1,
    });
    boat.mean_velocity_mps = Some(velocity(1.5, 120.0));
    w.tracks.insert("T-0003".into(), boat);
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 80.0, dist_m), T0));
    (w, nomination("viper-1"))
}

#[test]
fn a_circling_tour_boat_blocks_by_its_manoeuvre_reach_and_the_explanation_says_how_close() {
    let (w, spec) = tour_boat_world(1_800.0);
    let ex = keepout::explain(&spec, &w, &cfg(), &mem()).unwrap();
    assert!(ex.blocked);
    let boat = ex.items.iter().find(|i| i.id == "T-0003").unwrap();
    assert!(boat.blocks && boat.manoeuvring_now == Some(true), "{boat:?}");
    let region = boat.region.as_ref().unwrap();
    assert_eq!(region.kind, "manoeuvre");
    assert!(boat.short_m.unwrap() > 0.0);
    assert!(boat.reason.contains("manoeuvre") && boat.reason.contains("short by"), "{}", boat.reason);
    // waiting helps only if it stops manoeuvring (then it would move off on its course, or
    // its manoeuvre would leave the tracker's five-minute memory): the hint says so
    let wait = boat.wait.as_ref().unwrap();
    assert!(wait.why.contains("stops manoeuvring"), "{}", wait.why);
    assert!(wait.nominate_t > T0 && wait.nominate_t.ms_since(T0) <= 300_000, "{wait:?}");
    // closer: within the stated distance every keep-out holds; at 1.8 km none would
    let mw = ex.move_within.as_ref().unwrap();
    assert!(mw.possible && mw.holding, "{mw:?}");
    let within = mw.within_m.unwrap();
    assert!(within < 1_000.0 && mw.now_m.unwrap() > 1_700.0, "{mw:?}");
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(7), &mut n);
    // following the hint: the platform moved within it, the nomination's keep-outs pass
    let (w2, _) = tour_boat_world(within * 0.9);
    let (f, t) = dry(&spec, &w2);
    assert!(passes(&f) && passes(&t), "{} / {}", f.detail, t.detail);
    eprintln!("{}", serde_json::to_string_pretty(&ex).unwrap());
}

#[test]
fn a_young_track_clears_once_the_tracker_has_followed_it_long_enough() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    // a ferry first seen 10 s ago, 1.2 km off, on a course away from the effect point
    let mut young = contact(T0, toward(TARGET_AT, 0.0, 1_200.0), Some(velocity(8.0, 0.0)), Affiliation::Neutral, Domain::Surface);
    young.mean_velocity_mps = None;
    young.first_seen_t = Some(Time(T0.0 - 10_000));
    w.tracks.insert("T-0004".into(), young);
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 900.0), T0));
    let spec = nomination("viper-1");
    let ex = keepout::explain(&spec, &w, &cfg(), &mem()).unwrap();
    let item = ex.items.iter().find(|i| i.id == "T-0004").unwrap();
    assert!(item.blocks);
    assert_eq!(item.region.as_ref().unwrap().kind, "young");
    assert!(item.reason.contains("Tracked for only 10 s"), "{}", item.reason);
    let wait = item.wait.as_ref().unwrap();
    // 45 s of history: 35 s from now
    assert_eq!(wait.nominate_t, T0.plus_ms(35_000), "{wait:?}");
    assert!(wait.why.contains("long enough for an average velocity"), "{}", wait.why);
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(11), &mut n);
}

#[test]
fn a_lost_friendly_does_not_clear_by_waiting_and_the_explanation_says_what_would() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 900.0), T0));
    let mut lost = ours("seahunter", toward(TARGET_AT, 90.0, 1_500.0), T0);
    lost.position = Some(Fact::new(toward(TARGET_AT, 90.0, 1_500.0), Time(T0.0 - 120_000)));
    w.vehicles.insert(uid("hunter-1"), lost);
    let spec = nomination("viper-1");
    let ex = keepout::explain(&spec, &w, &cfg(), &mem()).unwrap();
    let item = ex.items.iter().find(|i| i.id == "hunter-1").unwrap();
    assert!(item.blocks && item.subject == Subject::Vehicle);
    assert_eq!(item.region.as_ref().unwrap().kind, "lost");
    assert!(item.wait.is_none() && item.not_by_waiting.as_deref().unwrap().contains("lost"));
    assert!(item.other.as_deref().unwrap().contains("accepting its unknown position"), "{:?}", item.other);
    assert!(ex.wait.is_none() && ex.wait_none.as_deref().unwrap().contains("hunter-1"), "{:?}", ex.wait_none);
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(13), &mut n);
}

#[test]
fn an_aircraft_without_a_ranged_altitude_blocks_with_no_ceiling_and_says_so() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    // a light aircraft at 450 m: EO/IR only, no ranged altitude; no velocity estimate yet
    let mut plane = contact(T0, toward(TARGET_AT, 45.0, 2_500.0), None, Affiliation::Neutral, Domain::Air);
    plane.alt_m = Some(450.0);
    plane.sources = vec!["1:eo_ir".into()];
    plane.source_t = [("1:eo_ir".to_string(), T0)].into_iter().collect();
    w.tracks.insert("T-0006".into(), plane);
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 1_200.0), T0));
    let spec = nomination("viper-1");
    let ex = keepout::explain(&spec, &w, &cfg(), &mem()).unwrap();
    let item = ex.items.iter().find(|i| i.id == "T-0006").unwrap();
    assert!(item.blocks);
    assert_eq!(item.region.as_ref().unwrap().kind, "no_velocity");
    assert!(item.altitude.as_deref().unwrap().contains("no ceiling"), "{:?}", item.altitude);
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(17), &mut n);
}

#[test]
fn a_passing_nomination_lists_what_nearly_blocks_and_gives_no_hints() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    // a fishing boat holding 450 m off: 2-sigma about 60 m, clear of the 275 m clearance
    let near = contact(T0, toward(TARGET_AT, 270.0, 450.0), Some([0.0, 0.0]), Affiliation::Neutral, Domain::Surface);
    w.tracks.insert("T-0007".into(), near);
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 300.0), T0));
    let spec = nomination("viper-1");
    let ex = keepout::explain(&spec, &w, &cfg(), &mem()).unwrap();
    assert!(!ex.blocked && ex.move_within.is_none() && ex.wait.is_none());
    let item = ex.items.iter().find(|i| i.id == "T-0007").unwrap();
    assert!(!item.blocks && item.short_m.unwrap() < 0.0, "{item:?}");
    assert!(item.reason.contains("more than the 275 m needed"), "{}", item.reason);
    assert!(item.wait.is_none() && item.fits_until_t.is_none());
}

/// The explanation's cost on a scale picture: 100 vehicles of ours and 30 tracks around.
#[test]
fn the_explanation_stays_cheap_on_a_large_picture() {
    let mut rng = Rng(99);
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    for i in 0..30 {
        let mut t = contact(
            T0,
            toward(TARGET_AT, rng.range(0.0, 360.0), rng.range(400.0, 8_000.0)),
            Some(velocity(rng.range(0.0, 10.0), rng.range(0.0, 360.0))),
            Affiliation::Neutral,
            Domain::Surface,
        );
        if i % 3 == 0 {
            t.manoeuvre = Some(Manoeuvre {
                t: Time(T0.0 - 20_000),
                change_mps: 4.0,
                speed_mps: 8.0,
            });
        }
        w.tracks.insert(format!("T-{:04}", i + 2), t);
    }
    for i in 0..100 {
        let mut v = ours("seahunter", toward(TARGET_AT, rng.range(0.0, 360.0), rng.range(3_000.0, 20_000.0)), T0);
        v.velocity_mps = Some(Fact::new(velocity(rng.range(0.0, 6.0), rng.range(0.0, 360.0)), T0));
        w.vehicles.insert(uid(&format!("ours-{i}")), v);
    }
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 2_500.0), T0));
    let spec = nomination("viper-1");
    let started = std::time::Instant::now();
    let ex = keepout::explain(&spec, &w, &cfg(), &mem()).unwrap();
    let took = started.elapsed();
    eprintln!("{} items, blocked {}, {took:?} (this build)", ex.items.len(), ex.blocked);
    assert!(ex.blocked);
}
