//! Satellite tasking: collect and cover intents on a fixed element-set fixture (no network,
//! no cache): murmur-space's `tests/data/gp_fixture.json`, CelesTrak GP data of 2026-10-02,
//! with the real `sim/space/constellations.toml`. Windows sit on the fixture's epoch, so the
//! time map is direct (no shift).
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, OnceLock};

use common::*;
use murmur_decide::replay::{replay_str, replay_str_with_space};
use murmur_decide::*;
use murmur_space::{Catalog as SpaceCatalog, Space};

const FIXTURE: &str = include_str!("../../murmur-space/tests/data/gp_fixture.json");
const CONSTELLATIONS: &str = include_str!("../../../sim/space/constellations.toml");

fn build_space(fixture: &str, catalog: &str) -> Space {
    let catalog = SpaceCatalog::parse(catalog).unwrap();
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, fixture.to_string()))
        .collect();
    Space::from_sources(catalog, &sources).unwrap()
}

fn space() -> Arc<Space> {
    static SPACE: OnceLock<Arc<Space>> = OnceLock::new();
    SPACE
        .get_or_init(|| Arc::new(build_space(FIXTURE, CONSTELLATIONS)))
        .clone()
}

fn segment() -> SpaceSegment {
    SpaceSegment::new(space()).unwrap()
}

fn ms(rfc3339: &str) -> Time {
    Time(
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .timestamp_millis(),
    )
}

/// 2026-10-02T14:00Z: request time; the window runs 12 h from here.
fn t0() -> Time {
    ms("2026-10-02T14:00:00Z")
}

const GOLDEN_GATE: LonLat = LonLat::new(-122.47, 37.825);

fn area() -> SpaceArea {
    SpaceArea::Circle {
        center: GOLDEN_GATE,
        radius_km: 5.0,
    }
}

fn window() -> Window {
    Window {
        from: t0(),
        to: t0().plus_ms(12 * 3600 * 1000),
    }
}

fn collect(chosen: Option<ChosenCollect>) -> Proposal {
    Proposal::space(Task::Collect {
        area: area(),
        window: window(),
        capability: CollectCapability::Any,
        min_resolution_m: None,
        product_deadline: None,
        chosen,
    })
}

fn cover(service: CoverService, min_level: Option<f64>) -> Proposal {
    Proposal::space(Task::Cover {
        area: area(),
        window: Window {
            from: t0(),
            to: t0().plus_ms(6 * 3600 * 1000),
        },
        service,
        min_level,
    })
}

/// The sim policy with a long approval deadline, so a test can move time hours ahead
/// before approving, and the submitter's own collects waiting for that approval.
fn patient_policy() -> Policy {
    let mut p = waiting_policy();
    p.approval.expire_s = 86400;
    p
}

struct Run {
    clock: ManualClock,
    engine: Engine,
    log: MemorySink,
}

fn run(policy: Policy, with_space: bool) -> Run {
    let clock = ManualClock::new(t0());
    let log = MemorySink::new();
    let mut engine = Engine::new(policy, Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    if with_space {
        engine.set_space(Box::new(segment())).unwrap();
    }
    Run { clock, engine, log }
}

impl Run {
    fn world(&self) -> WorldSnapshot {
        WorldSnapshot::new(self.clock.now())
    }

    fn advance_to(&self, t: Time) {
        self.clock.set(t);
    }
}

fn check(i: &Intent, rule: RuleId) -> &CheckResult {
    i.checks()
        .iter()
        .find(|c| c.rule == rule)
        .unwrap_or_else(|| panic!("no {rule:?} check in {:#?}", i.checks()))
}

fn assert_replays(log: &MemorySink) {
    let text = log.text();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    let seg = segment();
    let r = replay_str_with_space(&text, Some(&seg)).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert!(r.space_reproduced(), "{r:#?}");
    assert!(r.space_queries > 0);
}

/// The first ranked option of a fresh query at `t0`.
fn first_option(run: &mut Run) -> (IntentId, ChosenCollect) {
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let q = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    let opt = &q.space_answer().unwrap().tasking().unwrap().options[0];
    (q.id(), ChosenCollect::from_option(opt))
}

// ---- query --------------------------------------------------------------------------

#[test]
fn collect_without_a_choice_returns_ranked_options_and_exclusions() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let seg = run.engine.space_segment().unwrap().clone();
    let i = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();

    // A query, not an order: it ends rejected, with the options as evidence.
    assert_eq!(i.state(), State::Rejected);
    assert!(i.spec().uids.is_empty());
    let q = check(i, RuleId::SpaceQuery);
    assert_eq!(q.outcome, Outcome::Pass, "{}", q.detail);
    assert!(q.detail.contains("feasible options"), "{}", q.detail);
    assert!(!q.detail.contains("TIME-SHIFTED"), "{}", q.detail);
    let f = check(i, RuleId::Feasibility);
    assert_eq!(f.outcome, Outcome::Fail);
    assert!(f.detail.starts_with("no option chosen:"), "{}", f.detail);
    assert!(f.detail.contains("#1 "), "{}", f.detail);
    assert!(f.detail.contains("excluded:"), "{}", f.detail);
    // No vehicle rule ran.
    assert!(i.checks().iter().all(|c| c.uid.is_none()));

    let answer = i.space_answer().unwrap();
    let result = answer.tasking().unwrap();
    assert!(!result.options.is_empty());
    assert!(!result.exclusions.is_empty());
    assert_eq!(result.compute_ms, 0, "compute_ms is zeroed for the log");
    for w in result.options.windows(2) {
        assert!(w[0].rank < w[1].rank);
    }
    for o in &result.options {
        assert!(o.taskable);
        assert!(o.collect_time >= o.earliest_collect);
    }
    assert_eq!(answer.segment, seg);

    // The answer is logged once, with the time map and segment, and referenced by hash.
    let text = run.log.text();
    assert_eq!(text.matches("\"type\":\"space.loaded\"").count(), 1);
    assert_eq!(text.matches("\"type\":\"space.query\"").count(), 1);
    assert!(text.contains("\"time_map\""));
    assert!(text.contains("\"segment\""));
    assert_replays(&run.log);
}

#[test]
fn collect_fails_closed_without_a_space_segment() {
    let mut run = run(policy(), false);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let chosen = ChosenCollect {
        norad_id: 1,
        collect_time: t0().plus_ms(3 * 3600 * 1000),
    };
    let i = run
        .engine
        .submit(
            session.authority(),
            collect(Some(chosen)),
            &world,
            &NoTerrain,
        )
        .unwrap();
    assert_eq!(i.state(), State::Rejected);
    let q = check(i, RuleId::SpaceQuery);
    assert_eq!(q.outcome, Outcome::Fail);
    assert_eq!(q.detail, "cannot evaluate: no space segment loaded");
    assert_eq!(check(i, RuleId::Feasibility).outcome, Outcome::Fail);
    let r = replay_str(&run.log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
}

// ---- choose, approve, dispatch ------------------------------------------------------

#[test]
fn chosen_option_is_approved_by_a_human_and_dispatched() {
    let mut run = run(waiting_policy(), true);
    let (query, chosen) = first_option(&mut run);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let id = run
        .engine
        .amend_collect(session.authority(), query, chosen, &world, &NoTerrain)
        .unwrap()
        .id();
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert_eq!(i.policy().unwrap().rule, "satellite-collect");
    let f = check(i, RuleId::Feasibility);
    assert_eq!(f.outcome, Outcome::Pass, "{}", f.detail);
    assert!(f.detail.contains(&chosen.norad_id.to_string()));
    assert!(run.log.text().contains("\"action\":\"intent.amend\""));

    // A minute later the option still holds: approve re-checks (a second query) and passes.
    run.clock.advance_ms(60_000);
    let world = run.world();
    let i = run
        .engine
        .approve(id, session.authority(), None, &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::Approved);
    // The query and the amended collect asked the same question at the same time (one
    // answer, logged once); the approval asked again a minute later.
    assert_eq!(
        run.log.text().matches("\"type\":\"space.query\"").count(),
        2
    );

    // The vehicle dispatch path refuses a space intent and changes nothing.
    assert!(matches!(
        run.engine.dispatch(id),
        Err(EngineError::SpaceIntent(_))
    ));
    assert_eq!(run.engine.intent(id).unwrap().state(), State::Approved);

    let Order::Collect(order) = run.engine.dispatch_order(id).unwrap() else {
        panic!("not a collect order");
    };
    assert_eq!(order.norad_id, chosen.norad_id);
    assert_eq!(order.collect_time, chosen.collect_time);
    assert_eq!(
        order.resource,
        Uid::new(format!("norad:{}", chosen.norad_id))
    );
    let product = order.product_time.unwrap();
    assert!(product > order.collect_time);
    assert_eq!(run.engine.intent(id).unwrap().state(), State::Dispatched);

    // The bridge's simulated tasking reports back.
    for s in ["tasked", "collected", "product_ready"] {
        run.engine
            .record_op_state(id, &order.resource, s, "simulated")
            .unwrap();
    }
    run.engine.finish(id, TaskOutcome::Done).unwrap();
    assert_eq!(run.engine.intent(id).unwrap().state(), State::Done);
    assert!(run.log.text().contains("\"order\":\"collect\""));
    assert_replays(&run.log);
}

#[test]
fn option_gone_by_approval_time_is_voided() {
    let mut run = run(patient_policy(), true);
    let (_, chosen) = first_option(&mut run);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let id = run
        .engine
        .submit(
            session.authority(),
            collect(Some(chosen)),
            &world,
            &NoTerrain,
        )
        .unwrap()
        .id();
    assert_eq!(
        run.engine.intent(id).unwrap().state(),
        State::PendingApproval
    );

    // Nobody approved before the pass: time moves past it.
    run.advance_to(chosen.collect_time.plus_ms(60_000));
    let world = run.world();
    let err = run
        .engine
        .approve(id, session.authority(), None, &world, &NoTerrain)
        .unwrap_err();
    assert!(
        matches!(err, EngineError::ChecksFailedAtApproval(_)),
        "{err}"
    );
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Voided);
    let f = check(i, RuleId::Feasibility);
    assert_eq!(f.outcome, Outcome::Fail);
    assert!(f.detail.contains("the pass is over"), "{}", f.detail);
    assert!(matches!(
        run.engine.dispatch_order(id),
        Err(EngineError::Illegal { .. })
    ));
    assert_replays(&run.log);
}

#[test]
fn lead_time_lost_by_approval_time_is_voided() {
    let mut run = run(patient_policy(), true);
    let (_, chosen) = first_option(&mut run);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let id = run
        .engine
        .submit(
            session.authority(),
            collect(Some(chosen)),
            &world,
            &NoTerrain,
        )
        .unwrap()
        .id();

    // Still before the pass, but too late to task it (every class needs >= 60 min).
    run.advance_to(chosen.collect_time.plus_ms(-10 * 60_000));
    let world = run.world();
    assert!(
        run.engine
            .approve(id, session.authority(), None, &world, &NoTerrain)
            .is_err()
    );
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Voided);
    let f = check(i, RuleId::Feasibility);
    assert!(
        f.detail.contains("lead time not met") || f.detail.contains("no longer feasible"),
        "{}",
        f.detail
    );
    assert_replays(&run.log);
}

#[test]
fn deadline_is_enforced_for_a_chosen_option() {
    let mut run = run(policy(), true);
    let (_, chosen) = first_option(&mut run);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let Task::Collect {
        area,
        window,
        capability,
        ..
    } = collect(None).task
    else {
        unreachable!()
    };
    // Product due before the chosen collect even happens.
    let p = Proposal::space(Task::Collect {
        area,
        window,
        capability,
        min_resolution_m: None,
        product_deadline: Some(chosen.collect_time.plus_ms(-1)),
        chosen: Some(chosen),
    });
    let i = run
        .engine
        .submit(session.authority(), p, &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::Rejected);
    assert_eq!(check(i, RuleId::Feasibility).outcome, Outcome::Fail);
    assert_replays(&run.log);
}

// ---- agents -------------------------------------------------------------------------

#[test]
fn agents_propose_and_amend_collects_but_only_a_human_approves() {
    let mut run = run(policy(), true);
    let world = run.world();
    let planner = AgentId::new("planner");
    let mut port = run.engine.agent_port(planner.clone());
    let query = port
        .propose(
            collect(None),
            rationale("need eyes on the bridge"),
            &world,
            &NoTerrain,
        )
        .unwrap();
    assert!(query.spec().source.is_agent());
    let (qid, opt) = (
        query.id(),
        ChosenCollect::from_option(&query.space_answer().unwrap().tasking().unwrap().options[0]),
    );
    let id = run
        .engine
        .agent_port(planner)
        .amend_collect(qid, opt, rationale("earliest product"), &world, &NoTerrain)
        .unwrap()
        .id();
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.spec().source.is_agent());
    assert_eq!(i.policy().unwrap().rule, "satellite-collect");
    assert!(run.log.text().contains("\"type\":\"agent.action\""));

    // Another agent can't withdraw it; a human approves it.
    assert!(matches!(
        run.engine
            .agent_port(AgentId::new("other"))
            .withdraw(id, "mine now"),
        Err(EngineError::NotOwner { .. })
    ));
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let i = run
        .engine
        .approve(id, session.authority(), None, &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::Approved);
    assert!(!i.approval().unwrap().self_approved);
    assert_replays(&run.log);
}

// ---- cover --------------------------------------------------------------------------

#[test]
fn satcom_cover_runs_on_its_own_and_dispatches_a_coverage_watch() {
    let mut run = run(policy(), true);
    let world = run.world();
    let mut port = run.engine.agent_port(AgentId::new("planner"));
    let i = port
        .propose(
            cover(
                CoverService::Satcom {
                    class: "wgs".into(),
                },
                None,
            ),
            rationale("relay for the USV"),
            &world,
            &NoTerrain,
        )
        .unwrap();
    assert_eq!(i.state(), State::AutoApproved, "{:#?}", i.checks());
    assert_eq!(i.policy().unwrap().rule, "coverage-query");
    let c = check(i, RuleId::Coverage);
    assert_eq!(c.outcome, Outcome::Pass);
    assert!(c.detail.starts_with("wgs: covered 100.0%"), "{}", c.detail);
    let id = i.id();
    let Order::Cover(order) = run.engine.dispatch_order(id).unwrap() else {
        panic!("not a cover order");
    };
    assert_eq!(order.resource, Uid::new("satcom:wgs"));
    run.engine.finish(id, TaskOutcome::Done).unwrap();
    assert_replays(&run.log);
}

#[test]
fn gnss_cover_below_the_level_fails() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let gps = || CoverService::Gnss {
        class: "gps".into(),
    };
    let ok = run
        .engine
        .submit(session.authority(), cover(gps(), None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(ok.state(), State::AutoApproved, "{:#?}", ok.checks());
    let too_many = run
        .engine
        .submit(
            session.authority(),
            cover(gps(), Some(40.0)),
            &world,
            &NoTerrain,
        )
        .unwrap();
    assert_eq!(too_many.state(), State::Rejected);
    let c = check(too_many, RuleId::Coverage);
    assert!(c.detail.contains("needs 40"), "{}", c.detail);
    // A SATCOM cover naming a GNSS class fails closed.
    let wrong = run
        .engine
        .submit(
            session.authority(),
            cover(
                CoverService::Satcom {
                    class: "gps".into(),
                },
                None,
            ),
            &world,
            &NoTerrain,
        )
        .unwrap();
    assert_eq!(wrong.state(), State::Rejected);
    assert!(
        check(wrong, RuleId::SpaceQuery)
            .detail
            .contains("not a SATCOM class")
    );
    assert_replays(&run.log);
}

// ---- replay -------------------------------------------------------------------------

#[test]
fn replay_reports_a_different_cache_as_a_cache_mismatch() {
    let mut run = run(policy(), true);
    let (query, chosen) = first_option(&mut run);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    run.engine
        .amend_collect(session.authority(), query, chosen, &world, &NoTerrain)
        .unwrap();
    let text = run.log.text();

    // Element sets with one satellite dropped: a different cache.
    let mut sats: Vec<serde_json::Value> = serde_json::from_str(FIXTURE).unwrap();
    sats.retain(|s| s["NORAD_CAT_ID"].as_u64() != Some(chosen.norad_id));
    let other = build_space(&serde_json::to_string(&sats).unwrap(), CONSTELLATIONS);
    let other = SpaceSegment::new(Arc::new(other)).unwrap();
    assert_ne!(other.segment().sha256, segment().segment().sha256);

    let r = replay_str_with_space(&text, Some(&other)).unwrap();
    assert!(r.ok(), "decisions still reproduce: {:#?}", r.mismatches);
    assert_eq!(r.cache_mismatches.len(), r.space_queries);
    assert_eq!(r.space_recomputed, 0);
    assert!(r.cache_mismatches[0].what.starts_with("cache mismatch"));
    assert!(!r.space_reproduced());
}

/// A segment that claims the same identity but answers differently: a real mismatch.
struct Drifted(SpaceSegment);

impl SpaceOracle for Drifted {
    fn segment(&self) -> &SegmentId {
        self.0.segment()
    }
    fn answer(&self, q: &SpaceQuery) -> Result<SpaceAnswer, String> {
        let mut a = self.0.answer(q)?;
        if let murmur_decide::space::SpaceResult::Tasking(t) = &mut a.result {
            t.options.truncate(1);
        }
        Ok(a)
    }
}

#[test]
fn replay_on_the_same_cache_catches_a_different_answer() {
    let mut run = run(policy(), true);
    first_option(&mut run);
    let r = replay_str_with_space(&run.log.text(), Some(&Drifted(segment()))).unwrap();
    assert!(r.cache_mismatches.is_empty());
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "space answer differs on the same segment"),
        "{:#?}",
        r.mismatches
    );
}

#[test]
fn edited_space_answer_is_caught() {
    let mut run = run(policy(), true);
    first_option(&mut run);
    // Move the first option's collect time in the logged answer.
    let lines: Vec<String> = run
        .log
        .lines()
        .into_iter()
        .map(|l| {
            if l.contains("\"type\":\"space.query\"") {
                l.replacen(
                    "\"collect_time\":\"2026-10-02T",
                    "\"collect_time\":\"2026-10-03T",
                    1,
                )
            } else {
                l
            }
        })
        .collect();
    let r = replay_str(&(lines.join("\n") + "\n")).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what == "space answer hash does not match its content"),
        "{:#?}",
        r.mismatches
    );
}

// ---- the type model -----------------------------------------------------------------

#[test]
fn space_kinds_take_no_vehicles_and_vehicle_kinds_need_them() {
    let mut p = collect(None);
    p.uids = vec![uid("sky-1")];
    assert_eq!(
        p.validate(),
        Err(ProposalError::VehiclesOnSpaceTask(TaskKind::Collect))
    );
    assert_eq!(
        Proposal::new(Task::Hold {}, vec![]).validate(),
        Err(ProposalError::NoVehicles)
    );
    assert_eq!(Task::Hold {}.targets(), Targets::Vehicles);
    assert_eq!(collect(None).task.targets(), Targets::Space);

    // JSON from the API: uids may be omitted for a space kind.
    let p: Proposal = serde_json::from_str(&format!(
        r#"{{"kind":"collect","params":{{"area":{{"kind":"circle","center":[-122.47,37.825],"radius_km":5.0}},"window":{{"from":{},"to":{}}},"capability":"sar","chosen":{{"norad_id":57693,"collect_time":{}}}}}}}"#,
        t0().0,
        window().to.0,
        t0().0 + 3_600_000
    ))
    .unwrap();
    assert!(p.uids.is_empty());
    assert!(p.validate().is_ok());

    // A chosen time outside the window, or a backwards window, is a bad request.
    let mut bad = collect(Some(ChosenCollect {
        norad_id: 1,
        collect_time: window().to.plus_ms(1),
    }));
    assert!(matches!(bad.validate(), Err(ProposalError::BadParams(_))));
    if let Task::Collect { window, chosen, .. } = &mut bad.task {
        *chosen = None;
        window.to = window.from;
    }
    assert!(matches!(bad.validate(), Err(ProposalError::BadParams(_))));
}

#[test]
fn vehicle_decisions_log_exactly_as_before() {
    // No space field in any event of a vehicle-only run (the policy in run.started has
    // its [rules.space] settings).
    let clock = ManualClock::new(T0);
    let (mut engine, log) = engine(&clock);
    let session = engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    engine
        .submit(
            session.authority(),
            orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
            &world(),
            &terrain(),
        )
        .unwrap();
    let text = log.text();
    for line in text
        .lines()
        .filter(|l| !l.contains("\"type\":\"run.started\""))
    {
        assert!(!line.contains("\"space\""), "{line}");
    }
    assert!(replay_str(&text).unwrap().ok());
}

// ---- end to end: a cached segment, a JSONL log, the murmur-replay binary -------------

fn import_cache(dir: &std::path::Path, fixture: &str) {
    let catalog = SpaceCatalog::parse(CONSTELLATIONS).unwrap();
    let cache = murmur_space::cache::Cache::new(dir);
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-02T09:00:00Z")
        .unwrap()
        .to_utc();
    for q in catalog.queries() {
        cache.import(&q, fixture, at, "test fixture").unwrap();
    }
}

#[test]
fn june_scenario_is_time_shifted_and_murmur_replay_recomputes_from_the_cache() {
    let tmp = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("space-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (same, other) = (tmp.join("cache-same"), tmp.join("cache-other"));
    import_cache(&same, FIXTURE);
    let mut sats: Vec<serde_json::Value> = serde_json::from_str(FIXTURE).unwrap();
    sats.pop();
    import_cache(&other, &serde_json::to_string(&sats).unwrap());
    let catalog_path = sim_dir().join("space/constellations.toml");

    // Scenario day 14 June, 07:00 local; the fixture's elements are from October.
    let start = ms("2026-06-14T14:00:00Z");
    let clock = ManualClock::new(start);
    let log_path = tmp.join("audit.jsonl");
    let mut engine = Engine::new(
        waiting_policy(),
        Box::new(clock.clone()),
        Box::new(JsonlFile::open(&log_path).unwrap()),
    )
    .unwrap();
    let space = Space::load(&catalog_path, &same).unwrap();
    engine
        .set_space(Box::new(SpaceSegment::new(Arc::new(space)).unwrap()))
        .unwrap();
    let session = engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = WorldSnapshot::new(start);
    let mut p = collect(None);
    if let Task::Collect { window, .. } = &mut p.task {
        *window = Window {
            from: start,
            to: start.plus_ms(12 * 3600 * 1000),
        };
    }
    let q = engine
        .submit(session.authority(), p, &world, &NoTerrain)
        .unwrap();
    let detail = &check(q, RuleId::SpaceQuery).detail;
    assert!(detail.contains("TIME-SHIFTED +"), "{detail}");
    assert!(detail.contains("fetched 2026-10-02T09:00:00Z"), "{detail}");
    let answer = q.space_answer().unwrap();
    assert!(answer.time_map.shift_days > 100);
    assert!(!answer.segment.cache_meta.is_empty());
    let chosen = ChosenCollect::from_option(&answer.tasking().unwrap().options[0]);
    let qid = q.id();
    let id = engine
        .amend_collect(session.authority(), qid, chosen, &world, &NoTerrain)
        .unwrap()
        .id();
    clock.advance_ms(30_000);
    let world = WorldSnapshot::new(clock.now());
    engine
        .approve(id, session.authority(), None, &world, &NoTerrain)
        .unwrap();
    assert!(matches!(
        engine.dispatch_order(id).unwrap(),
        Order::Collect(_)
    ));
    drop(engine);

    let replay = |cache: Option<&std::path::Path>| {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_murmur-replay"));
        cmd.arg(&log_path);
        if let Some(cache) = cache {
            cmd.arg("--space").arg(&catalog_path).arg(cache);
        }
        let out = cmd.output().unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (code, out) = replay(None);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("not recomputed"), "{out}");
    let (code, out) = replay(Some(&same));
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("2 recomputed"), "{out}");
    let (code, out) = replay(Some(&other));
    assert_eq!(code, Some(3), "{out}");
    assert!(
        out.contains("OK: every decision reproduced exactly"),
        "{out}"
    );
    assert!(out.contains("CACHE MISMATCH"), "{out}");
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn an_operators_own_chosen_collect_is_approved_at_submission() {
    // The sim policy as shipped: the operator who commits a collect to an option is the
    // human the satellite-collect tier asks for.
    let mut run = run(policy(), true);
    let (query, chosen) = first_option(&mut run);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let id = run
        .engine
        .amend_collect(session.authority(), query, chosen, &world, &NoTerrain)
        .unwrap()
        .id();
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Approved);
    assert_eq!(i.policy().unwrap().rule, "satellite-collect");
    assert!(i.approval().unwrap().at_submission);
    assert!(matches!(
        run.engine.dispatch_order(id).unwrap(),
        Order::Collect(_)
    ));
    assert_replays(&run.log);
}
