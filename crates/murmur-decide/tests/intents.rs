//! The intent state machine (every transition, legal and illegal), the engine's lifecycle
//! paths, expiry, the agent/human split, and fail-closed audit writes.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use murmur_decide::*;

// ---- the table --------------------------------------------------------------------------

#[test]
fn every_transition_legal_and_illegal() {
    use State as S;
    use Transition as T;
    let legal = [
        (S::Proposed, T::Check, S::Checked),
        (S::Checked, T::ChecksFailed, S::Rejected),
        (S::Checked, T::AutoApprove, S::AutoApproved),
        (S::Checked, T::RequireApproval, S::PendingApproval),
        (S::PendingApproval, T::Approve, S::Approved),
        (S::PendingApproval, T::Reject, S::Rejected),
        (S::PendingApproval, T::Expire, S::Expired),
        (S::PendingApproval, T::Void, S::Voided),
        (S::AutoApproved, T::Void, S::Voided),
        (S::Approved, T::Void, S::Voided),
        (S::AutoApproved, T::Dispatch, S::Dispatched),
        (S::Approved, T::Dispatch, S::Dispatched),
        (S::Dispatched, T::Complete, S::Done),
        (S::Dispatched, T::Fail, S::Failed),
    ];
    let mut checked = 0;
    for from in State::ALL {
        for tr in Transition::ALL {
            let want = legal
                .iter()
                .find(|(f, t, _)| *f == from && *t == tr)
                .map(|l| l.2);
            match (next_state(from, tr), want) {
                (Ok(to), Some(w)) => assert_eq!(to, w, "{from:?} -{tr:?}->"),
                (Err(e), None) => assert_eq!((e.from, e.transition), (from, tr)),
                (got, want) => panic!("{from:?} -{tr:?}-> got {got:?}, want {want:?}"),
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 121);
    for s in State::ALL {
        if s.is_terminal() {
            assert!(
                Transition::ALL.iter().all(|t| next_state(s, *t).is_err()),
                "{s:?} is terminal"
            );
        }
    }
    // Approved has exactly one way in.
    let into_approved: Vec<_> = State::ALL
        .iter()
        .flat_map(|f| Transition::ALL.iter().map(move |t| (*f, *t)))
        .filter(|(f, t)| next_state(*f, *t) == Ok(State::Approved))
        .collect();
    assert_eq!(
        into_approved,
        vec![(State::PendingApproval, Transition::Approve)]
    );
}

// ---- engine paths -----------------------------------------------------------------------

struct Fx {
    clock: ManualClock,
    engine: Engine,
    log: MemorySink,
    session: OperatorSession,
}

fn fx() -> Fx {
    let clock = ManualClock::new(T0);
    let (mut engine, log) = engine(&clock);
    let session = engine
        .open_operator_session(OperatorId::new("op-1"))
        .unwrap();
    Fx {
        clock,
        engine,
        log,
        session,
    }
}

impl Fx {
    fn submit(&mut self, p: Proposal) -> IntentId {
        self.engine
            .submit(self.session.authority(), p, &world(), &terrain())
            .unwrap()
            .id()
    }
    fn state(&self, id: IntentId) -> State {
        self.engine.intent(id).unwrap().state()
    }
}

fn assert_illegal(r: Result<impl std::fmt::Debug, EngineError>) {
    assert!(matches!(r, Err(EngineError::Illegal { .. })), "{r:?}");
}

#[test]
fn auto_path_to_done() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 150.0, &["sky-1"]));
    assert_eq!(f.state(id), State::AutoApproved);
    let order = f.engine.dispatch(id).unwrap();
    assert_eq!(order.task.kind(), TaskKind::Orbit);
    f.engine
        .record_op_state(id, &uid("sky-1"), "active", "on station")
        .unwrap();
    f.engine.finish(id, TaskOutcome::Done).unwrap();
    let i = f.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Done);
    let states: Vec<_> = i.history().iter().map(|h| h.state).collect();
    assert_eq!(
        states,
        vec![
            State::Proposed,
            State::Checked,
            State::AutoApproved,
            State::Dispatched,
            State::Done
        ]
    );
}

#[test]
fn human_path_approve_dispatch_fail() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    assert_eq!(f.state(id), State::PendingApproval);
    assert_eq!(f.engine.inbox().len(), 1);
    f.engine
        .approve(
            id,
            f.session.authority(),
            Some("checked the picture".into()),
            &world(),
            &terrain(),
        )
        .unwrap();
    let a = f.engine.intent(id).unwrap().approval().unwrap();
    assert_eq!(a.by, OperatorId::new("op-1"));
    f.engine.dispatch(id).unwrap();
    f.engine
        .finish(
            id,
            TaskOutcome::Failed {
                detail: "no ACK".into(),
            },
        )
        .unwrap();
    assert_eq!(f.state(id), State::Failed);
}

#[test]
fn reject_cancel_and_system_void() {
    let mut f = fx();
    let a = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    f.engine
        .reject(a, f.session.authority(), Some("no".into()))
        .unwrap();
    assert_eq!(f.state(a), State::Rejected);

    let b = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    f.engine
        .cancel(b, f.session.authority(), "changed my mind")
        .unwrap();
    assert_eq!(f.state(b), State::Voided);

    let c = f.submit(orbit(NEAR_CRISSY, 150.0, &["sky-1"]));
    f.engine.void(c, "vehicle lost").unwrap();
    assert_eq!(f.state(c), State::Voided);
}

#[test]
fn failing_checks_reject_before_policy() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 250.0, &["hawk-1"]));
    let i = f.engine.intent(id).unwrap();
    assert_eq!(i.state(), State::Rejected);
    assert!(i.policy().is_none());
    assert!(i.history().last().unwrap().reason.contains("below 4034 m"));
}

#[test]
fn illegal_operations_are_typed_errors_and_change_nothing() {
    let mut f = fx();
    let auto = f.submit(orbit(NEAR_CRISSY, 150.0, &["sky-1"]));
    let pending = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    let lines = f.log.lines().len();

    let auth = f.session.authority();
    assert_illegal(
        f.engine
            .approve(auto, auth, None, &world(), &terrain())
            .map(|i| i.id()),
    );
    assert_illegal(f.engine.reject(auto, auth, None).map(|i| i.id()));
    assert_illegal(f.engine.dispatch(pending));
    assert_illegal(f.engine.finish(auto, TaskOutcome::Done).map(|i| i.id()));
    assert_eq!(
        f.log.lines().len(),
        lines,
        "a refused operation logs nothing"
    );

    f.engine.dispatch(auto).unwrap();
    assert_illegal(f.engine.dispatch(auto));
    assert_illegal(f.engine.void(auto, "too late").map(|i| i.id()));

    f.engine.reject(pending, auth, None).unwrap();
    assert_illegal(
        f.engine
            .approve(pending, auth, None, &world(), &terrain())
            .map(|i| i.id()),
    );
    assert!(matches!(
        f.engine.dispatch(IntentId(99)),
        Err(EngineError::UnknownIntent(_))
    ));
}

// ---- expiry -----------------------------------------------------------------------------

#[test]
fn pending_approvals_expire() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    assert_eq!(
        f.engine.intent(id).unwrap().expires_t(),
        Some(T0.plus_ms(120_000))
    );
    f.clock.advance_ms(119_999);
    assert!(f.engine.expire_due().unwrap().is_empty());
    f.clock.advance_ms(1);
    assert_eq!(f.engine.expire_due().unwrap(), vec![id]);
    assert_eq!(f.state(id), State::Expired);
    assert!(f.engine.inbox().is_empty());
}

#[test]
fn approving_after_the_deadline_expires_instead() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    f.clock.advance_ms(121_000);
    let r = f
        .engine
        .approve(id, f.session.authority(), None, &world(), &terrain());
    assert!(matches!(r, Err(EngineError::Expired(_))));
    assert_eq!(f.state(id), State::Expired);
}

#[test]
fn expiry_is_configurable() {
    let clock = ManualClock::new(T0);
    let mut p = policy();
    p.approval.expire_s = 5;
    let mut e = Engine::new(p, Box::new(clock.clone()), Box::new(MemorySink::new())).unwrap();
    let s = e.open_operator_session(OperatorId::new("op")).unwrap();
    let id = e
        .submit(
            s.authority(),
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            &world(),
            &terrain(),
        )
        .unwrap()
        .id();
    clock.advance_ms(5_000);
    assert_eq!(e.expire_due().unwrap(), vec![id]);
}

// ---- approval re-checks the world -------------------------------------------------------

#[test]
fn approval_of_a_stale_picture_is_void() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    let mut changed = world();
    changed.vehicles.get_mut(&uid("blade-1")).unwrap().nav_ready = Some(Fact::new(false, T0));
    let r = f
        .engine
        .approve(id, f.session.authority(), None, &changed, &terrain());
    assert!(
        matches!(r, Err(EngineError::ChecksFailedAtApproval(_))),
        "{r:?}"
    );
    assert_eq!(f.state(id), State::Voided);
    assert!(f.engine.intent(id).unwrap().approval().is_none());
}

// ---- agents -----------------------------------------------------------------------------

#[test]
fn agents_nominate_and_humans_authorize() {
    let mut f = fx();
    let id = {
        let mut port = f.engine.agent_port(AgentId::new("planner"));
        let i = port
            .propose(
                orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
                rationale("loiter over the landing zone"),
                &world(),
                &terrain(),
            )
            .unwrap();
        assert_eq!(i.spec().source, agent());
        assert_eq!(i.state(), State::PendingApproval);
        i.id()
    };
    // Nothing on the agent side moves it on; it waits for a human or expires.
    f.clock.advance_ms(60_000);
    assert_eq!(f.state(id), State::PendingApproval);
    f.engine
        .approve(id, f.session.authority(), None, &world(), &terrain())
        .unwrap();
    assert_eq!(f.state(id), State::Approved);
}

#[test]
fn agent_intents_on_auto_tiers_run_without_a_human() {
    let mut f = fx();
    let mut port = f.engine.agent_port(AgentId::new("planner"));
    let i = port
        .propose(
            orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
            rationale("routine coverage"),
            &world(),
            &terrain(),
        )
        .unwrap();
    assert_eq!(i.state(), State::AutoApproved);
    assert_eq!(
        i.spec().rationale.as_ref().unwrap().as_str(),
        "routine coverage"
    );
}

#[test]
fn agents_withdraw_only_their_own() {
    let mut f = fx();
    let human_id = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    let mine = f
        .engine
        .agent_port(AgentId::new("a"))
        .propose(
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            rationale("r"),
            &world(),
            &terrain(),
        )
        .unwrap()
        .id();
    let mut other = f.engine.agent_port(AgentId::new("b"));
    assert!(matches!(
        other.withdraw(mine, "x"),
        Err(EngineError::NotOwner { .. })
    ));
    assert!(matches!(
        other.withdraw(human_id, "x"),
        Err(EngineError::NotOwner { .. })
    ));
    let mut a = f.engine.agent_port(AgentId::new("a"));
    assert_eq!(
        a.withdraw(mine, "superseded").unwrap().state(),
        State::Voided
    );
}

#[test]
fn rationale_cannot_be_empty() {
    assert_eq!(Rationale::new("   "), Err(ProposalError::EmptyRationale));
    let r: Result<Rationale, _> = serde_json::from_str("\"\"");
    assert!(r.is_err());
}

#[test]
fn proposals_are_validated() {
    let mut f = fx();
    let a = f.session.authority();
    let bad = [
        Proposal::new(Task::Hold {}, vec![]),
        hold(&["sky-1", "sky-1"]),
        orbit(NEAR_CRISSY, -5.0, &["sky-1"]),
        orbit(LonLat::new(200.0, 0.0), 100.0, &["sky-1"]),
        Proposal::new(
            Task::Patrol {
                route: vec![CRISSY],
            },
            vec![uid("sky-1")],
        ),
        Proposal::new(
            Task::Track {
                target: uid("track-1"),
            },
            vec![uid("track-1")],
        ),
    ];
    for p in bad {
        let r = f
            .engine
            .submit(a, p.clone(), &world(), &terrain())
            .map(|i| i.id());
        assert!(matches!(r, Err(EngineError::Proposal(_))), "{p:?} -> {r:?}");
    }
    assert_eq!(f.engine.intents().count(), 0);
}

#[test]
fn proposal_json_shape() {
    // What POST /api/intents (and an agent's propose call) carries. "radius" is accepted
    // as an alias of today's task param; unknown constraint keys are refused.
    let p: Proposal = serde_json::from_str(
        r#"{"kind":"orbit","params":{"center":[-122.45,37.80],"radius":150},"uids":["sky-1"],
            "constraints":{"reserve_pct":30}}"#,
    )
    .unwrap();
    assert_eq!(
        p.task,
        Task::Orbit {
            center: LonLat::new(-122.45, 37.80),
            radius_m: 150.0
        }
    );
    let h: Proposal =
        serde_json::from_str(r#"{"kind":"hold","params":{},"uids":["sky-1"]}"#).unwrap();
    assert_eq!(h.task, Task::Hold {});
    let bad = serde_json::from_str::<Proposal>(
        r#"{"kind":"hold","params":{},"uids":["a"],"constraints":{"reserve":5}}"#,
    );
    assert!(bad.is_err());
}

#[test]
fn intent_serializes_for_the_api() {
    let mut f = fx();
    let id = f.submit(orbit(NEAR_CRISSY, 150.0, &["blade-1"]));
    let v = serde_json::to_value(f.engine.intent(id).unwrap()).unwrap();
    assert_eq!(v["state"], "pending_approval");
    assert_eq!(v["kind"], "orbit");
    assert_eq!(v["source"]["kind"], "human");
    assert_eq!(v["policy"]["tier"], "human");
    assert_eq!(v["checks"].as_array().unwrap().len(), 6);
}

// ---- audit failures fail closed ---------------------------------------------------------

/// Accepts `ok` more lines, then fails every write.
#[derive(Clone)]
struct FlakySink(Arc<AtomicUsize>);

impl AuditSink for FlakySink {
    fn append(&mut self, _line: &str) -> std::io::Result<()> {
        if self.0.load(Ordering::SeqCst) == 0 {
            return Err(std::io::Error::other("disk full"));
        }
        self.0.fetch_sub(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn no_audit_record_no_change() {
    let budget = Arc::new(AtomicUsize::new(2)); // run.started + session.opened
    let mut e = Engine::new(
        policy(),
        Box::new(ManualClock::new(T0)),
        Box::new(FlakySink(budget.clone())),
    )
    .unwrap();
    let s = e.open_operator_session(OperatorId::new("op")).unwrap();
    let r = e
        .submit(
            s.authority(),
            orbit(NEAR_CRISSY, 150.0, &["sky-1"]),
            &world(),
            &terrain(),
        )
        .map(|i| i.id());
    assert!(matches!(r, Err(EngineError::Audit(_))));
    assert_eq!(
        e.intents().count(),
        0,
        "no intent without its intent.created record"
    );

    // Enough budget to reach pending (created, check, state, policy, requested, state),
    // then the approval record fails: the intent must stay pending.
    budget.store(6, Ordering::SeqCst);
    let id = e
        .submit(
            s.authority(),
            orbit(NEAR_CRISSY, 150.0, &["blade-1"]),
            &world(),
            &terrain(),
        )
        .unwrap()
        .id();
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    let r = e
        .approve(id, s.authority(), None, &world(), &terrain())
        .map(|i| i.id());
    assert!(matches!(r, Err(EngineError::Audit(_))));
    assert_eq!(e.intent(id).unwrap().state(), State::PendingApproval);
    assert!(e.intent(id).unwrap().approval().is_none());
}
