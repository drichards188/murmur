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
//! chain stays the engine's alone. The bridge itself enforces a blinded run: unless the
//! document says `"reveal": true` (exactly), it keeps only the keys a blinded panel shows
//! ([`BLIND_KEYS`]: so not the director's notes, `watch_upcoming` or any key it does not know),
//! of the timeline only what is done, and drops every timeline entry and watch item whose
//! `director` is anything but absent or `false`, whatever the runner sent (`docs/DECISIONS.md`,
//! "Situation status is display only").
//! The stored bytes are served as they are (`GET` is open on the local port, like every read).

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
    /// The document as stored (blinded unless revealed), serialized once when it arrived.
    body: Arc<str>,
    title: String,
    /// When it arrived (the bridge's clock, s).
    received_t: f64,
    /// Documents received so far.
    posts: u64,
}

/// The longest title the bridge's log repeats, characters.
const LOG_TITLE_CHARS: usize = 80;

/// The top-level keys a blinded run's document keeps (`eval/situation.py` `status_doc`):
/// everything else, the director's notes and `watch_upcoming` among them, is dropped.
/// `phase_detail` and `problems` are the runner's own fixed messages (it never puts a step's or
/// the director's text in them); they are kept only as strings.
pub const BLIND_KEYS: &[&str] = &[
    "schema", "name", "title", "brief", "reveal", "modes", "scripted", "phase", "phase_detail", "clock",
    "orders", "timeline", "watch", "agent", "autoplay", "run", "problems",
];

/// The longest `phase_detail` or `problems` line a blinded document keeps, characters.
const BLIND_TEXT_CHARS: usize = 400;

/// A blinded run's document (`reveal` not exactly `true`): only [`BLIND_KEYS`]; of the
/// timeline only `done`; no timeline entry or watch item whose `director` is anything but
/// absent or `false` (a `"yes"` or a `1` counts as the director's).
pub fn blind(doc: &mut Value) {
    if doc.get("reveal") == Some(&Value::Bool(true)) {
        return;
    }
    let Some(o) = doc.as_object_mut() else { return };
    o.retain(|k, _| BLIND_KEYS.contains(&k.as_str()));
    let not_director = |e: &Value| matches!(e.get("director"), None | Some(Value::Bool(false)));
    let text = |v: &Value| v.as_str().map(|t| Value::String(t.chars().take(BLIND_TEXT_CHARS).collect()));
    match o.get("phase_detail").and_then(text) {
        Some(t) => {
            o.insert("phase_detail".into(), t);
        }
        None => {
            o.remove("phase_detail");
        }
    }
    if let Some(p) = o.get_mut("problems") {
        let lines: Vec<Value> = p.as_array().map(|a| a.iter().filter_map(text).collect()).unwrap_or_default();
        *p = Value::Array(lines);
    }
    if let Some(t) = o.get_mut("timeline").and_then(Value::as_object_mut) {
        t.retain(|k, _| k == "done");
        if let Some(done) = t.get_mut("done").and_then(Value::as_array_mut) {
            done.retain(not_director);
        }
    } else {
        o.remove("timeline");
    }
    if let Some(w) = o.get_mut("watch").and_then(Value::as_array_mut) {
        w.retain(not_director);
    } else {
        o.remove("watch");
    }
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

    /// `GET /api/situation`: the stored document as it is (null when no situation runs), when
    /// it arrived and how many came. Only an `Arc` is cloned under the lock.
    pub fn body(&self) -> String {
        let got = self.lock().as_ref().map(|s| (s.body.clone(), s.received_t, s.posts));
        match got {
            Some((body, t, posts)) => format!("{{\"situation\":{body},\"received_t\":{t},\"posts\":{posts}}}"),
            None => "{\"situation\":null,\"received_t\":null,\"posts\":0}".to_string(),
        }
    }

    /// Store a document, blinded unless it is revealed; the title is announced on stdout when
    /// it changes (the bridge's log, not the audit log; truncated).
    fn set(&self, mut doc: Value) -> f64 {
        blind(&mut doc);
        let title: String = doc["title"].as_str().unwrap_or_default().chars().take(LOG_TITLE_CHARS).collect();
        let body: Arc<str> = Arc::from(doc.to_string());
        let t = clock::wall();
        let mut g = self.lock();
        let changed = g.as_ref().is_none_or(|s| s.title != title);
        let posts = g.as_ref().map_or(0, |s| s.posts) + 1;
        *g = Some(Stored { body, title: title.clone(), received_t: t, posts });
        drop(g);
        if changed {
            println!("situation: {title:?} (status for display only: not a decision input, not in the audit log)");
        }
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
    ([(axum::http::header::CONTENT_TYPE, "application/json")], s.body()).into_response()
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

    /// Review B3: the bridge blinds the document itself unless it says `reveal: true`.
    #[tokio::test]
    async fn a_document_not_revealed_is_stored_blind() {
        let s = Situation::new(Some(TOKEN.into()));
        let full = |reveal: Value| {
            json!({
                "title": "Harbor watch",
                "reveal": reveal,
                "director": "a fast boat comes in at 2:30 (truth)",
                "timeline": {
                    "done": [
                        { "t_s": 3.0, "text": "fleet ready" },
                        { "t_s": 150.0, "text": "the boat enters (truth)", "director": true }
                    ],
                    "upcoming": [{ "at_s": 370.0, "text": "the boat holds (truth)" }]
                },
                "watch": [
                    { "at_s": 0.0, "text": "watch", "director": false },
                    { "at_s": 60.0, "text": "the answer", "director": true },
                    { "at_s": 61.0, "text": "another answer (truth)", "director": "yes" },
                    { "at_s": 62.0, "text": "a third answer (truth)", "director": 1 }
                ],
                "watch_upcoming": [{ "at_s": 600.0, "text": "later" }],
                // keys a blinded panel does not show: dropped, whatever they hold
                "notes": "the boat is unknown (truth)",
                "phase_detail": { "detail": "an object (truth)" },
                "problems": ["the timeline stopped (StepError)", { "text": "(truth)" }]
            })
            .to_string()
        };
        for reveal in [json!(false), json!(null), json!("true"), json!(1)] {
            call(&s, "POST", Some(TOKEN), Some(full(reveal.clone()))).await;
            let (_, v) = call(&s, "GET", None, None).await;
            let doc = &v["situation"];
            let text = doc.to_string();
            assert!(doc.get("director").is_none() && doc.get("watch_upcoming").is_none(), "{reveal}: {text}");
            assert!(doc["timeline"].get("upcoming").is_none(), "{reveal}: {text}");
            assert!(!text.contains("(truth)") && !text.contains("the answer"), "{reveal}: {text}");
            assert_eq!(doc["timeline"]["done"].as_array().map(Vec::len), Some(1));
            assert_eq!(doc["watch"].as_array().map(Vec::len), Some(1));
            assert!(doc.get("notes").is_none() && doc.get("phase_detail").is_none(), "{reveal}: {text}");
            assert_eq!(doc["problems"], json!(["the timeline stopped (StepError)"]));
        }
        call(&s, "POST", Some(TOKEN), Some(full(json!(true)))).await;
        let (_, v) = call(&s, "GET", None, None).await;
        let doc = &v["situation"];
        assert_eq!(doc["director"], "a fast boat comes in at 2:30 (truth)");
        assert_eq!(doc["timeline"]["done"].as_array().map(Vec::len), Some(2));
        assert_eq!(doc["timeline"]["upcoming"].as_array().map(Vec::len), Some(1));
        assert_eq!(doc["watch"].as_array().map(Vec::len), Some(4));
        assert!(doc.get("watch_upcoming").is_some() && doc.get("notes").is_some());
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
