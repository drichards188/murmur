//! Who is acting (rules 7, murmur-der.1): operators log in with their own password, agents
//! present their own key, and nothing else is believed.
//!
//! **Credentials run** (the default; `MURMUR_AUTH` unset or `credentials`). The credential
//! file (`crates/murmur-auth`: `MURMUR_CREDENTIALS`, else `~/.config/murmur/credentials.json`)
//! names the run's operators and who of them holds engagement authority; the roster logged in
//! `run.started` is that file when the bridge starts.
//!
//! - `POST /api/auth/login {"operator", "password"}` checks the password (argon2id, constant
//!   time; an unknown id costs the same), opens an engine session for this login
//!   (`session.opened`, `auth: password`) and sets an HttpOnly, SameSite=Strict cookie
//!   (`murmur_session_<http port>`, Path=/, Max-Age the session's lifetime; `Secure` with
//!   `MURMUR_COOKIE_SECURE=1`). The cookie holds a random 256-bit token; the bridge keeps only
//!   its SHA-256. Five failures for an id lock it (30 s, doubling to 15 min, 429 with
//!   Retry-After); a locked id is refused without checking the password.
//! - `POST /api/auth/logout` ends the session (`session.closed`) and clears the cookie.
//!   `GET /api/auth/session` says who is signed in (without counting as activity).
//! - A session ends after `MURMUR_SESSION_IDLE_S` (1800) without an action, or
//!   `MURMUR_SESSION_MAX_S` (28800) after login, and at once when its operator's credential is
//!   removed or changed (a new password, authority granted or revoked): the file is re-read
//!   when it changes, checked every 2 s. Each end is logged (`session.closed` with why); a
//!   replayed or expired cookie is a 401.
//! - Every operator request acts for the login its cookie names; an `X-Murmur-Operator` header
//!   naming someone else is a 403, an agent's key is not a login (401).
//! - A consequential approval (every engagement signature; a tier with `step_up`) needs the
//!   password again in the request (`"password"` in the approve body): checked like a login,
//!   for the session's own operator only, counted against the same lock (a lock from failed
//!   step-ups also ends the session), then logged as `operator.action step_up` and quoted by
//!   the signature ([`StepUpChecked`], made only here); a refused one is logged too
//!   (`operator.action step_up.refused`: no password, a wrong one, a lock; never the password).
//! - Agents present `Authorization: Bearer <key>` on every `/mcp` request (`murmur-auth
//!   agent-add`); the key's digest is compared in constant time. A removed or rotated key stops
//!   at once.
//!
//! **Dev run** (`MURMUR_AUTH=dev`): the old placeholder, explicit and loud. Operators are
//! whoever `X-Murmur-Operator` names (`MURMUR_OPERATORS`, `MURMUR_ENGAGE_OPERATORS`; the
//! legacy `POST /api/tasks` and track declarations without a header act as the first),
//! agents whoever `X-Murmur-Agent` or `clientInfo.name` names (`MURMUR_AGENTS`). The bridge
//! says so at start, `run.started` records `auth: dev`, and replay and the evaluation exclude
//! such a run from any claim about people. For scripted tests and the evaluation's proxy.
//!
//! **Both**: a state-changing request (any method but GET, HEAD, OPTIONS) or a WebSocket
//! upgrade from a browser must come from an allowed origin ([`check_origin`]): an `Origin`
//! header must be one of `MURMUR_ALLOWED_ORIGINS` (default: the console's and the bridge's own
//! on 127.0.0.1 and localhost, `MURMUR_WEB_PORT` 5173 and the HTTP port), and without one a
//! `Sec-Fetch-Site` other than `same-origin` or `none` is refused. SameSite=Strict alone does
//! not stop a page on another port of the same host (same site, other origin). Clients that
//! send neither header are not browsers and carry no one's cookie.
//!
//! Nothing secret is logged anywhere: no password, token or key reaches the audit log, stdout
//! or an error message (the audit log names operators, sessions, methods and key ids).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use murmur_auth::{CredentialFile, Limiter, Lookup, SessionTable};
use murmur_decide::{AgentId, AuthMode, OperatorId, SessionId};
use serde::Deserialize;
use serde_json::json;

use crate::decision::{Actor, Decision, DecisionError};

/// The header a dev run names its operator in.
pub const OPERATOR_HEADER: &str = "x-murmur-operator";

/// How many password checks run at once (argon2id takes ~20 MiB and tens of ms each).
const VERIFY_AT_ONCE: usize = 2;
/// How often the credential file is looked at and expired sessions are closed.
const SWEEP_EVERY: Duration = Duration::from_secs(2);

/// From the environment (see the module docs).
#[derive(Clone, Debug)]
pub struct Settings {
    pub mode: AuthMode,
    /// The credential file (credentials run).
    pub file: Option<PathBuf>,
    /// The session cookie's name: one per bridge port, since browsers share cookies across
    /// the ports of one host.
    pub cookie: String,
    pub secure_cookie: bool,
    pub idle: Duration,
    pub max: Duration,
    /// Origins a browser's state-changing request may come from.
    pub origins: Vec<String>,
}

impl Settings {
    pub fn from_env(http_port: u16) -> Result<Settings, String> {
        let var = |k: &str| std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let mode = match var("MURMUR_AUTH").as_deref() {
            None | Some("credentials") => AuthMode::Credentials,
            Some("dev") => AuthMode::Dev,
            Some(other) => {
                return Err(format!("MURMUR_AUTH={other:?}: credentials (the default) or dev"));
            }
        };
        let secs = |k: &str, d: u64| -> Result<Duration, String> {
            match var(k) {
                None => Ok(Duration::from_secs(d)),
                Some(s) => s
                    .parse::<u64>()
                    .ok()
                    .filter(|n| (60..=7 * 86_400).contains(n))
                    .map(Duration::from_secs)
                    .ok_or_else(|| format!("{k}={s:?}: seconds, 60 to 604800")),
            }
        };
        let web = var("MURMUR_WEB_PORT").and_then(|p| p.parse::<u16>().ok()).unwrap_or(5173);
        let origins = match var("MURMUR_ALLOWED_ORIGINS") {
            Some(list) => list.split(',').map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).collect(),
            None => default_origins(web, http_port),
        };
        Ok(Settings {
            mode,
            file: match mode {
                AuthMode::Credentials => Some(murmur_auth::default_path().ok_or("no credential file: set MURMUR_CREDENTIALS or HOME")?),
                AuthMode::Dev => None,
            },
            cookie: format!("murmur_session_{http_port}"),
            secure_cookie: var("MURMUR_COOKIE_SECURE").as_deref() == Some("1"),
            idle: secs("MURMUR_SESSION_IDLE_S", 1800)?,
            max: secs("MURMUR_SESSION_MAX_S", 8 * 3600)?,
            origins,
        })
    }

    /// Dev-mode settings for tests.
    #[cfg(test)]
    pub fn dev() -> Settings {
        Settings {
            mode: AuthMode::Dev,
            file: None,
            cookie: "murmur_session_test".into(),
            secure_cookie: false,
            idle: Duration::from_secs(1800),
            max: Duration::from_secs(8 * 3600),
            origins: default_origins(5173, 8765),
        }
    }
}

/// The console's origin (Vite, `web_port`) and the bridge's own, on 127.0.0.1 and localhost.
pub fn default_origins(web_port: u16, http_port: u16) -> Vec<String> {
    let mut v = Vec::new();
    for port in [web_port, http_port] {
        for host in ["127.0.0.1", "localhost"] {
            v.push(format!("http://{host}:{port}"));
        }
    }
    v
}

/// The operators of a credentials run's roster, and who of them holds engagement authority:
/// the credential file's, in id order.
pub fn roster_from(file: &CredentialFile) -> (Vec<OperatorId>, Vec<OperatorId>) {
    let ops = file.operators.keys().map(OperatorId::new).collect();
    let engage = file
        .operators
        .iter()
        .filter(|(_, e)| e.engage)
        .map(|(id, _)| OperatorId::new(id))
        .collect();
    (ops, engage)
}

/// Proof, inside the bridge, that the password of `operator` was just checked again for
/// login `session`: what [`Decision::approve_stepped`] takes before it asks the engine for a
/// step-up. Made only by [`Auth::step_up`] (private fields, no constructor elsewhere).
#[derive(Debug)]
pub struct StepUpChecked {
    session: SessionId,
    operator: OperatorId,
}

impl StepUpChecked {
    pub fn session(&self) -> Option<SessionId> {
        Some(self.session)
    }

    pub fn operator(&self) -> &OperatorId {
        &self.operator
    }
}

/// One login, as the session table keeps it (the token itself is never kept).
#[derive(Clone, Debug)]
struct Login {
    operator: OperatorId,
    session: SessionId,
    /// The credential's stamp at login: a change ends the session.
    stamp: String,
    engage: bool,
}

/// The credential file as last read, and the digest that tells it changed.
struct Store {
    file: CredentialFile,
    seen: Option<String>,
}

/// Why a password check refused.
enum Refused {
    Wrong,
    Locked(Duration),
}

pub struct Auth {
    pub settings: Settings,
    decision: Arc<Decision>,
    store: RwLock<Store>,
    sessions: Mutex<SessionTable<Login>>,
    limiter: Mutex<Limiter>,
    verify: tokio::sync::Semaphore,
}

/// The clock sessions and locks run on (tokio's, so tests can pause and advance it).
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The SHA-256 of the credential file as it is now (None: unreadable), to tell a change: a
/// size or a time can stay the same across a quick edit, the content can't.
fn stat(path: &PathBuf) -> Option<String> {
    std::fs::read(path).ok().map(|b| murmur_decide::hash::sha256_hex(&b))
}

fn refusal(status: StatusCode, detail: &str) -> Response {
    (status, Json(json!({ "detail": detail }))).into_response()
}

fn decision_refusal(e: DecisionError) -> Response {
    refusal(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), &e.detail)
}

fn locked(retry: Duration) -> Response {
    let secs = retry.as_secs().max(1);
    let mut r = refusal(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("too many failed attempts for this operator: try again in {secs} s"),
    );
    if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

impl Auth {
    /// The bridge's authentication: for a credentials run, `file` is the credential file as
    /// read at start (the roster came from it).
    pub fn new(settings: Settings, decision: Arc<Decision>, file: CredentialFile) -> Arc<Auth> {
        let seen = settings.file.as_ref().and_then(stat);
        let sessions = SessionTable::new(settings.idle, settings.max);
        Arc::new(Auth {
            settings,
            decision,
            store: RwLock::new(Store { file, seen }),
            sessions: Mutex::new(sessions),
            limiter: Mutex::new(Limiter::default()),
            verify: tokio::sync::Semaphore::new(VERIFY_AT_ONCE),
        })
    }

    pub fn mode(&self) -> AuthMode {
        self.settings.mode
    }

    fn credentials(&self) -> bool {
        self.settings.mode == AuthMode::Credentials
    }

    // ---- requests -------------------------------------------------------------------------

    /// The session token a request's cookie carries.
    fn token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|kv| kv.trim().split_once('='))
            .find(|(k, _)| *k == self.settings.cookie)
            .map(|(_, v)| v.trim())
    }

    /// Who an operator request acts for. Credentials run: the login the cookie names (an
    /// action, so its idle timer restarts); a dev run: the `X-Murmur-Operator` header (or,
    /// `legacy`, the first operator).
    pub fn actor(&self, headers: &HeaderMap, legacy: bool) -> Result<Actor, DecisionError> {
        let named = headers.get(OPERATOR_HEADER).and_then(|v| v.to_str().ok()).map(str::trim).filter(|s| !s.is_empty());
        if !self.credentials() {
            return self.decision.operator(named, legacy).map(Actor::asserted);
        }
        let Some(token) = self.token(headers) else {
            let why = if headers.contains_key(header::AUTHORIZATION) {
                "sign in: an agent's key is for /mcp, never for an operator's request"
            } else if named.is_some() {
                "sign in: this bridge authenticates operators with their own password; the X-Murmur-Operator header is for dev runs (MURMUR_AUTH=dev)"
            } else {
                "sign in: this bridge authenticates operators with their own password"
            };
            return Err(DecisionError::new(401, why));
        };
        let login = self.resolve(token, true)?;
        if let Some(n) = named
            && n != login.operator.as_str()
        {
            return Err(DecisionError::new(
                403,
                format!("signed in as {}, the request names {n}", login.operator),
            ));
        }
        Ok(Actor::logged_in(login.operator, login.session))
    }

    /// The live login a token names; an expired one is closed (logged) on the way.
    fn resolve(&self, token: &str, activity: bool) -> Result<Login, DecisionError> {
        let found = {
            let mut s = lock(&self.sessions);
            if activity { s.lookup(token, now()) } else { s.peek(token, now()) }
        };
        match found {
            Lookup::Valid(l) => Ok(l),
            Lookup::Expired(l, why) => {
                self.end(&l, why.as_str());
                Err(DecisionError::new(401, "your session has ended: sign in again"))
            }
            Lookup::Unknown => Err(DecisionError::new(401, "your session has ended: sign in again")),
        }
    }

    fn end(&self, l: &Login, why: &str) {
        self.decision.close_login(l.session, why);
        println!("auth: {} signed out (session {}: {why})", l.operator, l.session.0);
    }

    /// Check `password` for operator `id`: limited, at most [`VERIFY_AT_ONCE`] at a time, off
    /// the async runtime. Ok with the credential's stamp and engagement authority.
    async fn check(&self, id: &str, password: String) -> Result<(String, bool), Refused> {
        if let Err(d) = lock(&self.limiter).check(id, now()) {
            return Err(Refused::Locked(d));
        }
        let entry = self
            .store
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .file
            .operators
            .get(id)
            .cloned();
        let ok = {
            let _permit = self.verify.acquire().await;
            let e = entry.clone();
            tokio::task::spawn_blocking(move || match e {
                Some(e) => murmur_auth::password::verify(&e.password, &password),
                None => murmur_auth::password::verify_dummy(&password),
            })
            .await
            .unwrap_or(false)
        };
        match (ok, entry) {
            (true, Some(e)) => {
                lock(&self.limiter).success(id);
                Ok((e.stamp, e.engage))
            }
            _ => match lock(&self.limiter).failure(id, now()) {
                Some(d) => Err(Refused::Locked(d)),
                None => Err(Refused::Wrong),
            },
        }
    }

    /// A consequential approval's step-up on intent `id`: the session's own operator
    /// re-entered their password (`password`). `needed`: the intent's decision asks for one.
    /// Ok(None) when no password came and none is needed (and always in a dev run, which has
    /// no credential). A refusal (no password where one is needed, a wrong one, a lock) is
    /// logged under the signer's session as `operator.action step_up.refused` (never the
    /// password), so the record shows a signature attempted without the credential.
    pub async fn step_up(
        &self,
        actor: &Actor,
        id: u64,
        password: Option<String>,
        needed: bool,
    ) -> Result<Option<StepUpChecked>, Box<Response>> {
        if !self.credentials() {
            return Ok(None);
        }
        let Some(session) = actor.login() else {
            return Err(Box::new(refusal(StatusCode::UNAUTHORIZED, "sign in first")));
        };
        let refused = |why: &str| {
            if let Err(e) = self.decision.record_action(actor, "step_up.refused", &format!("i{id}: {why}")) {
                eprintln!("auth: logging a refused step-up: {}", e.detail);
            }
        };
        let Some(password) = password.filter(|p| !p.is_empty()) else {
            if needed {
                refused("no password given");
                return Err(Box::new(
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({
                            "detail": "re-enter your password to sign this: a consequential signature is made with your credential, not only your session",
                            "step_up": true,
                        })),
                    )
                        .into_response(),
                ));
            }
            return Ok(None);
        };
        match self.check(actor.operator().as_str(), password).await {
            Ok(_) => Ok(Some(StepUpChecked {
                session,
                operator: actor.operator().clone(),
            })),
            Err(Refused::Wrong) => {
                refused("wrong password");
                Err(Box::new(
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({ "detail": "wrong password: not signed", "step_up": true })),
                    )
                        .into_response(),
                ))
            }
            Err(Refused::Locked(d)) => {
                refused("too many failed attempts: the session ends");
                // repeated wrong passwords on a live session: it may not be its operator's
                let gone = lock(&self.sessions).end_where(|l| l.session == session);
                for l in &gone {
                    self.end(l, "step-up failed repeatedly");
                }
                Err(Box::new(locked(d)))
            }
        }
    }

    /// The agent an `Authorization: Bearer` key belongs to, and the key's id.
    pub fn agent_for_key(&self, key: &str) -> Option<(AgentId, String)> {
        let store = self.store.read().unwrap_or_else(|e| e.into_inner());
        store
            .file
            .agent_for_key(key)
            .map(|(id, e)| (AgentId::new(id), e.key_id.clone()))
    }

    // ---- the credential file and expiry ---------------------------------------------------

    /// Re-read the credential file if it changed, and end every session whose credential
    /// went or changed. A file that can't be read counts as empty: every session ends (fail
    /// closed).
    pub fn refresh(&self) {
        let Some(path) = &self.settings.file else { return };
        let now_seen = stat(path);
        {
            let store = self.store.read().unwrap_or_else(|e| e.into_inner());
            if store.seen == now_seen {
                return;
            }
        }
        let file = match CredentialFile::load(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("auth: {e}: no operator may act until it can be read");
                CredentialFile::default()
            }
        };
        let ended = lock(&self.sessions).end_where(|l| {
            file.operators
                .get(l.operator.as_str())
                .is_none_or(|e| e.stamp != l.stamp)
        });
        for l in &ended {
            let why = if file.operators.contains_key(l.operator.as_str()) {
                "credential changed"
            } else {
                "credential removed"
            };
            self.end(l, why);
        }
        println!(
            "auth: credential file re-read: {} operator(s), {} agent(s)",
            file.operators.len(),
            file.agents.len()
        );
        *self.store.write().unwrap_or_else(|e| e.into_inner()) = Store { file, seen: now_seen };
    }

    /// Close every session past its idle or absolute limit.
    pub fn sweep(&self) {
        let gone = lock(&self.sessions).sweep(now());
        for (l, why) in gone {
            self.end(&l, why.as_str());
        }
    }

    /// Every [`SWEEP_EVERY`]: the credential file and expiries.
    pub async fn run(self: Arc<Self>) {
        if !self.credentials() {
            return;
        }
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            self.refresh();
            self.sweep();
        }
    }

    fn cookie(&self, token: &str, max_age: Duration) -> HeaderValue {
        let secure = if self.settings.secure_cookie { "; Secure" } else { "" };
        let v = format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{secure}",
            self.settings.cookie,
            max_age.as_secs()
        );
        HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static(""))
    }
}

// ---- endpoints ----------------------------------------------------------------------------

/// `/api/auth/login`, `/api/auth/logout`, `/api/auth/session`.
pub fn router<S: Clone + Send + Sync + 'static>(auth: Arc<Auth>) -> Router<S> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/session", get(session))
        .with_state(auth)
}

#[derive(Deserialize)]
struct LoginBody {
    operator: String,
    password: String,
}

async fn login(State(auth): State<Arc<Auth>>, headers: HeaderMap, body: Bytes) -> Response {
    if !auth.credentials() {
        return refusal(
            StatusCode::CONFLICT,
            "this is a dev run (MURMUR_AUTH=dev): there is no login; operators are named by the X-Murmur-Operator header",
        );
    }
    // the body holds a password: never echo it back in an error
    let Ok(LoginBody { operator, password }) = serde_json::from_slice::<LoginBody>(&body) else {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "login wants {\"operator\": id, \"password\": ...}");
    };
    let id = operator.trim().to_string();
    let wrong = || refusal(StatusCode::UNAUTHORIZED, "unknown operator or wrong password");
    if murmur_auth::validate_id(&id).is_err() {
        return wrong();
    }
    auth.refresh();
    let (stamp, engage) = match auth.check(&id, password).await {
        Ok(v) => v,
        Err(Refused::Wrong) => {
            println!("auth: failed login for {id}");
            return wrong();
        }
        Err(Refused::Locked(d)) => {
            println!("auth: login for {id} refused: locked for {} s", d.as_secs());
            return locked(d);
        }
    };
    let op = OperatorId::new(&id);
    if auth.decision.roster_entry(&op).is_none() {
        return refusal(
            StatusCode::FORBIDDEN,
            "your credential was added after this run started: it is not on the run's roster (restart the bridge to admit it)",
        );
    }
    // a login from a browser that still holds a session ends that one
    if let Some(old) = auth.token(&headers).map(str::to_string) {
        let prev = lock(&auth.sessions).remove(&old);
        if let Some(l) = prev {
            auth.end(&l, "signed in again");
        }
    }
    let session = match auth.decision.open_login(&op, engage) {
        Ok(s) => s,
        Err(e) => return decision_refusal(e),
    };
    let engage = engage && auth.decision.roster_entry(&op) == Some(true);
    let token = match lock(&auth.sessions).create(
        Login {
            operator: op.clone(),
            session,
            stamp,
            engage,
        },
        now(),
    ) {
        Ok(t) => t,
        Err(e) => {
            auth.decision.close_login(session, "could not issue a token");
            return refusal(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    };
    println!("auth: {op} signed in (session {})", session.0);
    let mut r = Json(json!({
        "auth": "credentials",
        "signed_in": true,
        "operator": op,
        "engage": engage,
        "idle_s": auth.settings.idle.as_secs(),
        "max_s": auth.settings.max.as_secs(),
    }))
    .into_response();
    r.headers_mut().insert(header::SET_COOKIE, auth.cookie(&token, auth.settings.max));
    r
}

async fn logout(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    if let Some(token) = auth.token(&headers).map(str::to_string) {
        let gone = lock(&auth.sessions).remove(&token);
        if let Some(l) = gone {
            auth.end(&l, "logout");
        }
    }
    let mut r = Json(json!({ "signed_in": false })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, auth.cookie("", Duration::ZERO));
    r
}

/// Who is signed in on this browser (a status query, not activity). A dev run says so and
/// lists the ids a request may name.
async fn session(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    if !auth.credentials() {
        let ops = auth.decision.operators_json();
        return Json(json!({
            "auth": "dev",
            "signed_in": false,
            "operators": ops["operators"],
            "engage": ops["engage"],
            "warning": "dev run: operator ids are asserted by the X-Murmur-Operator header, not authenticated; no human-control claim",
        }))
        .into_response();
    }
    let out = auth.token(&headers).map(str::to_string).and_then(|t| {
        let l = auth.resolve(&t, false).ok()?;
        let ends = lock(&auth.sessions).ends_at(&t)?;
        Some((l, ends.saturating_duration_since(now())))
    });
    match out {
        Some((l, left)) => Json(json!({
            "auth": "credentials",
            "signed_in": true,
            "operator": l.operator,
            "engage": l.engage,
            "ends_in_s": left.as_secs(),
        }))
        .into_response(),
        None => Json(json!({ "auth": "credentials", "signed_in": false })).into_response(),
    }
}

// ---- origins ------------------------------------------------------------------------------

/// Refuse a browser's state-changing request (or WebSocket upgrade) from another origin.
pub async fn check_origin(State(origins): State<Arc<Vec<String>>>, req: Request, next: Next) -> Response {
    let changes = !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        || req.headers().contains_key(header::UPGRADE);
    if changes && let Some(why) = cross_origin(req.headers(), &origins) {
        return refusal(StatusCode::FORBIDDEN, &why);
    }
    next.run(req).await
}

/// Why a request's headers say it came from another origin, if they do.
pub fn cross_origin(headers: &HeaderMap, origins: &[String]) -> Option<String> {
    if let Some(o) = headers.get(header::ORIGIN) {
        let o = o.to_str().unwrap_or("").trim_end_matches('/');
        if origins.iter().any(|a| a == o) {
            return None;
        }
        return Some(format!("cross-origin request refused (Origin {o:?} is not this console's)"));
    }
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        Some("same-origin" | "none") | None => None,
        Some(site) => Some(format!("cross-origin request refused (Sec-Fetch-Site: {site})")),
    }
}

/// `/mcp` in a credentials run: no request without an agent's valid key.
pub async fn require_agent_key(State(auth): State<Arc<Auth>>, req: Request, next: Next) -> Response {
    if auth.credentials() && bearer(req.headers()).and_then(|k| auth.agent_for_key(k)).is_none() {
        return refusal(
            StatusCode::UNAUTHORIZED,
            "an agent connects with its own key: Authorization: Bearer <key> (murmur-auth agent-add)",
        );
    }
    next.run(req).await
}

/// The key in `Authorization: Bearer <key>`.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|k| !k.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).expect("name"),
                HeaderValue::from_str(v).expect("value"),
            );
        }
        h
    }

    #[test]
    fn browsers_from_other_origins_are_refused() {
        let ok = default_origins(5173, 8765);
        assert!(cross_origin(&headers(&[("origin", "http://127.0.0.1:5173")]), &ok).is_none());
        assert!(cross_origin(&headers(&[("origin", "http://localhost:8765")]), &ok).is_none());
        // another port of the same host is the same site, not the same origin
        assert!(cross_origin(&headers(&[("origin", "http://127.0.0.1:9999")]), &ok).is_some());
        assert!(cross_origin(&headers(&[("origin", "http://evil.example")]), &ok).is_some());
        assert!(cross_origin(&headers(&[("origin", "null")]), &ok).is_some());
        assert!(cross_origin(&headers(&[("sec-fetch-site", "cross-site")]), &ok).is_some());
        assert!(cross_origin(&headers(&[("sec-fetch-site", "same-site")]), &ok).is_some());
        assert!(cross_origin(&headers(&[("sec-fetch-site", "same-origin")]), &ok).is_none());
        // not a browser: no Origin, no Sec-Fetch-Site (curl, the harness, the contacts process)
        assert!(cross_origin(&HeaderMap::new(), &ok).is_none());
    }

    #[test]
    fn bearer_keys_and_cookies_are_read_exactly() {
        let h = headers(&[("authorization", "Bearer mma_1234abcd_xyz")]);
        assert_eq!(bearer(&h), Some("mma_1234abcd_xyz"));
        assert_eq!(bearer(&headers(&[("authorization", "Basic abc")])), None);
        assert_eq!(bearer(&headers(&[("authorization", "Bearer ")])), None);
    }
}
