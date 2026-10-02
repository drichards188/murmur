//! HTTP and WebSocket API (same routes and JSON as the Python bridge, keyed by uid), plus
//! the decision layer's intents and inbox.
//!
//! Every task goes through `crate::decision`: `POST /api/tasks` (the console) is a thin
//! wrapper that creates a human intent. Its response is the tasking as before plus
//! `intent_id`, `state` and `dispatched`; an intent that needs approval answers 202 with
//! `dispatched: false` and why, one the rules reject answers 422 with the reasons.
//!
//! Intents: `POST /api/intents` (the task request plus optional `constraints` and
//! `rationale`), `GET /api/intents`, `GET /api/intents/{id}`, `POST
//! /api/intents/{id}/approve|reject|cancel` (optional body `{"note": "..."}`), `GET
//! /api/inbox`. These name the operator in the `X-Murmur-Operator` header, which must be one
//! of `MURMUR_OPERATORS`: a placeholder for real authentication. The WebSocket also sends
//! `{"type": "decisions", "inbox": ..., "intents": [...]}` and `{"type": "tracks", ...}`
//! (Phase C2, see `tracks`) on connect and whenever they change (existing clients ignore
//! message types they don't know).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::clock;
use crate::decision::{Decision, DecisionError, IntentRequest, Outcome};
use crate::platforms;
use crate::push::{self, ClientView, PushFrame, TrailCache};
use crate::tasks::TaskRequest;
use crate::tracks::Tracks;
use crate::world::World;

pub const OPERATOR_HEADER: &str = "x-murmur-operator";

#[derive(Clone)]
pub struct App {
    pub world: Arc<World>,
    pub frames: Arc<watch::Sender<Option<Arc<PushFrame>>>>,
    pub decision: Arc<Decision>,
    /// contacts that are not ours, as our sensors see them (Phase C2)
    pub tracks: Arc<Tracks>,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/tasks", post(tasks))
        .route("/api/intents", post(create_intent).get(list_intents))
        .route("/api/intents/{id}", get(get_intent))
        .route("/api/intents/{id}/{action}", post(intent_action))
        .route("/api/inbox", get(inbox))
        .route("/api/platforms", get(platforms))
        .route("/api/stats", get(stats))
        .route("/ws", get(ws))
        .route("/api/space/runs", get(space_runs))
        .merge(crate::space::router(app.world.clone(), {
            let d = app.decision.clone();
            Some(Box::new(move |space, map| d.install_space(space, map)))
        }))
        .merge(crate::cot::router())
        .merge(crate::tracks::router(
            app.tracks.clone(),
            app.world.clone(),
            app.decision.clone(),
        ))
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

fn operator_header(headers: &HeaderMap) -> Option<&str> {
    headers.get(OPERATOR_HEADER).and_then(|v| v.to_str().ok())
}

fn raw_json(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// Full state, the same message a WebSocket client gets first.
async fn state(State(app): State<App>) -> Response {
    raw_json(push::state_json(&push::build_frame(
        &app.world,
        &mut TrailCache::default(),
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
    let op = match app.decision.operator(operator_header(&headers), true) {
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
    body: Result<Json<IntentRequest>, JsonRejection>,
) -> Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let op = match app.decision.operator(operator_header(&headers), false) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    match app.decision.submit(&op, req).await {
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
    let note = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(v) => v["note"].as_str().map(str::to_string),
            Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, format!("body: {e}")),
        }
    };
    let op = match app.decision.operator(operator_header(&headers), false) {
        Ok(op) => op,
        Err(e) => return decision_error(e),
    };
    let result = match action.as_str() {
        "approve" => app.decision.approve(&op, id, note).await,
        "reject" => app.decision.reject(&op, id, note),
        "cancel" => app.decision.cancel(&op, id, note),
        _ => {
            return error(
                StatusCode::NOT_FOUND,
                format!("unknown action {action:?} (approve, reject, cancel)"),
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

/// Per-vehicle packet counters (for load tests; kept out of the UI stream).
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
    Json(json!({ "t": clock::wall(), "vehicles": vehicles })).into_response()
}

async fn ws(State(app): State<App>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| client(socket, app))
}

async fn client(mut socket: WebSocket, app: App) {
    let mut frames = app.frames.subscribe();
    let (mut view, full) =
        ClientView::full_state(&push::build_frame(&app.world, &mut TrailCache::default()));
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
            // client->server traffic goes over REST; this just notices the close
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
