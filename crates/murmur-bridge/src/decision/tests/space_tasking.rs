//! Satellite tasking through the bridge: collect and cover intents on the element-set
//! fixture (CelesTrak GP data of 2026-10-02, murmur-space's `tests/data`), the scenario
//! clock running 60 times faster than the manual engine clock, executed by the space
//! executor and replayed with the same segment.

use super::*;
use murmur_decide::space::from_utc;
use murmur_decide::{ChosenCollect, SpaceAnswer};

const GP_FIXTURE: &str = include_str!("../../../../murmur-space/tests/data/gp_fixture.json");

fn fixture_space() -> Arc<murmur_space::Space> {
    let catalog =
        murmur_space::Catalog::parse(include_str!("../../../../../sim/space/constellations.toml"))
            .expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, GP_FIXTURE.to_string()))
        .collect();
    Arc::new(murmur_space::Space::from_sources(catalog, &sources).expect("space"))
}

/// Scenario start, on the fixture's epoch.
fn t0() -> Time {
    Time(
        chrono::DateTime::parse_from_rfc3339("2026-10-02T14:00:00Z")
            .expect("time")
            .timestamp_millis(),
    )
}

/// The rig with the space segment installed: the scenario clock starts at `t0` now and runs
/// `rate` times faster than the engine clock.
fn space_rig(rate: f64) -> (Rig, ScenarioMap) {
    let r = rig();
    let map = ScenarioMap {
        engine_origin: r.clock.now(),
        scenario_origin: t0(),
        rate,
    };
    r.d.install_space(fixture_space(), map);
    (r, map)
}

fn space_req(body: Value) -> IntentRequest {
    serde_json::from_value(body).expect("request")
}

fn area() -> Value {
    json!({ "kind": "circle", "center": [-122.47, 37.825], "radius_km": 5.0 })
}

fn collect_req(chosen: Option<&ChosenCollect>) -> IntentRequest {
    space_req(json!({
        "task": "collect",
        "area": area(),
        "window": { "from": t0().0, "to": t0().0 + 12 * 3600 * 1000 },
        "capability": "any",
        "chosen": chosen.map(|c| json!({ "norad_id": c.norad_id, "collect_time": c.collect_time.0 })),
    }))
}

struct Picked {
    chosen: ChosenCollect,
    product: Option<Time>,
    lead_ms: i64,
}

impl Rig {
    /// Query (a rejected intent with the options), then the first ranked option.
    async fn first_option(&self) -> Picked {
        let q = self
            .d
            .submit(&op1(), collect_req(None))
            .await
            .expect("query");
        assert_eq!(
            q.state,
            State::Rejected,
            "a collect without a choice is a query"
        );
        assert!(q.intent["space"]["options_total"].as_u64().unwrap_or(0) > 0);
        let core = self.d.lock();
        let r = core
            .engine
            .intent(q.id)
            .and_then(Intent::space_answer)
            .and_then(SpaceAnswer::tasking)
            .expect("tasking answer")
            .clone();
        let o = &r.options[0];
        let chosen = ChosenCollect::from_option(o);
        let product = o.product_time.map(|p| {
            chosen
                .collect_time
                .plus_ms(from_utc(p).ms_since(from_utc(o.collect_time)))
        });
        let lead_ms = from_utc(o.earliest_collect).ms_since(from_utc(r.request.now));
        Picked {
            chosen,
            product,
            lead_ms,
        }
    }

    fn op_states(&self) -> Vec<String> {
        self.log
            .lines()
            .iter()
            .filter(|l| l.contains("\"type\":\"op.state\""))
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v["state"].as_str().map(str::to_string))
            .collect()
    }

    fn agent_collect(&self, chosen: &ChosenCollect) -> IntentId {
        let req = collect_req(Some(chosen));
        let proposal = space::proposal(&req.task, false, &req.space, None).expect("proposal");
        let mut guard = self.d.lock();
        let world = self.d.snapshot();
        let core = &mut *guard;
        core.engine
            .agent_port(AgentId::new("planner"))
            .propose(
                proposal,
                Rationale::new("eyes on the bridge before the fog").expect("rationale"),
                &world,
                &self.d.terrain.oracle(),
            )
            .expect("propose")
            .id()
    }

    fn replays_with_space(&self) {
        self.replays();
        let seg = SpaceSegment::new(fixture_space()).expect("segment");
        let report =
            replay::replay_str_with_space(&self.log.text(), Some(&seg)).expect("log parses");
        assert!(report.ok(), "replay mismatches: {:?}", report.mismatches);
        assert!(report.space_reproduced(), "{report:#?}");
        assert!(report.space_queries > 0);
    }
}

#[tokio::test]
async fn an_operators_collect_is_tasked_collected_and_its_product_arrives() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let out =
        r.d.submit(&op1(), collect_req(Some(&p.chosen)))
            .await
            .expect("collect");
    // the operator's own collect: authorized at submission, handed to the executor
    assert_eq!(out.state, State::Dispatched, "{}", out.intent);
    assert!(out.dispatched());
    assert_eq!(out.intent["approval"]["at_submission"], true);
    let run = out.space.clone().expect("space run");
    assert_eq!(run["state"], "tasked");
    assert_eq!(run["resource"], format!("norad:{}", p.chosen.norad_id));
    assert_eq!(r.events("approval.requested"), 0);

    r.clock.set(map.engine(p.chosen.collect_time).plus_ms(-1));
    r.d.tick();
    assert_eq!(
        r.state(out.id),
        State::Dispatched,
        "still tasked before the pass"
    );
    r.clock.set(map.engine(p.chosen.collect_time));
    r.d.tick();
    match p.product {
        Some(t) => {
            assert_eq!(r.state(out.id), State::Dispatched);
            r.clock.set(map.engine(t));
            r.d.tick();
            assert_eq!(r.op_states(), ["tasked", "collected", "product_ready"]);
        }
        None => assert_eq!(r.op_states(), ["tasked", "collected"]),
    }
    assert_eq!(r.state(out.id), State::Done);
    let runs = r.d.space_runs_json();
    assert_eq!(runs["runs"][0]["outcome"], "done");
    assert_eq!(runs["clock"]["rate"], 60.0);
    assert!(r.d.message().contains("\"runs\""));
    r.replays_with_space();
}

#[tokio::test]
async fn a_collect_whose_pass_is_lost_reports_missed() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let out =
        r.d.submit(&op1(), collect_req(Some(&p.chosen)))
            .await
            .expect("collect");
    assert_eq!(out.state, State::Dispatched);
    // the bridge next sees the scenario clock ten minutes (bridge time) after the pass
    r.clock
        .set(map.engine(p.chosen.collect_time).plus_ms(10 * 60 * 1000));
    r.d.tick();
    assert_eq!(r.state(out.id), State::Failed);
    assert_eq!(r.op_states(), ["tasked", "missed"]);
    assert!(
        r.reason(out.id).contains("collection missed"),
        "{}",
        r.reason(out.id)
    );
    r.replays_with_space();
}

#[tokio::test]
async fn an_agents_collect_waits_and_expires_at_its_tasking_lead_deadline() {
    let (r, map) = space_rig(5.0);
    let p = r.first_option().await;
    let id = r.agent_collect(&p.chosen);
    assert_eq!(r.state(id), State::PendingApproval);
    let deadline = map.engine(p.chosen.collect_time.plus_ms(-p.lead_ms));
    let inbox = r.d.inbox_json();
    let pending = &inbox["pending"][0];
    assert_eq!(pending["id"], id.0);
    assert_eq!(pending["expires_t"], deadline.0);
    assert_eq!(pending["space"]["chosen"]["norad_id"], p.chosen.norad_id);
    assert_eq!(pending["space"]["tasking_lead_s"], p.lead_ms / 1000);
    assert!(
        pending["space"].get("exclusions").is_none(),
        "compact answer"
    );

    // not the generic 120 s
    r.clock.advance_ms(121_000);
    assert!(r.clock.now() < deadline);
    r.d.tick();
    assert_eq!(r.state(id), State::PendingApproval);
    r.clock.set(deadline.plus_ms(-1));
    r.d.tick();
    assert_eq!(r.state(id), State::PendingApproval);
    r.clock.set(deadline);
    r.d.tick();
    assert_eq!(r.state(id), State::Expired);
    assert!(r.op_states().is_empty(), "never dispatched");
    r.replays_with_space();
}

#[tokio::test]
async fn an_agents_collect_approved_in_the_inbox_runs() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let id = r.agent_collect(&p.chosen);
    r.clock.advance_ms(5_000);
    let out = r.d.approve(&op1(), id.0, None).await.expect("approve");
    assert_eq!(out.state, State::Dispatched);
    assert!(out.authorization.is_none(), "an inbox approval");
    assert_eq!(out.space.expect("run")["state"], "tasked");
    r.clock.set(map.engine(p.chosen.collect_time));
    r.d.tick();
    assert!(r.op_states().contains(&"collected".to_string()));
    r.replays_with_space();
}

#[tokio::test]
async fn covers_report_predicted_outages_and_end_with_the_window() {
    let (r, map) = space_rig(60.0);
    let window = json!({ "from": t0().0, "to": t0().0 + 2 * 3600 * 1000 });
    let satcom =
        r.d.submit(
            &op1(),
            space_req(json!({
                "task": "cover", "area": area(), "window": window,
                "service": { "type": "satcom", "class": "starlink" }, "min_level": 0.0,
            })),
        )
        .await
        .expect("satcom cover");
    assert_eq!(satcom.state, State::Dispatched, "{}", satcom.intent);
    let gnss =
        r.d.submit(
            &op1(),
            space_req(json!({
                "task": "cover", "area": area(), "window": window,
                "service": { "type": "gnss" }, "min_level": 0.0,
            })),
        )
        .await
        .expect("gnss cover");
    assert_eq!(gnss.state, State::Dispatched, "{}", gnss.intent);
    let outages = satcom.space.expect("run")["cover"]["outages"]
        .as_array()
        .map_or(0, Vec::len);
    // a tick a second: a scenario minute at a time
    let end = map.engine(Time(t0().0 + 2 * 3600 * 1000));
    while r.clock.now() < end {
        r.clock.advance_ms(1_000);
        r.d.tick();
    }
    assert_eq!(r.state(satcom.id), State::Done);
    assert_eq!(r.state(gnss.id), State::Done);
    let states = r.op_states();
    let n = |s: &str| states.iter().filter(|x| *x == s).count();
    assert_eq!(n("outage"), outages);
    assert_eq!(n("restored"), outages);
    assert_eq!(n("watching"), 2);
    assert_eq!(n("ended"), 2);
    assert_eq!(n("fewest_in_view"), 1);
    let log = r.log.text();
    assert!(log.contains("\"uid\":\"satcom:starlink\""));
    assert!(log.contains("\"uid\":\"gnss:gps\""));
    r.replays_with_space();
}

#[tokio::test]
async fn a_cover_below_its_level_is_refused_with_the_outages() {
    let (r, _) = space_rig(60.0);
    let out =
        r.d.submit(
            &op1(),
            space_req(json!({
                "task": "cover", "area": area(),
                "window": { "from": t0().0, "to": t0().0 + 2 * 3600 * 1000 },
                "service": { "type": "gnss" }, "min_level": 40.0,
            })),
        )
        .await
        .expect("cover");
    assert_eq!(out.state, State::Rejected);
    assert!(out.space.is_none());
    assert!(
        r.reason(out.id).contains("coverage"),
        "{}",
        r.reason(out.id)
    );
    r.replays_with_space();
}

// ---- rules 11: cancel after dispatch, re-check on a reloaded segment, covers over the area

/// The fixture with satellite `norad` left out: what a reload of the element sets can do.
fn space_without(norad: u64) -> Arc<murmur_space::Space> {
    let raw: Vec<Value> = serde_json::from_str(GP_FIXTURE).expect("fixture");
    let kept: Vec<Value> = raw
        .into_iter()
        .filter(|r| r["NORAD_CAT_ID"].as_u64() != Some(norad))
        .collect();
    let text = serde_json::to_string(&kept).expect("json");
    let catalog =
        murmur_space::Catalog::parse(include_str!("../../../../../sim/space/constellations.toml"))
            .expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, text.clone()))
        .collect();
    Arc::new(murmur_space::Space::from_sources(catalog, &sources).expect("space"))
}

fn cover_req(class: &str, hours: i64) -> IntentRequest {
    space_req(json!({
        "task": "cover", "area": area(),
        "window": { "from": t0().0, "to": t0().0 + hours * 3600 * 1000 },
        "service": { "type": "satcom", "class": class },
    }))
}

#[tokio::test]
async fn an_operator_cancels_a_dispatched_collect_from_the_console() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let out =
        r.d.submit(&op1(), collect_req(Some(&p.chosen)))
            .await
            .expect("collect");
    assert_eq!(out.state, State::Dispatched);
    r.clock.advance_ms(5_000);
    let out =
        r.d.cancel(&op1(), out.id.0, Some("weather closed in".into()))
            .expect("cancel");
    assert_eq!(out.state, State::Aborted);
    let run = out.space.clone().expect("run");
    assert_eq!(run["outcome"], "cancelled");
    assert_eq!(run["state"], "cancelled");
    assert_eq!(r.op_states(), ["tasked", "cancelled"]);
    assert!(r.reason(out.id).starts_with("cancelled by op-1: weather closed in"), "{}", r.reason(out.id));
    let detail = run["events"][1]["detail"].as_str().unwrap_or("");
    assert!(detail.starts_with("tasking request withdrawn"), "{detail}");
    // the pass comes and goes: the executor reports nothing more
    r.clock.set(map.engine(p.chosen.collect_time).plus_ms(1_000));
    r.d.tick();
    assert_eq!(r.op_states(), ["tasked", "cancelled"]);
    assert_eq!(r.state(out.id), State::Aborted);
    assert_eq!(r.d.space_runs_json()["runs"][0]["outcome"], "cancelled");
    // cancelling again, or an ended one, is refused
    assert!(r.d.cancel(&op1(), out.id.0, None).is_err());
    r.replays_with_space();
}

#[tokio::test]
async fn a_reloaded_segment_rechecks_every_running_space_task() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let collect =
        r.d.submit(&op1(), collect_req(Some(&p.chosen)))
            .await
            .expect("collect");
    assert_eq!(collect.state, State::Dispatched);
    let cover = r.d.submit(&op1(), cover_req("wgs", 2)).await.expect("cover");
    assert_eq!(cover.state, State::Dispatched, "{}", cover.intent);
    assert!(
        cover.space.clone().expect("run")["cover"]["summary"]
            .as_str()
            .unwrap_or("")
            .contains("sample points over the area"),
        "the watch is over the area's sample points"
    );
    r.clock.advance_ms(5_000);
    // the same element sets again: the segment is logged again, every answer is unchanged
    let before = r.log.lines().len();
    r.d.install_space(fixture_space(), map);
    assert_eq!(r.events("space.loaded"), 2);
    assert_eq!(r.log.lines().len(), before + 1, "nothing but the segment logged");
    assert_eq!(r.op_states(), ["tasked", "watching"]);
    // the element sets without the chosen satellite: the collect's pass is gone, the cover
    // (WGS, untouched) holds on the new segment
    r.d.install_space(space_without(p.chosen.norad_id), map);
    assert_eq!(r.state(collect.id), State::Failed);
    assert!(
        r.reason(collect.id).starts_with("collection missed: no longer feasible: feasibility: NORAD"),
        "{}",
        r.reason(collect.id)
    );
    assert_eq!(r.state(cover.id), State::Dispatched);
    assert_eq!(r.op_states(), ["tasked", "watching", "missed", "rechecked"]);
    let runs = r.d.space_runs_json();
    let by_id = |id: IntentId| {
        runs["runs"]
            .as_array()
            .expect("runs")
            .iter()
            .find(|v| v["intent_id"] == id.0)
            .cloned()
            .expect("run")
    };
    assert_eq!(by_id(collect.id)["outcome"], "failed");
    assert_eq!(by_id(cover.id)["outcome"], Value::Null);
    // the collect's pass comes and goes without a word; the cover's watch runs to its end
    let end = map.engine(Time(t0().0 + 2 * 3600 * 1000));
    while r.clock.now() < end {
        r.clock.advance_ms(60_000);
        r.d.tick();
    }
    assert_eq!(r.state(collect.id), State::Failed);
    assert_eq!(r.state(cover.id), State::Done);
    let states = r.op_states();
    assert_eq!(states.iter().filter(|s| *s == "missed").count(), 1);
    assert_eq!(states.last().map(String::as_str), Some("ended"));
    // two segments in the log: re-decided from the logged answers alone
    r.replays();
}

// ---- the review of 8738abd: the lock, the pass that has come, the pin ------------------

/// A watcher that keeps taking the decision lock and remembers the longest it waited.
struct LockWatch {
    stop: Arc<std::sync::atomic::AtomicBool>,
    worst: Arc<std::sync::Mutex<std::time::Duration>>,
    handle: std::thread::JoinHandle<()>,
}

fn watch_lock(d: Arc<Decision>) -> LockWatch {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worst = Arc::new(std::sync::Mutex::new(std::time::Duration::ZERO));
    let (s, w) = (stop.clone(), worst.clone());
    let handle = std::thread::spawn(move || {
        while !s.load(std::sync::atomic::Ordering::SeqCst) {
            let t = std::time::Instant::now();
            let guard = d.lock();
            let waited = t.elapsed();
            drop(guard);
            let mut m = w.lock().expect("worst");
            if waited > *m {
                *m = waited;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    LockWatch { stop, worst, handle }
}

impl LockWatch {
    fn worst(self) -> std::time::Duration {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.handle.join().expect("watcher");
        *self.worst.lock().expect("worst")
    }
}

/// A week of Starlink over a 50 km circle: what the review measured at 13 to 14 s under the
/// lock.
fn long_cover_req() -> IntentRequest {
    space_req(json!({
        "task": "cover",
        "area": { "kind": "circle", "center": [-122.47, 37.825], "radius_km": 50.0 },
        "window": { "from": t0().0, "to": t0().0 + 7 * 24 * 3600 * 1000 },
        "service": { "type": "satcom", "class": "starlink" }, "min_level": 0.0,
    }))
}

/// The fixture pinned to a given shift (the same elements, another pin).
fn pinned_space(days: Option<i64>) -> Arc<murmur_space::Space> {
    let catalog =
        murmur_space::Catalog::parse(include_str!("../../../../../sim/space/constellations.toml"))
            .expect("catalog");
    let sources: Vec<(String, String)> = catalog
        .queries()
        .into_iter()
        .map(|q| (q, GP_FIXTURE.to_string()))
        .collect();
    let mut s = murmur_space::Space::from_sources(catalog, &sources).expect("space");
    s.pinned_shift_days = days;
    Arc::new(s)
}

/// How long the decision lock may be held while a long cover is submitted or previewed:
/// the query (seconds of SGP4) runs outside it, and what stays under it is the decision on
/// the answer and its audit write (50 to 100 ms for a week of Starlink over nine points on
/// the fixture, more on a loaded host). Half a second is a tenth of the picture-gap limit
/// that aborts engagements (5 s) and a thirtieth of what the review measured (13 to 14 s).
const LOCK_HOLD_BOUND: std::time::Duration = std::time::Duration::from_millis(500);

/// The lock is never held longer than the bound while a long cover is submitted: the query
/// runs outside it, the decision on the answer under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_cover_never_holds_the_decision_lock_long() {
    let (r, _) = space_rig(1.0);
    let watch = watch_lock(r.d.clone());
    let started = std::time::Instant::now();
    let out = r.d.submit(&op1(), long_cover_req()).await.expect("cover");
    let took = started.elapsed();
    assert_eq!(out.state, State::Dispatched, "{}", out.intent);
    let worst = watch.worst();
    eprintln!("long cover: submit {took:?}, longest lock wait {worst:?}");
    assert!(worst < LOCK_HOLD_BOUND, "the lock was held {worst:?}");
    // and a tick, which the engagement watch depends on, goes through
    r.d.tick();
    r.replays_with_space();
}

/// An agent's dry run of a long cover blocks neither the tick nor an operator's read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agents_preview_of_a_long_cover_cannot_block_the_tick() {
    let (r, _) = space_rig(1.0);
    let watch = watch_lock(r.d.clone());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slowest = Arc::new(std::sync::Mutex::new(std::time::Duration::ZERO));
    let ticker = {
        let (d, s, w) = (r.d.clone(), stop.clone(), slowest.clone());
        std::thread::spawn(move || {
            while !s.load(std::sync::atomic::Ordering::SeqCst) {
                let t = std::time::Instant::now();
                d.tick();
                let _ = d.inbox_json();
                let took = t.elapsed();
                let mut m = w.lock().expect("slowest");
                if took > *m {
                    *m = took;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        })
    };
    let mut req = long_cover_req();
    req.rationale = Some("relay for the whole week".into());
    let pv = r.d.agent_preview(&AgentId::new("agent-planner"), req).await.expect("preview");
    assert_eq!(pv["would_reach"], "auto_approved", "{pv}");
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    ticker.join().expect("ticker");
    let tick_worst = *slowest.lock().expect("slowest");
    let worst = watch.worst();
    eprintln!("agent preview: longest lock wait {worst:?}, slowest tick and inbox read {tick_worst:?}");
    assert!(worst < LOCK_HOLD_BOUND, "the lock was held {worst:?}");
    assert!(tick_worst < LOCK_HOLD_BOUND, "a tick took {tick_worst:?}");
    assert_eq!(r.events("intent.created"), 0, "a preview creates no intent");
}

/// A collect whose pass has come was collected: a reload without its satellite afterwards
/// does not miss it.
#[tokio::test]
async fn a_reload_after_the_pass_does_not_miss_a_collected_collect() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let out = r.d.submit(&op1(), collect_req(Some(&p.chosen))).await.expect("collect");
    assert_eq!(out.state, State::Dispatched);
    r.clock.set(map.engine(p.chosen.collect_time).plus_ms(1_000));
    r.d.tick();
    assert_eq!(r.op_states(), ["tasked", "collected"]);
    assert_eq!(r.state(out.id), State::Dispatched, "waiting for its product");
    r.d.install_space(space_without(p.chosen.norad_id), map);
    assert_eq!(r.state(out.id), State::Dispatched);
    assert_eq!(r.op_states(), ["tasked", "collected"], "nothing re-checked");
    assert!(!r.log.text().contains("\"phase\":\"dispatched\""));
    match p.product {
        Some(t) => {
            r.clock.set(map.engine(t));
            r.d.tick();
            assert_eq!(r.state(out.id), State::Done);
        }
        None => assert_eq!(r.state(out.id), State::Done),
    }
    r.replays();
}

/// Rules 13 on the bridge's own clock (`clock::test_clock`: pinned to the rig's start, its
/// wall clock stepping back as this host's did, the engine on `clock::wall`): a collect
/// chosen and dispatched (its answers prepared outside the lock), then re-checked on a
/// reloaded segment without its satellite and missed. Every decision on an answer says when
/// it was computed, each collect's request time is that time's scenario time, and the log
/// replays.
#[tokio::test(start_paused = true)]
async fn space_answers_on_the_bridges_own_clock_carry_their_times_and_replay() {
    let r = rig_clocked(Policy::load(&root().join("sim/policy.toml")).expect("policy"), murmur_decide::AuthMode::Dev, EngineClock::Bridge);
    let map = ScenarioMap {
        engine_origin: snapshot::ms(clock::wall()),
        scenario_origin: t0(),
        rate: 1.0,
    };
    r.d.install_space(fixture_space(), map);
    let p = r.first_option().await;
    sleep(Duration::from_secs(3)).await;
    clock::test_clock::step_utc(-0.7);
    let out = r.d.submit(&op1(), collect_req(Some(&p.chosen))).await.expect("collect");
    assert_eq!(out.state, State::Dispatched, "{}", out.intent);
    for _ in 0..3 {
        sleep(Duration::from_secs(16)).await;
        clock::test_clock::step_utc(-0.7);
        r.d.tick();
    }
    r.d.install_space(space_without(p.chosen.norad_id), map);
    assert_eq!(r.state(out.id), State::Failed);
    let text = r.log.text();
    let answered = text.matches("\"status\":\"answered\"").count();
    assert!(answered >= 4, "{text}");
    assert_eq!(text.matches("\"computed_t\":").count(), answered);
    assert!(text.contains("\"phase\":\"dispatched\""));
    r.replays();
}

/// The same elements on the same pin are the same segment (only `space.loaded`); pinned
/// otherwise they would be another, which rules 13 refuses mid-run (`SPACE_REPLAY_SINCE`:
/// the bridge pins once per session): nothing logged, the cover not re-checked, the segment
/// the engine and the bridge hold unchanged.
#[tokio::test]
async fn another_pin_is_refused_mid_run() {
    let (r, map) = space_rig(60.0);
    let cover = r.d.submit(&op1(), cover_req("wgs", 2)).await.expect("cover");
    assert_eq!(cover.state, State::Dispatched, "{}", cover.intent);
    let before = r.log.lines().len();
    r.d.install_space(pinned_space(None), map);
    assert_eq!(r.log.lines().len(), before + 1, "the same segment and pin: only space.loaded");
    assert_eq!(r.op_states(), ["watching"]);
    r.d.install_space(pinned_space(Some(-3)), map);
    assert_eq!(r.log.lines().len(), before + 1, "a re-pinned segment refused: nothing logged");
    assert_eq!(r.op_states(), ["watching"]);
    assert!(!r.log.text().contains("pinned -3 d"));
    let held = r.d.space.read().unwrap_or_else(|e| e.into_inner()).clone().expect("a segment");
    assert_eq!(murmur_decide::SpaceOracle::pinned_shift_days(held.as_ref()), None);
    assert_eq!(r.state(cover.id), State::Dispatched);
    r.replays();
}

/// A re-check whose later answer could not be computed keeps the task and tells the
/// operator; a lost pass is an inbox alert with the fresh options.
#[tokio::test]
async fn a_lost_pass_and_an_unanswered_recheck_reach_the_inbox() {
    let (r, map) = space_rig(60.0);
    let p = r.first_option().await;
    let out = r.d.submit(&op1(), collect_req(Some(&p.chosen))).await.expect("collect");
    assert_eq!(out.state, State::Dispatched);
    r.clock.advance_ms(5_000);
    r.d.install_space(space_without(p.chosen.norad_id), map);
    assert_eq!(r.state(out.id), State::Failed);
    let inbox = r.d.inbox_json();
    let alerts = inbox["alerts"].as_array().expect("alerts");
    let lost = alerts.iter().find(|a| a["kind"] == "space_missed").expect("an alert for the lost pass");
    assert_eq!(lost["intent_id"], out.id.0);
    assert!(lost["detail"].as_str().unwrap_or("").contains("no longer feasible"), "{lost}");
}
