//! Situation status, for display only (`sim/situations/`, the runner `eval/situation_run.py`).
//!
//! A situation is a pre-built scenario started with one command: a stack, opening orders, an
//! AI watch officer, a scripted timeline. Its runner posts one status document here on the
//! simulation channel, `POST /api/sim/situation` (the stack's own token in
//! `X-Murmur-Sim-Token`, the same guard as `POST /api/sim/contacts`: checked on the headers
//! before the body is read); the console reads it back on `GET /api/situation` and shows the
//! Situation panel (title, brief, modes, the clock, the timeline done so far, what to watch
//! for, the AI's recent activity). `DELETE /api/sim/situation` (same token) clears it.
//!
//! **Display only.** The document is kept in memory here and nowhere else: it never reaches
//! the decision engine (this module holds no reference to it), the world snapshot, the
//! tracks, the MCP server's tools or the audit log. Truth and a director's notes must not
//! leak into decisions; the runner writes its own record (`situation.jsonl`), and the audit
//! chain stays the engine's alone. What the document shows a person is the runner's choice:
//! with `reveal = false` (a blinded run) it carries no director's notes and nothing of the
//! timeline still to come (`docs/DECISIONS.md`, "Situation status is display only").

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use crate::clock;

/// The largest status document the channel reads, bytes: a title, a brief, a few dozen
/// timeline entries and activity lines fit many times over.
pub const BODY_LIMIT: usize = 256 * 1024;

struct Stored {
    doc: Value,
    /// When it arrived (the bridge's clock, s).
    received_t: f64,
    /// Documents received so far.
    posts: u64,
}

/// The one status document, in memory.
pub struct Situation {
    /// The simulation channel's token (`MURMUR_SIM_TOKEN`, as `crate::tracks` reads it).
    token: Option<String>,
    doc: Mutex<Option<Stored>>,
}

impl Situation {
    pub fn new(token: Option<String>) -> Arc<Situation> {
        Arc::new(Situation { token, doc: Mutex::new(None) })
    }

    /// With the token the tracks' simulation channel takes (`MURMUR_SIM_TOKEN`).
    pub fn from_env() -> Arc<Situation> {
        Situation::new(crate::tracks::Settings::from_env().sim_token)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Stored>> {
        self.doc.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `GET /api/situation`: the document (null when no situation runs) and when it arrived.
    pub fn json(&self) -> Value {
        match &*self.lock() {
            Some(s) => json!({ "situation": s.doc, "received_t": s.received_t, "posts": s.posts }),
            None => json!({ "situation": null, "received_t": null, "posts": 0 }),
        }
    }

    /// Store a document; the title is announced on stdout when it changes (the bridge's log,
    /// not the audit log).
    fn set(&self, doc: Value) -> f64 {
        let t = clock::wall();
        let mut g = self.lock();
        let title = doc["title"].as_str().unwrap_or_default().to_string();
        let was = g.as_ref().and_then(|s| s.doc["title"].as_str().map(str::to_string));
        if was.as_deref() != Some(title.as_str()) {
            println!("situation: {title:?} (status for display only: not a decision input, not in the audit log)");
        }
        let posts = g.as_ref().map_or(0, |s| s.posts) + 1;
        *g = Some(Stored { doc, received_t: t, posts });
        t
    }

    fn clear(&self) -> bool {
        self.lock().take().is_some()
    }
}

fn error(status: StatusCode, detail: impl Into<String>) -> Response {
    (status, Json(json!({ "detail": detail.into() }))).into_response()
}

/// `GET /api/situation`, `POST` and `DELETE /api/sim/situation`.
pub fn router<S: Clone + Send + Sync + 'static>(situation: Arc<Situation>) -> Router<S> {
    Router::new()
        .route("/api/situation", get(read))
        .route("/api/sim/situation", axum::routing::post(post).delete(clear))
        .with_state(situation)
}

async fn read(State(s): State<Arc<Situation>>) -> Response {
    Json(s.json()).into_response()
}

/// The refusal when a request does not carry the stack's simulation token, else None.
fn refused(s: &Situation, headers: &HeaderMap) -> Option<Response> {
    crate::tracks::sim_token_check(s.token.as_deref(), headers)
        .err()
        .map(|why| error(StatusCode::UNAUTHORIZED, why))
}

/// The runner's status document: a JSON object with a non-empty `title`, at most
/// [`BODY_LIMIT`] bytes. The token is checked on the headers before the body is read.
async fn post(State(s): State<Arc<Situation>>, request: axum::extract::Request) -> Response {
    if let Some(r) = refused(&s, request.headers()) {
        return r;
    }
    let bytes = match axum::body::to_bytes(request.into_body(), BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::PAYLOAD_TOO_LARGE, format!("situation status: {e}")),
    };
    let doc: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return error(StatusCode::UNPROCESSABLE_ENTITY, format!("situation status: {e}")),
    };
    if !doc.is_object() {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "situation status: a JSON object");
    }
    if doc["title"].as_str().is_none_or(|t| t.trim().is_empty()) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "situation status: needs a title");
    }
    let t = s.set(doc);
    Json(json!({ "ok": true, "received_t": t })).into_response()
}

async fn clear(State(s): State<Arc<Situation>>, headers: HeaderMap) -> Response {
    if let Some(r) = refused(&s, &headers) {
        return r;
    }
    Json(json!({ "ok": true, "cleared": s.clear() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    const TOKEN: &str = "situation-test-token";

    async fn call(s: &Arc<Situation>, method: &str, token: Option<&str>, body: Option<String>) -> (StatusCode, Value) {
        let path = if method == "GET" { "/api/situation" } else { "/api/sim/situation" };
        let mut b = Request::builder().method(method).uri(path).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(crate::tracks::SIM_TOKEN_HEADER, t);
        }
        let req = b.body(body.map_or_else(Body::empty, Body::from)).expect("request");
        let res = router::<()>(s.clone()).oneshot(req).await.expect("response");
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.expect("body");
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn doc(title: &str) -> String {
        json!({ "title": title, "brief": "Hold the Gate.", "modes": { "operator": "live", "ai": "none" } }).to_string()
    }

    #[tokio::test]
    async fn nothing_is_shown_until_a_runner_posts() {
        let s = Situation::new(Some(TOKEN.into()));
        let (st, v) = call(&s, "GET", None, None).await;
        assert_eq!(st, StatusCode::OK);
        assert!(v["situation"].is_null(), "{v}");
        assert_eq!(v["posts"], 0);
    }

    #[tokio::test]
    async fn the_status_takes_the_stacks_simulation_token() {
        let s = Situation::new(Some(TOKEN.into()));
        for token in [None, Some("situation-test-tokeN"), Some("")] {
            let (st, v) = call(&s, "POST", token, Some(doc("Harbor watch"))).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{token:?}: {v}");
        }
        let (st, _) = call(&s, "DELETE", Some("wrong"), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
        assert!(call(&s, "GET", None, None).await.1["situation"].is_null());
        // no token configured: the channel is closed, whatever a request carries
        let closed = Situation::new(None);
        let (st, _) = call(&closed, "POST", Some(TOKEN), Some(doc("Harbor watch"))).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_posted_status_is_served_back_until_cleared() {
        let s = Situation::new(Some(TOKEN.into()));
        let (st, v) = call(&s, "POST", Some(TOKEN), Some(doc("Harbor watch"))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert!(v["received_t"].as_f64().is_some());
        let (_, v) = call(&s, "GET", None, None).await;
        assert_eq!(v["situation"]["title"], "Harbor watch");
        assert_eq!(v["situation"]["modes"]["ai"], "none");
        assert_eq!(v["posts"], 1);
        // a newer document replaces it whole
        call(&s, "POST", Some(TOKEN), Some(doc("Harbor watch, ended"))).await;
        let (_, v) = call(&s, "GET", None, None).await;
        assert_eq!(v["situation"]["title"], "Harbor watch, ended");
        assert_eq!(v["posts"], 2);
        let (st, v) = call(&s, "DELETE", Some(TOKEN), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["cleared"], true);
        assert!(call(&s, "GET", None, None).await.1["situation"].is_null());
    }

    #[tokio::test]
    async fn only_a_titled_object_within_the_limit_is_taken() {
        let s = Situation::new(Some(TOKEN.into()));
        for bad in ["[1, 2]", "\"Harbor watch\"", "{\"brief\": \"no title\"}", "{\"title\": \"  \"}", "not json"] {
            let (st, v) = call(&s, "POST", Some(TOKEN), Some(bad.into())).await;
            assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {v}");
        }
        let big = json!({ "title": "big", "brief": "x".repeat(BODY_LIMIT) }).to_string();
        let (st, _) = call(&s, "POST", Some(TOKEN), Some(big)).await;
        assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(call(&s, "GET", None, None).await.1["situation"].is_null());
    }
}
