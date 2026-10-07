//! Who is acting (rules 8, murmur-der.1): in a credentials run operators log in with their own
//! password, agents present their own key, and nothing else is believed; a dev run (the
//! default for `make up`) asserts ids, says so loudly, and is never evidence about people.
//!
//! **Credentials run** (`MURMUR_AUTH=credentials`; the evaluation's study mode always). The
//! credential file (`crates/murmur-auth`: `MURMUR_CREDENTIALS`, else
//! `~/.config/murmur/credentials.json`) names the run's operators and who of them holds
//! engagement authority; the roster logged in `run.started` is that file when the bridge
//! starts.
//!
//! - `POST /api/auth/login {"operator", "password"}` checks the password (argon2id, constant
//!   time; an unknown id costs the same), opens an engine session for this login
//!   (`session.opened`, `auth: password`, or `auth: scripted` for a credential marked
//!   harness-held: authenticated, never a person's) and sets an HttpOnly, SameSite=Strict
//!   cookie (`murmur_session_<http port>`, Path=/, Max-Age the session's lifetime; `Secure`
//!   with `MURMUR_COOKIE_SECURE=1`). The cookie holds a random 256-bit token; the bridge keeps
//!   only its SHA-256. Five failures for an id lock it (30 s, doubling to 15 min, 429 with
//!   Retry-After); a locked id is refused without checking the password, and an attempt is
//!   reserved before its password is checked, so concurrent attempts can't outrun the lock.
//!   Each failed password for an operator on the roster is logged (`auth.refused`: the id,
//!   why, the lock it set; never the password); attempts refused while locked, and ids not
//!   on the roster, go to stdout only (no one can flood the audit log). A lock stops the next
//!   login only: it ends no session (a stranger's wrong passwords can't sign anyone out).
//! - `POST /api/auth/logout` ends the session (`session.closed`) and clears the cookie.
//!   `GET /api/auth/session` says who is signed in (without counting as activity).
//! - A session ends after `MURMUR_SESSION_IDLE_S` (1800) without an action, or
//!   `MURMUR_SESSION_MAX_S` (28800) after login, and at once when its operator's credential is
//!   removed or changed (a new password, authority granted or revoked): the file is re-read
//!   when it changes, checked every 2 s. Each end is logged (`session.closed` with why); a
//!   replayed or expired cookie is a 401.
//! - Every operator request acts for the login its cookie names (more than one cookie of that
//!   name, as a page on another port of the host could plant, is a 401: never guess which);
//!   an `X-Murmur-Operator` header naming someone else is a 403, an agent's key is not a
//!   login (401). The console's display reports (`/api/operator/view`) only peek at the
//!   session: they don't keep it from going idle.
//! - A consequential act needs the password again in the request (`"password"` in its body):
//!   every engagement signature and an approval its tier marks `step_up`; and, when the
//!   policy's `[approval] step_up_acts` lists them, a hostile declaration and an acceptance of
//!   a lost vehicle. Checked for the session's own operator only, against a limit of the
//!   session's own (five wrong in a row lock it and end the session: only failures made under
//!   a session can end it), then logged as `operator.action step_up` and quoted by the act
//!   ([`StepUpChecked`], made only here); a refused one is logged too (`operator.action
//!   step_up.refused`: no password, a wrong one, a lock; never the password).
//! - Agents present `Authorization: Bearer <key>` on every `/mcp` request (`murmur-auth
//!   agent-add`); the key's digest is compared in constant time. A removed or rotated key stops
//!   at once.
//! - **Passkeys** (rules 10; WebAuthn through `murmur_auth::passkeys`, the relying party
//!   `MURMUR_PASSKEY_RP_ID`, `localhost`, and the console's origins on it: an IP address can't
//!   be a relying party, so the console at 127.0.0.1 sends people to localhost for these). An
//!   operator registers one while signed in (`/api/auth/passkeys/register/options`, then
//!   `/register` with the password re-entered: a session left open adds no credential), or by a
//!   one-time invitation (`murmur-auth passkey-invite`: `/api/auth/passkeys/enroll/options`,
//!   `/enroll`); logged as `passkey.registered`, kept in the credential file (public key,
//!   counter). A login by passkey: `/api/auth/login/passkey` gives the options (alike for an id
//!   without one: decoys drawn like real credentials, the same at every request; at most four
//!   live per id, a request past that served the newest again, murmur-i58),
//!   `/api/auth/login {"operator", "passkey"}` takes the assertion (`session.opened`,
//!   `auth: passkey`; a wrong one refused in the same words whoever the id is). A step-up by passkey: `/api/auth/step_up {"act", ...}` gives the options
//!   for that act, its challenge derived by the engine from the run, the session and the act
//!   (an engagement signature: the intent and the picture token); the act's request carries
//!   `"passkey"`. Each assertion answers its challenge once, within two minutes, from one of the
//!   operator's passkeys, its counter counting up (kept in the file before the act goes ahead);
//!   the engine verifies it again and logs it, and refuses it for another act. Where the policy
//!   asks people for a passkey (`step_up_factor`, a study always) a password is refused unchecked.
//!   A passkey removed from the file (`murmur-auth passkey-remove`) is logged as
//!   `passkey.removed`, and the operator's sessions end.
//!
//! **Dev run** (the default: `MURMUR_AUTH` unset or `dev`): explicit and loud. Operators are
//! whoever `X-Murmur-Operator` names (`MURMUR_OPERATORS`, `MURMUR_ENGAGE_OPERATORS`; the
//! legacy `POST /api/tasks` and track declarations without a header act as the first),
//! agents whoever `X-Murmur-Agent` or `clientInfo.name` names (`MURMUR_AGENTS`). The bridge
//! says so at start, `run.started` records `auth: dev`, and replay and the evaluation exclude
//! such a run from any claim about people. For development, scripted tests and the
//! evaluation's proxy batches; it needs no credential file.
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

use std::collections::HashMap;
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
use serde_json::{Value, json};

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
    /// Rules 10: the WebAuthn relying party id (`MURMUR_PASSKEY_RP_ID`, `localhost`: a domain,
    /// never an IP address) and the origins on it a passkey may be used from (the allowed
    /// origins whose host is the relying party id or under it).
    pub passkey_rp: String,
    pub passkey_origins: Vec<String>,
    /// Rules 10: a study run (`MURMUR_STUDY=<participant>`; `MURMUR_STUDY_PASSWORD_STEP_UP=<why>`
    /// is the explicit override that lets the participant step up with a password).
    pub study: Option<murmur_decide::Study>,
}

/// The origins of `origins` on host `rp` (or a subdomain of it): where a passkey for `rp` may
/// be used. An origin on 127.0.0.1 is never one (WebAuthn takes no IP address as a relying
/// party).
pub fn passkey_origins(rp: &str, origins: &[String]) -> Vec<String> {
    origins
        .iter()
        .filter(|o| {
            let host = o
                .split_once("://")
                .map_or(o.as_str(), |(_, rest)| rest)
                .split(['/', ':'])
                .next()
                .unwrap_or("");
            host == rp || host.ends_with(&format!(".{rp}"))
        })
        .cloned()
        .collect()
}

impl Settings {
    pub fn from_env(http_port: u16) -> Result<Settings, String> {
        let var = |k: &str| std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        let mode = mode_from(var("MURMUR_AUTH").as_deref())?;
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
        let origins: Vec<String> = match var("MURMUR_ALLOWED_ORIGINS") {
            Some(list) => list.split(',').map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty()).collect(),
            None => default_origins(web, http_port),
        };
        let passkey_rp = var("MURMUR_PASSKEY_RP_ID").unwrap_or_else(|| "localhost".into());
        if passkey_rp.parse::<std::net::IpAddr>().is_ok() {
            return Err(format!("MURMUR_PASSKEY_RP_ID={passkey_rp}: a domain (WebAuthn takes no IP address as a relying party)"));
        }
        let study = match var("MURMUR_STUDY") {
            None => None,
            Some(p) => {
                if mode != AuthMode::Credentials {
                    return Err("MURMUR_STUDY: a study run authenticates with credentials (MURMUR_AUTH=credentials)".into());
                }
                murmur_auth::validate_id(&p).map_err(|e| format!("MURMUR_STUDY: {e}"))?;
                Some(murmur_decide::Study {
                    participant: OperatorId::new(p),
                    password_step_up: var("MURMUR_STUDY_PASSWORD_STEP_UP"),
                })
            }
        };
        Ok(Settings {
            passkey_origins: passkey_origins(&passkey_rp, &origins),
            passkey_rp,
            study,
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
            passkey_rp: "localhost".into(),
            passkey_origins: passkey_origins("localhost", &default_origins(5173, 8765)),
            study: None,
        }
    }
}

/// `MURMUR_AUTH`: unset or `dev`, a dev run (ids asserted, no credential file); `credentials`,
/// a credentials run.
pub fn mode_from(v: Option<&str>) -> Result<AuthMode, String> {
    match v {
        None | Some("dev") => Ok(AuthMode::Dev),
        Some("credentials") => Ok(AuthMode::Credentials),
        Some(other) => Err(format!("MURMUR_AUTH={other:?}: dev (the default) or credentials")),
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

/// Rules 12 (murmur-jfi): the operators whose credential is harness-held (`scripted`) in
/// `file` when the run starts: fixed on the run's roster, so a credential re-marked mid-run
/// (`murmur-auth scripted <op> on|off`) can't move its operator between people and scripts in
/// the log; its next login is refused until the bridge restarts.
pub fn scripted_from(file: &CredentialFile) -> Vec<OperatorId> {
    file.operators
        .iter()
        .filter(|(_, e)| e.scripted)
        .map(|(id, _)| OperatorId::new(id))
        .collect()
}

/// Proof, inside the bridge, that the password of `operator` was just checked again for
/// login `session`: what [`Decision::approve_stepped`] takes before it asks the engine for a
/// step-up. Made only by [`Auth::step_up`] (private fields, no constructor elsewhere).
#[derive(Debug)]
pub struct StepUpChecked {
    session: SessionId,
    operator: OperatorId,
    factor: StepFactor,
}

/// How a step-up was made (rules 10): the password re-entered, or a passkey's assertion this
/// module checked (with what the log records of it).
#[derive(Debug)]
pub enum StepFactor {
    Password,
    Passkey(Box<murmur_decide::PasskeyUse>),
}

impl StepUpChecked {
    pub fn session(&self) -> Option<SessionId> {
        Some(self.session)
    }

    pub fn operator(&self) -> &OperatorId {
        &self.operator
    }

    /// The factor, taken (the engine logs a passkey's assertion with the step-up).
    pub fn into_factor(self) -> StepFactor {
        self.factor
    }
}

/// What a request brings for a step-up: the password again, a passkey's assertion (the
/// browser's `PublicKeyCredential` JSON), or neither.
#[derive(Debug, Default)]
pub struct Proof {
    pub password: Option<String>,
    pub passkey: Option<Value>,
}

impl Proof {
    pub fn is_some(&self) -> bool {
        self.password.as_deref().is_some_and(|p| !p.is_empty()) || self.passkey.is_some()
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

/// Rules 10: a passkey challenge issued and not yet answered (by its challenge, base64url):
/// who it was for, in which session (none: a login), what the engine derived it from, and the
/// ceremony's state. Answered once; gone after [`murmur_decide::CHALLENGE_TTL_MS`].
struct PendingUse {
    operator: OperatorId,
    session: Option<SessionId>,
    issued: murmur_decide::passkey::Issued,
    pending: murmur_auth::passkeys::Pending,
    until: Instant,
}

/// Rules 10: a passkey registration in progress: through a session (the password is
/// re-entered with its finish) or an invitation (its code's digest).
struct PendingReg {
    operator: OperatorId,
    reg: murmur_auth::passkeys::Registration,
    until: Instant,
}

/// Rules 10, murmur-i58: a passkey login waiting for its answer, by operator id claimed: an
/// operator's challenge (its options, served again past the bound, and the ceremony), or a
/// decoy's (its options are drawn again from the id and the challenge).
enum LoginWait {
    Real { options: Value, pending: Box<PendingUse> },
    Decoy,
}

/// How long a registration may take (finding the authenticator, a PIN, a touch).
const REGISTRATION_TTL: Duration = Duration::from_secs(300);
/// At most this many registrations wait at once (a flood can't grow memory).
const PENDING_CAP: usize = 256;
/// Step-ups waiting per session (the oldest gives way): bounded by the sessions, so no
/// stranger can fill a shared cap.
const PENDING_PER_SESSION: usize = 8;
/// Passkey logins live per id claimed (murmur-i58, [`murmur_auth::Ceremonies`]): a request
/// past them is served the newest again; and the ids kept (a flood of made-up ones pushes out
/// the id issued to longest ago, whose next request is a fresh one).
const LOGINS_PER_ID: usize = 4;
pub(crate) const LOGIN_IDS_CAP: usize = 10_000;

/// What a refused passkey login is told, whoever the id is (murmur-i58).
pub(crate) const LOGIN_PASSKEY_REFUSED: &str =
    "passkey refused: not an answer from one of this operator's passkeys to a challenge waiting for them (ask again)";

/// A passkey challenge's lifetime, as the engine checks it.
fn challenge_ttl() -> Duration {
    Duration::from_millis(u64::try_from(murmur_decide::CHALLENGE_TTL_MS).unwrap_or(120_000))
}

/// The credential file as last read, and the digest that tells it changed.
struct Store {
    file: CredentialFile,
    seen: Option<String>,
}

/// Why a password check refused.
enum Refused {
    /// The password was checked and is wrong.
    Wrong,
    /// The password was checked, is wrong, and this failure locked the key.
    WrongLocked(Duration),
    /// Not checked: the key is locked, or as many attempts as the limit are being checked.
    Locked(Duration),
}

pub struct Auth {
    pub settings: Settings,
    decision: Arc<Decision>,
    store: RwLock<Store>,
    sessions: Mutex<SessionTable<Login>>,
    /// Failed logins, per operator id claimed.
    limiter: Arc<Mutex<Limiter>>,
    /// Failed step-ups, per login session: apart from the login limit, so failures made
    /// elsewhere (a stranger guessing at the login form) never end a live session.
    step_limiter: Arc<Mutex<Limiter>>,
    verify: Arc<tokio::sync::Semaphore>,
    /// Rules 10: the WebAuthn relying party (a credentials run with a console origin on the
    /// relying party's host), its ceremonies in progress (step-ups by challenge, logins by id
    /// claimed), and the secret decoy options for an id without a passkey derive from (kept
    /// beside the credential file, so the decoys stay the same across restarts as real
    /// operators' credentials do: [`decoy_secret`]).
    rp: Option<murmur_auth::RelyingParty>,
    pending: Mutex<HashMap<String, PendingUse>>,
    logins: Mutex<murmur_auth::Ceremonies<LoginWait>>,
    registering: Mutex<HashMap<String, PendingReg>>,
    decoy: [u8; 32],
    /// One passkey check-and-store at a time (a counter is compared and written as one step).
    passkey_io: tokio::sync::Mutex<()>,
}

/// An attempt reserved on a limiter ([`Limiter::begin`]) whose password check has not
/// started: given back if the request goes away first (a client that hangs up must not
/// leave the id busy).
struct Reserved {
    limiter: Arc<Mutex<Limiter>>,
    key: String,
    held: bool,
}

impl Drop for Reserved {
    fn drop(&mut self) {
        if self.held {
            lock(&self.limiter).release(&self.key);
        }
    }
}

/// The clock sessions and locks run on (tokio's, so tests can pause and advance it).
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// Unix seconds (a passkey's last use, an invitation's expiry).
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The SHA-256 of the credential file as it is now (None: unreadable), to tell a change: a
/// size or a time can stay the same across a quick edit, the content can't.
fn stat(path: &PathBuf) -> Option<String> {
    std::fs::read(path).ok().map(|b| murmur_decide::hash::sha256_hex(&b))
}

/// The secret passkey decoys are drawn under (murmur-i58): kept beside the credential file
/// (`<file>.decoy-key`, made once, mode 0600), so an id without a passkey is answered the same
/// across restarts, as an operator's real credentials are. Not a credential (it only makes the
/// decoys unpredictable). Without a file to keep it beside, or if it can't be read or made, a
/// random one for this run (said: the decoys then change at a restart, which tells an observer
/// comparing before and after which ids are decoys).
fn decoy_secret(file: Option<&std::path::Path>) -> [u8; 32] {
    let kept = file.map(|f| {
        let mut name = f.file_name().unwrap_or_default().to_os_string();
        name.push(".decoy-key");
        let p = f.with_file_name(name);
        murmur_auth::keys::secret_file(&p).map_err(|e| format!("{}: {e}", p.display()))
    });
    match kept {
        Some(Ok(s)) => s,
        other => {
            let why = match other {
                Some(Err(e)) => e,
                _ => "no credential file to keep it beside".into(),
            };
            eprintln!("auth: passkey decoys under a secret for this run only ({why}): they change at a restart");
            let mut s = [0u8; 32];
            // (without the OS's random source no session token can be issued either)
            if let Err(e) = murmur_auth::keys::fill(&mut s) {
                eprintln!("auth: {e}");
            }
            s
        }
    }
}

fn refusal(status: StatusCode, detail: &str) -> Response {
    (status, Json(json!({ "detail": detail }))).into_response()
}

fn decision_refusal(e: DecisionError) -> Response {
    refusal(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), &e.detail)
}

fn locked(retry: Duration) -> Response {
    locked_for(retry, "this operator")
}

/// A step-up's limit is the session's own, not the operator's (murmur-jfi review): its 429
/// says so.
fn locked_session(retry: Duration) -> Response {
    locked_for(retry, "this session")
}

fn locked_for(retry: Duration, whose: &str) -> Response {
    let secs = retry.as_secs().max(1);
    let mut r = refusal(
        StatusCode::TOO_MANY_REQUESTS,
        &format!("too many failed attempts for {whose}: try again in {secs} s"),
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
        // rules 10: passkeys when the run logged a relying party (a credentials run of rules
        // 10 with a console origin on the relying party's host)
        let rp = decision
            .passkey_roster()
            .and_then(|p| match murmur_auth::RelyingParty::new(&p.rp_id, &p.origins) {
                Ok(rp) => Some(rp),
                Err(e) => {
                    eprintln!("auth: passkeys off: {e}");
                    None
                }
            });
        let decoy = if rp.is_some() { decoy_secret(settings.file.as_deref()) } else { [0u8; 32] };
        Arc::new(Auth {
            settings,
            decision,
            store: RwLock::new(Store { file, seen }),
            sessions: Mutex::new(sessions),
            limiter: Arc::new(Mutex::new(Limiter::default())),
            step_limiter: Arc::new(Mutex::new(Limiter::default())),
            verify: Arc::new(tokio::sync::Semaphore::new(VERIFY_AT_ONCE)),
            rp,
            pending: Mutex::new(HashMap::new()),
            logins: Mutex::new(murmur_auth::Ceremonies::new(LOGINS_PER_ID, challenge_ttl(), LOGIN_IDS_CAP)),
            registering: Mutex::new(HashMap::new()),
            decoy,
            passkey_io: tokio::sync::Mutex::new(()),
        })
    }

    pub fn mode(&self) -> AuthMode {
        self.settings.mode
    }

    fn credentials(&self) -> bool {
        self.settings.mode == AuthMode::Credentials
    }

    // ---- requests -------------------------------------------------------------------------

    /// Every value the request's cookies give the session cookie's name.
    fn tokens<'a>(&self, headers: &'a HeaderMap) -> Vec<&'a str> {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|kv| kv.trim().split_once('='))
            .filter(|(k, _)| *k == self.settings.cookie)
            .map(|(_, v)| v.trim())
            .collect()
    }

    /// The session token a request's cookie carries: only when there is exactly one (a
    /// second cookie of the name, as another origin on this host can plant with another
    /// path, would make the request act for whoever planted it).
    fn token<'a>(&self, headers: &'a HeaderMap) -> Option<&'a str> {
        match self.tokens(headers).as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// Who an operator request acts for. Credentials run: the login the cookie names (an
    /// action, so its idle timer restarts); a dev run: the `X-Murmur-Operator` header (or,
    /// `legacy`, the first operator).
    pub fn actor(&self, headers: &HeaderMap, legacy: bool) -> Result<Actor, DecisionError> {
        self.actor_as(headers, legacy, true)
    }

    /// [`Auth::actor`] for a report that is not an operator's action (the console's display
    /// events): the login is looked up without restarting its idle timer.
    pub fn actor_peek(&self, headers: &HeaderMap) -> Result<Actor, DecisionError> {
        self.actor_as(headers, false, false)
    }

    fn actor_as(&self, headers: &HeaderMap, legacy: bool, activity: bool) -> Result<Actor, DecisionError> {
        let named = headers.get(OPERATOR_HEADER).and_then(|v| v.to_str().ok()).map(str::trim).filter(|s| !s.is_empty());
        if !self.credentials() {
            return self.decision.operator(named, legacy).map(Actor::asserted);
        }
        if self.tokens(headers).len() > 1 {
            return Err(DecisionError::new(
                401,
                "more than one session cookie came with this request (one may have been planted by another page on this host): sign out and sign in again",
            ));
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
        let login = self.resolve(token, activity)?;
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

    /// Check `password` for operator `id`, counted against `limiter` under `key` (the id for a
    /// login, the session for a step-up): the attempt is reserved before the check (refused
    /// unchecked while locked, or while as many as the limit are being checked), at most
    /// [`VERIFY_AT_ONCE`] run at a time, off the async runtime. Once a check starts, its
    /// verdict is counted whether or not the request is still there to hear it. Ok with the
    /// credential.
    async fn check(
        &self,
        limiter: &Arc<Mutex<Limiter>>,
        key: &str,
        id: &str,
        password: String,
    ) -> Result<murmur_auth::OperatorEntry, Refused> {
        let t = now();
        if let Err(d) = lock(limiter).begin(key, t) {
            return Err(Refused::Locked(d));
        }
        let mut reserved = Reserved {
            limiter: limiter.clone(),
            key: key.to_string(),
            held: true,
        };
        let entry = self
            .store
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .file
            .operators
            .get(id)
            .cloned();
        let Ok(permit) = self.verify.clone().acquire_owned().await else {
            return Err(Refused::Locked(Duration::from_secs(1)));
        };
        // from here the check's own task records its verdict
        reserved.held = false;
        let (l, k, e) = (limiter.clone(), key.to_string(), entry.clone());
        let verdict = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let ok = match &e {
                Some(e) => murmur_auth::password::verify(&e.password, &password),
                None => murmur_auth::password::verify_dummy(&password),
            };
            let mut l = lock(&l);
            if ok && e.is_some() {
                l.success(&k);
                Ok(())
            } else {
                Err(l.failure(&k, t))
            }
        })
        .await
        .unwrap_or_else(|_| Err(lock(limiter).failure(key, t)));
        match (verdict, entry) {
            (Ok(()), Some(e)) => Ok(e),
            (Err(Some(d)), _) => Err(Refused::WrongLocked(d)),
            _ => Err(Refused::Wrong),
        }
    }

    /// A consequential act's step-up (`what`: `i<id>` for a signature, or the act): the
    /// session's own operator re-entered their password (`password`). `needed`: the decision
    /// or the policy asks for one. Ok(None) when no password came and none is needed (and
    /// always in a dev run, which has no credential). Counted against the session's own limit
    /// (five wrong lock it and end the session; failures elsewhere never do). A refusal (no
    /// password where one is needed, a wrong one, a lock) is logged under the session as
    /// `operator.action step_up.refused` (never the password), so the record shows the act
    /// attempted without the credential.
    pub async fn step_up(
        &self,
        actor: &Actor,
        what: &str,
        proof: Proof,
        needed: bool,
    ) -> Result<Option<StepUpChecked>, Box<Response>> {
        if !self.credentials() {
            return Ok(None);
        }
        let Some(session) = actor.login() else {
            return Err(Box::new(refusal(StatusCode::UNAUTHORIZED, "sign in first")));
        };
        let refused = |why: &str| {
            if let Err(e) = self.decision.record_action(actor, "step_up.refused", &format!("{what}: {why}")) {
                eprintln!("auth: logging a refused step-up: {}", e.detail);
            }
        };
        // rules 10: what this person's step-up must be (the policy's step_up_factor)
        let factor = self.decision.step_up_factor_for(actor);
        let ask = |detail: &str| {
            Box::new(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "detail": detail, "step_up": true, "factor": factor.as_str() })),
                )
                    .into_response(),
            )
        };
        if let Some(assertion) = proof.passkey {
            return self.passkey_step_up(actor, session, what, &assertion).await.map(Some);
        }
        let Some(password) = proof.password.filter(|p| !p.is_empty()) else {
            if needed {
                refused("no password given");
                return Err(ask(match factor {
                    murmur_decide::StepUpFactor::Passkey => {
                        "use your passkey for this: a consequential act is made with your passkey, not only your session"
                    }
                    murmur_decide::StepUpFactor::Password => {
                        "re-enter your password for this: a consequential act is made with your credential, not only your session"
                    }
                }));
            }
            return Ok(None);
        };
        if factor == murmur_decide::StepUpFactor::Passkey {
            // the password is not checked (nor counted): this run asks this person for a passkey
            refused("a password where the policy asks for a passkey");
            return Err(ask("this run asks for your passkey here, not your password ([approval] step_up_factor = \"passkey\")"));
        }
        let key = format!("session {}", session.0);
        match self.check(&self.step_limiter, &key, actor.operator().as_str(), password).await {
            Ok(_) => Ok(Some(StepUpChecked {
                session,
                operator: actor.operator().clone(),
                factor: StepFactor::Password,
            })),
            Err(Refused::Wrong) => {
                refused("wrong password");
                Err(Box::new(
                    (
                        StatusCode::UNAUTHORIZED,
                        Json(json!({ "detail": "wrong password: not done", "step_up": true })),
                    )
                        .into_response(),
                ))
            }
            Err(Refused::WrongLocked(d)) => {
                refused("too many failed attempts: the session ends");
                // repeated wrong passwords on this live session: it may not be its operator's
                let gone = lock(&self.sessions).end_where(|l| l.session == session);
                for l in &gone {
                    self.end(l, "step-up failed repeatedly");
                }
                Err(Box::new(locked_session(d)))
            }
            Err(Refused::Locked(d)) => {
                refused("too many attempts at once");
                Err(Box::new(locked_session(d)))
            }
        }
    }

    // ---- passkeys (rules 10) ------------------------------------------------------------------

    /// `op`'s passkeys this run can use: in the credential file and known to the engine (on the
    /// run's roster, or registered while it is up), never a scripted credential's.
    fn usable_passkeys(&self, op: &OperatorId) -> Vec<murmur_auth::PasskeyEntry> {
        let known: std::collections::BTreeSet<String> = self
            .decision
            .passkey_roster()
            .map(|p| p.keys.into_iter().filter(|k| &k.operator == op).map(|k| k.id).collect())
            .unwrap_or_default();
        let store = self.store.read().unwrap_or_else(|e| e.into_inner());
        store
            .file
            .operators
            .get(op.as_str())
            .filter(|e| !e.scripted)
            .map(|e| e.passkeys.iter().filter(|p| known.contains(&p.id)).cloned().collect())
            .unwrap_or_default()
    }

    /// Drop ceremonies past their time.
    fn sweep_pending(&self) {
        let now = now();
        lock(&self.pending).retain(|_, p| p.until > now);
        lock(&self.registering).retain(|_, p| p.until > now);
    }

    /// A fresh random nonce (16 bytes, base64url).
    fn nonce() -> Result<String, DecisionError> {
        let mut b = [0u8; 16];
        murmur_auth::keys::fill(&mut b).map_err(|e| DecisionError::new(500, e.to_string()))?;
        Ok(murmur_decide::passkey::b64(&b))
    }

    /// The options for `navigator.credentials.get`: a challenge the engine derives for
    /// `purpose` by `op` (in `session`; none for a login), over `op`'s passkeys, kept until it
    /// is answered or [`murmur_decide::CHALLENGE_TTL_MS`] passes. A login's go through
    /// [`Self::issue_login`].
    fn issue(&self, op: &OperatorId, session: Option<SessionId>, purpose: murmur_decide::StepUpFor) -> Result<Value, DecisionError> {
        let Some(rp) = &self.rp else {
            return Err(DecisionError::new(409, "this run takes no passkey (a credentials run with a console on localhost does)"));
        };
        let Some(session) = session else {
            return self.issue_login(rp, op);
        };
        self.sweep_pending();
        let entries = self.usable_passkeys(op);
        if entries.is_empty() || self.decision.roster_entry(op).is_none() {
            return Err(DecisionError::new(
                409,
                "you have no passkey this run can use: register one from your operator menu (one registered elsewhere since this run started is usable after a restart)",
            ));
        }
        let issued = self.decision.passkey_challenge(op, Some(session), purpose, Self::nonce()?)?;
        let (options, pending) = rp
            .start_assertion(&entries, issued.challenge)
            .map_err(|e| DecisionError::new(500, e.to_string()))?;
        let mut waiting = lock(&self.pending);
        // bounded without a global cap anyone could fill: a few step-ups per session (a
        // session is a login's); the oldest gives way
        let mut mine: Vec<(String, Instant)> = waiting
            .iter()
            .filter(|(_, p)| p.session == Some(session))
            .map(|(k, p)| (k.clone(), p.until))
            .collect();
        mine.sort_by_key(|m| m.1);
        for (k, _) in mine.iter().take((mine.len() + 1).saturating_sub(PENDING_PER_SESSION)) {
            waiting.remove(k);
        }
        waiting.insert(
            murmur_decide::passkey::b64(&issued.challenge),
            PendingUse {
                operator: op.clone(),
                session: Some(session),
                issued,
                pending,
                until: now() + challenge_ttl(),
            },
        );
        Ok(options)
    }

    /// A login's options (murmur-i58), asked for before anyone is signed in: an operator on
    /// the roster with a usable passkey gets a challenge over their passkeys, any other id
    /// (no passkey, no such operator) options alike naming decoys
    /// ([`murmur_auth::RelyingParty::decoy_options`]: drawn like real credentials, the same at
    /// every request, keyed by the bridge's secret). Both through one table
    /// ([`murmur_auth::Ceremonies`]): at most [`LOGINS_PER_ID`] live per id, a request past that
    /// served the newest again, never refused and nothing pushed out, so a stranger's flood
    /// can't turn an operator away or take the bridge's decision lock more than that.
    fn issue_login(&self, rp: &murmur_auth::RelyingParty, op: &OperatorId) -> Result<Value, DecisionError> {
        let t = now();
        let mut logins = lock(&self.logins);
        if let Some(l) = logins.full(op.as_str(), t) {
            return Ok(match &l.item {
                LoginWait::Real { options, .. } => options.clone(),
                LoginWait::Decoy => rp.decoy_options(op.as_str(), &self.decoy, l.challenge),
            });
        }
        let entries = self.usable_passkeys(op);
        let real = !entries.is_empty() && self.decision.roster_entry(op).is_some();
        let (challenge, options, item) = if real {
            let issued = self.decision.passkey_challenge(op, None, murmur_decide::StepUpFor::Login, Self::nonce()?)?;
            let (options, pending) = rp
                .start_assertion(&entries, issued.challenge)
                .map_err(|e| DecisionError::new(500, e.to_string()))?;
            let challenge = issued.challenge;
            let wait = PendingUse {
                operator: op.clone(),
                session: None,
                issued,
                pending,
                until: t + challenge_ttl(),
            };
            (challenge, options.clone(), LoginWait::Real { options, pending: Box::new(wait) })
        } else {
            let mut c = [0u8; 32];
            murmur_auth::keys::fill(&mut c).map_err(|e| DecisionError::new(500, e.to_string()))?;
            (c, rp.decoy_options(op.as_str(), &self.decoy, c), LoginWait::Decoy)
        };
        logins.add(op.as_str(), t, challenge, item);
        Ok(options)
    }

    /// The challenge an assertion's client data carries (base64url), if it reads as one.
    fn challenge_of(assertion: &Value) -> Option<String> {
        let cdj = assertion.get("response")?.get("clientDataJSON")?.as_str()?;
        let bytes = murmur_decide::passkey::unb64(cdj).ok()?;
        let v: Value = serde_json::from_slice(&bytes).ok()?;
        Some(v.get("challenge")?.as_str()?.trim_end_matches('=').to_string())
    }

    /// Tests: the ids the login table keeps (a flood needs `LOGIN_IDS_CAP` otherwise).
    #[cfg(test)]
    pub(crate) fn set_login_cap(&self, cap: usize) {
        lock(&self.logins).cap = cap;
    }

    /// The login ceremony of `op` that `assertion` answers, taken (answered once, right or
    /// wrong): Some for an operator's challenge, None for a decoy's (no authenticator holds
    /// one of its credentials). Err when `op` has no such challenge waiting.
    fn take_login(&self, assertion: &Value, op: &OperatorId) -> Result<Option<PendingUse>, String> {
        let waiting = "no such challenge is waiting (answered already, expired, or another run's): ask again";
        let c = Self::challenge_of(assertion).ok_or("not a passkey assertion")?;
        let c: [u8; 32] = murmur_decide::passkey::unb64(&c)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(waiting)?;
        match lock(&self.logins).take(op.as_str(), &c, now()) {
            Some(LoginWait::Real { pending, .. }) => Ok(Some(*pending)),
            Some(LoginWait::Decoy) => Ok(None),
            None => Err(waiting.into()),
        }
    }

    /// The step-up ceremony `assertion` answers, taken (it is answered once): it must have been
    /// issued to `op` in `session`.
    fn take_pending(&self, assertion: &Value, op: &OperatorId, session: Option<SessionId>) -> Result<PendingUse, String> {
        self.sweep_pending();
        let c = Self::challenge_of(assertion).ok_or("not a passkey assertion")?;
        let p = lock(&self.pending)
            .remove(&c)
            .ok_or("no such challenge is waiting (answered already, expired, or another run's): ask again")?;
        if &p.operator != op || p.session != session {
            return Err("the challenge was issued to another operator or session".into());
        }
        Ok(p)
    }

    /// Check an assertion against its ceremony and the operator's passkeys as they are now, and
    /// keep its counter in the credential file before anything goes ahead: what the audit log
    /// records of it. Err: (whether the counter showed a clone, why).
    async fn check_passkey(&self, p: PendingUse, assertion: &Value) -> Result<murmur_decide::PasskeyUse, (bool, String)> {
        let _io = self.passkey_io.lock().await;
        let Some(rp) = &self.rp else {
            return Err((false, "this run takes no passkey".into()));
        };
        let entries = self.usable_passkeys(&p.operator);
        let (checked, mut updated) = match rp.finish_assertion(&p.pending, &entries, assertion) {
            Ok(r) => r,
            Err(murmur_auth::passkeys::Refused::Counter) => {
                return Err((true, murmur_auth::passkeys::Refused::Counter.to_string()));
            }
            Err(e) => return Err((false, e.to_string())),
        };
        updated.last_used_unix = Some(now_unix());
        let op = p.operator.as_str().to_string();
        self.modify_file(move |f| f.update_passkey(&op, &updated))
            .await
            .map_err(|e| (false, format!("could not keep the passkey's counter: {e}")))?;
        Ok(murmur_decide::PasskeyUse {
            credential: checked.credential,
            counter: checked.counter,
            purpose: p.issued.purpose,
            nonce: p.issued.nonce,
            issued_t: p.issued.issued_t,
            assertion: murmur_decide::Assertion {
                authenticator_data: checked.authenticator_data,
                client_data_json: checked.client_data_json,
                signature: checked.signature,
            },
        })
    }

    /// A step-up by passkey: the assertion answers a challenge issued for this session (the
    /// engine compares what it was made for with the act); counted against the session's own
    /// limit, a refusal logged under the session as a wrong password is.
    async fn passkey_step_up(&self, actor: &Actor, session: SessionId, what: &str, assertion: &Value) -> Result<StepUpChecked, Box<Response>> {
        let refused = |why: &str| {
            if let Err(e) = self.decision.record_action(actor, "step_up.refused", &format!("{what}: passkey refused: {why}")) {
                eprintln!("auth: logging a refused step-up: {}", e.detail);
            }
        };
        let deny = |detail: String| {
            Box::new(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "detail": detail, "step_up": true, "factor": "passkey" })),
                )
                    .into_response(),
            )
        };
        let key = format!("session {}", session.0);
        let t = now();
        if let Err(d) = lock(&self.step_limiter).begin(&key, t) {
            refused("too many attempts at once");
            return Err(Box::new(locked_session(d)));
        }
        let outcome = match self.take_pending(assertion, actor.operator(), Some(session)) {
            Ok(p) => self.check_passkey(p, assertion).await.map_err(|(_, why)| why),
            Err(why) => Err(why),
        };
        match outcome {
            Ok(proof) => {
                lock(&self.step_limiter).success(&key);
                Ok(StepUpChecked {
                    session,
                    operator: actor.operator().clone(),
                    factor: StepFactor::Passkey(Box::new(proof)),
                })
            }
            Err(why) => {
                let lock_now = lock(&self.step_limiter).failure(&key, t);
                refused(&why);
                if let Some(d) = lock_now {
                    let gone = lock(&self.sessions).end_where(|l| l.session == session);
                    for l in &gone {
                        self.end(l, "step-up failed repeatedly");
                    }
                    return Err(Box::new(locked_session(d)));
                }
                Err(deny(format!("passkey refused: {why}")))
            }
        }
    }

    /// Change the credential file (under its lock, off the runtime) and read it again.
    async fn modify_file<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut CredentialFile) -> Result<T, murmur_auth::AuthError> + Send + 'static,
    ) -> Result<T, String> {
        let path = self.settings.file.clone().ok_or("no credential file")?;
        let out = tokio::task::spawn_blocking(move || -> Result<T, murmur_auth::AuthError> {
            let _lock = murmur_auth::FileLock::acquire(&path)?;
            let mut f = CredentialFile::load(&path)?;
            let out = change(&mut f)?;
            f.save(&path)?;
            Ok(out)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
        self.refresh();
        Ok(out)
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
        // rules 10: passkeys gone from the file (`murmur-auth passkey-remove`, an operator
        // removed): nothing may use them from here on, on the record
        {
            let old = self.store.read().unwrap_or_else(|e| e.into_inner());
            for (id, e) in &old.file.operators {
                for p in &e.passkeys {
                    let kept = file.operators.get(id).is_some_and(|n| n.passkeys.iter().any(|q| q.id == p.id));
                    if !kept {
                        self.decision.record_passkey_removed(
                            &OperatorId::new(id),
                            &p.id,
                            "removed from the credential file",
                        );
                    }
                }
            }
        }
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

/// `/api/auth/login`, `/api/auth/logout`, `/api/auth/session`; rules 10, the passkey
/// ceremonies: `/api/auth/login/passkey` (a login's options), `/api/auth/step_up` (a step-up's
/// options for an act), `/api/auth/passkeys` (the signed-in operator's), and registration
/// (`/api/auth/passkeys/register/options`, `/register`; by invitation `/enroll/options`,
/// `/enroll`).
pub fn router<S: Clone + Send + Sync + 'static>(auth: Arc<Auth>) -> Router<S> {
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/login/passkey", post(login_passkey_options))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/session", get(session))
        .route("/api/auth/step_up", post(step_up_options))
        .route("/api/auth/passkeys", get(my_passkeys))
        .route("/api/auth/passkeys/register/options", post(register_options))
        .route("/api/auth/passkeys/register", post(register_finish))
        .route("/api/auth/passkeys/enroll/options", post(enroll_options))
        .route("/api/auth/passkeys/enroll", post(enroll_finish))
        .with_state(auth)
}

#[derive(Deserialize)]
struct LoginBody {
    operator: String,
    #[serde(default)]
    password: Option<String>,
    /// Rules 10: a passkey's assertion (`PublicKeyCredential` JSON) on the options
    /// `/api/auth/login/passkey` gave.
    #[serde(default)]
    passkey: Option<Value>,
}

async fn login(State(auth): State<Arc<Auth>>, headers: HeaderMap, body: Bytes) -> Response {
    if !auth.credentials() {
        return refusal(
            StatusCode::CONFLICT,
            "this is a dev run (MURMUR_AUTH=dev): there is no login; operators are named by the X-Murmur-Operator header",
        );
    }
    // the body holds a password: never echo it back in an error
    let parsed = serde_json::from_slice::<LoginBody>(&body).ok();
    let Some(LoginBody { operator, password, passkey }) = parsed.filter(|b| b.password.is_some() != b.passkey.is_some()) else {
        return refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "login wants {\"operator\": id, \"password\": ...} or {\"operator\": id, \"passkey\": <assertion>}",
        );
    };
    let id = operator.trim().to_string();
    let wrong = || refusal(StatusCode::UNAUTHORIZED, "unknown operator or wrong password");
    if murmur_auth::validate_id(&id).is_err() {
        return wrong();
    }
    auth.refresh();
    let op = OperatorId::new(&id);
    // a failed password for an operator the run lists goes to the audit log (who, why, the
    // lock; never the password); other ids, and attempts refused unchecked, to stdout only
    let listed = auth.decision.roster_entry(&op).is_some();
    if let Some(assertion) = passkey {
        return login_by_passkey(&auth, &headers, op, listed, &assertion).await;
    }
    let password = password.unwrap_or_default();
    let entry = match auth.check(&auth.limiter, &id, &id, password).await {
        Ok(e) => e,
        Err(Refused::Wrong) => {
            println!("auth: failed login for {id}");
            if listed {
                auth.decision.record_login_refused(&op, "wrong password", None);
            }
            return wrong();
        }
        Err(Refused::WrongLocked(d)) => {
            println!("auth: failed login for {id}: locked for {} s", d.as_secs());
            if listed {
                auth.decision.record_login_refused(&op, "wrong password", Some(d.as_secs()));
            }
            return locked(d);
        }
        Err(Refused::Locked(d)) => {
            println!("auth: login for {id} refused unchecked: locked or busy for {} s", d.as_secs().max(1));
            return locked(d);
        }
    };
    let scripted = entry.scripted;
    signed_in(&auth, &headers, op.clone(), entry, |engage| auth.decision.open_login(&op, engage, scripted))
}

/// Rules 10: a login by passkey: the assertion answers the challenge
/// `/api/auth/login/passkey` issued for this operator; counted against the operator's login
/// limit as a password is, a refusal for an operator on the roster logged as `auth.refused`.
/// An answer to no live challenge of this id (never issued, answered, lapsed, or pushed out of
/// the table by a flood) can't succeed, so it is no guess: refused in the same words, not
/// counted and not logged (a stranger's flood must not push an operator's own answers toward
/// the lock).
async fn login_by_passkey(auth: &Arc<Auth>, headers: &HeaderMap, op: OperatorId, listed: bool, assertion: &Value) -> Response {
    let id = op.as_str().to_string();
    let t = now();
    if let Err(d) = lock(&auth.limiter).begin(&id, t) {
        println!("auth: passkey login for {id} refused unchecked: locked or busy for {} s", d.as_secs().max(1));
        return locked(d);
    }
    let entry = auth.store.read().unwrap_or_else(|e| e.into_inner()).file.operators.get(&id).cloned();
    // the challenge it answers, taken, an operator's or a decoy's alike (murmur-i58)
    let outcome = match auth.take_login(assertion, &op) {
        Ok(Some(p)) => auth.check_passkey(p, assertion).await,
        Ok(None) => Err((false, "not one of your passkeys (a decoy's challenge: no passkey, or no such operator)".to_string())),
        Err(why) => {
            lock(&auth.limiter).release(&id);
            println!("auth: passkey login for {id} refused, not counted: {why}");
            return refusal(StatusCode::UNAUTHORIZED, LOGIN_PASSKEY_REFUSED);
        }
    };
    let proof = match outcome {
        Ok(proof) => {
            lock(&auth.limiter).success(&id);
            proof
        }
        Err((clone, why)) => {
            let lock_now = lock(&auth.limiter).failure(&id, t);
            println!("auth: failed passkey login for {id}: {why}");
            if listed {
                let reason = if clone { "passkey refused: its counter did not count up (cloned?)" } else { "passkey refused" };
                auth.decision.record_login_refused(&op, reason, lock_now.map(|d| d.as_secs()));
            }
            if let Some(d) = lock_now {
                return locked(d);
            }
            // the refusal says no more than that, short of a counter only a valid signature
            // shows: whether the id exists or holds a passkey is not said (murmur-i58; the
            // bridge's own log above says why)
            let detail = if clone {
                "passkey refused: its counter did not count up (a cloned authenticator?)"
            } else {
                LOGIN_PASSKEY_REFUSED
            };
            return refusal(StatusCode::UNAUTHORIZED, detail);
        }
    };
    let Some(entry) = entry else {
        return refusal(StatusCode::UNAUTHORIZED, "unknown operator or wrong password");
    };
    signed_in(auth, headers, op.clone(), entry, move |engage| auth.decision.open_passkey_login(&op, engage, proof))
}

/// The end of a checked login (password or passkey): the operator is on the run's roster, the
/// engine session opens (`open`, given whether the credential holds engagement authority
/// now; a refusal there is logged as `auth.refused`), a session the browser still holds
/// ends, and the cookie is set.
fn signed_in(
    auth: &Arc<Auth>,
    headers: &HeaderMap,
    op: OperatorId,
    entry: murmur_auth::OperatorEntry,
    open: impl FnOnce(bool) -> Result<SessionId, DecisionError>,
) -> Response {
    let (stamp, engage, scripted) = (entry.stamp, entry.engage, entry.scripted);
    if auth.decision.roster_entry(&op).is_none() {
        return refusal(
            StatusCode::FORBIDDEN,
            "your credential was added after this run started: it is not on the run's roster (restart the bridge to admit it)",
        );
    }
    let session = match open(engage) {
        Ok(s) => s,
        Err(e) => {
            // a login the engine refuses after the credential checked (rules 12: a credential
            // re-marked scripted or not since the start; a factor the policy does not admit)
            // is a refused login like a wrong password, on the record, and the browser's own
            // session stays
            println!("auth: login for {op} refused: {}", e.detail);
            auth.decision.record_login_refused(&op, &e.detail, None);
            return decision_refusal(e);
        }
    };
    // a login from a browser that still holds a session ends that one, once this one opened
    let old: Vec<String> = auth.tokens(headers).into_iter().map(str::to_string).collect();
    for t in old {
        let prev = lock(&auth.sessions).remove(&t);
        if let Some(l) = prev {
            auth.end(&l, "signed in again");
        }
    }
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
    println!(
        "auth: {op} signed in (session {}{})",
        session.0,
        if scripted { ", scripted: a harness-held credential" } else { "" }
    );
    let mut body = json!({
        "auth": "credentials",
        "signed_in": true,
        "operator": op,
        "engage": engage,
        "scripted": scripted,
        "idle_s": auth.settings.idle.as_secs(),
        "max_s": auth.settings.max.as_secs(),
        "step_up_acts": auth.decision.operators_json()["step_up_acts"],
    });
    auth.passkey_json(&op, &mut body);
    let mut r = Json(body).into_response();
    r.headers_mut().insert(header::SET_COOKIE, auth.cookie(&token, auth.settings.max));
    r
}

impl Auth {
    /// What the console needs to know of passkeys in this run, added to a session answer:
    /// whether they are on (the relying party and its origins), how many `op` has, what a
    /// step-up and a login must be.
    fn passkey_json(&self, op: &OperatorId, body: &mut Value) {
        let (step, login) = self.decision.factors();
        body["step_up_factor"] = json!(step.as_str());
        body["login_factor"] = json!(login.as_str());
        if let Some(rp) = &self.rp {
            body["passkey"] = json!({
                "rp_id": rp.rp_id,
                "origins": rp.origins,
                "registered": self.usable_passkeys(op).len(),
            });
        }
        if let Some(s) = self.decision.study() {
            body["study"] = json!({ "participant": s.participant, "password_step_up": s.password_step_up.is_some() });
        }
    }
}

/// Rules 10: `POST /api/auth/login/passkey {"operator"}`: the options for
/// `navigator.credentials.get` to sign in by passkey (a challenge for a login, over the
/// operator's passkeys). An id without one, or no such operator, gets options alike that no
/// authenticator answers.
async fn login_passkey_options(State(auth): State<Arc<Auth>>, body: Bytes) -> Response {
    if !auth.credentials() {
        return refusal(StatusCode::CONFLICT, "this is a dev run (MURMUR_AUTH=dev): there is no login");
    }
    #[derive(Deserialize)]
    struct B {
        operator: String,
    }
    let Ok(B { operator }) = serde_json::from_slice::<B>(&body) else {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "wants {\"operator\": id}");
    };
    let id = operator.trim();
    if murmur_auth::validate_id(id).is_err() {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "not an operator id");
    }
    auth.refresh();
    let (_, login) = auth.decision.factors();
    if login == murmur_decide::LoginFactor::Password {
        return refusal(StatusCode::CONFLICT, "this run's policy signs people in by password only ([approval] login_factor)");
    }
    match auth.issue(&OperatorId::new(id), None, murmur_decide::StepUpFor::Login) {
        Ok(o) => Json(o).into_response(),
        Err(e) => decision_refusal(e),
    }
}

/// Rules 10: `POST /api/auth/step_up {"act": ...}`: the options for `navigator.credentials.get`
/// to step up by passkey for one act, its challenge derived from it: `{"act": "sign",
/// "intent": n, "token": t}` (an engagement's signature quotes its picture token), `{"act":
/// "declare_hostile", "track": id}`, `{"act": "confirm_declaration", "track": id}`, `{"act":
/// "accept_unknown", "intent": n or null, "accept_unknown": [{"uid", "since"}]}`. The act's
/// request then carries the assertion as `"passkey"`.
async fn step_up_options(State(auth): State<Arc<Auth>>, headers: HeaderMap, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct B {
        act: String,
        #[serde(default)]
        intent: Option<u64>,
        #[serde(default)]
        token: Option<u64>,
        #[serde(default)]
        track: Option<String>,
        #[serde(default)]
        accept_unknown: Vec<crate::decision::engage::AcceptUnknown>,
    }
    let actor = match auth.actor(&headers, false) {
        Ok(a) => a,
        Err(e) => return decision_refusal(e),
    };
    let Some(session) = actor.login() else {
        return refusal(StatusCode::CONFLICT, "a dev run has no step-up");
    };
    let b: B = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return refusal(StatusCode::UNPROCESSABLE_ENTITY, &format!("step-up options: {e}")),
    };
    let track = || b.track.clone().ok_or_else(|| DecisionError::new(422, "this act names its track"));
    let purpose = match b.act.as_str() {
        "sign" => match b.intent {
            Some(i) => auth.decision.sign_purpose(i, b.token),
            None => Err(DecisionError::new(422, "a signature names its intent")),
        },
        "declare_hostile" => track().map(|track| murmur_decide::StepUpFor::DeclareHostile { track }),
        "confirm_declaration" => track().and_then(|t| auth.decision.confirm_purpose(&t)),
        "accept_unknown" if !b.accept_unknown.is_empty() => Ok(auth.decision.accept_purpose(b.intent, &b.accept_unknown)),
        other => Err(DecisionError::new(
            422,
            format!("act {other:?}: sign, declare_hostile, confirm_declaration or accept_unknown (with its vehicles)"),
        )),
    };
    match purpose.and_then(|p| auth.issue(actor.operator(), Some(session), p)) {
        Ok(o) => Json(o).into_response(),
        Err(e) => decision_refusal(e),
    }
}

/// Rules 10: `GET /api/auth/passkeys`: the signed-in operator's passkeys (labels, ids' starts,
/// when registered and last used; nothing else).
async fn my_passkeys(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    let actor = match auth.actor_peek(&headers) {
        Ok(a) => a,
        Err(e) => return decision_refusal(e),
    };
    let usable = auth.usable_passkeys(actor.operator());
    let list: Vec<Value> = usable
        .iter()
        .map(|p| {
            json!({
                "label": p.label,
                "id": p.id.chars().take(12).collect::<String>(),
                "created_unix": p.created_unix,
                "last_used_unix": p.last_used_unix,
            })
        })
        .collect();
    let mut body = json!({ "operator": actor.operator(), "passkeys": list });
    auth.passkey_json(actor.operator(), &mut body);
    Json(body).into_response()
}

/// Rules 10: `POST /api/auth/passkeys/register/options`: the signed-in operator starts
/// registering a passkey (`navigator.credentials.create`'s options). Its finish carries the
/// password again.
async fn register_options(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    let actor = match auth.actor(&headers, false) {
        Ok(a) => a,
        Err(e) => return decision_refusal(e),
    };
    let Some(session) = actor.login() else {
        return refusal(StatusCode::CONFLICT, "a dev run registers no passkey");
    };
    auth.start_registration(actor.operator(), format!("s{}", session.0))
}

/// Rules 10: `POST /api/auth/passkeys/register {"credential", "label", "password"}`: the
/// registration's finish, the operator's password re-entered (a session left open does not
/// add a credential to someone's account). Logged as `passkey.registered` quoting the step-up;
/// kept in the credential file (public key and counter).
async fn register_finish(State(auth): State<Arc<Auth>>, headers: HeaderMap, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct B {
        credential: Value,
        #[serde(default)]
        label: String,
        #[serde(default)]
        password: Option<String>,
    }
    let actor = match auth.actor(&headers, false) {
        Ok(a) => a,
        Err(e) => return decision_refusal(e),
    };
    let Some(session) = actor.login() else {
        return refusal(StatusCode::CONFLICT, "a dev run registers no passkey");
    };
    // the body holds a password: never echo it back
    let Ok(b) = serde_json::from_slice::<B>(&body) else {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "wants {\"credential\": <registration>, \"label\": ..., \"password\": ...}");
    };
    let Some(reg) = lock(&auth.registering).remove(&format!("s{}", session.0)) else {
        return refusal(StatusCode::CONFLICT, "no registration is waiting for this session (expired?): start again");
    };
    if &reg.operator != actor.operator() {
        return refusal(StatusCode::FORBIDDEN, "another operator's registration");
    }
    // the password, for this session's own operator, counted against the session's limit
    // (a registration's step-up is a password whatever the step-up factor: the passkey being
    // registered can't vouch for itself)
    let step = match auth.password_step_up(&actor, "register a passkey", b.password).await {
        Ok(s) => s,
        Err(r) => return *r,
    };
    auth.finish_registration(&actor.operator().clone(), reg, &b.credential, &b.label, Some((&actor, step))).await
}

/// Rules 10: `POST /api/auth/passkeys/enroll/options {"operator", "code"}`: registration by a
/// one-time invitation (`murmur-auth passkey-invite`), no session needed.
async fn enroll_options(State(auth): State<Arc<Auth>>, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct B {
        operator: String,
        code: String,
    }
    let Ok(b) = serde_json::from_slice::<B>(&body) else {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "wants {\"operator\": id, \"code\": ...}");
    };
    let op = OperatorId::new(b.operator.trim());
    if let Err(r) = auth.invitation(&op, &b.code) {
        return *r;
    }
    auth.start_registration(&op, format!("i{op}"))
}

/// Rules 10: `POST /api/auth/passkeys/enroll {"operator", "code", "credential", "label"}`: the
/// invitation's registration finished; the invitation is used up. Logged as
/// `passkey.registered` (`via: invitation`).
async fn enroll_finish(State(auth): State<Arc<Auth>>, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct B {
        operator: String,
        code: String,
        credential: Value,
        #[serde(default)]
        label: String,
    }
    let Ok(b) = serde_json::from_slice::<B>(&body) else {
        return refusal(StatusCode::UNPROCESSABLE_ENTITY, "wants {\"operator\", \"code\", \"credential\", \"label\"}");
    };
    let op = OperatorId::new(b.operator.trim());
    if let Err(r) = auth.invitation(&op, &b.code) {
        return *r;
    }
    let Some(reg) = lock(&auth.registering).remove(&format!("i{op}")) else {
        return refusal(StatusCode::CONFLICT, "no registration is waiting for this invitation (expired?): open the link again");
    };
    auth.finish_registration(&op, reg, &b.credential, &b.label, None).await
}

impl Auth {
    /// A password step-up for `actor`'s session (whatever the step-up factor: a registration's).
    async fn password_step_up(&self, actor: &Actor, what: &str, password: Option<String>) -> Result<StepUpChecked, Box<Response>> {
        let Some(session) = actor.login() else {
            return Err(Box::new(refusal(StatusCode::UNAUTHORIZED, "sign in first")));
        };
        let refused = |why: &str| {
            if let Err(e) = self.decision.record_action(actor, "step_up.refused", &format!("{what}: {why}")) {
                eprintln!("auth: logging a refused step-up: {}", e.detail);
            }
        };
        let Some(password) = password.filter(|p| !p.is_empty()) else {
            refused("no password given");
            return Err(Box::new(
                (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "detail": "re-enter your password to register a passkey", "step_up": true, "factor": "password" })),
                )
                    .into_response(),
            ));
        };
        let key = format!("session {}", session.0);
        match self.check(&self.step_limiter, &key, actor.operator().as_str(), password).await {
            Ok(_) => Ok(StepUpChecked {
                session,
                operator: actor.operator().clone(),
                factor: StepFactor::Password,
            }),
            Err(Refused::Wrong) => {
                refused("wrong password");
                Err(Box::new(
                    (StatusCode::UNAUTHORIZED, Json(json!({ "detail": "wrong password: not registered", "step_up": true }))).into_response(),
                ))
            }
            Err(Refused::WrongLocked(d)) => {
                refused("too many failed attempts: the session ends");
                let gone = lock(&self.sessions).end_where(|l| l.session == session);
                for l in &gone {
                    self.end(l, "step-up failed repeatedly");
                }
                Err(Box::new(locked_session(d)))
            }
            Err(Refused::Locked(d)) => {
                refused("too many attempts at once");
                Err(Box::new(locked_session(d)))
            }
        }
    }

    /// Whether `code` is `op`'s open invitation (an operator on the roster, a person's
    /// credential); refused otherwise (the same answer for every reason). Failures count
    /// against the id's login limit.
    fn invitation(&self, op: &OperatorId, code: &str) -> Result<(), Box<Response>> {
        if self.rp.is_none() {
            return Err(Box::new(refusal(StatusCode::CONFLICT, "this run takes no passkey (a credentials run with a console on localhost does)")));
        }
        let t = now();
        let key = op.as_str().to_string();
        if let Err(d) = lock(&self.limiter).begin(&key, t) {
            return Err(Box::new(locked(d)));
        }
        self.refresh();
        let ok = {
            let store = self.store.read().unwrap_or_else(|e| e.into_inner());
            store.file.invite_valid(op.as_str(), code.trim(), now_unix())
                && store.file.operators.get(op.as_str()).is_some_and(|e| !e.scripted)
        } && self.decision.roster_entry(op).is_some();
        if ok {
            lock(&self.limiter).success(&key);
            Ok(())
        } else {
            let lock_now = lock(&self.limiter).failure(&key, t);
            println!("auth: a passkey invitation for {op} refused");
            match lock_now {
                Some(d) => Err(Box::new(locked(d))),
                None => Err(Box::new(refusal(
                    StatusCode::UNAUTHORIZED,
                    "this invitation is not open (used, expired, or for someone else): ask for a new one",
                ))),
            }
        }
    }

    /// Start a registration for `op` (kept under `slot`: a session's, or an invitation's).
    fn start_registration(&self, op: &OperatorId, slot: String) -> Response {
        let Some(rp) = &self.rp else {
            return refusal(StatusCode::CONFLICT, "this run takes no passkey (a credentials run with a console on localhost does)");
        };
        self.sweep_pending();
        if lock(&self.registering).len() >= PENDING_CAP {
            return refusal(StatusCode::TOO_MANY_REQUESTS, "too many registrations waiting: try again in a minute");
        }
        let existing = {
            let store = self.store.read().unwrap_or_else(|e| e.into_inner());
            match store.file.operators.get(op.as_str()) {
                Some(e) if e.scripted => {
                    return refusal(StatusCode::FORBIDDEN, "a scripted credential (a harness's) holds no passkey");
                }
                Some(e) => e.passkeys.clone(),
                None => return refusal(StatusCode::FORBIDDEN, "not an operator in the credential file"),
            }
        };
        match rp.start_registration(op.as_str(), &existing) {
            Ok((options, reg)) => {
                lock(&self.registering).insert(
                    slot,
                    PendingReg {
                        operator: op.clone(),
                        reg,
                        until: now() + REGISTRATION_TTL,
                    },
                );
                Json(options).into_response()
            }
            Err(e) => refusal(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
    }

    /// Finish a registration: verify it (webauthn-rs: user verification, ES256), keep it in the
    /// credential file, and log it (`passkey.registered`: through `by`'s session quoting their
    /// password step-up, or by invitation, which this uses up). The engine refusing it takes it
    /// out of the file again.
    async fn finish_registration(
        &self,
        op: &OperatorId,
        reg: PendingReg,
        credential: &Value,
        label: &str,
        by: Option<(&Actor, StepUpChecked)>,
    ) -> Response {
        let Some(rp) = &self.rp else {
            return refusal(StatusCode::CONFLICT, "this run takes no passkey");
        };
        let _io = self.passkey_io.lock().await;
        let entry = match rp.finish_registration(&reg.reg, credential, label, now_unix()) {
            Ok(e) => e,
            Err(e) => return refusal(StatusCode::UNPROCESSABLE_ENTITY, &e.to_string()),
        };
        let key = murmur_decide::PasskeyKey {
            operator: op.clone(),
            id: entry.id.clone(),
            public_key: entry.public_key.clone(),
            counter: entry.counter,
        };
        let invited = by.is_none();
        let (id, kept) = (op.as_str().to_string(), entry.clone());
        if let Err(e) = self
            .modify_file(move |f| {
                f.add_passkey(&id, kept)?;
                if invited {
                    f.take_invite(&id)?;
                }
                Ok(())
            })
            .await
        {
            return refusal(StatusCode::CONFLICT, &format!("not registered: {e}"));
        }
        if let Err(e) = self.decision.record_passkey_registered(key, by) {
            // the log refused it: nor does the file keep it
            let (id, cred) = (op.as_str().to_string(), entry.id.clone());
            let undone = self
                .modify_file(move |f| {
                    if let Some(o) = f.operators.get_mut(&id) {
                        o.passkeys.retain(|p| p.id != cred);
                    }
                    Ok(())
                })
                .await;
            if let Err(u) = undone {
                eprintln!("auth: undoing a refused passkey registration of {op}: {u}");
            }
            return decision_refusal(e);
        }
        println!("auth: {op} registered a passkey ({}){}", entry.label, if invited { " by invitation" } else { "" });
        let mut body = json!({
            "registered": { "label": entry.label, "id": entry.id.chars().take(12).collect::<String>() },
            "operator": op,
        });
        self.passkey_json(op, &mut body);
        Json(body).into_response()
    }
}

async fn logout(State(auth): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    let tokens: Vec<String> = auth.tokens(&headers).into_iter().map(str::to_string).collect();
    for token in tokens {
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
            "step_up_acts": ops["step_up_acts"],
        }))
        .into_response();
    }
    let out = auth.token(&headers).map(str::to_string).and_then(|t| {
        let l = auth.resolve(&t, false).ok()?;
        let ends = lock(&auth.sessions).ends_at(&t)?;
        Some((l, ends.saturating_duration_since(now())))
    });
    match out {
        Some((l, left)) => {
            let mut body = json!({
                "auth": "credentials",
                "signed_in": true,
                "operator": l.operator,
                "engage": l.engage,
                "ends_in_s": left.as_secs(),
                "step_up_acts": auth.decision.operators_json()["step_up_acts"],
            });
            auth.passkey_json(&l.operator, &mut body);
            Json(body).into_response()
        }
        None => {
            // signed out: whether a passkey sign-in is possible here, and on which host
            let mut body = json!({ "auth": "credentials", "signed_in": false });
            let (_, login) = auth.decision.factors();
            body["login_factor"] = json!(login.as_str());
            if let Some(rp) = &auth.rp {
                body["passkey"] = json!({ "rp_id": rp.rp_id, "origins": rp.origins });
            }
            Json(body).into_response()
        }
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

    /// A request that goes away before its password check starts gives its reservation back:
    /// hanging up can't leave an id busy.
    #[test]
    fn a_reservation_is_given_back_when_its_request_goes_away() {
        let l = Arc::new(Mutex::new(Limiter::default()));
        let t = Instant::now();
        let held: Vec<Reserved> = (0..5)
            .map(|_| {
                lock(&l).begin("op-3", t).expect("room");
                Reserved { limiter: l.clone(), key: "op-3".into(), held: true }
            })
            .collect();
        assert!(lock(&l).begin("op-3", t).is_err(), "five in flight");
        drop(held);
        assert!(lock(&l).begin("op-3", t).is_ok(), "given back");
    }

    /// User decision: a run is dev unless it asks for credentials.
    #[test]
    fn a_run_is_dev_unless_it_asks_for_credentials() {
        assert_eq!(mode_from(None), Ok(AuthMode::Dev));
        assert_eq!(mode_from(Some("dev")), Ok(AuthMode::Dev));
        assert_eq!(mode_from(Some("credentials")), Ok(AuthMode::Credentials));
        assert!(mode_from(Some("password")).is_err());
    }

    #[test]
    fn bearer_keys_and_cookies_are_read_exactly() {
        let h = headers(&[("authorization", "Bearer mma_1234abcd_xyz")]);
        assert_eq!(bearer(&h), Some("mma_1234abcd_xyz"));
        assert_eq!(bearer(&headers(&[("authorization", "Basic abc")])), None);
        assert_eq!(bearer(&headers(&[("authorization", "Bearer ")])), None);
    }
}
