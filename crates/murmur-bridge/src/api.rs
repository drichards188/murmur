//! HTTP and WebSocket API (same routes and JSON as the Python bridge, keyed by uid).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Map, Value, json};
use tokio::sync::watch;

use crate::clock;
use crate::platforms;
use crate::push::{self, ClientView, PushFrame, TrailCache};
use crate::tasks::{OrderError, TaskOrder, TaskRequest};
use crate::world::World;

#[derive(Clone)]
pub struct App {
    pub world: Arc<World>,
    pub frames: Arc<watch::Sender<Option<Arc<PushFrame>>>>,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/api/state", get(state))
        .route("/api/tasks", post(tasks))
        .route("/api/platforms", get(platforms))
        .route("/api/stats", get(stats))
        .route("/ws", get(ws))
        .merge(crate::space::router(app.world.clone()))
        .merge(crate::cot::router())
        .with_state(app)
}

fn error(status: StatusCode, detail: impl Into<String>) -> Response {
    (status, Json(json!({ "detail": detail.into() }))).into_response()
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

async fn tasks(State(app): State<App>, body: Result<Json<TaskRequest>, JsonRejection>) -> Response {
    let req = match body {
        Ok(Json(req)) => req,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, e.body_text()),
    };
    let fleet = app
        .world
        .vehicles()
        .into_iter()
        .map(|v| (v.uid, v))
        .collect();
    match TaskOrder::new(req, &fleet) {
        Ok(order) => Json(app.world.tasks.dispatch(order).snapshot()).into_response(),
        Err(e @ OrderError::UnknownVehicles(_)) => error(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => error(StatusCode::BAD_REQUEST, e.to_string()),
    }
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
    loop {
        tokio::select! {
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
