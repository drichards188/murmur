//! Rules 5: the safe command queue. Orders for vehicles that can't be reached are held with
//! an expiry, re-checked against the world when they can be, dispatched once, sent back to
//! the inbox when a human's picture changed, expired at their deadline, and carried over a
//! bridge restart; replay recomputes every step and binds each dispatch to its re-check.
#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use murmur_decide::audit::EventBody;
use murmur_decide::replay::{ReplayOptions, ReplayReport, replay_str, replay_str_with};
use murmur_decide::*;

struct Fx {
    clock: ManualClock,
    engine: Engine,
    log: MemorySink,
    session: OperatorSession,
}

fn fx_on(policy: Policy) -> Fx {
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    let mut engine = Engine::new(
        policy,
        roster(),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    let session = engine.open_operator_session(OperatorId::new("op-1")).unwrap();
    Fx {
        clock,
        engine,
        log,
        session,
    }
}

fn fx() -> Fx {
    fx_on(policy())
}

/// The world at the fixture clock's time, every fact fresh then.
fn world_now(fx: &Fx) -> WorldSnapshot {
    let mut w = world();
    let t = fx.clock.now();
    w.t = t;
    for v in w.vehicles.values_mut() {
        restamp(v, t);
    }
    w
}

fn restamp(v: &mut VehicleFacts, t: Time) {
    if let Some(f) = &mut v.autopilot {
        f.t = t;
    }
    if let Some(f) = &mut v.nav_ready {
        f.t = t;
    }
    if let Some(f) = &mut v.position {
        f.t = t;
    }
    if let Some(f) = &mut v.battery_pct {
        f.t = t;
    }
    v.report_interval_ms = Some(1_000);
}

/// `uid` last heard `silent_s` ago (its other facts as fresh as that), the rest fresh.
fn silent(fx: &Fx, uid: &str, silent_s: i64) -> WorldSnapshot {
    let mut w = world_now(fx);
    let then = fx.clock.now().plus_ms(-silent_s * 1000);
    restamp(w.vehicles.get_mut(&Uid::new(uid)).unwrap(), then);
    w
}

fn replays(fx: &Fx) -> ReplayReport {
    let r = replay_str(&fx.log.text()).unwrap();
    assert!(r.ok(), "{:?}", r.mismatches);
    r
}

fn count(fx: &Fx, kind: &str) -> usize {
    fx.log
        .lines()
        .iter()
        .filter(|l| l.contains(&format!("\"type\":\"{kind}\"")))
        .count()
}

fn orbit_sky() -> Proposal {
    orbit(NEAR_CRISSY, 150.0, &["sky-1"])
}

impl Fx {
    fn submit(&mut self, p: Proposal, w: &WorldSnapshot) -> IntentId {
        self.engine
            .submit(self.session.authority(), p, w, &terrain())
            .unwrap()
            .id()
    }

    fn state(&self, id: IntentId) -> State {
        self.engine.intent(id).unwrap().state()
    }

    fn advance(&self, s: i64) {
        self.clock.set(self.clock.now().plus_ms(s * 1000));
    }
}

#[test]
fn an_order_for_a_silent_vehicle_is_queued_then_dispatched_once_on_reconnect() {
    let mut fx = fx();
    // the orbit passes its checks on facts 5 s old (fresh enough to decide, limit 10 s),
    // but the link counts as lost after 3 s: queued, not sent into a dead link
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    assert_eq!(fx.state(id), State::AutoApproved);
    let routed = fx.engine.dispatch_or_queue(id, &w).unwrap();
    let Routed::Queued { expires_t, reach } = routed else {
        panic!("{routed:?}")
    };
    assert_eq!(fx.state(id), State::Queued);
    assert_eq!(expires_t, fx.clock.now().plus_ms(900_000));
    assert!(!reach[0].reachable);
    assert!(reach[0].detail.starts_with("silent for 5.0 s"), "{}", reach[0].detail);
    // still silent: nothing happens, nothing is logged
    fx.advance(10);
    let lines = fx.log.lines().len();
    let w = silent(&fx, "sky-1", 15);
    assert!(matches!(
        fx.engine.dequeue(id, &w, &terrain()).unwrap(),
        Dequeued::Waiting(_)
    ));
    assert_eq!(fx.log.lines().len(), lines);
    // heard again: re-checked on the world now, then dispatched (once)
    fx.advance(10);
    let w = world_now(&fx);
    let out = fx.engine.dequeue(id, &w, &terrain()).unwrap();
    let Dequeued::Dispatched(Order::Vehicles(o)) = out else {
        panic!("{out:?}")
    };
    assert_eq!(o.task, orbit_sky().task);
    assert_eq!(fx.state(id), State::Dispatched);
    // a second dequeue is refused: it left the queue
    assert!(fx.engine.dequeue(id, &w, &terrain()).is_err());
    assert_eq!(count(&fx, "intent.dispatched"), 1);
    assert_eq!(count(&fx, "queue.recheck"), 1);
    let r = replays(&fx);
    assert_eq!((r.queued, r.rechecks, r.recheck_dispatched), (1, 1, 1));
    assert!(r.open.iter().any(|o| o.spec.id == id && o.state == State::Dispatched));
}

#[test]
fn a_reachable_vehicle_is_dispatched_at_once() {
    let mut fx = fx();
    let w = world_now(&fx);
    let id = fx.submit(orbit_sky(), &w);
    assert!(matches!(
        fx.engine.dispatch_or_queue(id, &w).unwrap(),
        Routed::Dispatched(_)
    ));
    assert_eq!(count(&fx, "intent.queued"), 0);
    replays(&fx);
}

#[test]
fn a_mission_cant_cross_an_acoustic_link_but_a_hold_can() {
    let mut fx = fx();
    // remus-1 under water: reports every 20 s, heard 15 s ago (fresh for its link)
    let mut w = world_now(&fx);
    let t = fx.clock.now().plus_ms(-15_000);
    let remus = w.vehicles.get_mut(&uid("remus-1")).unwrap();
    restamp(remus, t);
    remus.report_interval_ms = Some(20_000);
    // a hold is one command: it goes over the modem
    let hold_id = fx.submit(hold(&["remus-1"]), &w);
    assert!(matches!(
        fx.engine.dispatch_or_queue(hold_id, &w).unwrap(),
        Routed::Dispatched(_)
    ));
    // a newer order, a patrol: a mission upload can't cross the modem, so it waits
    let patrol = Proposal::new(
        Task::Patrol {
            route: vec![ON_WATER, BAY],
        },
        vec![uid("remus-1")],
    );
    let id = fx.submit(patrol, &w);
    assert!(
        matches!(fx.state(id), State::AutoApproved | State::Approved),
        "{:?}",
        fx.engine.intent(id).unwrap().history()
    );
    let Routed::Queued { reach, .. } = fx.engine.dispatch_or_queue(id, &w).unwrap() else {
        panic!("not queued")
    };
    assert!(reach[0].detail.contains("a mission upload can't cross it"), "{}", reach[0].detail);
    // surfaced: streaming again; the patrol goes
    fx.advance(30);
    let w = world_now(&fx);
    assert!(matches!(
        fx.engine.dequeue(id, &w, &terrain()).unwrap(),
        Dequeued::Dispatched(_)
    ));
    replays(&fx);
}

#[test]
fn a_queued_order_expires_at_its_deadline_with_a_reason() {
    let mut p = policy();
    p.queue = Some(QueueConfig {
        expire_s: 60,
        ..QueueConfig::default()
    });
    let mut fx = fx_on(p);
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.advance(59);
    assert!(fx.engine.expire_due().unwrap().is_empty());
    fx.advance(1);
    assert_eq!(fx.engine.expire_due().unwrap(), vec![id]);
    assert_eq!(fx.state(id), State::Expired);
    let why = &fx.engine.intent(id).unwrap().history().last().unwrap().reason;
    assert!(why.starts_with("not dispatched within 60 s"), "{why}");
    assert!(why.contains("silent for"), "{why}");
    let r = replays(&fx);
    assert_eq!(r.queue_expired, 1);
}

#[test]
fn safety_commands_have_a_short_queue_or_none() {
    let mut p = policy();
    let mut q = QueueConfig::default();
    q.kind.insert(TaskKind::Hold, KindQueue { expire_s: Some(0) });
    q.kind.insert(TaskKind::Rtl, KindQueue { expire_s: Some(30) });
    p.queue = Some(q);
    let mut fx = fx_on(p);
    let w = silent(&fx, "sky-1", 5);
    // hold: never queued, sent at once whatever the link (the task layer retries)
    let h = fx.submit(hold(&["sky-1"]), &w);
    assert!(matches!(
        fx.engine.dispatch_or_queue(h, &w).unwrap(),
        Routed::Dispatched(_)
    ));
    // rtl: 30 s
    let r = fx.submit(rtl(&["sky-1"]), &w);
    let Routed::Queued { expires_t, .. } = fx.engine.dispatch_or_queue(r, &w).unwrap() else {
        panic!("rtl not queued")
    };
    assert_eq!(expires_t, fx.clock.now().plus_ms(30_000));
    replays(&fx);
    // the bounds: a wait over an hour, or a queue for satellites, is refused
    let mut bad = QueueConfig::default();
    bad.kind.insert(TaskKind::Hold, KindQueue { expire_s: Some(7_200) });
    assert!(bad.validate().is_err());
    let mut bad = QueueConfig::default();
    bad.kind.insert(TaskKind::Collect, KindQueue { expire_s: Some(60) });
    assert!(bad.validate().is_err());
    assert!(QueueConfig { expire_s: 0, ..QueueConfig::default() }.validate().is_err());
}

#[test]
fn a_failing_recheck_voids_it() {
    let mut fx = fx();
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    // back, but its battery is now below what the orbit needs: the reserves rule fails
    fx.advance(20);
    let mut w = world_now(&fx);
    w.vehicles.get_mut(&uid("sky-1")).unwrap().battery_pct = Some(Fact::new(5.0, fx.clock.now()));
    assert_eq!(fx.engine.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Voided);
    assert_eq!(fx.state(id), State::Voided);
    let why = &fx.engine.intent(id).unwrap().history().last().unwrap().reason;
    assert!(why.starts_with("re-check failed at dequeue"), "{why}");
    assert!(why.contains("reserves sky-1"), "{why}");
    assert_eq!(count(&fx, "intent.dispatched"), 0);
    let r = replays(&fx);
    assert_eq!(r.recheck_voided, 1);
}

#[test]
fn a_human_approval_whose_picture_changed_goes_back_to_the_inbox() {
    let mut fx = fx();
    // an armed platform: a human tier, approved by the operator's own submission
    let w = silent(&fx, "blade-1", 5);
    let p = orbit(NEAR_CRISSY, 200.0, &["blade-1"]);
    let id = fx.submit(p, &w);
    assert_eq!(fx.state(id), State::Approved);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    assert_eq!(fx.state(id), State::Queued);
    // back after a reboot (within the human bound): the vehicle booted after the approval
    fx.advance(60);
    let mut w = world_now(&fx);
    w.vehicles.get_mut(&uid("blade-1")).unwrap().booted_t = Some(fx.clock.now().plus_ms(-10_000));
    assert_eq!(fx.engine.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Inbox);
    let i = fx.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.approval().is_none(), "the old approval does not stand");
    let rc = i.queue().unwrap().recheck.as_ref().unwrap();
    assert_eq!(rc.verdict, Verdict::Inbox);
    assert!(rc.changes[0].contains("rebooted after the approval"), "{:?}", rc.changes);
    assert!(fx.engine.inbox().iter().any(|i| i.id() == id));
    // a human approves it again (re-checked): dispatched then
    let i = fx
        .engine
        .approve(id, fx.session.authority(), None, &w, &terrain())
        .unwrap();
    assert_eq!(i.state(), State::Approved);
    assert!(matches!(
        fx.engine.dispatch_or_queue(id, &w).unwrap(),
        Routed::Dispatched(_)
    ));
    let r = replays(&fx);
    assert_eq!(r.recheck_inbox, 1);
    // an auto-approved order is dispatched to the rebooted vehicle: no human relied on it
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.advance(120);
    let mut w = world_now(&fx);
    w.vehicles.get_mut(&uid("sky-1")).unwrap().booted_t = Some(fx.clock.now().plus_ms(-60_000));
    assert!(matches!(
        fx.engine.dequeue(id, &w, &terrain()).unwrap(),
        Dequeued::Dispatched(_)
    ));
    replays(&fx);
}

#[test]
fn an_agents_order_whose_tier_turned_human_goes_to_the_inbox() {
    let mut fx = fx();
    let w = silent(&fx, "sky-1", 5);
    let id = fx
        .engine
        .agent_port(AgentId::new("planner"))
        .propose(orbit_sky(), rationale("cover the tower"), &w, &terrain())
        .unwrap()
        .id();
    assert_eq!(fx.state(id), State::AutoApproved);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    // back, but now just outside the approved area (it drifted east, past -122.39): its
    // transit leg leaves it, so policy needs a human
    fx.advance(20);
    let mut w = world_now(&fx);
    let now = fx.clock.now();
    let drifted = LonLat::new(-122.385, 37.80);
    w.vehicles.get_mut(&uid("sky-1")).unwrap().position = Some(Fact::new(drifted, now));
    let out = fx.engine.dequeue(id, &w, &terrain()).unwrap();
    assert_eq!(out, Dequeued::Inbox, "{:?}", fx.engine.intent(id).unwrap().queue());
    let r = replays(&fx);
    assert_eq!(r.recheck_inbox, 1);
}

#[test]
fn a_cancelled_queued_order_never_goes() {
    let mut fx = fx();
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.engine
        .cancel(id, fx.session.authority(), "changed my mind")
        .unwrap();
    assert_eq!(fx.state(id), State::Voided);
    let w = world_now(&fx);
    assert!(fx.engine.dequeue(id, &w, &terrain()).is_err());
    replays(&fx);
}

/// A forger's edit of the log, re-chained (only recomputation can catch it).
fn forged(fx: &Fx, edit: impl FnOnce(&mut Vec<Event>)) -> Vec<String> {
    let text = forge(&fx.log.text(), edit);
    let r = replay_str(&text).unwrap();
    r.mismatches.iter().map(|m| m.what.clone()).collect()
}

fn queued_then_dispatched() -> (Fx, IntentId) {
    let mut fx = fx();
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.advance(20);
    let w = world_now(&fx);
    fx.engine.dequeue(id, &w, &terrain()).unwrap();
    (fx, id)
}

#[test]
fn replay_catches_a_dispatch_from_the_queue_without_its_recheck() {
    let (fx, _) = queued_then_dispatched();
    // the re-check and its check removed: the dispatch from the queue has no evidence
    let whats = forged(&fx, |ev| {
        ev.retain(|e| {
            !matches!(e.body, EventBody::QueueRecheck { .. })
                && !matches!(&e.body, EventBody::CheckResult { phase, .. } if *phase == audit::CheckPhase::Dequeue)
        })
    });
    assert!(
        whats.iter().any(|w| w.contains("dispatched from the queue without a passing re-check")),
        "{whats:?}"
    );
    // the verdict flipped to dispatch over a failing re-check
    let mut fx2 = fx_on(policy());
    let w = silent(&fx2, "sky-1", 5);
    let id = fx2.submit(orbit_sky(), &w);
    fx2.engine.dispatch_or_queue(id, &w).unwrap();
    fx2.advance(20);
    let mut w = world_now(&fx2);
    w.vehicles.get_mut(&uid("sky-1")).unwrap().battery_pct = Some(Fact::new(5.0, fx2.clock.now()));
    fx2.engine.dequeue(id, &w, &terrain()).unwrap();
    let whats = forged(&fx2, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::QueueRecheck { verdict, .. } = &mut e.body {
                *verdict = Verdict::Dispatch;
            }
        }
    });
    assert!(whats.iter().any(|w| w.contains("re-check verdict differs")), "{whats:?}");
}

#[test]
fn replay_catches_a_queue_that_was_not_needed_or_a_stretched_deadline() {
    let (fx, _) = queued_then_dispatched();
    // the vehicle made to look heard just now: it was reachable, nothing to queue
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentQueued { inputs, .. } = &mut e.body {
                let t = inputs.t;
                if let Some(v) = inputs.vehicles.get_mut(&uid("sky-1"))
                    && let Some(a) = &mut v.autopilot
                {
                    a.t = t;
                }
            }
        }
    });
    assert!(whats.iter().any(|w| w.contains("reach differs")), "{whats:?}");
    assert!(
        whats.iter().any(|w| w.contains("queued although every vehicle could be reached")),
        "{whats:?}"
    );
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentQueued { expires_t, .. } = &mut e.body {
                *expires_t = expires_t.plus_ms(3_600_000);
            }
        }
    });
    assert!(whats.iter().any(|w| w.contains("queue deadline differs")), "{whats:?}");
}

#[test]
fn replay_binds_the_dequeued_dispatch_to_the_approved_order() {
    let (fx, _) = queued_then_dispatched();
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentDispatched { task, .. } = &mut e.body {
                *task = Task::Orbit {
                    center: OUTSIDE_APPROVED,
                    radius_m: 150.0,
                };
            }
        }
    });
    assert!(
        whats.iter().any(|w| w.contains("dispatched task differs from the approved intent's")),
        "{whats:?}"
    );
}

#[test]
fn a_rules_4_claim_cant_carry_a_queue() {
    let (fx, _) = queued_then_dispatched();
    let text = fx
        .log
        .text()
        .replacen(&format!("\"rules_version\":{}", murmur_decide::RULES_VERSION), "\"rules_version\":4", 1);
    let text = forge(&text, |_| {});
    let r = replay_str_with(
        &text,
        &ReplayOptions {
            rules: replay::RulesAccepted::Exactly(4),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let whats: Vec<&str> = r.mismatches.iter().map(|m| m.what.as_str()).collect();
    assert!(
        whats.iter().any(|w| w.contains("a queued intent: only builds of rules 5")),
        "{whats:?}"
    );
}

// ---- restart -------------------------------------------------------------------------

/// Run A queues an orbit; the bridge dies; run B carries it over from A's log.
fn restarted() -> (Fx, Fx, IntentId) {
    let mut a = fx();
    // two intents before it, so B's ids continue after A's
    let w = world_now(&a);
    a.submit(hold(&["boat-1"]), &w);
    let w = silent(&a, "sky-1", 5);
    let id = a.submit(orbit_sky(), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    let ra = replay_str(&a.log.text()).unwrap();
    assert!(ra.ok(), "{:?}", ra.mismatches);
    let open: Vec<&OpenIntent> = ra.open.iter().filter(|o| o.state == State::Queued).collect();
    assert_eq!(open.len(), 1);
    // run B: a new engine and log, 30 s later
    let b = fx();
    b.clock.set(a.clock.now().plus_ms(30_000));
    let mut b = b;
    let from = ResumedFrom {
        log: "a.jsonl".into(),
        head: ra.head_sha256.clone().unwrap(),
        seq: ra.last_seq,
        was: open[0].state,
    };
    b.engine.reserve_ids(ra.next_intent);
    assert_eq!(b.engine.resume(open[0], from).unwrap(), id);
    (a, b, id)
}

/// The queue is rules 5: an engine deciding under rules 5 (not only the current rules)
/// carries a queued intent over from the run before. A regression test for the gate in
/// `Engine::resume` (a rules renumbering once moved it to the next version).
#[test]
fn a_rules_5_engine_carries_a_queued_intent_over() {
    let rules5 = || {
        let mut p = policy();
        p.rules.rules_version = 5;
        p
    };
    let accept5 = ReplayOptions {
        rules: replay::RulesAccepted::Exactly(5),
        ..ReplayOptions::default()
    };
    // the world as a rules 5 bridge logs it: no fact only rules 6 builds write
    let as_rules5 = |mut w: WorldSnapshot| {
        for v in w.vehicles.values_mut() {
            v.alt_msl_m = None;
            if let Some(p) = &mut v.platform {
                p.climb_mps = None;
                p.max_mps = None;
            }
        }
        w
    };
    let mut a = fx_on(rules5());
    let w = as_rules5(silent(&a, "sky-1", 5));
    let id = a.submit(orbit_sky(), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    let ra = replay_str_with(&a.log.text(), &accept5).unwrap();
    assert!(ra.ok(), "{:?}", ra.mismatches);
    assert_eq!(ra.rules_version, 5);
    let open: Vec<&OpenIntent> = ra.open.iter().filter(|o| o.state == State::Queued).collect();
    assert_eq!(open.len(), 1);
    let mut b = fx_on(rules5());
    b.clock.set(a.clock.now().plus_ms(30_000));
    let from = ResumedFrom {
        log: "a.jsonl".into(),
        head: ra.head_sha256.clone().unwrap(),
        seq: ra.last_seq,
        was: open[0].state,
    };
    b.engine.reserve_ids(ra.next_intent);
    assert_eq!(b.engine.resume(open[0], from).unwrap(), id);
    assert_eq!(b.state(id), State::Queued);
    // and the run that carried it replays against the one it came from, under rules 5
    let text_a = a.log.text();
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            previous: Some(&text_a),
            ..accept5
        },
    )
    .unwrap();
    assert!(rb.ok(), "{:?}", rb.mismatches);
    assert_eq!(rb.resumed, 1);
}

#[test]
fn a_queued_order_survives_a_restart_and_goes_once() {
    let (a, mut b, id) = restarted();
    assert_eq!(b.state(id), State::Queued);
    // B's own new intents number on after A's
    let w = world_now(&b);
    let next = b.submit(hold(&["boat-1"]), &w);
    assert!(next.0 > id.0, "{next} after {id}");
    // the vehicle is back: re-checked on B's world against A's approval, then dispatched
    assert!(matches!(
        b.engine.dequeue(id, &w, &terrain()).unwrap(),
        Dequeued::Dispatched(_)
    ));
    // B replays against A
    let a_text = a.log.text();
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            previous: Some(&a_text),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(rb.ok(), "{:?}", rb.mismatches);
    assert_eq!((rb.resumed, rb.recheck_dispatched), (1, 1));
    // without A, the carried-over approval is not taken on trust
    let rb = replay_str(&b.log.text()).unwrap();
    assert!(
        rb.mismatches.iter().any(|m| m.what.contains("not verified")),
        "{:?}",
        rb.mismatches
    );
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            trust_resumed: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(rb.ok(), "{:?}", rb.mismatches);
}

#[test]
fn a_forged_carry_over_is_caught_against_the_previous_log() {
    let (a, b, _) = restarted();
    let a_text = a.log.text();
    let check = |text: &str| -> Vec<String> {
        replay_str_with(
            text,
            &ReplayOptions {
                previous: Some(&a_text),
                ..ReplayOptions::default()
            },
        )
        .unwrap()
        .mismatches
        .iter()
        .map(|m| m.what.clone())
        .collect()
    };
    // the carried basis made to say a human approved it
    let text = forge(&b.log.text(), |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentResumed { basis, .. } = &mut e.body {
                basis.by_human = true;
            }
        }
    });
    let w = check(&text);
    assert!(w.iter().any(|m| m.contains("carried over differently")), "{w:?}");
    // an intent A never had open
    let text = forge(&b.log.text(), |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentResumed { intent, .. } = &mut e.body {
                intent.id = IntentId(77);
            }
        }
    });
    let w = check(&text);
    assert!(w.iter().any(|m| m.contains("does not leave it open")), "{w:?}");
    // a longer deadline than A gave it
    let text = forge(&b.log.text(), |ev| {
        for e in ev.iter_mut() {
            if let EventBody::IntentResumed { expires_t, .. } = &mut e.body {
                *expires_t = expires_t.map(|t| t.plus_ms(3_600_000));
            }
        }
    });
    let w = check(&text);
    assert!(w.iter().any(|m| m.contains("queue deadline differs")), "{w:?}");
    // another previous log than the one named
    let other = fx();
    let w = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            previous: Some(&other.log.text()),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(
        w.mismatches.iter().any(|m| m.what.contains("another log than the one given")),
        "{:?}",
        w.mismatches
    );
}

#[test]
fn a_started_order_resumes_dispatched_and_is_never_sent_again() {
    let mut a = fx();
    let w = world_now(&a);
    let id = a.submit(orbit_sky(), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    a.engine.record_op_state(id, &uid("sky-1"), "sent", "uploading").unwrap();
    a.engine.record_op_state(id, &uid("sky-1"), "acked", "accepted").unwrap();
    let ra = replay_str(&a.log.text()).unwrap();
    let open = ra.open.iter().find(|o| o.spec.id == id).unwrap();
    assert_eq!(open.committed(), vec![uid("sky-1")]);
    assert_eq!(open.resumes_as(), State::Dispatched);
    let mut b = fx();
    b.clock.set(a.clock.now().plus_ms(10_000));
    let from = ResumedFrom {
        log: "a.jsonl".into(),
        head: ra.head_sha256.clone().unwrap(),
        seq: ra.last_seq,
        was: open.state,
    };
    b.engine.resume(open, from).unwrap();
    assert_eq!(b.state(id), State::Dispatched);
    // followed: its op states are reported, no second intent.dispatched
    b.engine.record_op_state(id, &uid("sky-1"), "active", "adopted").unwrap();
    b.engine.finish(id, TaskOutcome::Done).unwrap();
    assert_eq!(count(&b, "intent.dispatched"), 0);
    let a_text = a.log.text();
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            previous: Some(&a_text),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(rb.ok(), "{:?}", rb.mismatches);
    // an uncommitted dispatch resumes queued (delivered once, after a re-check)
    let mut a = fx();
    let w = world_now(&a);
    let id = a.submit(orbit_sky(), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    a.engine.record_op_state(id, &uid("sky-1"), "sent", "uploading").unwrap();
    let ra = replay_str(&a.log.text()).unwrap();
    let open = ra.open.iter().find(|o| o.spec.id == id).unwrap();
    assert_eq!(open.resumes_as(), State::Queued);
}

#[test]
fn a_reboot_is_logged_and_replayed() {
    let mut fx = fx();
    fx.engine
        .record_reboot(&uid("sky-1"), 812_300, 4_100, fx.clock.now(), None)
        .unwrap();
    // one seen by its boot instant jumping later (an outage longer than its uptime)
    let now = fx.clock.now();
    fx.engine
        .record_reboot(&uid("blade-1"), 60_000, 95_000, now, Some(now.plus_ms(-125_000)))
        .unwrap();
    // neither: refused, nothing logged
    let lines = fx.log.lines().len();
    assert!(fx
        .engine
        .record_reboot(&uid("blade-1"), 60_000, 95_000, now, Some(now.plus_ms(-20_000)))
        .is_err());
    assert_eq!(fx.log.lines().len(), lines);
    let r = replays(&fx);
    assert_eq!(r.reboots, 2);
    let text = forge(&fx.log.text(), |ev| {
        for e in ev.iter_mut() {
            if let EventBody::VehicleRebooted { boot_ms, .. } = &mut e.body {
                *boot_ms = 900_000;
            }
        }
    });
    let r = replay_str(&text).unwrap();
    assert!(!r.ok());
}

#[test]
fn a_link_that_just_came_back_waits_for_the_picture_before_the_recheck() {
    // the heartbeat arrives first; the re-check must not read the position from before the
    // outage (it would fail closed on a stale fact and void the order spuriously)
    let mut fx = fx();
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.advance(40);
    let mut w = world_now(&fx);
    let before = fx.clock.now().plus_ms(-40_000);
    let v = w.vehicles.get_mut(&uid("sky-1")).unwrap();
    v.position = v.position.clone().map(|f| Fact::new(f.value, before));
    let Dequeued::Waiting(reach) = fx.engine.dequeue(id, &w, &terrain()).unwrap() else {
        panic!("dequeued on a stale picture")
    };
    assert!(reach[0].detail.starts_with("heard again, waiting for its position"), "{}", reach[0].detail);
    assert_eq!(count(&fx, "check.result"), 1, "no re-check yet");
    // the position comes in: re-checked, sent
    let w = world_now(&fx);
    assert!(matches!(
        fx.engine.dequeue(id, &w, &terrain()).unwrap(),
        Dequeued::Dispatched(_)
    ));
    replays(&fx);
}

// ---- review fixes ---------------------------------------------------------------------

fn reason(fx: &Fx, id: IntentId) -> String {
    fx.engine.intent(id).unwrap().history().last().unwrap().reason.clone()
}

#[test]
fn a_dispatched_order_whose_link_was_lost_before_the_start_goes_back_to_the_queue() {
    let mut fx = fx();
    let w = world_now(&fx);
    let id = fx.submit(orbit_sky(), &w);
    assert!(matches!(
        fx.engine.dispatch_or_queue(id, &w).unwrap(),
        Routed::Dispatched(_)
    ));
    fx.engine
        .record_op_state(id, &uid("sky-1"), "sent", "uploading 15 mission items")
        .unwrap();
    // its op has not ended so: refused, nothing logged
    let lines = fx.log.lines().len();
    assert!(fx.engine.requeue(id).is_err());
    assert_eq!(fx.log.lines().len(), lines);
    fx.engine
        .record_op_state(
            id,
            &uid("sky-1"),
            "requeued",
            "link lost mid-upload: back to the queue, re-checked before it is sent again",
        )
        .unwrap();
    fx.advance(5);
    let expires_t = fx.engine.requeue(id).unwrap();
    assert_eq!(fx.state(id), State::Queued);
    // the kind's wait from the approval, not a fresh one
    assert_eq!(expires_t, T0.plus_ms(900_000));
    assert!(fx.engine.intent(id).unwrap().dispatched_before());
    assert!(reason(&fx, id).starts_with("back to the queue"), "{}", reason(&fx, id));
    // the link is back: re-checked on the world now, then dispatched again
    fx.advance(20);
    let w = world_now(&fx);
    assert!(matches!(
        fx.engine.dequeue(id, &w, &terrain()).unwrap(),
        Dequeued::Dispatched(_)
    ));
    let r = replays(&fx);
    assert_eq!((r.requeued, r.recheck_dispatched), (1, 1));
    assert!(r.open.iter().any(|o| o.spec.id == id && o.dispatched_before));
    // replay binds the requeue to its evidence
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::OpState { state, .. } = &mut e.body
                && state == "requeued"
            {
                *state = "failed".into();
            }
        }
    });
    assert!(
        whats.iter().any(|w| w.contains("requeued without every vehicle's op ending")),
        "{whats:?}"
    );
    // a vehicle that started it: never requeued
    let mut fx = fx_on(policy());
    let w = world_now(&fx);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.engine.record_op_state(id, &uid("sky-1"), "acked", "accepted").unwrap();
    fx.engine
        .record_op_state(id, &uid("sky-1"), "requeued", "link lost")
        .unwrap();
    assert!(fx.engine.requeue(id).is_err());
    assert_eq!(fx.state(id), State::Dispatched);
}

#[test]
fn an_older_queued_order_never_goes_over_a_newer_one() {
    // review T2: an older order waits in the queue; the operator submits a newer one for
    // the same vehicle that waits for a second approver. The link comes back: the older is
    // voided (superseded), the newer stands
    let mut fx = fx_on(waiting_policy());
    let op2 = fx
        .engine
        .open_operator_session(OperatorId::new("op-2"))
        .unwrap();
    let w = silent(&fx, "blade-1", 5);
    let a = fx.submit(orbit(NEAR_CRISSY, 200.0, &["blade-1"]), &w);
    assert_eq!(fx.state(a), State::PendingApproval);
    fx.engine
        .approve(a, op2.authority(), None, &w, &terrain())
        .unwrap();
    assert!(matches!(
        fx.engine.dispatch_or_queue(a, &w).unwrap(),
        Routed::Queued { .. }
    ));
    let b = fx.submit(orbit(NEAR_CRISSY, 300.0, &["blade-1"]), &w);
    assert_eq!(fx.state(b), State::PendingApproval);
    fx.advance(20);
    let w = world_now(&fx);
    assert_eq!(fx.engine.dequeue(a, &w, &terrain()).unwrap(), Dequeued::Voided);
    assert!(
        reason(&fx, a).contains(&format!("{b} is a newer order for vehicle blade-1")),
        "{}",
        reason(&fx, a)
    );
    assert_eq!(fx.state(b), State::PendingApproval, "the newer order stands");
    replays(&fx);
}

#[test]
fn a_human_approval_held_past_its_bound_goes_back_to_the_inbox() {
    let mut fx = fx();
    // an armed platform: approved by the operator's own submission
    let w = silent(&fx, "blade-1", 5);
    let id = fx.submit(orbit(NEAR_CRISSY, 200.0, &["blade-1"]), &w);
    assert_eq!(fx.state(id), State::Approved);
    let Routed::Queued { expires_t, .. } = fx.engine.dispatch_or_queue(id, &w).unwrap() else {
        panic!("not queued")
    };
    // a human's approval is held 120 s at most (`[queue] human_expire_s`), not the orbit's
    // 900 s
    assert_eq!(expires_t, T0.plus_ms(120_000));
    assert!(fx.engine.intent(id).unwrap().queue().unwrap().inbox);
    fx.advance(119);
    fx.engine.expire_due().unwrap();
    assert_eq!(fx.state(id), State::Queued);
    fx.advance(1);
    fx.engine.expire_due().unwrap();
    let i = fx.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.approval().is_none(), "the old approval does not stand");
    assert!(reason(&fx, id).contains("back to the inbox"), "{}", reason(&fx, id));
    assert!(fx.engine.inbox().iter().any(|i| i.id() == id));
    replays(&fx);
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::QueueExpired { inbox, .. } = &mut e.body {
                *inbox = false;
            }
        }
    });
    assert!(whats.iter().any(|w| w.contains("queue expiry differs")), "{whats:?}");
    // the bound is the policy's, bounded
    let q = |h: u32| QueueConfig {
        human_expire_s: Some(h),
        ..QueueConfig::default()
    };
    assert!(q(0).validate().is_err());
    assert!(q(7_200).validate().is_err());
    assert!(q(300).validate().is_ok());
}

#[test]
fn replay_catches_a_recheck_after_the_deadline_and_a_dispatch_apart_from_its_recheck() {
    let (fx, _) = queued_then_dispatched();
    // review F1: the deadline cut to before the re-check
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::RunStarted { policy, .. } = &mut e.body {
                policy.queue = Some(QueueConfig {
                    expire_s: 10,
                    ..QueueConfig::default()
                });
            }
            if let EventBody::IntentQueued { expires_t, .. } = &mut e.body {
                *expires_t = expires_t.plus_ms(-890_000);
            }
        }
    });
    assert!(
        whats.iter().any(|w| w.contains("re-checked at or after its queue deadline")),
        "{whats:?}"
    );
    // review F1b: the dequeue moved past the deadline
    let whats = forged(&fx, |ev| {
        let mut later = false;
        for e in ev.iter_mut() {
            later |= matches!(&e.body, EventBody::CheckResult { phase, .. } if *phase == audit::CheckPhase::Dequeue);
            if later {
                e.t = e.t.plus_ms(900_000);
            }
        }
    });
    assert!(
        whats.iter().any(|w| w.contains("re-checked at or after its queue deadline")),
        "{whats:?}"
    );
    // the dispatch held back after its re-check (something logged between)
    let whats = forged(&fx, |ev| {
        let i = ev
            .iter()
            .position(|e| matches!(e.body, EventBody::QueueRecheck { .. }))
            .unwrap();
        let mut between = ev[i].clone();
        between.body = EventBody::VehicleEvent {
            uid: uid("sky-1"),
            kind: "mode".into(),
            text: "AUTO -> BRAKE".into(),
        };
        ev.insert(i + 1, between);
    });
    assert!(
        whats.iter().any(|w| w.contains("dispatched from the queue, but not right after its re-check")),
        "{whats:?}"
    );
}

#[test]
fn replay_catches_a_direct_dispatch_to_a_vehicle_the_link_could_not_reach() {
    // review F9: the queueing replaced by a dispatch straight to the silent vehicle
    let mut fx = fx();
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    assert_eq!(fx.state(id), State::Queued);
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            match &mut e.body {
                EventBody::IntentQueued { intent, .. } => {
                    let intent = *intent;
                    e.body = EventBody::IntentDispatched {
                        intent,
                        task: orbit_sky().task,
                        uids: vec![uid("sky-1")],
                        space: None,
                        engage: None,
                        inputs: None,
                        reach: None,
                    };
                }
                EventBody::IntentState {
                    transition, to, ..
                } if *transition == Transition::Queue => {
                    *transition = Transition::Dispatch;
                    *to = State::Dispatched;
                }
                _ => {}
            }
        }
    });
    assert!(
        whats.iter().any(|w| w.contains("dispatched without the reach that allows it")),
        "{whats:?}"
    );
    // and the engine itself refuses to dispatch it straight
    let mut fx = fx_on(policy());
    let w = silent(&fx, "sky-1", 5);
    let id = fx.submit(orbit_sky(), &w);
    let lines = fx.log.lines().len();
    assert!(matches!(
        fx.engine.dispatch(id, &w),
        Err(EngineError::NotDispatchable { .. })
    ));
    assert!(fx.engine.dispatch_order(id).is_err());
    assert_eq!(fx.log.lines().len(), lines, "nothing logged");
    // reachable: dispatched with the reach that allowed it
    let w = world_now(&fx);
    fx.engine.dispatch(id, &w).unwrap();
    assert!(fx.log.text().contains("\"reach\":[{\"uid\":\"sky-1\",\"reachable\":true"));
    replays(&fx);
}

#[test]
fn an_agents_order_on_a_vehicle_with_a_human_order_waiting_goes_to_a_human() {
    let mut fx = fx();
    // the operator's own order for blade-1 waits in the queue (its link is down)
    let w = silent(&fx, "blade-1", 5);
    let id = fx.submit(orbit(NEAR_CRISSY, 200.0, &["blade-1"]), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    assert_eq!(fx.state(id), State::Queued);
    // an agent's hold on blade-1 would run on its own and supersede it: a human decides
    let hold_id = fx
        .engine
        .agent_port(AgentId::new("planner"))
        .propose(hold(&["blade-1"]), rationale("park it"), &w, &terrain())
        .unwrap()
        .id();
    let i = fx.engine.intent(hold_id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert_eq!(i.policy().unwrap().rule, "(human order waiting)");
    // on another vehicle, it runs on its own as before
    let other = fx
        .engine
        .agent_port(AgentId::new("planner"))
        .propose(hold(&["sky-1"]), rationale("park it"), &w, &terrain())
        .unwrap()
        .id();
    assert_eq!(fx.state(other), State::AutoApproved);
    replays(&fx);
    // a forged fact is caught
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::CheckResult { inputs, .. } | EventBody::PolicyDecision { inputs, .. } = &mut e.body {
                inputs.waiting.clear();
            }
        }
    });
    assert!(whats.iter().any(|w| w.contains("human-order-waiting facts differ")), "{whats:?}");
}

#[test]
fn a_carried_human_approval_goes_back_to_the_inbox() {
    // review T3: a human's approval from the run before a restart is not acted on unseen
    let mut a = fx();
    let w = silent(&a, "blade-1", 5);
    let id = a.submit(orbit(NEAR_CRISSY, 200.0, &["blade-1"]), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    let ra = replay_str(&a.log.text()).unwrap();
    let open = ra.open.iter().find(|o| o.spec.id == id).unwrap();
    assert_eq!(open.platforms.get(&uid("blade-1")).map(String::as_str), Some("switchblade"));
    let mut b = fx();
    b.clock.set(a.clock.now().plus_ms(30_000));
    let from = ResumedFrom {
        log: "a.jsonl".into(),
        head: ra.head_sha256.clone().unwrap(),
        seq: ra.last_seq,
        was: open.state,
    };
    b.engine.resume(open, from).unwrap();
    assert!(b.engine.intent(id).unwrap().basis_carried());
    let w = world_now(&b);
    assert_eq!(b.engine.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Inbox);
    let rc = b.engine.intent(id).unwrap().queue().unwrap().recheck.clone().unwrap();
    assert!(rc.changes[0].contains("approved before a bridge restart"), "{:?}", rc.changes);
    let a_text = a.log.text();
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            previous: Some(&a_text),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(rb.ok(), "{:?}", rb.mismatches);
}

#[test]
fn a_vehicle_that_started_an_order_stays_started_across_a_second_restart() {
    // review T6: run A dispatched it and the vehicle started it; run B carried it over and
    // its follow op reported `queued` before B crashed; run C must still follow it, never
    // send it again
    let mut a = fx();
    let w = world_now(&a);
    let id = a.submit(orbit_sky(), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    a.engine.record_op_state(id, &uid("sky-1"), "acked", "accepted").unwrap();
    let ra = replay_str(&a.log.text()).unwrap();
    let open = ra.open.iter().find(|o| o.spec.id == id).unwrap().clone();
    let resume = |run: &mut Fx, prev: &ReplayReport, open: &OpenIntent| {
        let from = ResumedFrom {
            log: "prev.jsonl".into(),
            head: prev.head_sha256.clone().unwrap(),
            seq: prev.last_seq,
            was: open.state,
        };
        run.engine.resume(open, from).unwrap();
    };
    let mut b = fx();
    b.clock.set(a.clock.now().plus_ms(10_000));
    resume(&mut b, &ra, &open);
    b.engine
        .record_op_state(id, &uid("sky-1"), "queued", "waiting for navigation (GPS + EKF)")
        .unwrap();
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            trust_resumed: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(rb.ok(), "{:?}", rb.mismatches);
    let open_b = rb.open.iter().find(|o| o.spec.id == id).unwrap().clone();
    assert_eq!(open_b.ops.get(&uid("sky-1")).map(String::as_str), Some("queued"));
    assert_eq!(open_b.committed(), vec![uid("sky-1")], "started stays started");
    assert!(open_b.dispatched_before);
    assert_eq!(open_b.resumes_as(), State::Dispatched);
    let mut c = fx();
    c.clock.set(b.clock.now().plus_ms(10_000));
    resume(&mut c, &rb, &open_b);
    assert_eq!(c.state(id), State::Dispatched);
    let b_text = b.log.text();
    let rc = replay_str_with(
        &c.log.text(),
        &ReplayOptions {
            previous: Some(&b_text),
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    assert!(rc.ok(), "{:?}", rc.mismatches);
    // a dispatch never started, carried twice: still asks the vehicle first
    let mut a = fx();
    let w = world_now(&a);
    let id = a.submit(orbit_sky(), &w);
    a.engine.dispatch_or_queue(id, &w).unwrap();
    a.engine.record_op_state(id, &uid("sky-1"), "sent", "uploading").unwrap();
    let ra = replay_str(&a.log.text()).unwrap();
    let open = ra.open.iter().find(|o| o.spec.id == id).unwrap().clone();
    assert_eq!(open.resumes_as(), State::Queued);
    let mut b = fx();
    b.clock.set(a.clock.now().plus_ms(10_000));
    resume(&mut b, &ra, &open);
    assert!(b.engine.intent(id).unwrap().dispatched_before());
    let rb = replay_str_with(
        &b.log.text(),
        &ReplayOptions {
            trust_resumed: true,
            ..ReplayOptions::default()
        },
    )
    .unwrap();
    let open_b = rb.open.iter().find(|o| o.spec.id == id).unwrap();
    assert_eq!(open_b.state, State::Queued);
    assert!(open_b.dispatched_before, "a vehicle may still hold it");
}

// ---- second review --------------------------------------------------------------------

#[test]
fn an_order_that_may_have_started_is_only_confirmed_and_goes_to_a_human_when_not_running() {
    // review 2, P2b at the engine: the start went out (op `starting`), the link was lost
    // (`unconfirmed`): back to the queue; the next delivery only confirms it there, and when
    // the vehicle does not run it (`unconfirmed` again) a human decides
    let mut fx = fx();
    let w = world_now(&fx);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    let sky = uid("sky-1");
    fx.engine.record_op_state(id, &sky, "sent", "uploading").unwrap();
    fx.engine.record_op_state(id, &sky, "starting", "starting mission: mission mode commanded").unwrap();
    fx.engine
        .record_op_state(id, &sky, "unconfirmed", "link lost waiting for the MAV_CMD_DO_SET_MODE ACK, after its start went out")
        .unwrap();
    fx.engine.requeue(id).unwrap();
    assert_eq!(fx.state(id), State::Queued);
    assert!(fx.engine.intent(id).unwrap().maybe_started().contains(&sky));
    assert!(reason(&fx, id).contains("after its start went out"), "{}", reason(&fx, id));
    fx.advance(20);
    let w = world_now(&fx);
    assert!(matches!(fx.engine.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Dispatched(_)));
    assert!(fx.engine.intent(id).unwrap().confirming().contains(&sky), "only confirmed");
    fx.engine
        .record_op_state(id, &sky, "unconfirmed", "it may have started before its link was lost, and does not run it now (mode RTL, mission still held): not started again; a human decides")
        .unwrap();
    fx.engine.requeue(id).unwrap();
    let i = fx.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::PendingApproval);
    assert!(i.maybe_started().is_empty(), "the human decides from here");
    assert!(reason(&fx, id).contains("mode RTL"), "{}", reason(&fx, id));
    let r = replays(&fx);
    assert_eq!(r.requeued, 2);
    // replay holds it to that: the second requeue made to go back to the queue instead
    let whats = forged(&fx, |ev| {
        let mut n = 0;
        for e in ev.iter_mut() {
            if let EventBody::IntentRequeued { inbox, expires_t, .. } = &mut e.body {
                n += 1;
                if n == 2 {
                    *inbox = false;
                    *expires_t = expires_t.plus_ms(600_000);
                }
            }
        }
    });
    assert!(whats.iter().any(|w| w.contains("requeue deadline differs")), "{whats:?}");
}

/// An orbit dispatched to sky-1, its start out (op `starting`), then its link lost: the op
/// ends `unconfirmed` and the order goes back to the queue.
fn requeued_after_its_start_went_out() -> (Fx, IntentId) {
    let mut fx = fx();
    let w = world_now(&fx);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    let sky = uid("sky-1");
    fx.engine.record_op_state(id, &sky, "sent", "uploading").unwrap();
    fx.engine.record_op_state(id, &sky, "starting", "starting mission: mission mode commanded").unwrap();
    fx.engine
        .record_op_state(id, &sky, "unconfirmed", "link lost waiting for the MAV_CMD_DO_SET_MODE ACK, after its start went out")
        .unwrap();
    fx.engine.requeue(id).unwrap();
    (fx, id)
}

#[test]
fn replay_catches_an_op_requeued_after_its_start_went_out() {
    // murmur-rcf: a start that may have gone out is never requeued (the bridge ends that op
    // `unconfirmed`); a log whose op went `starting` then `requeued` passed replay, every
    // other event still consistent with it
    let (fx, _) = requeued_after_its_start_went_out();
    replays(&fx);
    let whats = forged(&fx, |ev| {
        for e in ev.iter_mut() {
            if let EventBody::OpState { state, .. } = &mut e.body
                && state == "unconfirmed"
            {
                *state = "requeued".into();
            }
        }
    });
    assert_eq!(
        whats,
        ["op requeued after its start went out (it may be running: the bridge ends such an op unconfirmed)"],
        "the only thing wrong with it"
    );
    // nor after the vehicle started it
    let mut fx = fx_on(policy());
    let w = world_now(&fx);
    let id = fx.submit(orbit_sky(), &w);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    fx.engine.record_op_state(id, &uid("sky-1"), "acked", "accepted").unwrap();
    fx.engine.record_op_state(id, &uid("sky-1"), "active", "flying it").unwrap();
    replays(&fx);
    fx.engine.record_op_state(id, &uid("sky-1"), "requeued", "link lost").unwrap();
    let r = replay_str(&fx.log.text()).unwrap();
    assert!(
        r.mismatches.iter().any(|m| m.what.starts_with("op requeued after its start went out")),
        "{:?}",
        r.mismatches
    );
}

#[test]
fn a_confirming_delivery_whose_link_drops_before_it_asks_is_requeued() {
    // the genuine log murmur-rcf must not reject: the start went out on the first delivery
    // (`starting`, `unconfirmed`); the next delivery only confirms it, and its link drops
    // before it asked the vehicle (the bridge's `Resume::Confirm` op, still queued or sent,
    // ends `requeued`): back to the queue, the vehicle still one that may have started it
    let (mut fx, id) = requeued_after_its_start_went_out();
    let sky = uid("sky-1");
    fx.advance(20);
    let w = world_now(&fx);
    assert!(matches!(fx.engine.dequeue(id, &w, &terrain()).unwrap(), Dequeued::Dispatched(_)));
    assert!(fx.engine.intent(id).unwrap().confirming().contains(&sky), "only confirmed");
    fx.engine.record_op_state(id, &sky, "sent", "asking what it holds").unwrap();
    fx.engine
        .record_op_state(id, &sky, "requeued", "link lost waiting for MISSION_COUNT: back to the queue, re-checked before it is sent again")
        .unwrap();
    fx.engine.requeue(id).unwrap();
    assert_eq!(fx.state(id), State::Queued);
    assert!(fx.engine.intent(id).unwrap().maybe_started().contains(&sky));
    let r = replays(&fx);
    assert_eq!(r.requeued, 2);
}

#[test]
fn an_agents_order_cant_replace_a_humans_order_no_vehicle_started_yet() {
    // review 2, finding 7: a human's order dispatched, its vehicle not started yet (link
    // slow, say): an agent's hold there goes to a human; once the vehicle started the
    // human's order, it runs on its own as before
    let mut fx = fx();
    let w = world_now(&fx);
    let id = fx.submit(orbit(NEAR_CRISSY, 200.0, &["blade-1"]), &w);
    assert_eq!(fx.state(id), State::Approved);
    fx.engine.dispatch_or_queue(id, &w).unwrap();
    assert_eq!(fx.state(id), State::Dispatched);
    fx.engine.record_op_state(id, &uid("blade-1"), "sent", "uploading").unwrap();
    let agent = |fx: &mut Fx| {
        let w = world_now(fx);
        fx.engine
            .agent_port(AgentId::new("planner"))
            .propose(hold(&["blade-1"]), rationale("park it"), &w, &terrain())
            .unwrap()
            .id()
    };
    let a = agent(&mut fx);
    assert_eq!(fx.state(a), State::PendingApproval);
    assert_eq!(fx.engine.intent(a).unwrap().policy().unwrap().rule, "(human order waiting)");
    fx.engine.record_op_state(id, &uid("blade-1"), "acked", "accepted").unwrap();
    let b = agent(&mut fx);
    assert_ne!(fx.engine.intent(b).unwrap().policy().unwrap().rule, "(human order waiting)");
    replays(&fx);
}
