//! HTTP and WebSocket API (same routes and JSON as the Python bridge, keyed by uid), plus
//! the decision layer's intents and inbox.
//!
//! Every task goes through `crate::decision`: `POST /api/tasks` (the console) is a thin
//! wrapper that creates a human intent. Its response is the tasking as before plus
//! `intent_id`, `state` and `dispatched`; an intent that needs approval answers 202 with
//! `dispatched: false` and why, one approved but held for a vehicle the link can't reach now
//! (rules 5, state `queued`) answers 202 saying so, one the rules reject answers 422 with the
//! reasons. `GET /api/inbox` lists the queue as `queued` (each with `reach_now`).
//!
//! Intents: `POST /api/intents` (the task request plus optional `constraints` and
//! `rationale`; `task: "engage"` with `track`, `effect_radius_m`, `window_s`, `fallback`
//! nominates an engagement, Phase E), `GET /api/intents`, `GET /api/intents/{id}`, `POST
//! /api/intents/{id}/approve|reject|cancel|abort|picture` (optional body `{"note": "..."}`;
//! `picture` asks for what an operator with engagement authority would sign, a picture
//! token, and an engagement's approve quotes it as `"token": <id>`; an approval whose
//! dispatch would stop an engagement in play needs `"acknowledge_supersede": true`; `abort`
//! stops an approved or dispatched engagement before its effect, any operator), `GET
//! /api/inbox`, `GET /api/operators` (who may sign an engagement), `POST /api/engage/options`
//! (the nomination form's dry run: which armed platforms could take it, with checks and
//! preview; nothing is created or logged), `POST /api/restores/write_off` (rules 12: an
//! operator writes off a vehicle's lost-link values owed back, or an earlier log whose carry
//! failed, saying why). Who acts is `crate::auth`'s answer: the operator
//! signed in on this browser (the session cookie; `POST /api/auth/login`), or in a dev run
//! (`MURMUR_AUTH=dev`) the `X-Murmur-Operator` header. An approval the decision marks
//! consequential (every engagement signature) carries `"password"`, the signer's own,
//! checked again before it is signed (rules 8, a step-up). The WebSocket also sends
//! `{"type": "decisions", "inbox": ..., "intents": [...]}` and `{"type": "tracks", ...}`
//! (Phase C2, see `tracks`) on connect and whenever they change (existing clients ignore
//! message types they don't know).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use murmur_decide::OperatorView;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::clock;
use crate::decision::{Decision, DecisionError, IntentRequest, Outcome};
use crate::platforms;
use crate::push::{self, ClientView, FrameCache, LodSettings, PushFrame, Subscription};
use crate::tasks::TaskRequest;
use crate::tracks::Tracks;
use crate::world::World;

#[derive(Clone)]
pub struct App {
    pub world: Arc<World>,
    pub frames: Arc<watch::Sender<Option<Arc<PushFrame>>>>,
    pub decision: Arc<Decision>,
    /// contacts that are not ours, as our sensors see them (Phase C2)
    pub tracks: Arc<Tracks>,
    /// the console feed's level-of-detail limits (`/ws?lod=1`, see `push`)
    pub lod: LodSettings,
    /// Who is acting (rules 8): logins, step-ups and agent keys, or dev-mode headers.
    pub auth: Arc<crate::auth::Auth>,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/tasks", post(tasks))
        .route("/api/intents", post(create_intent).get(list_intents))
        .route("/api/intents/{id}", get(get_intent))
        .route("/api/intents/{id}/{action}", post(intent_action))
        .route("/api/inbox", get(inbox))
        .route("/api/restores/write_off", post(write_off))
        .route("/api/restores/unspend", post(unspend))
        .route("/api/operator/view", post(operator_view))
        .route("/api/operators", get(operators))
        .route("/api/engage/options", post(engage_options))
        .route("/api/platforms", get(platforms))
        .route("/api/stats", get(stats))
        .route("/ws", get(ws))
        .route("/api/space/runs", get(space_runs))
        .merge(crate::space::router(app.world.clone(), {
            let d = app.decision.clone();
            Some(Arc::new(move |space, map| d.install_space(space, map)))
        }))
        .merge(crate::cot::router())
        .merge(crate::tracks::router(
            app.tracks.clone(),
            app.world.clone(),
            app.decision.clone(),
            app.auth.clone(),
        ))
        .merge(crate::auth::router(app.auth.clone()))
        .with_state(app)
}

fn error(status: StatusCode, detail: impl Into<String>) -> Response {
    (status, Json(json!({ "detail": detail.into() }))).into_response()
}

fn decision_error(e: DecisionError) -> Response {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut body = json!({ "detail": e.detail });
    if let Some(i) = e.intent {
        body["intent_id"] = i["id"].clone();
        body["state"] = i["state"].clone();
        body["intent"] = i;
    }
    (status, Json(body)).into_response()
}

fn raw_json(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Full state, the same message a WebSocket client gets first.
async fn state(State(app): State<App>) -> Response {
    raw_json(push::state_json(&push::build_frame(
        &app.world,
        &mut FrameCache::default(),
    )))
}

fn last_reason(intent: &Value) -> String {
    intent["history"]
        .as_array()
        .and_then(|h| h.last())
        .and_then(|h| h["reason"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The console's endpoint: a human intent, answered in the tasking shape it always had.
async fn tasks(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<TaskRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let op = match app.auth.actor(&headers, true) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    let task = req.task.clone();
    let params = serde_json::to_value(&req.params).unwrap_or(Value::Null);
    let Outcome {
        id,
        state,
        intent,
        tasking,
        authorization,
        ..
    } = match app.decision.submit(&op, IntentRequest::from(req)).await {
        Ok(o) => o,
        Err(e) => return decision_error(e),
    };
    let state_name = serde_json::to_value(state).unwrap_or(Value::Null);
    if let Some(t) = tasking {
        let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
        v["intent_id"] = json!(id.0);
        v["state"] = state_name;
        v["dispatched"] = json!(true);
        // A human-tier task the operator authorized by sending it: say which tier and why.
        if let Some(a) = authorization {
            v["authorization"] = a;
        }
        return Json(v).into_response();
    }
    let reason = last_reason(&intent);
    let (status, detail) = if state == murmur_decide::State::PendingApproval {
        (
            StatusCode::ACCEPTED,
            format!("not dispatched: waiting for an operator to approve intent {id} ({reason})"),
        )
    } else if state == murmur_decide::State::Queued {
        // rules 5: approved, held for a vehicle the link can't carry it to now
        (
            StatusCode::ACCEPTED,
            format!(
                "queued: intent {id} is approved and goes when the link can carry it, re-checked first ({reason})"
            ),
        )
    } else {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "not dispatched: intent {id} {}: {reason}",
                state_name.as_str().unwrap_or("?")
            ),
        )
    };
    let body = json!({
        "id": null,
        "intent_id": id.0,
        "state": state_name,
        "dispatched": false,
        "detail": detail,
        "task": task,
        "params": params,
        "ops": [],
        "expires_t": intent["expires_t"].clone(),
        "intent": intent,
    });
    (status, Json(body)).into_response()
}

async fn create_intent(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    // rules 8: a nomination accepting lost vehicles may carry the password again (a step-up,
    // when the policy lists accept_unknown); it is taken out here, checked and dropped,
    // never logged or echoed
    let mut v = match body {
        Ok(Json(v)) => v,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let password = v
        .as_object_mut()
        .and_then(|o| o.remove("password"))
        .and_then(|p| p.as_str().map(str::to_string));
    // rules 10: or a passkey's assertion (made for these acceptances)
    let passkey = v.as_object_mut().and_then(|o| o.remove("passkey"));
    let proof = crate::auth::Proof { password, passkey };
    let req: IntentRequest = match serde_json::from_value(v) {
        Ok(r) => r,
        Err(e) => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Failed to deserialize the JSON body into the target type: {e}"),
            );
        }
    };
    let op = match app.auth.actor(&headers, false) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    let accepting = !req.engage.accept_unknown.is_empty();
    if proof.is_some() && !accepting {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a password or passkey goes with a nomination that accepts lost vehicles (a step-up) only",
        );
    }
    let needed = accepting && app.decision.steps_up(murmur_decide::StepUpAct::AcceptUnknown);
    let step = match app.auth.step_up(&op, "accept_unknown", proof, needed).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    match app.decision.submit_stepped(&op, req, step).await {
        Ok(o) => (
            StatusCode::CREATED,
            Json(json!({
                "dispatched": o.dispatched(),
                "intent": o.intent,
                "tasking": o.tasking,
                "space": o.space,
                "authorization": o.authorization,
            })),
        )
            .into_response(),
        Err(e) => decision_error(e),
    }
}

async fn list_intents(State(app): State<App>) -> Response {
    Json(app.decision.intents_json()).into_response()
}

async fn get_intent(State(app): State<App>, Path(id): Path<u64>) -> Response {
    match app.decision.intent_json(id) {
        Some(v) => Json(v).into_response(),
        None => error(StatusCode::NOT_FOUND, format!("unknown intent {id}")),
    }
}

async fn intent_action(
    State(app): State<App>,
    Path((id, action)): Path<(u64, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // rules 8: an approval may carry the approver's password again (a step-up); it is
    // checked and dropped here, never logged or echoed; rules 10: or a passkey's assertion
    // made for this act (`passkey`), which is logged
    let (note, token, acknowledge, password, passkey) = if body.iter().all(u8::is_ascii_whitespace) {
        (None, None, false, None, None)
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) => {
                let token = match v.get("token").filter(|s| !s.is_null()) {
                    None => None,
                    Some(t) => match t.as_u64() {
                        Some(t) => Some(t),
                        None => {
                            return error(StatusCode::UNPROCESSABLE_ENTITY, "token: a picture token id".to_string());
                        }
                    },
                };
                (
                    v["note"].as_str().map(str::to_string),
                    token,
                    v["acknowledge_supersede"].as_bool().unwrap_or(false),
                    v["password"].as_str().map(str::to_string),
                    v.get("passkey").filter(|p| !p.is_null()).cloned(),
                )
            }
            Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "body: not JSON"),
        }
    };
    let proof = crate::auth::Proof { password, passkey };
    let op = match app.auth.actor(&headers, false) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    if action == "picture" {
        return match app.decision.issue_picture(&op, id) {
            Ok(v) => Json(v).into_response(),
            Err(e) => decision_error(e),
        };
    }
    if proof.is_some() && !matches!(action.as_str(), "approve" | "accept_unknown") {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "a password or passkey goes with an approval or an acceptance of a lost vehicle (a step-up) only",
        );
    }
    let result = match action.as_str() {
        "approve" => {
            // a consequential signature re-enters the credential first (credentials run)
            let needed = app.decision.needs_step_up(id);
            let step = match app.auth.step_up(&op, &format!("i{id}"), proof, needed).await {
                Ok(s) => s,
                Err(r) => return *r,
            };
            app.decision.approve_stepped(&op, id, note, token, acknowledge, step).await
        }
        "reject" => app.decision.reject(&op, id, note),
        "cancel" => app.decision.cancel(&op, id, note),
        "abort" => app.decision.abort(&op, id, note),
        // rules 6: {"uid": n, "since": engine ms or null}, as the keep-out check names it
        "accept_unknown" => match serde_json::from_slice::<crate::decision::engage::AcceptUnknown>(&body) {
            Ok(a) => {
                // rules 8: re-enters the credential when the policy lists accept_unknown
                let needed = app.decision.steps_up(murmur_decide::StepUpAct::AcceptUnknown);
                let step = match app.auth.step_up(&op, &format!("i{id} accept_unknown"), proof, needed).await {
                    Ok(s) => s,
                    Err(r) => return *r,
                };
                app.decision.accept_unknown(&op, id, &a, step)
            }
            Err(e) => {
                return error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("accept_unknown wants {{\"uid\": n, \"since\": engine ms or null}}: {e}"),
                );
            }
        },
        // rules 6: {"uid": n}
        "revoke_unknown" => match serde_json::from_slice::<crate::decision::engage::AcceptUnknown>(&body) {
            Ok(a) => app.decision.revoke_unknown(&op, id, a.uid),
            Err(e) => {
                return error(StatusCode::UNPROCESSABLE_ENTITY, format!("revoke_unknown wants {{\"uid\": n}}: {e}"));
            }
        },
        _ => {
            return error(
                StatusCode::NOT_FOUND,
                format!(
                    "unknown action {action:?} (approve, reject, cancel, abort, picture, accept_unknown, revoke_unknown)"
                ),
            );
        }
    };
    match result {
        Ok(o) => Json(json!({
            "dispatched": o.dispatched(),
            "intent": o.intent,
            "tasking": o.tasking,
            "space": o.space,
        }))
        .into_response(),
        Err(e) => decision_error(e),
    }
}

async fn inbox(State(app): State<App>) -> Response {
    Json(app.decision.inbox_json()).into_response()
}

/// `POST /api/restores/write_off` (rules 12): an operator writes off what is owed back: a
/// vehicle's own lost-link values (`{"uid": n, "note": "..."}`: it is lost or spent, or another
/// vehicle has its uid now) or an earlier log whose carry failed (`{"log": "<file>", "note":
/// "..."}`: gone for good). Logged as `restore.closed` under the operator's own session; the
/// note is required. 409 when nothing such is owed or an engagement is under way on the vehicle.
async fn write_off(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct WriteOff {
        #[serde(default)]
        uid: Option<crate::vehicle::Uid>,
        #[serde(default)]
        log: Option<String>,
        #[serde(default)]
        note: String,
    }
    let req = match serde_json::from_slice::<WriteOff>(&body) {
        Ok(r) => r,
        Err(e) => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("write_off wants {{\"uid\": n}} or {{\"log\": \"<file>\"}} with a \"note\": {e}"),
            );
        }
    };
    let op = match app.auth.actor(&headers, false) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    match app.decision.write_off(&op, req.uid, req.log, &req.note) {
        Ok(v) => Json(v).into_response(),
        Err(e) => decision_error(e),
    }
}

/// `POST /api/restores/unspend` (rules 15): an operator with engagement authority un-spends a
/// platform spent by its own effect that they believe survived (`{"uid": n, "note": "..."}`),
/// once the engagement that spent it ended and the platform was heard since and is heard now.
/// Logged as `platform.unspent` under the operator's own session; the note is required. Its
/// own lost-link values are owed back again (a mission waits for the restore). In a credentials
/// run it makes an armed platform taskable again, so it re-enters the credential (`"password"`,
/// or `"passkey"`: an assertion made for this un-spend, `/api/auth/step_up {"act": "unspend",
/// "uid": n}`), checked and dropped here, never logged or echoed. In that order: 403 without
/// engagement authority, 409 for a platform not spent or one the rules refuse now (a dry check:
/// nothing logged, no credential asked for), then 401 without a valid step-up.
async fn unspend(State(app): State<App>, headers: HeaderMap, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct Unspend {
        uid: crate::vehicle::Uid,
        #[serde(default)]
        note: String,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        passkey: Option<Value>,
    }
    let req = match serde_json::from_slice::<Unspend>(&body) {
        Ok(r) => r,
        Err(e) => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("unspend wants {{\"uid\": n, \"note\": \"why\"}}: {e}"),
            );
        }
    };
    let op = match app.auth.actor(&headers, false) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    // only engagement authority un-spends, and only what the rules take now: both said before
    // any credential is asked for (a refused un-spend never takes, nor counts, a password)
    if let Err(e) = app.decision.unspend_check(&op, req.uid) {
        return decision_error(e);
    }
    let proof = crate::auth::Proof {
        password: req.password,
        passkey: req.passkey.filter(|p| !p.is_null()),
    };
    let step = match app.auth.step_up(&op, &format!("unspend vehicle {}", req.uid), proof, true).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    match app.decision.unspend(&op, req.uid, &req.note, step) {
        Ok(v) => Json(v).into_response(),
        Err(e) => decision_error(e),
    }
}

/// `POST /api/operator/view` (Phase G): the console says what it showed for a pending
/// intent. `{"intent_id": n, "event": "open"|"close"|"decide", "action": "approve"|"reject"
/// (decide only), "shown": {"checks": [rule ids], "warnings": [lines], "rationale": bool,
/// "tracks": [track ids]}, "open_ms": n, "client_t": ms}`. Logged as `operator.view` under
/// the operator's session; it decides nothing. The operator header is required (no default
/// operator): a display event attributed to someone who did not send it would corrupt the
/// measurement.
#[derive(Deserialize)]
struct ViewBody {
    intent_id: u64,
    #[serde(flatten)]
    view: OperatorView,
}

async fn operator_view(
    State(app): State<App>,
    headers: HeaderMap,
    body: Result<Json<ViewBody>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    // a display report is not an operator's action: it does not keep the session alive
    let op = match app.auth.actor_peek(&headers) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    match app.decision.record_view(&op, body.intent_id, body.view) {
        Ok(()) => Json(json!({ "ok": true, "intent_id": body.intent_id })).into_response(),
        Err(e) => decision_error(e),
    }
}

/// The nomination form's dry run: which armed platforms could take an engagement on a track
/// now, each with its checks and preview (read-only; nothing is created or logged).
async fn engage_options(
    State(app): State<App>,
    body: Result<Json<crate::decision::engage::OptionsRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    match app.decision.engage_options(req).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => decision_error(e),
    }
}

/// The operators the bridge accepts, those with engagement authority, and how they are
/// authenticated (`credentials`: the credential file; `dev`: `MURMUR_OPERATORS`,
/// `MURMUR_ENGAGE_OPERATORS`, asserted by header).
async fn operators(State(app): State<App>) -> Response {
    Json(app.decision.operators_json()).into_response()
}

/// Collect and cover runs: the scenario clock map and each run's states (also in the
/// WebSocket `decisions` message as `space`).
async fn space_runs(State(app): State<App>) -> Response {
    Json(app.decision.space_runs_json()).into_response()
}

async fn platforms(State(app): State<App>) -> Response {
    let world = app.world.clone();
    match tokio::task::spawn_blocking(move || platforms::catalog(&world)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Per-vehicle packet counters (for load tests; kept out of the UI stream), and the GCS
/// heartbeat's jitter (`crate::heartbeat`).
async fn stats(State(app): State<App>) -> Response {
    let vehicles: Map<String, Value> = app
        .world
        .vehicles()
        .iter()
        .map(|v| {
            (
                v.uid.to_string(),
                serde_json::to_value(v.link_counters()).unwrap_or(Value::Null),
            )
        })
        .collect();
    // the heartbeat thread's jitter: worst send delay and longest gap per minute
    let heartbeat = crate::heartbeat::stats().map_or(Value::Null, |s| s.json());
    Json(json!({ "t": clock::wall(), "vehicles": vehicles, "gcs_heartbeat": heartbeat }))
        .into_response()
}

/// `/ws` is the feed as it always was; `/ws?lod=1` asks for level of detail by the view
/// the client then sends (`{"type":"view",...}` messages, see `push`).
#[derive(Deserialize)]
struct WsQuery {
    #[serde(default)]
    lod: Option<String>,
}

async fn ws(State(app): State<App>, Query(q): Query<WsQuery>, upgrade: WebSocketUpgrade) -> Response {
    let lod = matches!(q.lod.as_deref(), Some("1" | "true" | "yes"));
    upgrade.on_upgrade(move |socket| client(socket, app, lod))
}

async fn client(mut socket: WebSocket, app: App, lod: bool) {
    let mut frames = app.frames.subscribe();
    let mut view = ClientView::new(lod.then_some(app.lod));
    let full = view.full_state(&push::build_frame(&app.world, &mut FrameCache::default()));
    if socket.send(Message::Text(full.into())).await.is_err() {
        return;
    }
    // the inbox and intents: a message type of its own, after the state
    let mut decisions = app.decision.subscribe();
    let first = decisions.borrow_and_update().clone();
    if socket
        .send(Message::Text(first.as_str().into()))
        .await
        .is_err()
    {
        return;
    }
    // tracks: another message type, sent whole whenever a track changes
    let mut tracks = app.tracks.subscribe();
    let first = tracks.borrow_and_update().clone();
    if socket
        .send(Message::Text(first.as_str().into()))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            changed = decisions.changed() => {
                if changed.is_err() {
                    break;
                }
                let msg = decisions.borrow_and_update().clone();
                if socket.send(Message::Text(msg.as_str().into())).await.is_err() {
                    break;
                }
            }
            changed = tracks.changed() => {
                if changed.is_err() {
                    break;
                }
                let msg = tracks.borrow_and_update().clone();
                if socket.send(Message::Text(msg.as_str().into())).await.is_err() {
                    break;
                }
            }
            changed = frames.changed() => {
                if changed.is_err() {
                    break;
                }
                let frame = frames.borrow_and_update().clone();
                let Some(frame) = frame else { continue };
                if socket.send(Message::Text(view.delta(&frame).into())).await.is_err() {
                    break;
                }
            }
            // tasks go over REST; the only message a client sends here is its view, which
            // gets a delta at once so a pan does not wait for the next tick
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Text(text))) => {
                    if let Some(sub) = Subscription::parse(&text) {
                        view.subscribe(sub);
                        let frame = frames.borrow().clone();
                        if let Some(frame) = frame
                            && socket.send(Message::Text(view.delta(&frame).into())).await.is_err()
                        {
                            break;
                        }
                    }
                }
                Some(Ok(_)) => {}
            },
        }
    }
}
