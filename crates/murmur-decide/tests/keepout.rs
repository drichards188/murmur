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
//!   comes one step later, it does not;
//! - the windows of effect times (and, for a holding target, of distances) that pass: the
//!   platform placed so the effect lands exactly at a time inside one passes, at a time on the
//!   scan's grid outside every one (up to the latest listed) fails; "no position clears it"
//!   fails at every time tried. A moving target's effect time is set exactly by placing the
//!   platform ahead of it on its course.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::engage::{self, EngageWindow, Fallback};
use murmur_decide::keepout::{
    self, EffectWindow, Explanation, Item, MAX_WINDOWS, Options, SPAN_STEP_MS, Subject, WAIT_STEP_MS,
};
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
        velocity_sigma_mps: None,
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

fn opts() -> Options {
    Options::default()
}

/// A random picture around a target holding (or under way) west of the bridge: the platform
/// 0.3 to 3.5 km out, one to four protected tracks within 3 km and up to three vehicles of ours.
fn random_world(rng: &mut Rng) -> (WorldSnapshot, IntentSpec) {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    let (platform, kind) = if rng.chance(0.5) { ("viper-1", "magura") } else { ("blade-1", "switchblade") };
    let mut p = ours(kind, toward(TARGET_AT, rng.range(0.0, 360.0), rng.range(50.0, 3500.0)), T0);
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

/// Both keep-outs pass for `spec` on `w`.
fn both_pass(spec: &IntentSpec, w: &WorldSnapshot) -> bool {
    let (f, t) = dry(spec, w);
    passes(&f) && passes(&t)
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

/// The picture with the platform placed so the dry run's effect comes exactly at `at` (no
/// window opening later): a holding target, at cruise speed times the time from the start on
/// `bearing`; a moving one, that far ahead of where it will be then, on its course (it closes
/// on the platform, which meets it then and no sooner). None when rounding moved the effect.
fn platform_for(spec: &IntentSpec, w: &WorldSnapshot, at: Time, bearing: f64) -> Option<WorldSnapshot> {
    let ec = cfg().engage_rules();
    let pv = engage::preview(spec, w, &cfg()).unwrap();
    let reach = pv.cruise_mps * (at.ms_since(pv.start_t) as f64 / 1000.0);
    let target = &w.tracks[TARGET];
    let p = engage::target_at(target, at, &ec);
    let [e, n] = engage::target_velocity(target, &ec);
    let s = e.hypot(n);
    let place = if s < 1e-9 {
        toward(p, bearing, reach)
    } else {
        engage::offset(p, e / s * reach, n / s * reach)
    };
    let moved = platform_at(spec, w, place);
    (effect_t(spec, &moved) == at).then_some(moved)
}

/// Effect times on the scan's grid from `from` to `to`, at most `k` of them, spread.
fn some_grid_times(rng: &mut Rng, from: Time, to: Time, k: usize) -> Vec<Time> {
    let steps = (to.ms_since(from) / SPAN_STEP_MS).max(0);
    (0..k)
        .map(|_| from.plus_ms(((rng.next() % (steps as u64 + 1)) as i64) * SPAN_STEP_MS))
        .collect()
}

fn in_any(windows: &[EffectWindow], t: Time) -> bool {
    windows.iter().any(|w| w.from_t <= t && t <= w.until_t)
}

#[derive(Default, Debug)]
struct Counts {
    worlds: usize,
    explained: usize,
    blocked: usize,
    items: usize,
    item_waits: usize,
    item_fits: usize,
    item_later_windows: usize,
    item_no_window: usize,
    not_by_waiting: usize,
    waits: usize,
    moves: usize,
    later_only: usize,
    no_moves: usize,
    window_passes: usize,
    window_fails: usize,
    vice_versa: usize,
}

/// The windows' claims for one item (`item`: Some) or every keep-out (None), against a dry run
/// with the platform placed so the effect lands exactly at times inside and outside them.
#[allow(clippy::too_many_arguments)]
fn check_windows(
    spec: &IntentSpec,
    w: &WorldSnapshot,
    item: Option<&Item>,
    windows: &[EffectWindow],
    earliest: Time,
    latest: Time,
    rng: &mut Rng,
    n: &mut Counts,
) {
    let ok = |moved: &WorldSnapshot| match item {
        Some(i) => item_passes(spec, moved, i),
        None => both_pass(spec, moved),
    };
    let what = item.map_or("every keep-out".to_string(), |i| i.id.clone());
    for win in windows {
        for at in some_grid_times(rng, win.from_t, win.until_t, 2) {
            if let Some(moved) = platform_for(spec, w, at, rng.range(0.0, 360.0)) {
                n.window_passes += 1;
                assert!(ok(&moved), "{what}: an effect at {at:?} is in {win:?}");
            }
        }
        // a holding target: distances inside the band
        if let (Some(a), Some(b)) = (win.from_m, win.until_m) {
            let target = engage::target_at(&w.tracks[TARGET], T0, &cfg().engage_rules());
            let d = a + (b - a) * rng.f();
            let moved = platform_at(spec, w, toward(target, rng.range(0.0, 360.0), d));
            n.window_passes += 1;
            assert!(ok(&moved), "{what}: {d:.0} m out is inside {win:?}");
        }
    }
    // times outside every window, up to the last listed one when more may follow
    let upto = if windows.len() >= MAX_WINDOWS {
        windows.last().map_or(latest, |w| w.until_t)
    } else {
        latest
    };
    for at in some_grid_times(rng, earliest, upto, 3) {
        if in_any(windows, at) {
            continue;
        }
        if let Some(moved) = platform_for(spec, w, at, rng.range(0.0, 360.0)) {
            n.window_fails += 1;
            assert!(!ok(&moved), "{what}: an effect at {at:?} is outside {windows:?}");
        }
    }
}

/// Every claim of `ex` against the dry run.
fn check_claims(spec: &IntentSpec, w: &WorldSnapshot, ex: &Explanation, rng: &mut Rng, n: &mut Counts) {
    let (cfg, opts) = (cfg(), opts());
    // the checks it reports are the dry run's, and it blocks exactly where they fail
    let (kf, kt) = dry(spec, w);
    assert_eq!(ex.checks, vec![kf.clone(), kt.clone()]);
    assert_eq!(ex.blocked, !passes(&kf) || !passes(&kt));
    let first = |s: Subject| {
        let mut b: Vec<&Item> = ex.items.iter().filter(|i| i.blocks && i.subject == s).collect();
        b.sort_by(|a, b| a.id.cmp(&b.id));
        b.first().copied()
    };
    if ex.omitted == 0 {
        match first(Subject::Track) {
            Some(i) => assert!(kt.detail.starts_with(&format!("track {} ", i.label)), "{} / {}", kt.detail, i.label),
            None => assert!(passes(&kt), "{}", kt.detail),
        }
        match first(Subject::Vehicle) {
            Some(i) => assert_eq!(kf.uid.as_ref().map(|u| u.to_string()), Some(i.id.clone()), "{}", kf.detail),
            None => assert!(passes(&kf), "{}", kf.detail),
        }
        // the listed set is the checks' own: every track and vehicle alone
        for (id, _) in w.tracks.iter().filter(|(id, _)| *id != TARGET) {
            let listed = ex.items.iter().find(|i| i.subject == Subject::Track && i.id == *id);
            let probe = listed.cloned().unwrap_or_else(|| Item { subject: Subject::Track, id: id.clone(), ..ex_item() });
            assert_eq!(!item_passes(spec, w, &probe), listed.is_some_and(|i| i.blocks), "{id}");
        }
        for (u, _) in w.vehicles.iter().filter(|(u, _)| **u != spec.uids[0]) {
            let listed = ex.items.iter().find(|i| i.subject == Subject::Vehicle && i.id == u.to_string());
            let probe = listed.cloned().unwrap_or_else(|| Item { subject: Subject::Vehicle, id: u.to_string(), ..ex_item() });
            assert_eq!(!item_passes(spec, w, &probe), listed.is_some_and(|i| i.blocks), "{u}");
        }
    }
    let pv = engage::preview(spec, w, &cfg).unwrap();
    let mw = ex.move_within.as_ref();
    for item in ex.items.iter().filter(|i| i.blocks) {
        n.items += 1;
        assert!(!item_passes(spec, w, item), "{} blocks alone", item.id);
        if let Some(sh) = item.short_m {
            assert!(sh > 0.0 || item.region.is_none(), "{}: short {sh}", item.id);
        }
        // waiting: the nomination made then, on the picture carried forward
        if let Some(wait) = &item.wait {
            n.item_waits += 1;
            let at = |t: Time| (keepout::spec_at(spec, T0, t), keepout::project(w, spec, t, &cfg, &opts.memory));
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
        let Some(mw) = mw else { continue };
        match item.fits_until_t {
            Some(by) => {
                n.item_fits += 1;
                assert_eq!(item.clear_windows.first().map(|w| (w.from_t, w.until_t)), Some((mw.earliest_t, by)));
                let reach = pv.cruise_mps * (by.ms_since(pv.start_t) as f64 / 1000.0);
                let point = engage::target_at(&w.tracks[TARGET], by, &cfg.engage_rules());
                for f in [0.0, 0.5, 0.97] {
                    let moved = platform_at(spec, w, toward(point, rng.range(0.0, 360.0), f * reach));
                    assert!(effect_t(spec, &moved) <= by);
                    assert!(item_passes(spec, &moved, item), "{} fits until {by:?}, at {f}", item.id);
                }
                if !in_any(&item.clear_windows, by.plus_ms(SPAN_STEP_MS))
                    && by < mw.latest_t
                    && let Some(moved) = platform_for(spec, w, by.plus_ms(SPAN_STEP_MS), rng.range(0.0, 360.0))
                {
                    n.vice_versa += 1;
                    assert!(!item_passes(spec, &moved, item), "{}: one step later", item.id);
                }
            }
            None if item.clear_windows.is_empty() => n.item_no_window += 1,
            None => n.item_later_windows += 1,
        }
        check_windows(spec, w, Some(item), &item.clear_windows, mw.earliest_t, mw.latest_t, rng, n);
    }
    if !ex.blocked {
        assert!(ex.move_within.is_none() && ex.wait.is_none());
        return;
    }
    n.blocked += 1;
    let mw = ex.move_within.as_ref().unwrap();
    assert_eq!(mw.possible, !mw.windows.is_empty());
    if let (Some(by), Some(point), Some(within)) = (mw.effect_by_t, mw.point, mw.within_m) {
        n.moves += 1;
        assert_eq!(mw.windows[0].from_t, mw.earliest_t);
        for f in [0.0, 0.3, 0.6, 0.9, 0.99] {
            let moved = platform_at(spec, w, toward(point, rng.range(0.0, 360.0), f * within));
            assert!(effect_t(spec, &moved) <= by);
            let (f2, t2) = dry(spec, &moved);
            assert!(passes(&f2) && passes(&t2), "move within {within:.0} m at {f}: {} / {}", f2.detail, t2.detail);
        }
    } else if mw.possible {
        n.later_only += 1;
        assert!(mw.windows[0].from_t > mw.earliest_t && mw.why.contains("No closer position"), "{mw:?}");
    } else {
        n.no_moves += 1;
        assert!(mw.why.contains("No position"), "{}", mw.why);
    }
    check_windows(spec, w, None, &mw.windows, mw.earliest_t, mw.latest_t, rng, n);
    match &ex.wait {
        Some(wait) => {
            n.waits += 1;
            let s = keepout::spec_at(spec, T0, wait.nominate_t);
            let pw = keepout::project(w, spec, wait.nominate_t, &cfg, &opts.memory);
            let (f2, t2) = dry(&s, &pw);
            assert!(passes(&f2) && passes(&t2), "{}: {} / {}", wait.why, f2.detail, t2.detail);
            let before = wait.nominate_t.plus_ms(-WAIT_STEP_MS);
            let (f2, t2) = dry(
                &keepout::spec_at(spec, T0, before),
                &keepout::project(w, spec, before, &cfg, &opts.memory),
            );
            assert!(!passes(&f2) || !passes(&t2), "the first such time");
        }
        None => assert!(ex.wait_none.is_some()),
    }
}

/// A blank item, for probing a track or vehicle the explanation does not list.
fn ex_item() -> Item {
    Item {
        rule: RuleId::KeepOutTracks,
        subject: Subject::Track,
        id: String::new(),
        label: String::new(),
        domain: None,
        now: None,
        blocks: false,
        region: None,
        regions: Vec::new(),
        edge_m: None,
        short_m: None,
        altitude: None,
        clear_by_altitude: None,
        reason: String::new(),
        manoeuvring_now: None,
        fits_until_t: None,
        clear_windows: Vec::new(),
        wait: None,
        not_by_waiting: None,
        other: None,
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
        let Ok(ex) = keepout::explain(&spec, &w, &cfg(), &opts()) else {
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
    assert!(n.item_fits >= 30 && n.item_no_window >= 5 && n.moves >= 30 && n.no_moves >= 5, "{n:?}");
    assert!(n.later_only >= 1 && n.item_later_windows >= 1, "{n:?}");
    assert!(n.waits >= 20 && n.vice_versa >= 20 && n.window_passes >= 100 && n.window_fails >= 100, "{n:?}");
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
    // the words call our vehicles by their callsigns when told them
    let named = Options {
        names: [(uid("viper-1"), "VIPER 1".to_string())].into_iter().collect(),
        ..Options::default()
    };
    let ex = keepout::explain(&spec, &w, &cfg(), &named).unwrap();
    assert_eq!(ex.platform_name.as_deref(), Some("VIPER 1"));
    assert!(ex.move_within.as_ref().unwrap().why.contains("VIPER 1 within"), "{:?}", ex.move_within);
    assert!(!ex.move_within.as_ref().unwrap().why.contains("vehicle viper-1"));
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
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
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
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
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
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
    let item = ex.items.iter().find(|i| i.id == "T-0006").unwrap();
    assert!(item.blocks);
    assert_eq!(item.region.as_ref().unwrap().kind, "no_velocity");
    assert!(item.altitude.as_deref().unwrap().contains("no ceiling"), "{:?}", item.altitude);
    // rules 14: a velocity estimate would not clear an aircraft (it counts anywhere it could turn
    // or speed up to): the explanation says what would
    let other = item.other.as_deref().unwrap();
    assert!(other.contains("A velocity estimate would not clear it") && other.contains("altitude, ranged"), "{other}");
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(17), &mut n);
    // with a velocity, flying away at 50 m/s, it still blocks, by its aircraft reach, and says so
    let mut w2 = w.clone();
    let away = velocity(50.0, 45.0);
    if let Some(p) = w2.tracks.get_mut("T-0006") {
        p.velocity_mps = Some(away);
        p.mean_velocity_mps = Some(away);
        p.velocity_sigma_mps = Some(1.5);
    }
    let ex2 = keepout::explain(&spec, &w2, &cfg(), &opts()).unwrap();
    let item = ex2.items.iter().find(|i| i.id == "T-0006").unwrap();
    assert!(item.blocks);
    assert_eq!(item.region.as_ref().unwrap().kind, "aircraft");
    assert!(item.other.as_deref().unwrap().contains("A velocity estimate would not clear it"), "{:?}", item.other);
    check_claims(&spec, &w2, &ex2, &mut Rng(18), &mut n);
    // a surface contact without a velocity: a velocity estimate would make its reach a course
    let mut w3 = w.clone();
    if let Some(p) = w3.tracks.get_mut("T-0006") {
        p.domain = Domain::Surface;
        p.alt_m = None;
    }
    let ex3 = keepout::explain(&spec, &w3, &cfg(), &opts()).unwrap();
    let item = ex3.items.iter().find(|i| i.id == "T-0006").unwrap();
    if item.blocks && item.region.as_ref().unwrap().kind == "no_velocity" {
        assert!(item.other.as_deref().unwrap().contains("When the tracker has a velocity estimate"), "{:?}", item.other);
    }
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
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
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
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
    let took = started.elapsed();
    eprintln!("{} items, blocked {}, {took:?} (this build)", ex.items.len(), ex.blocked);
    assert!(ex.blocked);
    // L3: many in the way: at most MAX_ITEMS listed, blocking first, the rest counted
    for i in 0..20 {
        let t = contact(T0, toward(TARGET_AT, 18.0 * f64::from(i), 150.0), Some([0.0, 0.0]), Affiliation::Neutral, Domain::Surface);
        w.tracks.insert(format!("T-1{i:03}"), t);
    }
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
    assert_eq!(ex.items.len(), keepout::MAX_ITEMS);
    assert!(ex.blocking >= 20 && ex.omitted > 0 && ex.items.iter().all(|i| i.blocks), "{} {}", ex.blocking, ex.omitted);
    assert!(serde_json::to_string(&ex).unwrap().len() < 60_000);
    // an effect radius above the policy's maximum is not explained (nor run as one)
    let mut huge = spec.clone();
    if let Task::Engage { effect_radius_m, .. } = &mut huge.task {
        *effect_radius_m = 1e6;
    }
    let e = keepout::explain(&huge, &w, &cfg(), &opts()).unwrap_err();
    assert!(e.contains("outside the policy's"), "{e}");
}

/// M1 (review of 2f4f46a): a ferry 60 m north of the holding target, moving away north at
/// 6 m/s, and the Magura 100 m south. Before, the scan stopped at the current effect time and
/// said no position clears it; a farther platform (a later effect) does: the ferry has moved
/// off by then.
#[test]
fn a_farther_platform_clears_what_no_closer_one_does_and_the_explanation_says_so() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    let ferry = contact(T0, toward(TARGET_AT, 0.0, 60.0), Some(velocity(6.0, 0.0)), Affiliation::Neutral, Domain::Surface);
    w.tracks.insert("T-0002".into(), ferry);
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 100.0), T0));
    let spec = nomination("viper-1");
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
    assert!(ex.blocked);
    let mw = ex.move_within.as_ref().unwrap();
    assert!(mw.possible && mw.effect_by_t.is_none() && mw.holding, "{mw:?}");
    assert!(mw.why.contains("No closer position") && mw.why.contains("Farther"), "{}", mw.why);
    let band = &mw.windows[0];
    let (lo, hi) = (band.from_m.unwrap(), band.until_m.unwrap());
    assert!(lo < 600.0 && 600.0 < hi, "{lo} {hi}");
    // the platform 600 m out: both keep-outs pass
    let out = platform_at(&spec, &w, toward(TARGET_AT, 180.0, 600.0));
    assert!(both_pass(&spec, &out));
    // the item itself says when it is clear
    let item = ex.items.iter().find(|i| i.id == "T-0002").unwrap();
    assert!(item.fits_until_t.is_none() && !item.clear_windows.is_empty(), "{item:?}");
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(19), &mut n);
    assert!(n.later_only == 1 && n.window_passes > 0 && n.window_fails > 0, "{n:?}");
}

/// L1: a window that opens later than the intercept pushes the effect to its opening: the
/// windows start there, and "within" still holds (any platform close enough waits for it).
#[test]
fn a_window_opening_later_starts_the_windows_at_its_opening() {
    let (w, mut spec) = tour_boat_world(1_800.0);
    let opens = T0.plus_ms(240_000);
    if let Task::Engage { window: Some(win), .. } = &mut spec.task {
        win.from = Some(opens);
    }
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
    if let Some(mw) = &ex.move_within {
        assert_eq!(mw.earliest_t, opens);
        assert!(mw.why.contains("the window opens then") || mw.effect_by_t.is_some(), "{}", mw.why);
        let mut n = Counts::default();
        check_claims(&spec, &w, &ex, &mut Rng(23), &mut n);
    }
}

/// L2: a track out of view: no wait hint; what happens is said plainly (the tracker drops a
/// track unseen for 180 s, and a dropped track no longer counts). A track that flickered
/// (unseen for 7 s, under the tracker's 10 s stale limit) is still carried forward.
#[test]
fn a_track_out_of_view_says_when_the_tracker_drops_it_and_a_flicker_is_carried_forward() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    let unseen = contact(Time(T0.0 - 40_000), toward(TARGET_AT, 90.0, 500.0), Some([0.3, 0.0]), Affiliation::Neutral, Domain::Surface);
    w.tracks.insert("T-0005".into(), unseen);
    let flicker = contact(Time(T0.0 - 7_000), toward(TARGET_AT, 0.0, 400.0), Some(velocity(5.0, 0.0)), Affiliation::Neutral, Domain::Surface);
    w.tracks.insert("T-0006".into(), flicker);
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 1_200.0), T0));
    let spec = nomination("viper-1");
    let ex = keepout::explain(&spec, &w, &cfg(), &opts()).unwrap();
    let out = ex.items.iter().find(|i| i.id == "T-0005").unwrap();
    assert!(out.blocks && out.wait.is_none());
    let said = out.not_by_waiting.as_deref().unwrap();
    assert!(said.contains("Not seen for 40 s") && said.contains("drops a track unseen for 180 s") && said.contains("no longer counts"), "{said}");
    assert!(!said.contains("does not clear"), "{said}");
    assert!(ex.wait.is_none() && ex.wait_none.as_deref().unwrap().contains("T-0005"), "{:?}", ex.wait_none);
    if let Some(f) = ex.items.iter().find(|i| i.id == "T-0006" && i.blocks) {
        assert!(!f.not_by_waiting.as_deref().unwrap_or("").contains("Not seen"), "{f:?}");
    }
    let mut n = Counts::default();
    check_claims(&spec, &w, &ex, &mut Rng(29), &mut n);
}

/// L4: one of ours reporting a circle that does not hold it (its centre 2 km away) is carried
/// along its velocity, as the check predicts it; one its circle holds stays on it.
#[test]
fn a_vehicle_is_carried_forward_as_the_check_predicts_it() {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    w.tracks.insert(TARGET.into(), target(TARGET_AT, [0.0, 0.0]));
    w.vehicles.insert(uid("viper-1"), ours("magura", toward(TARGET_AT, 180.0, 1_200.0), T0));
    let at = toward(TARGET_AT, 90.0, 3_000.0);
    let circle = |center| {
        Some(Fact::new(
            Circle {
                center,
                radius_m: 150.0,
                source: Some(CircleSource::OrbitTask),
                center_t: None,
            },
            T0,
        ))
    };
    let mut off = ours("seahunter", at, T0);
    off.velocity_mps = Some(Fact::new([0.0, 5.0], T0));
    off.circling = circle(toward(at, 0.0, 2_000.0));
    let mut on = off.clone();
    on.circling = circle(toward(at, 0.0, 100.0));
    w.vehicles.insert(uid("off-1"), off);
    w.vehicles.insert(uid("on-1"), on);
    let spec = nomination("viper-1");
    let then = keepout::project(&w, &spec, T0.plus_ms(60_000), &cfg(), &opts().memory);
    let moved = |u: &str| murmur_decide::geo::haversine_m(at, then.vehicles[&uid(u)].position.as_ref().unwrap().value);
    assert!((moved("off-1") - 300.0).abs() < 1.0, "{}", moved("off-1"));
    assert!(moved("on-1") < 1.0, "{}", moved("on-1"));
}
