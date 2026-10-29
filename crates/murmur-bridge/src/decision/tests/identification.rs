//! Rules 9 through the bridge: an abstract hostile act on the simulation channel, observed by
//! the sensors that could see its author, goes to the engine in the loop's identification
//! step; the engine declares the author hostile by rule (or flags the act for review), the
//! track store mirrors it, the inbox shows it as a non-blocking review item, and an operator
//! confirms, dismisses or revokes it under their own session. Revoking a declaration stops
//! every engagement in play on the track first. Every log replays.

use murmur_decide::{Affiliation, FlagResolution, IdentConfig};

use super::engagement::{Erig, engage_rig_auth_policy};
use super::*;
use crate::tracks::sensing::{Observer, SensorSpec, SensorType};
use crate::tracks::truth::{ActTruth, ContactTruth, Domain as TDomain, Sensing, SensingModel, TruthAffiliation, TruthMessage};

/// In ez-gate-west, west of the bridge (as the engagement tests).
const TARGET: (f64, f64) = (37.8120, -122.4950);

fn op(s: &str) -> Actor {
    Actor::asserted(OperatorId::new(s))
}

/// The policy's identification for these runs: rule declaration on.
pub(super) fn rule_on(p: &mut Policy, nominate_unconfirmed: bool) {
    p.identification = Some(IdentConfig {
        rule_declaration: true,
        nominate_unconfirmed: Some(nominate_unconfirmed),
        ..IdentConfig::default()
    });
}

/// The engagement rig (a dev run) with rule declaration on.
fn rig(nominate_unconfirmed: bool) -> Erig {
    engage_rig_auth_policy(None, PathBuf::from("memory"), murmur_decide::AuthMode::Dev, |p| {
        rule_on(p, nominate_unconfirmed);
    })
}

fn boat() -> ContactTruth {
    ContactTruth {
        id: "fastboat".into(),
        name: "fast boat".into(),
        affiliation: TruthAffiliation::Hostile,
        domain: TDomain::Surface,
        kind: "small_fast_boat".into(),
        lat: TARGET.0,
        lon: TARGET.1,
        alt_m: 0.0,
        speed_mps: 0.0,
        course_deg: 0.0,
        emits: false,
        signature: 1.0,
        ais: None,
    }
}

/// The scenario seconds the tests' truth ticks carry, fixed whatever the wall clock: the
/// sensing model seeds its draws by the tick (the scenario second), so the tracks and an
/// act's fix are the same at any start time (the review found these tests failing at 9 of 300
/// pinned start seconds when the tick followed the wall clock).
const S0: f64 = 1000.0;

/// One truth tick at scenario second `scen_s` with `acts`, seen by a radar and EO/IR observer
/// 1.5 km east (as the engagement tests' sensing), at the bridge's time `now_s`.
pub(super) fn truth_with(e: &Erig, now_s: f64, scen_s: f64, acts: Vec<ActTruth>) {
    let observer = Observer {
        uid: 9,
        lat: TARGET.0,
        lon: TARGET.1 + 0.017,
        nav_error_m: (0.0, 0.0),
        alt_msl: 0.0,
        mast_m: 4.0,
        submerged: false,
        heading_deg: None,
        area: None,
        sensors: vec![
            SensorSpec { kind: SensorType::Radar, range_m: 8_000.0, guess: true, height: false, physical: Default::default() },
            SensorSpec { kind: SensorType::EoIr, range_m: 5_000.0, guess: true, height: false, physical: Default::default() },
        ],
    };
    let msg = TruthMessage {
        t: now_s,
        s: scen_s,
        seq: 0,
        scenario: "test".into(),
        seed: 1,
        // the legacy model's (the observer's sensors are their range_m alone)
        sensing: Sensing {
            misclassification_rate: 0.0,
            model: Some(SensingModel::Legacy),
            ..Default::default()
        },
        contacts: vec![boat()],
        acts,
        aim_at: Vec::new(), weapons: Vec::new(), impacts: Vec::new(),
    };
    e.tracks
        .on_truth_with(msg, std::slice::from_ref(&observer), now_s)
        .expect("sensing");
}

/// The boat fires on our platform (uid 1), or on `against_contact` when given.
pub(super) fn act(id: &str, now_s: f64, against_contact: Option<&str>) -> ActTruth {
    ActTruth {
        id: id.into(),
        s: now_s,
        t: now_s,
        by: "fastboat".into(),
        lat: TARGET.0,
        lon: TARGET.1,
        against_vehicle: against_contact.is_none().then_some(1),
        against_contact: against_contact.map(str::to_string),
        label: Some("fired on".into()),
        weapon: None, held: None,
    }
}

/// Our armed platform on the target, a minute and a half of detections (nobody declares).
pub(super) async fn picture(e: &Erig) {
    e.r.vehicle(1, Firmware::Copter, "fpv", TARGET);
    sleep(Duration::from_millis(500)).await;
    e.r.clock.set(snapshot::ms(clock::wall()));
    let now = e.now_s();
    for s in 0..90 {
        truth_with(e, now - 90.0 + f64::from(s), S0 + f64::from(s), Vec::new());
    }
    truth_with(e, now, S0 + 90.0, Vec::new());
}

/// A second on: the act on the simulation channel, then the decision loop.
pub(super) fn act_now(e: &Erig, against_contact: Option<&str>) {
    e.r.clock.advance_ms(1000);
    let now = e.now_s();
    truth_with(e, now, S0 + 91.0, vec![act("act-1", now, against_contact)]);
    e.r.d.tick();
}

fn track(e: &Erig) -> Value {
    e.tracks
        .tracks_json(e.now_s())
        .into_iter()
        .find(|t| t["id"] == "T-0001")
        .expect("the boat's track")
}

#[tokio::test(start_paused = true)]
async fn an_observed_hostile_act_is_declared_by_rule_and_an_operator_confirms_it() {
    let e = rig(false);
    picture(&e).await;
    assert!(track(&e)["declared"].is_null(), "nobody declared it");
    act_now(&e, None);
    // the engine's rule declared the author hostile: the store mirrors it with its basis
    let t = track(&e);
    assert_eq!(t["affiliation"], "hostile", "{t}");
    assert_eq!(t["declared"]["by"], "rule:hostile_act", "{t}");
    assert!(t["declared"]["confirmed_by"].is_null(), "{t}");
    // a non-blocking review item in the inbox: confirm or revoke
    let inbox = e.r.d.inbox_json();
    assert_eq!(inbox["counts"]["review"], 1, "{inbox}");
    let review = e.r.d.review_json();
    let d = &review["declarations"][0];
    assert_eq!(d["declared_by"], "rule:hostile_act", "{review}");
    assert_eq!(d["actions"], json!(["confirm", "revoke"]));
    assert!(d["basis"].as_str().expect("basis").contains("A-1"), "{review}");
    // the act as observed: the engine's attribution and verdict
    assert_eq!(inbox["acts"][0]["declaration"], "D-1", "{inbox}");
    // the default policy: no nomination on a rule declaration nobody confirmed
    let out = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("submitted");
    assert_eq!(out.state, State::Rejected, "{}", out.intent);
    assert!(e.r.reason(out.id).contains("confirmation"), "{}", e.r.reason(out.id));
    // any operator confirms it (op-1 has no engagement authority), under their session
    let v = e.r.d.confirm(&op("op-1"), None, "T-0001").expect("confirmed");
    assert!(v["declaration"].is_object(), "{v}");
    assert!(e.r.d.confirm(&op("op-2"), None, "T-0001").is_err(), "once");
    assert_eq!(track(&e)["declared"]["confirmed_by"], "op-1");
    assert_eq!(e.r.d.review_json()["counts"]["declarations"], 0);
    // now the nomination goes to the signers
    let out = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("submitted");
    assert_eq!(out.state, State::PendingApproval, "{}", out.intent);
    // the signer's picture says how the target was declared
    let token = e.look("op-2", out.id).expect("picture");
    let basis = token.to_string();
    assert!(basis.contains("rule:hostile_act") || basis.contains("by rule"), "{token}");
    assert!(basis.contains("op-1"), "the confirmation shows: {token}");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(
        (rep.acts, rep.rule_declarations, rep.confirmations, rep.operator_declarations),
        (1, 1, 1, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn revoking_a_rule_declaration_stops_the_engagement_on_it_first() {
    let e = rig(true);
    picture(&e).await;
    act_now(&e, None);
    assert_eq!(track(&e)["declared"]["by"], "rule:hostile_act");
    // the policy lets a rule declaration nobody confirmed be nominated; two people sign
    let out = e.r.d.submit(&op("op-1"), e.nomination("return")).await.expect("nominated");
    assert_eq!(out.state, State::PendingApproval, "{}", out.intent);
    let id = out.id;
    e.sign("op-2", id).await.expect("signs");
    let o = e.sign("op-3", id).await.expect("signs");
    assert_eq!(o.state, State::Dispatched, "{}", o.intent);
    e.preloaded(id).await;
    // op-1 revokes it: the engagement stops before anything else, its fallback commanded
    let v = e
        .r
        .d
        .revoke(&op("op-1"), "T-0001", Some("the cutter fired first".into()))
        .expect("revoked");
    assert_eq!(v["revoked"], "D-1", "{v}");
    assert_eq!(v["stopped"], json!([id.0]), "{v}");
    assert_eq!(e.r.state(id), State::Aborted, "{}", e.r.reason(id));
    assert!(e.r.reason(id).contains("revoked by op-1"), "{}", e.r.reason(id));
    let ops = e.r.world.tasks.current_ops();
    let op1 = ops.iter().find(|o| o.uid == 1).expect("an op on the platform");
    assert_eq!(op1.task, crate::tasks::TaskKind::Rtl);
    assert!(track(&e)["declared"].is_null(), "{}", track(&e));
    assert!(e.tracks.take_effects().is_empty(), "no effect after a revocation");
    // the rule's declaration revoked: nobody to tell (an operator's would be told)
    let review = e.r.d.review_json();
    assert_eq!(review["revocations"][0]["id"], "D-1", "{review}");
    assert_eq!(review["revocations"][0]["revoked_by"], "op-1");
    assert!(review["revocations"][0]["notify"].is_null());
    // the next ticks do not declare it again from the same act
    e.r.clock.advance_ms(1000);
    truth_with(&e, e.now_s(), S0 + 92.0, vec![act("act-1", e.now_s(), None)]);
    e.r.d.tick();
    assert!(track(&e)["declared"].is_null());
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((rep.rule_declarations, rep.revocations, rep.aborts), (1, 1, 1));
}

#[tokio::test(start_paused = true)]
async fn an_act_on_something_we_hold_no_track_of_is_flagged_and_an_operator_dismisses_it() {
    let e = rig(false);
    picture(&e).await;
    act_now(&e, Some("buoy"));
    // flagged, not declared: its victim can't be said to be ours
    let t = track(&e);
    assert!(t["declared"].is_null(), "{t}");
    assert_eq!(t["review"][0]["flag"], 1, "{t}");
    let review = e.r.d.review_json();
    let f = &review["flags"][0];
    assert_eq!(f["flag_kind"], "victim", "{review}");
    assert_eq!(f["tracks"], json!(["T-0001"]), "{review}");
    assert_eq!(f["actions"], json!(["confirm", "dismiss", "declare"]));
    assert_eq!(e.r.d.inbox_json()["counts"]["review"], 1);
    // an operator dismisses it, under their session; the flag stays in the record
    e.r.d
        .resolve_flag(&op("op-2"), 1, FlagResolution::Dismissed, Some("a buoy adrift".into()))
        .expect("dismissed");
    assert!(
        e.r.d.resolve_flag(&op("op-1"), 1, FlagResolution::Confirmed, None).is_err(),
        "a dismissed flag is closed"
    );
    let review = e.r.d.review_json();
    assert_eq!(review["counts"]["flags"], 0, "{review}");
    assert_eq!(review["resolved"][0]["status"]["status"], "dismissed", "{review}");
    assert_eq!(review["resolved"][0]["status"]["by"], "op-2", "{review}");
    assert!(track(&e)["review"].as_array().is_none_or(Vec::is_empty));
    // a human declaration stays available for any track
    e.r.d
        .declare(&op("op-1"), None, "T-0001", Affiliation::Hostile, Some("seen firing".into()), None)
        .expect("declared");
    assert_eq!(track(&e)["declared"]["by"], "op-1");
    // another operator revokes it: its declarer gets the notice, with the note
    e.r.d.revoke(&op("op-3"), "T-0001", Some("the cutter's warning shot".into())).expect("revoked");
    let review = e.r.d.review_json();
    assert_eq!(review["revocations"][0]["notify"], "op-1", "{review}");
    assert_eq!(review["revocations"][0]["note"], "the cutter's warning shot");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!(
        (rep.acts, rep.flags, rep.flags_resolved, rep.rule_declarations, rep.operator_declarations, rep.revocations),
        (1, 1, 1, 0, 1, 1)
    );
}

#[tokio::test(start_paused = true)]
async fn with_rule_declaration_off_an_act_is_recorded_and_an_operator_declares() {
    // `[identification] rule_declaration = false`: rules 7 behaviour, an act only recorded
    let e = engage_rig_auth_policy(None, PathBuf::from("memory"), murmur_decide::AuthMode::Dev, |p| {
        p.identification = Some(IdentConfig::default());
    });
    picture(&e).await;
    act_now(&e, None);
    assert!(track(&e)["declared"].is_null());
    let inbox = e.r.d.inbox_json();
    assert_eq!(inbox["counts"]["review"], 0, "{inbox}");
    assert_eq!(inbox["acts"][0]["verdict"]["verdict"], "recorded", "{inbox}");
    // the operator reads the act and declares
    e.r.d
        .declare(&op("op-1"), None, "T-0001", Affiliation::Hostile, Some("fired on uid 1".into()), None)
        .expect("declared");
    assert_eq!(track(&e)["declared"]["by"], "op-1");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((rep.acts, rep.rule_declarations, rep.flags, rep.operator_declarations), (1, 0, 0, 1));
}

/// Review, fix 1: an act on the simulation channel against a "vehicle" not in our fleet is not
/// against ours: flagged (victim), never declared.
#[tokio::test(start_paused = true)]
async fn an_act_against_a_vehicle_not_in_our_fleet_is_flagged_not_declared() {
    let e = rig(false);
    picture(&e).await;
    e.r.clock.advance_ms(1000);
    let now = e.now_s();
    let mut a = act("act-1", now, None);
    a.against_vehicle = Some(99);
    truth_with(&e, now, S0 + 91.0, vec![a]);
    e.r.d.tick();
    assert!(track(&e)["declared"].is_null(), "{}", track(&e));
    let review = e.r.d.review_json();
    assert_eq!(review["flags"][0]["flag_kind"], "victim", "{review}");
    let inbox = e.r.d.inbox_json();
    assert_eq!(inbox["acts"][0]["act"]["channel"], "sim:contacts", "{inbox}");
    let rep = replay::replay_str(&e.r.log.text()).expect("parses");
    assert!(rep.ok(), "{:?}", rep.mismatches);
    assert_eq!((rep.rule_declarations, rep.flags), (0, 1));
}
