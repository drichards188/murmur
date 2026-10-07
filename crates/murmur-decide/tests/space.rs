//! Satellite tasking: collect and cover intents on a fixed element-set fixture (no network,
//! no cache): murmur-space's `tests/data/gp_fixture.json`, CelesTrak GP data of 2026-10-02,
//! with the real `sim/space/constellations.toml`. Windows sit on the fixture's epoch, so the
//! time map is direct (no shift).
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, OnceLock};

use common::*;
use murmur_decide::replay::{replay_str, replay_str_with_space};
use murmur_decide::space::from_utc;
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
    // the fixed wait, not the collect's tasking-lead deadline
    p.approval.kind.clear();
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
    let mut engine = Engine::new(policy, roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
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

/// A cover's answer, prepared outside the engine as the bridge does before it takes its
/// lock: the engine computes no cover itself (rules 11; it would hold the decision lock for
/// seconds), so a test hands it one.
fn prime(run: &mut Run, p: &Proposal) {
    let rules = run.engine.policy().rules.clone();
    let prepared = prepare_space(&segment(), &p.task, run.clock.now(), &rules).unwrap();
    run.engine.use_prepared_space(prepared);
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
        run.engine.dispatch(id, &world),
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

/// A forged log's mismatches (its chain recomputed).
fn forged_whats(text: &str, edit: impl FnOnce(&mut Vec<Event>)) -> Vec<String> {
    let r = replay_str(&forge(text, edit)).unwrap();
    r.mismatches.into_iter().map(|m| m.what).collect()
}

fn edit_order(ev: &mut [Event], f: impl Fn(&mut Option<SpaceOrder>)) {
    for e in ev {
        if let audit::EventBody::IntentDispatched { space, .. } = &mut e.body {
            f(space);
        }
    }
}

fn collect_order(o: &mut Option<SpaceOrder>) -> &mut CollectOrder {
    match o {
        Some(SpaceOrder::Collect(c)) => c,
        other => panic!("not a collect order: {other:?}"),
    }
}

#[test]
fn a_dispatched_collect_is_the_chosen_option_as_its_last_answer_has_it() {
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
    run.engine
        .approve(id, session.authority(), None, &world, &NoTerrain)
        .unwrap();
    let Order::Collect(order) = run.engine.dispatch_order(id).unwrap() else {
        panic!("not a collect order");
    };
    // reports go under the satellite the order names, nothing else
    let other = Uid::new(format!("norad:{}", chosen.norad_id + 1));
    assert!(matches!(
        run.engine.record_op_state(id, &other, "tasked", "simulated"),
        Err(EngineError::NotItsOrder { .. })
    ));
    run.engine
        .record_op_state(id, &order.resource, "tasked", "simulated")
        .unwrap();
    run.engine.finish(id, TaskOutcome::Done).unwrap();
    assert_replays(&run.log);
    let honest = run.log.text();
    const DIFFERS: &str =
        "dispatched space order differs from the one its approved intent and last answer give";
    type Edit = fn(&mut Option<SpaceOrder>);
    let cases: [(Edit, &str); 6] = [
        (|o| collect_order(o).satellite = "ANOTHER SAT".into(), DIFFERS),
        (
            |o| {
                let c = collect_order(o);
                c.product_time = c.product_time.map(|t| t.plus_ms(3_600_000));
            },
            DIFFERS,
        ),
        (|o| collect_order(o).answer_sha256 = "0".repeat(64), DIFFERS),
        (|o| collect_order(o).resolution_m = Some(0.1), DIFFERS),
        (
            |o| {
                let c = collect_order(o);
                c.norad_id += 1;
                c.resource = Uid::new(format!("norad:{}", c.norad_id));
            },
            "dispatched collect is not the chosen option",
        ),
        (|o| *o = None, "space intent dispatched without its order"),
    ];
    for (edit, want) in cases {
        let w = forged_whats(&honest, |ev| edit_order(ev, edit));
        assert!(w.iter().any(|m| m == want), "{want:?} not in {w:?}");
    }
    // a report under another satellite
    let w = forged_whats(&honest, |ev| {
        for e in ev {
            if let audit::EventBody::OpState { uid, .. } = &mut e.body {
                *uid = other.clone();
            }
        }
    });
    assert!(
        w.iter()
            .any(|m| m == "op state for a vehicle or resource the intent was not dispatched to"),
        "{w:?}"
    );
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

/// An agent's chosen collect, pending, its deadline under `[approval.kind.collect] until =
/// "tasking_lead"` (the sim policy) and its scenario map.
fn agent_collect(run: &mut Run) -> (IntentId, Time, Time) {
    let world = run.world();
    let planner = AgentId::new("planner");
    let mut port = run.engine.agent_port(planner.clone());
    let q = port
        .propose(collect(None), rationale("eyes on"), &world, &NoTerrain)
        .unwrap();
    let result = q.space_answer().unwrap().tasking().unwrap();
    let opt = result.options[0].clone();
    let lead = from_utc(opt.earliest_collect).ms_since(from_utc(result.request.now));
    let chosen = ChosenCollect::from_option(&opt);
    let qid = q.id();
    let mut port = run.engine.agent_port(planner);
    let i = port
        .amend_collect(qid, chosen, rationale("first"), &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    (
        i.id(),
        i.expires_t().unwrap(),
        chosen.collect_time.plus_ms(-lead),
    )
}

#[test]
fn a_pending_collect_expires_at_the_tasking_lead_time_not_after_120_s() {
    let mut run = run(policy(), true);
    let (id, expires, last_tasking) = agent_collect(&mut run);
    // identity scenario map: the deadline is the last moment the pass can be tasked
    assert_eq!(expires, last_tasking);
    assert!(
        expires.ms_since(run.clock.now()) > 120_000,
        "longer than the fixed wait"
    );
    let text = run.log.text();
    assert!(
        text.contains("\"until\":{\"until\":\"tasking_lead\""),
        "{text}"
    );

    // still pending after the generic 120 s
    run.advance_to(run.clock.now().plus_ms(121_000));
    assert!(run.engine.expire_due().unwrap().is_empty());
    // expires at the deadline
    run.advance_to(expires.plus_ms(-1));
    assert!(run.engine.expire_due().unwrap().is_empty());
    run.advance_to(expires);
    assert_eq!(run.engine.expire_due().unwrap(), vec![id]);
    assert_eq!(run.engine.intent(id).unwrap().state(), State::Expired);
    assert_replays(&run.log);

    // a kind without its own setting keeps the fixed wait; tampering with the deadline is
    // caught
    let tampered = text.replacen(
        &format!("\"expires_t\":{}", expires.0),
        &format!("\"expires_t\":{}", expires.0 + 1),
        1,
    );
    assert_ne!(tampered, text);
    let r = replay_str(&tampered).unwrap();
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.what.contains("approval deadline")),
        "{:#?}",
        r.mismatches
    );
}

#[test]
fn the_tasking_deadline_follows_a_fast_scenario_clock() {
    // scenario runs 60 times faster than the engine clock, starting at t0
    let engine_start = Time(1_000_000_000);
    let map = ScenarioMap {
        engine_origin: engine_start,
        scenario_origin: t0(),
        rate: 60.0,
    };
    let clock = ManualClock::new(engine_start);
    let log = MemorySink::new();
    let mut engine = Engine::new(policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    engine
        .set_space(Box::new(segment().with_scenario_map(map).unwrap()))
        .unwrap();
    let mut run = Run { clock, engine, log };
    let (id, expires, last_tasking) = agent_collect(&mut run);
    assert_eq!(expires, map.engine(last_tasking));
    assert!(map.scenario(expires) >= last_tasking);
    assert!(map.scenario(expires.plus_ms(-1)) < last_tasking);
    // the wait in engine time is the scenario wait / 60
    let wait = expires.ms_since(engine_start);
    let scen_wait = last_tasking.ms_since(t0());
    assert!((wait - scen_wait / 60).abs() <= 1, "{wait} vs {scen_wait}");
    let text = run.log.text();
    assert!(
        text.contains("\"rate\":60.0"),
        "space.loaded carries the map"
    );
    run.advance_to(expires);
    assert_eq!(run.engine.expire_due().unwrap(), vec![id]);
    assert_replays(&run.log);
}

#[test]
fn scenario_map_round_trips_at_any_rate() {
    for rate in [1.0, 0.5, 3.0, 60.0, 1000.0, 7.3] {
        let m = ScenarioMap {
            engine_origin: Time(1_700_000_000_000),
            scenario_origin: Time(1_780_000_000_000),
            rate,
        };
        for d in [0_i64, 1, 999, 3_600_000, 86_400_000] {
            let s = m.scenario_origin.plus_ms(d);
            let t = m.engine(s);
            assert!(m.scenario(t) >= s, "rate {rate} d {d}");
            assert!(m.scenario(t.plus_ms(-1)) < s, "rate {rate} d {d}");
        }
    }
    assert!(
        ScenarioMap {
            rate: 0.0,
            ..ScenarioMap::IDENTITY
        }
        .validate()
        .is_err()
    );
}

#[test]
fn approval_kind_settings_are_validated() {
    let base = std::fs::read_to_string(sim_dir().join("policy.toml")).unwrap();
    let bad = base.replace(
        "[approval.kind.collect]\nuntil = \"tasking_lead\"",
        "[approval.kind.orbit]\nuntil = \"tasking_lead\"",
    );
    assert_ne!(bad, base);
    assert!(Policy::from_toml(&bad).is_err());
    let fixed = base.replace(
        "[approval.kind.collect]\nuntil = \"tasking_lead\"",
        "[approval.kind.collect]\nexpire_s = 600",
    );
    let p = Policy::from_toml(&fixed).unwrap();
    assert_eq!(p.approval.kind[&TaskKind::Collect].expire_s, Some(600));
}

// ---- cover --------------------------------------------------------------------------

#[test]
fn satcom_cover_runs_on_its_own_and_dispatches_a_coverage_watch() {
    let mut run = run(policy(), true);
    let world = run.world();
    let c = cover(
        CoverService::Satcom {
            class: "wgs".into(),
        },
        None,
    );
    prime(&mut run, &c);
    let mut port = run.engine.agent_port(AgentId::new("planner"));
    let i = port
        .propose(
            c,
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
    // the watch handed on is the approved service's
    let w = forged_whats(&run.log.text(), |ev| {
        edit_order(ev, |o| {
            if let Some(SpaceOrder::Cover(c)) = o {
                c.resource = Uid::new("satcom:mil");
            }
        })
    });
    assert!(
        w.iter().any(|m| m
            == "dispatched space order differs from the one its approved intent and last answer give"),
        "{w:?}"
    );
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
    prime(&mut run, &cover(gps(), None));
    let ok = run
        .engine
        .submit(session.authority(), cover(gps(), None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(ok.state(), State::AutoApproved, "{:#?}", ok.checks());
    prime(&mut run, &cover(gps(), Some(40.0)));
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
    prime(
        &mut run,
        &cover(
            CoverService::Satcom {
                class: "gps".into(),
            },
            None,
        ),
    );
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
        roster(),
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

/// Counts the questions asked of a segment.
struct Counting(SpaceSegment, Arc<std::sync::atomic::AtomicUsize>);

impl SpaceOracle for Counting {
    fn segment(&self) -> &SegmentId {
        self.0.segment()
    }
    fn answer(&self, q: &SpaceQuery) -> Result<SpaceAnswer, String> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.0.answer(q)
    }
}

#[test]
fn a_space_answer_computed_outside_the_engine_is_used_while_fresh() {
    use std::sync::atomic::Ordering::SeqCst;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut run = run(policy(), false);
    run.engine
        .set_space(Box::new(Counting(segment(), calls.clone())))
        .unwrap();
    let outside = Counting(segment(), calls.clone());
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let task = collect(None).task;
    // computed outside (the bridge, before its lock), used by a check 2 s later: the engine
    // doesn't ask again
    let prepared = prepare_space(&outside, &task, t0(), &policy().rules).unwrap();
    assert_eq!(calls.load(SeqCst), 1);
    run.engine.use_prepared_space(prepared);
    run.advance_to(t0().plus_ms(2_000));
    let world = run.world();
    let i = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(check(i, RuleId::SpaceQuery).outcome, Outcome::Pass);
    assert_eq!(calls.load(SeqCst), 1);
    // one computed 10 s before the check is stale: the engine asks itself
    let prepared = prepare_space(&outside, &task, t0().plus_ms(2_000), &policy().rules).unwrap();
    run.engine.use_prepared_space(prepared);
    run.advance_to(t0().plus_ms(12_000));
    let world = run.world();
    run.engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(calls.load(SeqCst), 3);
    assert_replays(&run.log);
}

// ---- rules 11: covers over the area, and the follow-ups after dispatch ------------------

use murmur_decide::audit::CheckPhase;
use murmur_decide::replay::{ReplayOptions, RulesAccepted, replay_str_with};
use murmur_decide::space::{
    CANCELLED_OP, COVER_BOUNDARY_SAMPLES, MISSED_OP, NO_LONGER_FEASIBLE, RECHECKED_OP,
    STILL_FEASIBLE, SpaceResult, cover_points,
};

/// The fixture with satellite `norad` left out: a later segment on which its passes are
/// gone (what a reload of the element sets can do).
fn space_without(norad: u64) -> Arc<Space> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(FIXTURE).unwrap();
    let kept: Vec<serde_json::Value> = raw
        .into_iter()
        .filter(|r| r["NORAD_CAT_ID"].as_u64() != Some(norad))
        .collect();
    Arc::new(build_space(
        &serde_json::to_string(&kept).unwrap(),
        CONSTELLATIONS,
    ))
}

fn wgs() -> CoverService {
    CoverService::Satcom {
        class: "wgs".into(),
    }
}

/// An operator's chosen collect under the sim policy (approved at submission), dispatched
/// and reported tasked.
fn dispatched_collect(run: &mut Run, session: &OperatorSession) -> (IntentId, ChosenCollect, CollectOrder) {
    let (query, chosen) = first_option(run);
    let world = run.world();
    let id = run
        .engine
        .amend_collect(session.authority(), query, chosen, &world, &NoTerrain)
        .unwrap()
        .id();
    assert_eq!(run.engine.intent(id).unwrap().state(), State::Approved);
    let Order::Collect(order) = run.engine.dispatch_order(id).unwrap() else {
        panic!("not a collect order");
    };
    run.engine
        .record_op_state(id, &order.resource, "tasked", "simulated tasking request accepted")
        .unwrap();
    (id, chosen, order)
}

fn whats(r: &murmur_decide::replay::ReplayReport) -> Vec<String> {
    r.mismatches.iter().map(|m| m.what.clone()).collect()
}

/// Rewrite every reference to a logged space answer after `edit` changed it (a forger who
/// keeps the log consistent with itself).
fn edit_answer(ev: &mut [Event], edit: impl Fn(&mut SpaceAnswer)) {
    let mut rehash: Vec<(String, String)> = Vec::new();
    for e in ev.iter_mut() {
        if let audit::EventBody::SpaceQuery { sha256, answer } = &mut e.body {
            edit(answer);
            let new = answer.content_hash().unwrap();
            rehash.push((sha256.clone(), new.clone()));
            *sha256 = new;
        }
    }
    for e in ev.iter_mut() {
        if let audit::EventBody::CheckResult { inputs, .. } | audit::EventBody::PolicyDecision { inputs, .. } = &mut e.body
            && let LoggedSpace::Answered { sha256, .. } = &mut inputs.space
            && let Some((_, new)) = rehash.iter().find(|(old, _)| old == sha256)
        {
            *sha256 = new.clone();
        }
    }
}

#[test]
fn a_cover_is_evaluated_at_sample_points_over_its_area() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    prime(&mut run, &cover(wgs(), None));
    let i = run
        .engine
        .submit(session.authority(), cover(wgs(), None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::AutoApproved, "{:#?}", i.checks());
    // a circle: the centroid and eight points around it, each answered on its own
    let a = i.space_answer().unwrap();
    let SpaceQuery::CoverLinks(q) = &a.query else {
        panic!("not an area query: {:?}", a.query);
    };
    assert_eq!(q.points.len(), 1 + COVER_BOUNDARY_SAMPLES);
    assert_eq!(q.points, cover_points(&area()));
    assert_eq!((q.points[0].lat_deg, q.points[0].lon_deg), (GOLDEN_GATE.lat, GOLDEN_GATE.lon));
    let SpaceResult::CoverLinks(r) = &a.result else {
        panic!("not an area answer");
    };
    assert_eq!(r.points.len(), q.points.len());
    assert!(r.points.iter().all(|p| p.links.class_id == "wgs"));
    let c = check(i, RuleId::Coverage);
    assert_eq!(c.outcome, Outcome::Pass);
    assert!(
        c.detail.contains("at the least covered of 9 sample points over the area ("),
        "{}",
        c.detail
    );
    assert!(c.detail.contains("needs 99.0% at every point"), "{}", c.detail);
    let id = i.id();
    // a polygon: the centroid and its vertices
    let ring = vec![
        LonLat::new(-122.55, 37.78),
        LonLat::new(-122.40, 37.78),
        LonLat::new(-122.40, 37.88),
        LonLat::new(-122.55, 37.88),
    ];
    let polygon = SpaceArea::Polygon { ring: ring.clone() };
    let points = cover_points(&polygon);
    assert_eq!(points.len(), 5);
    assert_eq!((points[1].lon_deg, points[1].lat_deg), (ring[0].lon, ring[0].lat));
    let many = SpaceArea::Polygon {
        ring: (0..24).map(|k| LonLat::new(-122.5 + 0.004 * f64::from(k), 37.8 + 0.001 * f64::from(k % 3))).collect(),
    };
    assert_eq!(cover_points(&many).len(), 1 + COVER_BOUNDARY_SAMPLES, "thinned to the sample count");
    let poly = Proposal::space(Task::Cover {
        area: polygon,
        window: Window {
            from: t0(),
            to: t0().plus_ms(3_600_000),
        },
        service: wgs(),
        min_level: None,
    });
    prime(&mut run, &poly);
    let p = run
        .engine
        .submit(session.authority(), poly, &world, &NoTerrain)
        .unwrap();
    assert!(check(p, RuleId::Coverage).detail.contains("of 5 sample points"), "{}", check(p, RuleId::Coverage).detail);
    // the watch handed on carries the area; its answer the points
    let Order::Cover(order) = run.engine.dispatch_order(id).unwrap() else {
        panic!("not a cover order");
    };
    assert_eq!(order.resource, Uid::new("satcom:wgs"));
    run.engine.finish(id, TaskOutcome::Done).unwrap();
    assert!(run.log.text().contains("\"query\":\"cover_links\""));
    assert_replays(&run.log);
}

#[test]
fn before_rules_11_a_cover_is_evaluated_at_the_centroid() {
    let mut p = policy();
    p.rules.rules_version = PASSKEY_SINCE;
    let mut run = run(p, true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    prime(&mut run, &cover(wgs(), None));
    let i = run
        .engine
        .submit(session.authority(), cover(wgs(), None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::AutoApproved, "{:#?}", i.checks());
    assert!(matches!(i.space_answer().unwrap().query, SpaceQuery::Links(_)));
    let c = check(i, RuleId::Coverage);
    assert!(c.detail.starts_with("wgs: covered 100.0% of the window (needs 99.0%)"), "{}", c.detail);
    let text = run.log.text();
    assert!(!text.contains("cover_links"));
    let r = replay_str_with(
        &text,
        &ReplayOptions {
            rules: RulesAccepted::Exactly(PASSKEY_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(r.rules_version, PASSKEY_SINCE);
}

#[test]
fn a_cover_verdict_that_does_not_match_the_area_evaluation_is_caught() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let gps = CoverService::Gnss {
        class: "gps".into(),
    };
    // the fewest satellites in view at the worst sample point decide: ask for more than that
    let c = cover(gps, Some(40.0));
    prime(&mut run, &c);
    let i = run
        .engine
        .submit(session.authority(), c, &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::Rejected);
    let c = check(i, RuleId::Coverage);
    assert!(c.detail.contains("at the worst of 9 sample points over the area ("), "{}", c.detail);
    let SpaceResult::CoverGnss(g) = &i.space_answer().unwrap().result else {
        panic!("not an area answer");
    };
    let worst = g.worst().unwrap().point;
    assert_eq!(g.points.len(), 9);
    assert_replays(&run.log);
    let honest = run.log.text();
    // the verdict edited to pass: the check is recomputed from the logged points
    let w = forged_whats(&honest, |ev| {
        for e in ev {
            if let audit::EventBody::CheckResult { results, passed, .. } = &mut e.body {
                for r in results.iter_mut().filter(|r| r.rule == RuleId::Coverage) {
                    r.outcome = Outcome::Pass;
                }
                *passed = true;
            }
        }
    });
    assert!(w.iter().any(|m| m == "check results differ"), "{w:?}");
    // the worst point dropped from the logged answer, every hash recomputed: the answer no
    // longer answers this intent's question (the point set is the area's)
    let forged = forge(&honest, |ev| {
        edit_answer(ev, |a| {
            if let SpaceQuery::CoverGnss(q) = &mut a.query {
                q.points.retain(|p| *p != worst);
            }
            if let SpaceResult::CoverGnss(c) = &mut a.result {
                c.points.retain(|p| p.point != worst);
            }
        });
    });
    let r = replay_str(&forged).unwrap();
    assert!(whats(&r).iter().any(|m| m == "check results differ"), "{:?}", whats(&r));
    assert!(
        r.mismatches
            .iter()
            .any(|m| m.recomputed.contains("the space answer is not for this intent's query")),
        "{:#?}",
        r.mismatches
    );
    // the worst point's own figure raised: the recomputed verdict passes where the logged
    // one failed
    let w = forged_whats(&honest, |ev| {
        edit_answer(ev, |a| {
            if let SpaceResult::CoverGnss(c) = &mut a.result {
                for p in &mut c.points {
                    p.gnss.visible_min = 40;
                }
            }
        });
    });
    assert!(w.iter().any(|m| m == "check results differ"), "{w:?}");
}

#[test]
fn an_operator_cancels_a_dispatched_collect_before_its_pass() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-2"))
        .unwrap();
    let (id, chosen, order) = dispatched_collect(&mut run, &session);
    // the cancel's report is the engine's: the bridge can't write it
    let err = run
        .engine
        .record_op_state(id, &order.resource, CANCELLED_OP, "x")
        .unwrap_err();
    assert!(matches!(err, EngineError::NotItsOrder { .. }), "{err}");
    run.clock.advance_ms(30_000);
    let before = chosen.collect_time.ms_since(run.clock.now()) / 1000;
    let i = run
        .engine
        .cancel(id, session.authority(), "weather closed in")
        .unwrap();
    assert_eq!(i.state(), State::Aborted);
    assert_eq!(
        i.history().last().unwrap().reason,
        "cancelled by op-2: weather closed in"
    );
    assert_eq!(i.ops()[&order.resource], CANCELLED_OP);
    let detail = i.op_details()[&order.resource].clone();
    assert!(
        detail.starts_with(&format!("tasking request withdrawn {before} s before the pass: ")),
        "{detail}"
    );
    assert!(detail.contains("nothing was collected"), "{detail}");
    // nothing more is taken for it
    assert!(matches!(
        run.engine.record_op_state(id, &order.resource, "collected", "x"),
        Err(EngineError::NotItsOrder { .. })
    ));
    assert!(matches!(
        run.engine.finish(id, TaskOutcome::Done),
        Err(EngineError::Illegal { .. })
    ));
    assert_replays(&run.log);
    let r = replay_str(&run.log.text()).unwrap();
    assert_eq!(r.space_cancelled, 1);
    let honest = run.log.text();
    // forged: the operator's cancel dropped (the report and the abort stand)
    let w = forged_whats(&honest, |ev| {
        ev.retain(|e| !matches!(&e.body, audit::EventBody::OperatorAction { action, .. } if action == "intent.cancel"));
    });
    assert!(
        w.iter().any(|m| m.starts_with("a cancelled report without an operator's cancel")),
        "{w:?}"
    );
    // forged: the cancel moved past the pass (its events re-timed), the report left saying
    // the request was withdrawn before it
    let after = chosen.collect_time.plus_ms(120_000);
    let w = forged_whats(&honest, |ev| {
        let mut moving = false;
        for e in ev {
            if matches!(&e.body, audit::EventBody::OperatorAction { action, .. } if action == "intent.cancel") {
                moving = true;
            }
            if moving {
                e.t = after;
            }
        }
    });
    assert!(
        w.iter().any(|m| m == "cancelled report differs from what the cancel means for the dispatched order at that time"),
        "{w:?}"
    );
    // forged: the report dropped
    let w = forged_whats(&honest, |ev| {
        ev.retain(|e| !matches!(&e.body, audit::EventBody::OpState { state, .. } if state == CANCELLED_OP));
    });
    assert!(
        w.iter().any(|m| m == "cancelled after dispatch without the engine's cancelled report"),
        "{w:?}"
    );
    // forged: the cancel claimed for another operator
    let w = forged_whats(&honest, |ev| {
        for e in ev {
            if let audit::EventBody::IntentState { reason, .. } = &mut e.body
                && reason.starts_with("cancelled by op-2")
            {
                *reason = reason.replace("op-2", "op-1");
            }
        }
    });
    assert!(
        w.iter().any(|m| m == "a cancel after dispatch that does not name the operator who cancelled"),
        "{w:?}"
    );
}

#[test]
fn a_collect_cancelled_after_its_pass_keeps_the_collection_and_drops_the_product() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, order) = dispatched_collect(&mut run, &session);
    assert!(order.product_time.is_some(), "the fixture's first option has product timing");
    run.advance_to(chosen.collect_time.plus_ms(90_000));
    run.engine
        .record_op_state(id, &order.resource, "collected", "simulated")
        .unwrap();
    let i = run
        .engine
        .cancel(id, session.authority(), "no longer needed")
        .unwrap();
    assert_eq!(i.state(), State::Aborted);
    let detail = i.op_details()[&order.resource].clone();
    assert!(detail.starts_with("cancelled 90 s after the pass: "), "{detail}");
    assert!(
        detail.contains("a pass under way cannot be recalled") && detail.contains("no longer awaited"),
        "{detail}"
    );
    assert!(detail.contains(&format!(", due {}", murmur_decide::space::fmt_time(order.product_time.unwrap()))), "{detail}");
    assert_replays(&run.log);
}

#[test]
fn a_watching_cover_is_cancelled_but_an_agent_withdraws_nothing_after_dispatch() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    prime(&mut run, &cover(wgs(), None));
    let id = run
        .engine
        .submit(session.authority(), cover(wgs(), None), &world, &NoTerrain)
        .unwrap()
        .id();
    let Order::Cover(order) = run.engine.dispatch_order(id).unwrap() else {
        panic!("not a cover order");
    };
    run.engine
        .record_op_state(id, &order.resource, "watching", "wgs SATCOM over the area")
        .unwrap();
    run.clock.advance_ms(600_000);
    let i = run.engine.cancel(id, session.authority(), "enough").unwrap();
    assert_eq!(i.state(), State::Aborted);
    let detail = i.op_details()[&order.resource].clone();
    assert!(detail.starts_with("coverage watch ended by the cancel at "), "{detail}");
    assert!(
        detail.contains("before the window ended at") && detail.ends_with("nothing was commanded"),
        "{detail}"
    );
    // an agent's own cover, dispatched: the agent withdraws nothing after dispatch
    prime(&mut run, &cover(wgs(), None));
    let aid = run
        .engine
        .agent_port(AgentId::new("planner"))
        .propose(cover(wgs(), None), rationale("relay for the USV"), &world, &NoTerrain)
        .unwrap()
        .id();
    run.engine.dispatch_order(aid).unwrap();
    let err = run
        .engine
        .agent_port(AgentId::new("planner"))
        .withdraw(aid, "changed my mind")
        .unwrap_err();
    assert!(matches!(err, EngineError::Illegal { .. }), "{err}");
    assert_eq!(run.engine.intent(aid).unwrap().state(), State::Dispatched);
    assert_replays(&run.log);
}

#[test]
fn a_dispatched_vehicle_task_is_still_not_cancellable() {
    let clock = ManualClock::new(T0);
    let (mut engine, log) = engine(&clock);
    let session = engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let id = engine
        .submit(
            session.authority(),
            orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
            &world(),
            &terrain(),
        )
        .unwrap()
        .id();
    engine.dispatch(id, &world()).unwrap();
    let err = engine.cancel(id, session.authority(), "x").unwrap_err();
    assert!(matches!(err, EngineError::Illegal { .. }), "{err}");
    assert_eq!(engine.intent(id).unwrap().state(), State::Dispatched);
    assert!(!log.text().contains("intent.cancel"));
}

#[test]
fn a_dispatched_collect_is_rechecked_against_a_later_segment() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, order) = dispatched_collect(&mut run, &session);
    let (task, now) = {
        let i = run.engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    let rules = policy().rules;
    run.clock.advance_ms(120_000);
    // the same segment: the same answer, nothing to decide, nothing logged
    let before = run.log.lines().len();
    let p = prepare_space_at(&segment(), &task, run.clock.now(), now, &rules).unwrap();
    assert_eq!(run.engine.recheck_space(id, p).unwrap(), SpaceRecheck::Unchanged);
    assert_eq!(run.log.lines().len(), before);
    // an answer at a fresh request time is refused: the lead time spent since is not news
    let p = prepare_space(&segment(), &task, run.clock.now(), &rules).unwrap();
    assert!(matches!(run.engine.recheck_space(id, p), Err(EngineError::NotDispatchable { .. })));
    // the element sets reloaded without the chosen satellite: its pass is gone
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    // prepared on a segment the engine does not hold: refused
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &rules).unwrap();
    assert!(matches!(run.engine.recheck_space(id, p), Err(EngineError::NotDispatchable { .. })));
    assert_eq!(run.log.lines().len(), before);
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &rules).unwrap();
    let SpaceRecheck::Lost { detail } = run.engine.recheck_space(id, p).unwrap() else {
        panic!("the pass should be gone");
    };
    assert!(detail.starts_with(&format!("{NO_LONGER_FEASIBLE}feasibility: NORAD {}", chosen.norad_id)), "{detail}");
    assert!(detail.contains("no pass of that satellite in the window"), "{detail}");
    assert!(
        detail.contains("feasible options from") || detail.contains("no feasible option left"),
        "{detail}"
    );
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Failed);
    assert_eq!(i.ops()[&order.resource], MISSED_OP);
    assert_eq!(i.history().last().unwrap().reason, format!("collection missed: {detail}"));
    assert_eq!(check(i, RuleId::Feasibility).outcome, Outcome::Fail);
    let text = run.log.text();
    assert!(text.contains("\"phase\":\"dispatched\""));
    assert_eq!(text.matches("\"type\":\"space.loaded\"").count(), 2);
    // re-decided from the log alone; given the first segment, the later query is a cache
    // mismatch, not a decision mismatch
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!((r.space_missed, r.space_rechecked), (1, 0));
    let r = replay_str_with_space(&text, Some(&segment())).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert!(!r.cache_mismatches.is_empty());
    // forged: the dispatched-phase check dropped
    let w = forged_whats(&text, |ev| {
        ev.retain(|e| !matches!(&e.body, audit::EventBody::CheckResult { phase: CheckPhase::Dispatched, .. }));
    });
    assert!(
        w.iter().any(|m| m == "a re-check report without its dispatched-phase check right before it"),
        "{w:?}"
    );
    // forged: the missed report reworded
    let w = forged_whats(&text, |ev| {
        for e in ev {
            if let audit::EventBody::OpState { state, detail, .. } = &mut e.body
                && state == MISSED_OP
            {
                *detail = format!("{NO_LONGER_FEASIBLE}the pass was lost to weather");
            }
        }
    });
    assert!(w.iter().any(|m| m == "re-check report differs from the recomputed check"), "{w:?}");
    // forged: the missed report dropped
    let w = forged_whats(&text, |ev| {
        ev.retain(|e| !matches!(&e.body, audit::EventBody::OpState { state, .. } if state == MISSED_OP));
    });
    assert!(
        w.iter().any(|m| m == "a transition between a space task's re-check and its report"),
        "{w:?}"
    );
    // forged: failed for another reason than the report
    let w = forged_whats(&text, |ev| {
        for e in ev {
            if let audit::EventBody::IntentState { transition: Transition::Fail, reason, .. } = &mut e.body {
                *reason = "collection missed: the bridge stalled".into();
            }
        }
    });
    assert!(
        w.iter().any(|m| m == "a space task's missed report not followed by its failure for it"),
        "{w:?}"
    );
    // forged: a missed mark with no re-check behind it (the check and the failure's reason
    // both gone)
    let w = forged_whats(&text, |ev| {
        ev.retain(|e| !matches!(&e.body, audit::EventBody::CheckResult { phase: CheckPhase::Dispatched, .. }));
        for e in ev {
            if let audit::EventBody::IntentState { transition: Transition::Fail, reason, .. } = &mut e.body {
                *reason = "collection missed: the bridge stalled".into();
            }
        }
    });
    assert!(w.iter().any(|m| m.contains("without its dispatched-phase check")), "{w:?}");
}

#[test]
fn a_rechecked_collect_that_still_holds_runs_on() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, order) = dispatched_collect(&mut run, &session);
    let (task, now, other) = {
        let i = run.engine.intent(id).unwrap();
        let a = i.space_answer().unwrap();
        let other = a
            .tasking()
            .unwrap()
            .options
            .iter()
            .find(|o| o.norad_id != chosen.norad_id)
            .expect("another option")
            .norad_id;
        (i.spec().task.clone(), a.now().unwrap(), other)
    };
    // another satellite's passes gone: a new answer, the chosen pass still in it
    let later = space_without(other);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    run.clock.advance_ms(60_000);
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &policy().rules).unwrap();
    let SpaceRecheck::Holds { detail } = run.engine.recheck_space(id, p).unwrap() else {
        panic!("the chosen pass should still hold");
    };
    assert!(detail.starts_with(STILL_FEASIBLE), "{detail}");
    assert!(detail.contains(&chosen.norad_id.to_string()), "{detail}");
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Dispatched);
    assert_eq!(i.ops()[&order.resource], RECHECKED_OP);
    assert_eq!(i.space_answer().unwrap().segment.sha256, seg2.segment().sha256);
    // the bridge can't write that report either
    assert!(matches!(
        run.engine.record_op_state(id, &order.resource, RECHECKED_OP, "x"),
        Err(EngineError::NotItsOrder { .. })
    ));
    // the collection goes on as approved
    run.advance_to(chosen.collect_time);
    run.engine
        .record_op_state(id, &order.resource, "collected", "simulated")
        .unwrap();
    run.engine.finish(id, TaskOutcome::Done).unwrap();
    let r = replay_str(&run.log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!((r.space_rechecked, r.space_missed), (1, 0));
    // forged: a rechecked report with no check behind it
    let w = forged_whats(&run.log.text(), |ev| {
        ev.retain(|e| !matches!(&e.body, audit::EventBody::CheckResult { phase: CheckPhase::Dispatched, .. }));
    });
    assert!(w.iter().any(|m| m.contains("without its dispatched-phase check")), "{w:?}");
}

#[test]
fn a_prepared_answer_from_another_segment_is_not_used() {
    use std::sync::atomic::Ordering::SeqCst;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut run = run(policy(), false);
    run.engine
        .set_space(Box::new(Counting(segment(), calls.clone())))
        .unwrap();
    // the ISS left out: another segment than the engine's
    let other = SpaceSegment::new(space_without(25544)).unwrap();
    assert_ne!(other.segment().sha256, segment().segment().sha256);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let task = collect(None).task;
    let prepared = prepare_space(&other, &task, t0(), &policy().rules).unwrap();
    run.engine.use_prepared_space(prepared);
    let world = run.world();
    let i = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    // the engine asked its own segment instead: the logged answer is its
    assert_eq!(calls.load(SeqCst), 1);
    assert_eq!(i.space_answer().unwrap().segment.sha256, segment().segment().sha256);
    assert_replays(&run.log);
}

/// The committed fixtures of the engine on the element-set fixture, replayed by
/// `tests/versions.rs` (`tests/data/audit/v11-space-followups-engine.jsonl` written by rules
/// 11's build, `v13-space-replay-engine.jsonl` by rules 13's, each answer with its time): an
/// area cover watched to its end, a collect cancelled before its pass, a collect re-checked on
/// a reloaded segment (the chosen satellite gone) and missed. Written to
/// `MURMUR_WRITE_FIXTURE` when that is set (from a clean build of the head the fixture names),
/// checked here either way.
#[test]
fn writes_the_space_engine_fixture_when_asked() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    // a cover over the area, watched to the end of its window
    let world = run.world();
    prime(&mut run, &cover(wgs(), None));
    let cid = run
        .engine
        .submit(session.authority(), cover(wgs(), None), &world, &NoTerrain)
        .unwrap()
        .id();
    let Order::Cover(watch) = run.engine.dispatch_order(cid).unwrap() else {
        panic!("not a cover order");
    };
    run.engine
        .record_op_state(cid, &watch.resource, "watching", "wgs SATCOM over the area until the window end")
        .unwrap();
    // a collect cancelled a minute after it was tasked, before its pass
    let (id, _, _) = dispatched_collect(&mut run, &session);
    run.clock.advance_ms(60_000);
    let i = run
        .engine
        .cancel(id, session.authority(), "weather closed in")
        .unwrap();
    assert_eq!(i.state(), State::Aborted);
    // a second collect, its segment reloaded without its satellite: missed
    let (id, chosen, _) = dispatched_collect(&mut run, &session);
    let (task, now) = {
        let i = run.engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    run.clock.advance_ms(60_000);
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &policy().rules).unwrap();
    assert!(matches!(run.engine.recheck_space(id, p).unwrap(), SpaceRecheck::Lost { .. }));
    assert_eq!(run.engine.intent(id).unwrap().state(), State::Failed);
    // the watch ends with its window
    run.advance_to(t0().plus_ms(6 * 3600 * 1000));
    run.engine
        .record_op_state(cid, &watch.resource, "ended", "coverage window ended")
        .unwrap();
    run.engine.finish(cid, TaskOutcome::Done).unwrap();
    let text = run.log.text();
    let r = replay_str(&text).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!((r.space_cancelled, r.space_rechecked, r.space_missed), (1, 0, 1));
    // rules 13: every decision on a space answer says when it was computed
    let answered = text.matches("\"status\":\"answered\"").count();
    assert!(answered > 0);
    assert_eq!(text.matches("\"computed_t\":").count(), answered, "{text}");
    if let Ok(path) = std::env::var("MURMUR_WRITE_FIXTURE") {
        std::fs::write(&path, &text).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
}

// ---- the review of 8738abd ---------------------------------------------------------------

/// An oracle whose every query fails (a segment that cannot answer).
struct Failing(SegmentId);

impl SpaceOracle for Failing {
    fn segment(&self) -> &SegmentId {
        &self.0
    }
    fn answer(&self, _: &SpaceQuery) -> Result<SpaceAnswer, String> {
        Err("the element sets could not be read".into())
    }
}

/// A clock that moves a second at every read (what two reads straddling a second do).
#[derive(Clone)]
struct Ticking(Arc<std::sync::atomic::AtomicI64>);

impl Clock for Ticking {
    fn now(&self) -> Time {
        Time(self.0.fetch_add(1_000, std::sync::atomic::Ordering::SeqCst))
    }
}

/// The fixture pinned to a given shift (the same elements, another pin).
fn pinned_space(days: Option<i64>) -> Arc<Space> {
    let mut s = build_space(FIXTURE, CONSTELLATIONS);
    s.pinned_shift_days = days;
    Arc::new(s)
}

/// Rewrite the last logged space answer (a re-check's) and every reference to it.
fn edit_last_answer(ev: &mut [Event], edit: impl Fn(&mut SpaceAnswer)) {
    let last = ev
        .iter()
        .rposition(|e| matches!(e.body, audit::EventBody::SpaceQuery { .. }))
        .expect("a space answer");
    let (old, new) = {
        let audit::EventBody::SpaceQuery { sha256, answer } = &mut ev[last].body else {
            unreachable!()
        };
        edit(answer);
        let new = answer.content_hash().unwrap();
        let old = sha256.clone();
        *sha256 = new.clone();
        (old, new)
    };
    for e in ev.iter_mut() {
        if let audit::EventBody::CheckResult { inputs, .. } | audit::EventBody::PolicyDecision { inputs, .. } = &mut e.body
            && let LoggedSpace::Answered { sha256, .. } = &mut inputs.space
            && *sha256 == old
        {
            *sha256 = new.clone();
        }
    }
}

/// H1: the engine computes no cover (seconds of SGP4 under whatever lock the caller holds):
/// a cover decides only on an answer prepared outside, which stays its answer whatever its
/// age (its query does not depend on when it was asked); a collect's query (milliseconds)
/// is still asked when none was prepared.
#[test]
fn a_cover_is_never_computed_by_the_engine_itself() {
    use std::sync::atomic::Ordering::SeqCst;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut run = run(policy(), false);
    run.engine
        .set_space(Box::new(Counting(segment(), calls.clone())))
        .unwrap();
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    // nothing prepared: the cover fails closed and the segment is not asked
    let i = run
        .engine
        .submit(session.authority(), cover(wgs(), None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::Rejected);
    let q = check(i, RuleId::SpaceQuery);
    assert!(q.detail.contains("no cover answer prepared"), "{}", q.detail);
    assert_eq!(calls.load(SeqCst), 0, "no cover computed under the lock");
    // an answer prepared a minute before the check (past the collect bound of 5 s) is still
    // the cover's answer
    let outside = Counting(segment(), calls.clone());
    let prepared = prepare_space(&outside, &cover(wgs(), None).task, t0(), &policy().rules).unwrap();
    assert_eq!(calls.load(SeqCst), 1);
    run.engine.use_prepared_space(prepared);
    run.advance_to(t0().plus_ms(60_000));
    let world = run.world();
    let i = run
        .engine
        .submit(session.authority(), cover(wgs(), None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::AutoApproved, "{:#?}", i.checks());
    assert_eq!(calls.load(SeqCst), 1, "not asked again under the lock");
    // a collect's tasking query is asked when nothing was prepared (milliseconds)
    let i = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(check(i, RuleId::SpaceQuery).outcome, Outcome::Pass);
    assert_eq!(calls.load(SeqCst), 2);
    assert_replays(&run.log);
}

/// M2: the pin is part of a segment's identity: the same elements pinned otherwise are
/// another segment, which a prepared answer from them does not fit.
#[test]
fn a_prepared_answer_fits_only_the_segment_and_pin_the_engine_holds() {
    let run = run(policy(), true);
    let task = cover(wgs(), None).task;
    let rules = policy().rules;
    let p = prepare_space(&segment(), &task, t0(), &rules).unwrap();
    assert!(run.engine.prepared_space_fits(&p));
    let pinned = SpaceSegment::new(pinned_space(Some(-7))).unwrap();
    assert!(pinned.segment().same_elements(segment().segment()));
    assert_ne!(*pinned.segment(), *segment().segment());
    assert!(pinned.segment().short().contains("pinned -7 d"), "{}", pinned.segment().short());
    let p = prepare_space(&pinned, &task, t0(), &rules).unwrap();
    assert!(!run.engine.prepared_space_fits(&p));
    let other = SpaceSegment::new(space_without(25544)).unwrap();
    let p = prepare_space(&other, &task, t0(), &rules).unwrap();
    assert!(!run.engine.prepared_space_fits(&p));
    // an answer that could not be computed carries no segment: it fits (and fails closed)
    let failing = Failing(segment().segment().clone());
    let p = prepare_space(&failing, &task, t0(), &rules).unwrap();
    assert!(run.engine.prepared_space_fits(&p));
}

/// M1: a collect whose pass has come was collected: a later answer does not re-check it,
/// and replay refuses a re-check written after the pass.
#[test]
fn a_collect_whose_pass_has_come_is_not_rechecked() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, order) = dispatched_collect(&mut run, &session);
    let (task, now) = {
        let i = run.engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    run.advance_to(chosen.collect_time.plus_ms(1_000));
    run.engine
        .record_op_state(id, &order.resource, "collected", "simulated")
        .unwrap();
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &policy().rules).unwrap();
    let err = run.engine.recheck_space(id, p).unwrap_err();
    assert!(err.to_string().contains("its pass has come"), "{err}");
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Dispatched);
    assert_eq!(i.ops()[&order.resource], "collected");
    assert!(!run.log.text().contains("\"phase\":\"dispatched\""));
    run.engine.finish(id, TaskOutcome::Done).unwrap();
    let r = replay_str(&run.log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    // forged from an honest re-check before the pass: its events re-timed past the pass
    let mut run = self::run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, _) = dispatched_collect(&mut run, &session);
    let (task, now) = {
        let i = run.engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &policy().rules).unwrap();
    assert!(matches!(run.engine.recheck_space(id, p).unwrap(), SpaceRecheck::Lost { .. }));
    let honest = run.log.text();
    let after = chosen.collect_time.plus_ms(5_000);
    let w = forged_whats(&honest, |ev| {
        let mut moving = false;
        for e in ev {
            if matches!(e.body, audit::EventBody::SpaceLoaded { .. }) && e.seq > 2 {
                moving = true;
            }
            if moving {
                e.t = after;
            }
        }
    });
    assert!(
        w.iter().any(|m| m.starts_with("a collect re-checked after its pass had come")),
        "{w:?}"
    );
}

/// M3: an area answer's result points are its query's: one dropped, moved or copied over
/// another (the query left as it was) is not the answer to the question.
#[test]
fn a_cover_answer_whose_points_are_not_its_querys_is_caught() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = run.world();
    let gps = CoverService::Gnss {
        class: "gps".into(),
    };
    let c = cover(gps, Some(40.0));
    prime(&mut run, &c);
    let i = run
        .engine
        .submit(session.authority(), c, &world, &NoTerrain)
        .unwrap();
    assert_eq!(i.state(), State::Rejected);
    let SpaceResult::CoverGnss(g) = &i.space_answer().unwrap().result else {
        panic!("not an area answer");
    };
    let worst = g.points.iter().position(|p| p.gnss.visible_min == g.worst().unwrap().gnss.visible_min).unwrap();
    // another point's entry (over a 5 km circle every point sees the same satellites, so
    // the entries differ by their coordinates alone)
    let best = (worst + 1) % g.points.len();
    let honest = run.log.text();
    type Edit = fn(&mut SpaceAnswer, usize, usize);
    let cases: [(Edit, &str); 3] = [
        (
            |a, worst, _| {
                if let SpaceResult::CoverGnss(c) = &mut a.result {
                    c.points.remove(worst);
                }
            },
            "dropped",
        ),
        (
            |a, worst, _| {
                if let SpaceResult::CoverGnss(c) = &mut a.result {
                    let other = (worst + 1) % c.points.len();
                    c.points[worst].point = c.points[other].point;
                }
            },
            "moved",
        ),
        (
            |a, worst, best| {
                if let SpaceResult::CoverGnss(c) = &mut a.result {
                    let entry = c.points[best].clone();
                    c.points[worst] = entry;
                }
            },
            "copied over",
        ),
    ];
    for (edit, what) in cases {
        let forged = forge(&honest, |ev| edit_answer(ev, |a| edit(a, worst, best)));
        let r = replay_str(&forged).unwrap();
        let w = whats(&r);
        assert!(w.iter().any(|m| m == "check results differ"), "{what}: {w:?}");
        assert!(
            r.mismatches.iter().any(|m| m.recomputed.contains("the space answer's points")),
            "{what}: {:#?}",
            r.mismatches
        );
    }
}

/// M4: a re-check is at the request time the task was tasked at and on the segment loaded
/// after it: one at a later request time, or with the reload cut from the log, is caught.
#[test]
fn a_recheck_is_tied_to_its_request_time_and_to_the_reload() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, _) = dispatched_collect(&mut run, &session);
    let (task, now) = {
        let i = run.engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    run.clock.advance_ms(60_000);
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &policy().rules).unwrap();
    assert!(matches!(run.engine.recheck_space(id, p).unwrap(), SpaceRecheck::Lost { .. }));
    let honest = run.log.text();
    assert!(replay_str(&honest).unwrap().ok());
    // F12: the re-check's answer asked an hour later (the engine refuses it)
    let w = forged_whats(&honest, |ev| {
        edit_last_answer(ev, |a| {
            if let SpaceQuery::Tasking(q) = &mut a.query {
                q.now += chrono::Duration::hours(1);
            }
            if let SpaceResult::Tasking(t) = &mut a.result {
                t.request.now += chrono::Duration::hours(1);
            }
        });
    });
    assert!(
        w.iter().any(|m| m == "a re-check at another request time than the task was tasked at"),
        "{w:?}"
    );
    // F7: the reload cut from the log
    let w = forged_whats(&honest, |ev| {
        let last = ev.iter().rposition(|e| matches!(e.body, audit::EventBody::SpaceLoaded { .. })).unwrap();
        ev.remove(last);
    });
    assert!(
        w.iter().any(|m| m.starts_with("a re-check on a segment other than the one loaded")),
        "{w:?}"
    );
}

/// L1: the cancel's report is logged at the instant its detail was computed for, so an
/// honest log whose clock moved between the two replays.
#[test]
fn a_cancel_whose_clock_moved_between_reads_still_replays() {
    let clock = Ticking(Arc::new(std::sync::atomic::AtomicI64::new(t0().0)));
    let log = MemorySink::new();
    let mut engine = Engine::new(policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    engine.set_space(Box::new(segment())).unwrap();
    let session = engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = WorldSnapshot::new(clock.now());
    let q = engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    let chosen = ChosenCollect::from_option(&q.space_answer().unwrap().tasking().unwrap().options[0]);
    let qid = q.id();
    let world = WorldSnapshot::new(clock.now());
    let id = engine
        .amend_collect(session.authority(), qid, chosen, &world, &NoTerrain)
        .unwrap()
        .id();
    let Order::Collect(order) = engine.dispatch_order(id).unwrap() else {
        panic!("not a collect order");
    };
    engine
        .record_op_state(id, &order.resource, "tasked", "simulated")
        .unwrap();
    let i = engine.cancel(id, session.authority(), "weather").unwrap();
    assert_eq!(i.state(), State::Aborted);
    let r = replay_str(&log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(r.space_cancelled, 1);
}

/// L2: a polygon is thinned by geometry (its far tip is always sampled) and a closed ring
/// samples its first vertex once.
#[test]
fn polygon_sampling_keeps_the_far_tip_and_a_closed_ring_once() {
    let c = GOLDEN_GATE;
    let around = |bearing: f64, km: f64| {
        let g = murmur_space::Geodetic::new(c.lat, c.lon, 0.0).destination(bearing, km);
        LonLat::new(g.lon_deg, g.lat_deg)
    };
    // eight vertices 20 km out and a ninth, the tip, 138 km east: thinning by index
    // (every k-th vertex) would leave the ninth out
    let mut ring: Vec<LonLat> = (0..8).map(|k| around(f64::from(k) * 45.0, 20.0)).collect();
    let tip = around(90.0, 138.0);
    ring.push(tip);
    let pts = cover_points(&SpaceArea::Polygon { ring: ring.clone() });
    assert_eq!(pts.len(), 1 + COVER_BOUNDARY_SAMPLES);
    assert!(
        pts.iter().any(|p| murmur_decide::geo::haversine_m(LonLat::new(p.lon_deg, p.lat_deg), tip) < 1.0),
        "the tip is sampled: {pts:?}"
    );
    // the same ring closed: the first vertex once
    let mut closed = ring.clone();
    closed.push(ring[0]);
    let pts2 = cover_points(&SpaceArea::Polygon { ring: closed });
    assert_eq!(pts2, pts);
    let small: Vec<LonLat> = (0..4).map(|k| around(f64::from(k) * 90.0, 10.0)).collect();
    let mut small_closed = small.clone();
    small_closed.push(small[0]);
    assert_eq!(cover_points(&SpaceArea::Polygon { ring: small_closed }).len(), 5);
}

/// L3: a later answer that could not be computed is no answer: the task runs on, nothing
/// is logged, and the bridge hears why.
#[test]
fn a_recheck_whose_answer_failed_keeps_the_task() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, _, order) = dispatched_collect(&mut run, &session);
    let (task, now) = {
        let i = run.engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    let before = run.log.lines().len();
    let failing = Failing(segment().segment().clone());
    let p = prepare_space_at(&failing, &task, run.clock.now(), now, &policy().rules).unwrap();
    let SpaceRecheck::Unknown { reason } = run.engine.recheck_space(id, p).unwrap() else {
        panic!("a failed query is no answer");
    };
    assert!(reason.contains("could not be read"), "{reason}");
    let i = run.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Dispatched);
    assert_eq!(i.ops()[&order.resource], "tasked");
    assert_eq!(run.log.lines().len(), before, "nothing logged");
    assert_replays(&run.log);
}

/// L4: a lost pass's fresh options are those still taskable at the re-check's own time, not
/// at the request time the answer was asked at.
#[test]
fn fresh_options_after_a_lost_pass_are_those_still_taskable_now() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let (id, chosen, _) = dispatched_collect(&mut run, &session);
    let (task, now, options) = {
        let i = run.engine.intent(id).unwrap();
        let a = i.space_answer().unwrap();
        (i.spec().task.clone(), a.now().unwrap(), a.tasking().unwrap().options.clone())
    };
    // just before the chosen pass: the earlier options are gone and the lead time of the
    // next ones is running out
    let at = chosen.collect_time.plus_ms(-10_000);
    run.advance_to(at);
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    run.engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    let p = prepare_space_at(&seg2, &task, run.clock.now(), now, &policy().rules).unwrap();
    let SpaceRecheck::Lost { detail } = run.engine.recheck_space(id, p).unwrap() else {
        panic!("the pass should be gone");
    };
    let from = murmur_decide::space::fmt_time(at);
    assert!(
        detail.contains(&format!("feasible options from {from}")) || detail.contains(&format!("no feasible option left in the window from {from}")),
        "{detail}"
    );
    // nothing listed collects before now plus its lead time
    for o in &options {
        let lead = from_utc(o.earliest_collect).ms_since(now).max(0);
        if from_utc(o.collect_time) < at.plus_ms(lead) {
            assert!(!detail.contains(&format!("{} {} collect", o.satellite, o.norad_id)), "{detail}");
        }
    }
    let r = replay_str(&run.log.text()).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
}

// ---- rules 13: when a space answer was computed, the re-check's instant, the pin ----------

/// Every decision input's `computed_t` rewritten by `f` (a forger editing the time alone).
fn edit_computed(ev: &mut [Event], f: impl Fn(&mut Option<Time>)) {
    for e in ev.iter_mut() {
        if let audit::EventBody::CheckResult { inputs, .. } | audit::EventBody::PolicyDecision { inputs, .. } = &mut e.body
            && let LoggedSpace::Answered { computed_t, .. } = &mut inputs.space
        {
            f(computed_t);
        }
    }
}

/// A tasking answer's request time moved by `ms` (the question asked at another time).
fn shift_request(a: &mut SpaceAnswer, ms: i64) {
    if let SpaceQuery::Tasking(q) = &mut a.query {
        q.now += chrono::Duration::milliseconds(ms);
    }
    if let SpaceResult::Tasking(t) = &mut a.result {
        t.request.now += chrono::Duration::milliseconds(ms);
    }
}

/// der.4: a check logs when its space answer was computed (`computed_t`); a collect decides
/// only on an answer computed within 5 s of its check and asked at that time, which replay
/// recomputes: an answer older than the bound (asked earlier, re-hashed, its time moved with
/// it, so the log agrees with itself), a time stripped, a time moved apart from the answer,
/// are each caught.
#[test]
fn a_space_answer_logs_when_it_was_computed_and_replay_holds_the_bound() {
    let mut run = run(policy(), true);
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    // computed outside the lock, used by the check 2 s later (the bridge's way)
    let prepared = prepare_space(&segment(), &collect(None).task, t0(), &policy().rules).unwrap();
    run.engine.use_prepared_space(prepared);
    run.advance_to(t0().plus_ms(2_000));
    let world = run.world();
    let i = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(check(i, RuleId::SpaceQuery).outcome, Outcome::Pass);
    let honest = run.log.text();
    assert!(honest.contains(&format!("\"computed_t\":{}", t0().0)), "{honest}");
    assert_replays(&run.log);
    // forged: the answer asked 10 s earlier, consistent with itself: 12 s before its check
    let forged = forge(&honest, |ev| {
        edit_answer(ev, |a| shift_request(a, -10_000));
        edit_computed(ev, |c| *c = c.map(|t| t.plus_ms(-10_000)));
    });
    let r = replay_str(&forged).unwrap();
    let w = whats(&r);
    assert!(w.iter().any(|m| m == "check results differ"), "{w:?}");
    assert!(
        r.mismatches.iter().any(|m| m.recomputed.contains("the space answer was computed 12.000 s before the check, more than 5 s")),
        "{:#?}",
        r.mismatches
    );
    assert!(!w.iter().any(|m| m.starts_with("a space answer asked at another time")), "{w:?}");
    // the same answer 4 s old is within the bound: replays
    let fine = forge(&honest, |ev| {
        edit_answer(ev, |a| shift_request(a, -2_000));
        edit_computed(ev, |c| *c = c.map(|t| t.plus_ms(-2_000)));
    });
    let r = replay_str(&fine).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    // forged: the time stripped
    let w = forged_whats(&honest, |ev| edit_computed(ev, |c| *c = None));
    assert!(w.iter().any(|m| m == "a space answer without when it was computed (computed_t)"), "{w:?}");
    assert!(w.iter().any(|m| m == "check results differ"), "{w:?}");
    // forged: the time moved, the answer left as asked
    let w = forged_whats(&honest, |ev| edit_computed(ev, |c| *c = c.map(|t| t.plus_ms(1_500))));
    assert!(
        w.iter().any(|m| m.starts_with("a space answer asked at another time than it was computed at")),
        "{w:?}"
    );
}

/// der.4: the engine takes a prepared collect only if it was asked at the scenario time of
/// its computation (what replay holds the log to); otherwise it asks itself.
#[test]
fn a_prepared_collect_asked_at_another_time_than_computed_is_not_used() {
    use std::sync::atomic::Ordering::SeqCst;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut run = run(policy(), false);
    run.engine
        .set_space(Box::new(Counting(segment(), calls.clone())))
        .unwrap();
    let session = run
        .engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let task = collect(None).task;
    // computed now, but asked as of an hour before
    let outside = Counting(segment(), calls.clone());
    let p = prepare_space_at(&outside, &task, t0(), t0().plus_ms(-3_600_000), &policy().rules).unwrap();
    run.engine.use_prepared_space(p);
    run.advance_to(t0().plus_ms(1_000));
    let world = run.world();
    let i = run
        .engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    assert_eq!(calls.load(SeqCst), 2, "the engine asked again");
    assert_eq!(i.space_answer().unwrap().now(), Some(t0().plus_ms(1_000)));
    assert!(run.log.text().contains(&format!("\"computed_t\":{}", t0().plus_ms(1_000).0)));
    assert_replays(&run.log);
}

/// A clock that moves a millisecond at every read (no two reads in a step are the same
/// instant), settable.
#[derive(Clone)]
struct Creeping(Arc<std::sync::atomic::AtomicI64>);

impl Clock for Creeping {
    fn now(&self) -> Time {
        Time(self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

/// lsj: a re-check judges "has the pass come" on one clock read and its check is logged at
/// that instant (replay judges on the check's time): under a clock that moves a millisecond
/// at every read, an honest re-check a millisecond before the pass replays. The same check
/// logged a millisecond later would be one after the pass, which replay refuses.
#[test]
fn a_recheck_a_millisecond_before_the_pass_replays_under_a_moving_clock() {
    let clock = Creeping(Arc::new(std::sync::atomic::AtomicI64::new(t0().0)));
    let log = MemorySink::new();
    let mut engine = Engine::new(policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    engine.set_space(Box::new(segment())).unwrap();
    let session = engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    let world = WorldSnapshot::new(clock.now());
    let q = engine
        .submit(session.authority(), collect(None), &world, &NoTerrain)
        .unwrap();
    let chosen = ChosenCollect::from_option(&q.space_answer().unwrap().tasking().unwrap().options[0]);
    let qid = q.id();
    let world = WorldSnapshot::new(clock.now());
    let id = engine
        .amend_collect(session.authority(), qid, chosen, &world, &NoTerrain)
        .unwrap()
        .id();
    let Order::Collect(order) = engine.dispatch_order(id).unwrap() else {
        panic!("not a collect order");
    };
    engine
        .record_op_state(id, &order.resource, "tasked", "simulated")
        .unwrap();
    let (task, now) = {
        let i = engine.intent(id).unwrap();
        (i.spec().task.clone(), i.space_answer().unwrap().now().unwrap())
    };
    // the element sets reloaded without the chosen satellite
    let later = space_without(chosen.norad_id);
    let seg2 = SpaceSegment::new(later.clone()).unwrap();
    engine
        .set_space(Box::new(SpaceSegment::new(later).unwrap()))
        .unwrap();
    // the later answer computed just before; the guard reads a millisecond before the pass
    let p = prepare_space_at(&seg2, &task, chosen.collect_time.plus_ms(-2), now, &policy().rules).unwrap();
    clock.0.store(chosen.collect_time.0 - 1, std::sync::atomic::Ordering::SeqCst);
    assert!(matches!(engine.recheck_space(id, p).unwrap(), SpaceRecheck::Lost { .. }));
    let honest = log.text();
    let checked: Event = honest
        .lines()
        .find(|l| l.contains("\"phase\":\"dispatched\""))
        .map(|l| serde_json::from_str(l).unwrap())
        .unwrap();
    assert_eq!(checked.t, chosen.collect_time.plus_ms(-1), "logged at the guard's instant");
    let r = replay_str(&honest).unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
    assert_eq!(r.space_missed, 1);
    // what a later clock read for the check would have written: after the pass, refused
    let w = forged_whats(&honest, |ev| {
        for e in ev.iter_mut() {
            if e.t == chosen.collect_time.plus_ms(-1) {
                e.t = chosen.collect_time;
            }
        }
    });
    assert!(
        w.iter().any(|m| m.starts_with("a collect re-checked after its pass had come")),
        "{w:?}"
    );
}

/// The review of rules 13: the scenario clock is the run's, as the pin is: the engine refuses
/// a segment mapped otherwise than the first (nothing logged), replay flags a mid-log change;
/// under rules 12 as before.
#[test]
fn a_mid_log_scenario_clock_change_is_refused_and_flagged() {
    let mut run = run(policy(), true);
    let before = run.log.lines().len();
    let err = run
        .engine
        .set_space(Box::new(segment().with_scenario_offset_ms(60_000)))
        .unwrap_err();
    assert!(matches!(err, EngineError::SpaceSegment(_)), "{err}");
    assert!(err.to_string().contains("its scenario clock can't change within a run"), "{err}");
    assert_eq!(run.log.lines().len(), before, "nothing logged");
    assert!(run.engine.scenario_map().is_identity());
    // a reload on the same clock is taken
    run.engine
        .set_space(Box::new(SpaceSegment::new(space_without(25544)).unwrap()))
        .unwrap();
    let honest = run.log.text();
    assert!(replay_str(&honest).unwrap().ok());
    // forged: the reload on another clock
    let w = forged_whats(&honest, |ev| {
        let last = ev
            .iter()
            .rposition(|e| matches!(e.body, audit::EventBody::SpaceLoaded { .. }))
            .unwrap();
        if let audit::EventBody::SpaceLoaded { scenario, .. } = &mut ev[last].body {
            *scenario = Some(ScenarioMap::offset(60_000));
        }
    });
    assert!(w.iter().any(|m| m.starts_with("a mid-log scenario clock change")), "{w:?}");
    // rules 12: taken, and replayed as before
    let mut p = policy();
    p.rules.rules_version = REVIEW_FOLLOWUPS_SINCE;
    let mut old = self::run(p, true);
    old.engine
        .set_space(Box::new(segment().with_scenario_offset_ms(60_000)))
        .unwrap();
    let r = replay_str_with(
        &old.log.text(),
        &ReplayOptions {
            rules: RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Which {
    First,
    Last,
}

/// A segment that says it is pinned otherwise than its identity says.
struct Repinned(SpaceSegment, Option<i64>);

impl SpaceOracle for Repinned {
    fn segment(&self) -> &SegmentId {
        self.0.segment()
    }
    fn answer(&self, q: &SpaceQuery) -> Result<SpaceAnswer, String> {
        self.0.answer(q)
    }
    fn pinned_shift_days(&self) -> Option<i64> {
        self.1
    }
}

/// lsj: the pin is the run's: under rules 13 the engine refuses a segment pinned otherwise
/// (nothing logged) and takes a reload on the same pin; replay flags a mid-log pin change.
/// Under rules 12 the engine took a re-pin and replay does not flag it (older logs replay as
/// before).
#[test]
fn a_mid_log_pin_change_is_refused_and_flagged() {
    let mut run = run(policy(), true);
    let before = run.log.lines().len();
    let err = run
        .engine
        .set_space(Box::new(SpaceSegment::new(pinned_space(Some(-3))).unwrap()))
        .unwrap_err();
    assert!(matches!(err, EngineError::SpaceSegment(_)), "{err}");
    assert!(err.to_string().contains("pinned none at the first load, -3 d given"), "{err}");
    assert_eq!(run.log.lines().len(), before, "nothing logged");
    assert_eq!(run.engine.space_segment().unwrap().pinned_shift_days, None);
    // a reload of the elements on the same pin is taken
    run.engine
        .set_space(Box::new(SpaceSegment::new(space_without(25544)).unwrap()))
        .unwrap();
    let honest = run.log.text();
    assert_eq!(honest.matches("\"type\":\"space.loaded\"").count(), 2);
    assert!(replay_str(&honest).unwrap().ok());
    // forged: the reload pinned to another shift
    let w = forged_whats(&honest, |ev| {
        let last = ev
            .iter()
            .rposition(|e| matches!(e.body, audit::EventBody::SpaceLoaded { .. }))
            .unwrap();
        if let audit::EventBody::SpaceLoaded { segment, pinned_shift_days, .. } = &mut ev[last].body {
            *pinned_shift_days = Some(-3);
            segment.pinned_shift_days = Some(-3);
        }
    });
    assert!(w.iter().any(|m| m.starts_with("a mid-log pin change")), "{w:?}");
    // the review: only the pin inside the segment's identity forged, in the second load or
    // the first (the top-level pin left as it was)
    for which in [Which::Last, Which::First] {
        let w = forged_whats(&honest, |ev| {
            let loads: Vec<usize> = ev
                .iter()
                .enumerate()
                .filter(|(_, e)| matches!(e.body, audit::EventBody::SpaceLoaded { .. }))
                .map(|(i, _)| i)
                .collect();
            let i = if which == Which::Last { loads[loads.len() - 1] } else { loads[0] };
            if let audit::EventBody::SpaceLoaded { segment, .. } = &mut ev[i].body {
                segment.pinned_shift_days = Some(-3);
            }
        });
        assert!(
            w.iter().any(|m| m == "a space segment whose identity names another pin than it was loaded with"),
            "{which:?}: {w:?}"
        );
        assert!(w.iter().any(|m| m.starts_with("a mid-log pin change")), "{which:?}: {w:?}");
    }
    // and the engine takes no segment whose identity and pin disagree
    let err = run
        .engine
        .set_space(Box::new(Repinned(SpaceSegment::new(space()).unwrap(), Some(-3))))
        .unwrap_err();
    assert!(err.to_string().contains("its identity names another pin (none) than it is pinned to (-3 d)"), "{err}");
    // rules 12: a re-pin taken, and replayed as before
    let mut p = policy();
    p.rules.rules_version = REVIEW_FOLLOWUPS_SINCE;
    let mut old = self::run(p, true);
    old.engine
        .set_space(Box::new(SpaceSegment::new(pinned_space(Some(-3))).unwrap()))
        .unwrap();
    let r = replay_str_with(
        &old.log.text(),
        &ReplayOptions {
            rules: RulesAccepted::Exactly(REVIEW_FOLLOWUPS_SINCE),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(r.ok(), "{:#?}", r.mismatches);
}
