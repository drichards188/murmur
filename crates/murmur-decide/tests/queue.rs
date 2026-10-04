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
    // a hold is one command: it goes over the modem
    let hold_id = fx.submit(hold(&["remus-1"]), &w);
    assert!(matches!(
        fx.engine.dispatch_or_queue(hold_id, &w).unwrap(),
        Routed::Dispatched(_)
    ));
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
    // back after a reboot: the vehicle booted after the approval
    fx.advance(120);
    let mut w = world_now(&fx);
    w.vehicles.get_mut(&uid("blade-1")).unwrap().booted_t = Some(fx.clock.now().plus_ms(-60_000));
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
    let text = fx.log.text().replacen("\"rules_version\":5", "\"rules_version\":4", 1);
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
        .record_reboot(&uid("sky-1"), 812_300, 4_100, fx.clock.now())
        .unwrap();
    let r = replays(&fx);
    assert_eq!(r.reboots, 1);
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
