//! The contacts process's own API (docs/AUTONOMOUS-NODES.md 2.5, decision D1; bd murmur-y85j.4):
//! orders to the nodes, and what the nodes are doing and perceive, served by the truth side
//! itself. Our bridge never relays or holds a red order: a situation's timeline (the `red` step,
//! `eval/situation_run.py`) and, later, a red-side agent with its own token come here.
//!
//! `murmur-contacts` listens on `127.0.0.1:$MURMUR_CONTACTS_PORT` (unset: no API) and every
//! request carries the stack's simulation token (`X-Murmur-Sim-Token`, [`token_check`]: the
//! bridge's own check of `POST /api/sim/contacts`, SHA-256 digests compared in constant time);
//! without `MURMUR_SIM_TOKEN` every request is refused. Routes, each answered by the contacts
//! process's own loop between its ticks ([`Desk`]):
//!
//! - `POST /orders`: one order as a file's `[[order]]` writes it ([`OrderSpec`], JSON; `at_s`
//!   absent: now, at the next tick; given, on the contacts' clock). `200 { id, accepted: true }`,
//!   or `422 { refused: <why>, code }`, codes: `unreadable` (not an order), `keys` (a key its
//!   kind of order does not take, or none an order has), `unknown_node`, `not_a_node` (a
//!   recorded contact, or one with a scripted weapon), `node_gone` (destroyed, spent or left the
//!   scenario), `unknown_target` (no such contact, or a vehicle not in this run's fleet),
//!   `weapon_not_carried` (its kind carries none, or not the one named), `legacy_sensing` (an
//!   attack where the bridge senses by the legacy model), `past` (an `at_s` gone by), `invalid`
//!   (anything else [`check`] refuses, said in words).
//! - `GET /orders/{id}` (and `GET /orders`, every one): its state, `pending`, `running`,
//!   `ended` or `refused`, why it ended or was refused (the node's own words: `target_lost`,
//!   `out_of_rounds`, `node_gone`, ...), what the node last said it was doing, and its shots. File
//!   orders are here from the start; an order a node gives itself (its kind's `on_empty`) from
//!   when it does.
//! - `GET /nodes`: each contact that can take an order, as it stands: where it is, its order and
//!   those to come, its rounds left, its damage, and what it perceives: our vehicles as our C2
//!   hears them within its sensors' reach and horizon (decision D2: the bridge's answer `ours`,
//!   never the truth of ours, and only near a node that looks), and the other contacts within it.
//!
//! The same JSON and the same intake ([`Desk::order`]) serve any client; a red-side agent later
//! gets its own token and sees only its nodes' perception (spec 2.5: not built here).

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path as UrlPath, Request as HttpRequest, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use murmur_standins::geo::distance_m;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, oneshot};

use crate::catalog::{Kind, Sensors};
use crate::motion::Mover;
use crate::node::{Do, NodeEvent, Nodes, OrderSpec, Seen, TargetRef, check, perceives};
use crate::scenario::{Domain, Motion, Scenario};
use crate::weapon::OursAt;

/// The header every request carries the stack's simulation token in (the bridge's
/// `tracks::SIM_TOKEN_HEADER`).
pub const TOKEN_HEADER: &str = "x-murmur-sim-token";
/// The largest order the API reads, bytes.
const BODY_LIMIT: usize = 64 * 1024;
/// How long a request waits for the contacts process's loop (it answers between ticks; a tick
/// waits on the bridge 3 s at most).
const ANSWER_WAIT: Duration = Duration::from_secs(10);

/// Whether a request carries the stack's token `want` (compared as SHA-256 digests in constant
/// time, as the bridge's `tracks::sim_token_check`). Without a token configured the API is
/// closed.
pub fn token_check(want: Option<&str>, headers: &HeaderMap) -> Result<(), &'static str> {
    let Some(want) = want.filter(|t| !t.is_empty()) else {
        return Err("the contacts API is closed: no MURMUR_SIM_TOKEN was set for this process");
    };
    let given = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or("the contacts API takes the stack's simulation token (X-Murmur-Sim-Token)")?;
    let digest = |s: &str| -> [u8; 32] { Sha256::digest(s.as_bytes()).into() };
    if bool::from(digest(given.trim()).ct_eq(&digest(want))) {
        Ok(())
    } else {
        Err("wrong simulation token")
    }
}

/// The uids of this run's fleet manifest (`sim/run/fleet.json`: `{"vehicles": [{"uid": ..}]}`),
/// so an order at a vehicle not in the run is refused.
pub fn fleet_uids(path: &Path) -> Result<BTreeSet<u64>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let list = doc["vehicles"].as_array().ok_or_else(|| format!("{}: no vehicles list", path.display()))?;
    Ok(list.iter().filter_map(|v| v["uid"].as_u64()).collect())
}

// ---------------------------------------------------------------------------------------------
// the desk: the contacts process's loop answers here

/// What a client asks.
#[derive(Debug, Clone)]
pub enum Ask {
    /// `POST /orders` with this body.
    Order(Value),
    /// `GET /orders`.
    Orders,
    /// `GET /orders/{id}`.
    OrderState(u64),
    /// `GET /nodes`.
    Nodes,
}

/// One request to the contacts process's loop and where its answer goes.
pub struct Request {
    pub ask: Ask,
    pub reply: oneshot::Sender<Reply>,
}

/// An answer: an HTTP status and a JSON body.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Reply {
    fn ok(body: Value) -> Reply {
        Reply { status: 200, body }
    }
}

/// Why an order is refused: a code a client can act on, and the words.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub why: String,
}

fn refuse(code: &'static str, why: impl Into<String>) -> Refusal {
    Refusal { code, why: why.into() }
}

/// What the loop holds when it answers: the scenario, its nodes and movers, our vehicles as the
/// bridge's latest answer gave them (fresh, else none), and the scenario second and unix time
/// now.
pub struct Here<'a> {
    pub scenario: &'a Scenario,
    pub nodes: &'a mut Nodes,
    pub movers: &'a [Mover],
    pub ours: &'a [OursAt],
    pub s: f64,
    pub t: f64,
}

/// One order's record.
#[derive(Debug, Clone, PartialEq)]
struct Record {
    node: String,
    what: String,
    /// `file`, `api`, or `node` (one a node gave itself: its kind's `on_empty`).
    source: &'static str,
    spec: Option<OrderSpec>,
    state: &'static str,
    taken_s: Option<f64>,
    ended_s: Option<f64>,
    why: Option<String>,
    /// What the node last said it was doing under it, why, and when.
    status: Option<(String, Option<String>, Option<String>, f64)>,
    shots: u32,
}

impl Record {
    fn new(spec: &OrderSpec, source: &'static str) -> Record {
        Record {
            node: spec.node.clone(),
            what: spec.what.as_str().into(),
            source,
            spec: Some(spec.clone()),
            state: "pending",
            taken_s: None,
            ended_s: None,
            why: None,
            status: None,
            shots: 0,
        }
    }

    fn json(&self, id: u64) -> Value {
        let mut v = json!({ "id": id, "node": self.node, "do": self.what, "source": self.source, "state": self.state, "shots": self.shots });
        if let Some(spec) = &self.spec {
            v["order"] = serde_json::to_value(spec).unwrap_or(Value::Null);
        }
        if let Some(s) = self.taken_s {
            v["taken_s"] = json!(s);
        }
        if let Some(s) = self.ended_s {
            v["ended_s"] = json!(s);
        }
        if let Some(w) = &self.why {
            v["why"] = json!(w);
        }
        if let Some((what, why, note, s)) = &self.status {
            v["status"] = json!({ "what": what, "why": why, "note": note, "s": s });
        }
        v
    }
}

/// The API's side of the contacts process: the intake every order goes through ([`Desk::order`])
/// and the record of every order, fed by the nodes' events ([`Desk::record`]).
pub struct Desk {
    /// This run's fleet, when its manifest was read (else a vehicle target is not checked).
    fleet: Option<BTreeSet<u64>>,
    /// The bridge senses by the legacy model (`MURMUR_SENSE_MODEL`, else the scenario's): no
    /// attack, as a file's orders ([`crate::node::check_file`]).
    legacy: bool,
    orders: BTreeMap<u64, Record>,
}

impl Desk {
    /// The desk for `nodes` (the file's orders recorded as pending under the ids they were given).
    pub fn new(nodes: &Nodes, fleet: Option<BTreeSet<u64>>, legacy: bool) -> Desk {
        let orders = nodes.pending().map(|(id, spec)| (id, Record::new(spec, "file"))).collect();
        Desk { fleet, legacy, orders }
    }

    /// One order through the intake: refused saying why, or queued for its second (`at_s`
    /// absent: the next tick) with its id. The checks a file's order passes at load
    /// ([`check`]), and what only a running process knows: whether the node is still in play,
    /// the vehicle in the run, the second still to come.
    pub fn order(&mut self, spec: OrderSpec, here: &mut Here) -> Result<u64, Refusal> {
        let s = here.scenario;
        if spec.at_s.is_some_and(|a| a < here.s) {
            return Err(refuse("past", format!("at_s {} has gone by: the contacts' clock is at {:.1} s", spec.at_s.unwrap_or(0.0), here.s)));
        }
        let Some(c) = s.contacts.iter().find(|c| c.id == spec.node) else {
            return Err(refuse("unknown_node", format!("node {:?} is no contact of the scenario", spec.node)));
        };
        if c.motion == "recorded" || c.weapon.is_some() {
            return Err(refuse("not_a_node", format!("contact {:?} takes no order (a recorded contact, or one with a scripted weapon)", spec.node)));
        }
        let mover = here.movers.iter().find(|m| m.contact.id == spec.node);
        let left = mover.is_some_and(|m| m.out_of_play || m.contact.end_s.is_some_and(|e| here.s >= e));
        if left || here.nodes.node(&spec.node).is_some_and(|n| n.gone()) {
            return Err(refuse("node_gone", format!("node {:?} is destroyed, spent or gone from the scenario", spec.node)));
        }
        if let Some(k) = spec.stray_key() {
            let takes = spec.what.takes();
            let list = if takes.is_empty() { "nothing".to_string() } else { takes.join(", ") };
            return Err(refuse("keys", format!("{} takes {list} (and label), not {k}", spec.what.as_str())));
        }
        for t in [&spec.target, &spec.from].into_iter().flatten() {
            match t {
                TargetRef::Contact(id) if *id == spec.node || !s.contacts.iter().any(|c| c.id == *id) => {
                    return Err(refuse("unknown_target", format!("contact {id:?} is no other contact of the scenario")));
                }
                TargetRef::Vehicle(uid) if self.fleet.as_ref().is_some_and(|f| !f.contains(uid)) => {
                    return Err(refuse("unknown_target", format!("vehicle {uid} is not in this run's fleet")));
                }
                _ => {}
            }
        }
        if spec.what == Do::Attack {
            let kind = mover.and_then(|m| m.contact.entry()).filter(|k| k.armed);
            let Some(kind) = kind else {
                return Err(refuse("weapon_not_carried", format!("its kind {:?} carries no weapon", c.kind)));
            };
            if let Some(name) = &spec.weapon
                && !kind.loadout.iter().any(|w| w.name == *name && w.kind != "mine")
            {
                return Err(refuse("weapon_not_carried", format!("its kind {:?} carries no {name:?} to fire", c.kind)));
            }
            if self.legacy {
                return Err(refuse("legacy_sensing", "orders to attack need the physical sensing model: this stack senses by the legacy one"));
            }
        }
        // the rest as a file's order is checked, at its second (now, when it names none)
        let mut at = spec.clone();
        at.at_s = Some(spec.at_s.unwrap_or(here.s));
        check(&at, s, false).map_err(|e| refuse("invalid", e))?;
        let id = here.nodes.order(spec.clone());
        self.orders.insert(id, Record::new(&spec, "api"));
        Ok(id)
    }

    /// `POST /orders`: the body read as an order, then [`Desk::order`].
    pub fn order_json(&mut self, body: Value, here: &mut Here) -> Reply {
        let spec: OrderSpec = match serde_json::from_value(body) {
            Ok(o) => o,
            Err(e) => {
                let e = e.to_string();
                let code = if e.contains("unknown field") { "keys" } else { "unreadable" };
                return Reply { status: 422, body: json!({ "refused": format!("not an order: {e}"), "code": code }) };
            }
        };
        match self.order(spec, here) {
            Ok(id) => Reply::ok(json!({ "id": id, "accepted": true })),
            Err(r) => Reply { status: 422, body: json!({ "refused": r.why, "code": r.code }) },
        }
    }

    /// Every order's record from what the nodes said this tick: taken, refused, ended (and why),
    /// what it is doing, its shots.
    pub fn record(&mut self, events: &[NodeEvent]) {
        for e in events {
            let Some(id) = e.order else { continue };
            let r = self.orders.entry(id).or_insert_with(|| Record {
                node: e.node.clone(),
                // an order a node gives itself says what it is in its note (its kind's on_empty)
                what: e.note.clone().unwrap_or_default(),
                source: "node",
                spec: None,
                state: "pending",
                taken_s: None,
                ended_s: None,
                why: e.why.clone(),
                status: None,
                shots: 0,
            });
            match e.what.as_str() {
                "order" => {
                    r.state = "running";
                    r.taken_s = Some(e.s);
                }
                "refused" | "ended" => {
                    r.state = if e.what == "ended" { "ended" } else { "refused" };
                    r.ended_s = Some(e.s);
                    r.why = e.why.clone();
                }
                what => {
                    if matches!(what, "fired" | "struck") {
                        r.shots += 1;
                    }
                    r.status = Some((what.to_string(), e.why.clone(), e.note.clone(), e.s));
                }
            }
        }
    }

    /// `GET /orders/{id}`.
    pub fn state(&self, id: u64) -> Reply {
        match self.orders.get(&id) {
            Some(r) => Reply::ok(r.json(id)),
            None => Reply { status: 404, body: json!({ "error": format!("no order {id}") }) },
        }
    }

    /// `GET /orders`.
    pub fn states(&self) -> Reply {
        Reply::ok(json!({ "orders": self.orders.iter().map(|(id, r)| r.json(*id)).collect::<Vec<_>>() }))
    }

    /// `GET /nodes`: every contact that can take an order, as it stands, and what it perceives.
    pub fn nodes(&self, here: &Here) -> Reply {
        let list: Vec<Value> = here
            .movers
            .iter()
            .filter(|m| !matches!(m.contact.motion, Motion::Recorded { .. }) && m.contact.weapon.is_none())
            .map(|m| node_json(m, here))
            .collect();
        Reply::ok(json!({ "s": round(here.s, 1), "t": round(here.t, 3), "nodes": list }))
    }

    /// One request answered.
    pub fn answer(&mut self, ask: Ask, here: &mut Here) -> Reply {
        match ask {
            Ask::Order(body) => self.order_json(body, here),
            Ask::Orders => self.states(),
            Ask::OrderState(id) => self.state(id),
            Ask::Nodes => self.nodes(here),
        }
    }
}

fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

fn domain(d: Domain) -> &'static str {
    match d {
        Domain::Air => "air",
        Domain::Surface => "surface",
        Domain::Subsurface => "subsurface",
        Domain::Ground => "ground",
    }
}

fn target_json(t: &TargetRef) -> Value {
    match t {
        TargetRef::Vehicle(u) => json!({ "vehicle": u }),
        TargetRef::Contact(c) => json!({ "contact": c }),
    }
}

/// What a node perceives of a target, and how far it is.
fn seen_json(t: &Seen, me: [f64; 3]) -> Value {
    json!({
        "lat": round(t.lat, 6),
        "lon": round(t.lon, 6),
        "alt_m": round(t.alt, 1),
        "ve": round(t.ve, 2),
        "vn": round(t.vn, 2),
        "domain": domain(t.domain),
        "range_m": round(distance_m(me[0], me[1], t.lat, t.lon).hypot(t.alt - me[2]), 0),
    })
}

fn node_json(m: &Mover, here: &Here) -> Value {
    let id = &m.contact.id;
    let node = here.nodes.node(id);
    let kind: Option<&Kind> = node.and_then(|n| n.kind()).or_else(|| m.contact.entry());
    let sensors: Sensors = node.map(|n| n.sensors()).or_else(|| kind.and_then(|k| k.sensors)).unwrap_or_default();
    let rounds: Vec<u32> = node.map_or_else(|| kind.map_or(Vec::new(), |k| k.loadout.iter().map(|w| w.count).collect()), |n| n.rounds.clone());
    let loadout: Vec<Value> = kind
        .map(|k| {
            k.loadout
                .iter()
                .zip(&rounds)
                .map(|(w, left)| {
                    json!({
                        "name": w.name, "kind": w.kind, "rounds_left": left, "count": w.count,
                        "min_range_m": w.min_range_m, "max_range_m": w.max_range_m,
                        "targets": w.targets.iter().map(|d| domain(*d)).collect::<Vec<_>>(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let in_play = m.st.exists && !node.is_some_and(|n| n.gone());
    let damage = if m.out_of_play {
        "destroyed"
    } else {
        m.damage.map_or("undamaged", |(d, _)| d.as_str())
    };
    let me = [m.st.lat, m.st.lon, m.st.alt_m];
    let mut v = json!({
        "id": id,
        "name": m.contact.name,
        "kind": m.contact.catalog_kind(),
        "domain": domain(m.contact.domain),
        "in_play": in_play,
        "damage": damage,
        "can_move": !m.crippled,
        "can_fire": !m.disarmed,
        "loadout": loadout,
        "sensors": { "eye_m": sensors.eye_m, "radar_m": sensors.radar_m, "sonar_m": sensors.sonar_m },
        "order": Value::Null,
        "pending": here.nodes.pending().filter(|(_, o)| o.node == *id).map(|(oid, o)| json!({ "id": oid, "do": o.what.as_str(), "at_s": o.at_s })).collect::<Vec<_>>(),
    });
    if in_play {
        v["position"] = json!({
            "lat": round(m.st.lat, 6),
            "lon": round(m.st.lon, 6),
            "alt_m": round(m.st.alt_m, 1),
            "course_deg": round(m.st.course_deg, 1),
            "speed_mps": round(m.st.speed_mps, 2),
        });
        let ours: Vec<Value> = here
            .ours
            .iter()
            .filter_map(|o| {
                let t = Seen::of_ours(o);
                perceives(&sensors, m.contact.domain, me, &t).then(|| {
                    let mut j = seen_json(&t, me);
                    j["uid"] = json!(o.uid);
                    j
                })
            })
            .collect();
        let contacts: Vec<Value> = here
            .movers
            .iter()
            .filter(|o| o.contact.id != *id && o.st.exists)
            .filter_map(|o| {
                let t = Seen::of_contact(o);
                perceives(&sensors, m.contact.domain, me, &t).then(|| {
                    let mut j = seen_json(&t, me);
                    j["id"] = json!(o.contact.id);
                    j
                })
            })
            .collect();
        v["perceives"] = json!({ "ours": ours, "contacts": contacts });
    }
    if let Some(o) = node.and_then(|n| n.running()) {
        let mut j = json!({
            "id": o.id,
            "do": o.spec.what.as_str(),
            "since_s": round(o.since_s, 1),
            "fired": o.fired,
            "status": o.status.map(|(w, y)| json!({ "what": w, "why": y })),
        });
        if let Some(t) = &o.spec.target {
            j["target"] = target_json(t);
            j["target_seen_s"] = json!(o.seen_s.map(|s| round(s, 1)));
            j["target_last"] = o.last.map_or(Value::Null, |l| seen_json(&l, me));
        }
        v["order"] = j;
    }
    v
}

// ---------------------------------------------------------------------------------------------
// HTTP

#[derive(Clone)]
struct Api {
    token: Option<Arc<str>>,
    tx: mpsc::Sender<Request>,
}

/// The routes, checking the token on every request before anything else, each answered by
/// whoever holds `tx`'s receiver (the contacts process's loop).
pub fn router(token: Option<String>, tx: mpsc::Sender<Request>) -> Router {
    let api = Api { token: token.map(|t| Arc::from(t.trim())), tx };
    Router::new()
        .route("/orders", post(post_order).get(get_orders))
        .route("/orders/{id}", get(get_order))
        .route("/nodes", get(get_nodes))
        .fallback(nowhere)
        .with_state(api)
}

/// Listen on 127.0.0.1:`port` and serve [`router`] on its own task; the address it listens on.
pub async fn serve(port: u16, token: Option<String>, tx: mpsc::Sender<Request>) -> Result<SocketAddr, String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("contacts API: cannot listen on 127.0.0.1:{port} (MURMUR_CONTACTS_PORT): {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let app = router(token, tx);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("contacts API: {e}");
        }
    });
    Ok(addr)
}

fn error(status: StatusCode, why: impl Into<String>) -> Response {
    (status, axum::Json(json!({ "error": why.into() }))).into_response()
}

fn refused(api: &Api, headers: &HeaderMap) -> Option<Response> {
    token_check(api.token.as_deref(), headers).err().map(|why| error(StatusCode::UNAUTHORIZED, why))
}

async fn ask(api: &Api, ask: Ask) -> Response {
    let (reply, rx) = oneshot::channel();
    if api.tx.send(Request { ask, reply }).await.is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "the contacts process is stopping");
    }
    match tokio::time::timeout(ANSWER_WAIT, rx).await {
        Ok(Ok(r)) => {
            let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, axum::Json(r.body)).into_response()
        }
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "the contacts process did not answer"),
    }
}

async fn post_order(State(api): State<Api>, request: HttpRequest) -> Response {
    if let Some(r) = refused(&api, request.headers()) {
        return r;
    }
    let bytes = match axum::body::to_bytes(request.into_body(), BODY_LIMIT).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::PAYLOAD_TOO_LARGE, format!("order: {e}")),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({ "refused": format!("not JSON: {e}"), "code": "unreadable" }))).into_response();
        }
    };
    ask(&api, Ask::Order(body)).await
}

async fn get_orders(State(api): State<Api>, headers: HeaderMap) -> Response {
    if let Some(r) = refused(&api, &headers) {
        return r;
    }
    ask(&api, Ask::Orders).await
}

async fn get_order(State(api): State<Api>, headers: HeaderMap, UrlPath(id): UrlPath<String>) -> Response {
    if let Some(r) = refused(&api, &headers) {
        return r;
    }
    match id.parse::<u64>() {
        Ok(id) => ask(&api, Ask::OrderState(id)).await,
        Err(_) => error(StatusCode::NOT_FOUND, format!("no order {id:?}")),
    }
}

async fn get_nodes(State(api): State<Api>, headers: HeaderMap) -> Response {
    if let Some(r) = refused(&api, &headers) {
        return r;
    }
    ask(&api, Ask::Nodes).await
}

async fn nowhere(State(api): State<Api>, headers: HeaderMap) -> Response {
    if let Some(r) = refused(&api, &headers) {
        return r;
    }
    error(StatusCode::NOT_FOUND, "routes: POST /orders, GET /orders, GET /orders/{id}, GET /nodes")
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request as Req;
    use tower::ServiceExt;

    use super::*;
    use crate::motion::offset;
    use crate::weapon::Armory;

    /// 2026-06-14 19:00Z, local noon at the Gate (as node.rs's tests).
    const T0: f64 = 1_781_463_600.0;
    const AT: [f64; 2] = [-122.4950, 37.8120];
    const SEED: u64 = 17;
    const TOKEN: &str = "contacts-test-token";

    /// An armed fast boat holding at AT, a fishing boat 3 km east, a recorded contact, and `orders`.
    fn toml(orders: &str) -> String {
        format!(
            r#"title = "t"
seed = {SEED}
[[contact]]
id = "raider"
affiliation = "hostile"
domain = "surface"
kind = "armed_fast_boat"
motion = "station"
at = [-122.4950, 37.8120]
[[contact]]
id = "fisher"
affiliation = "neutral"
domain = "surface"
kind = "fishing_vessel"
speed_mps = 3
motion = "station"
at = [-122.4610, 37.8120]
[[contact]]
id = "ghost"
affiliation = "neutral"
domain = "surface"
kind = "fishing_vessel"
motion = "recorded"
max_gap_s = 1000
samples = [{{ at_s = 0, position = [-122.50, 37.80] }}, {{ at_s = 600, position = [-122.49, 37.80] }}]
{orders}"#
        )
    }

    /// Our vehicle 1, still on the water `east` metres east of AT.
    fn ours(east: f64) -> OursAt {
        let (lat, lon) = offset(AT[1], AT[0], east, 0.0);
        OursAt { uid: 1, lat, lon, alt_m: 0.0, ve: 0.0, vn: 0.0, platform: Some("seahunter".into()), domain: Some("surface".into()), armed: false }
    }

    /// The contacts process's loop without the bridge or a socket.
    struct Loop {
        scenario: Scenario,
        movers: Vec<Mover>,
        nodes: Nodes,
        armory: Armory,
        desk: Desk,
        s: f64,
        ours: Vec<OursAt>,
    }

    impl Loop {
        fn new(orders: &str, fleet: Option<BTreeSet<u64>>) -> Loop {
            let scenario = Scenario::from_toml(&toml(orders)).expect("scenario");
            let movers: Vec<Mover> =
                scenario.contacts().expect("contacts").into_iter().enumerate().map(|(i, c)| Mover::new(c, SEED, i as u64)).collect();
            let nodes = Nodes::new(&scenario, &movers);
            let desk = Desk::new(&nodes, fleet, false);
            Loop { scenario, movers, nodes, armory: Armory::default(), desk, s: 0.0, ours: vec![ours(1500.0)] }
        }

        fn ask(&mut self, ask: Ask) -> Reply {
            let mut here = Here { scenario: &self.scenario, nodes: &mut self.nodes, movers: &self.movers, ours: &self.ours, s: self.s, t: T0 + self.s };
            self.desk.answer(ask, &mut here)
        }

        fn post(&mut self, body: Value) -> Reply {
            self.ask(Ask::Order(body))
        }

        /// One second: the movers step, the nodes tick; their events, recorded on the desk.
        fn tick(&mut self) -> Vec<NodeEvent> {
            self.s += 1.0;
            for m in &mut self.movers {
                m.step(self.s, 1.0, None);
            }
            let tick = self.nodes.tick(self.s, T0 + self.s, &mut self.movers, &self.ours, &mut self.armory);
            self.desk.record(&tick.events);
            tick.events
        }

        fn run(&mut self, secs: u32) -> Vec<NodeEvent> {
            (0..secs).flat_map(|_| self.tick()).collect()
        }
    }

    const ATTACK: &str = r#"{"node": "raider", "do": "attack", "target": {"vehicle": 1}, "weapon": "ATGM-class missile", "stay": true, "label": "attacks HUNTER 1"}"#;

    /// The same order by the file and by the API, taken at the same second, makes the same
    /// node events: posted early for its second, or posted now just before it.
    #[test]
    fn an_order_by_the_api_runs_as_the_same_order_in_the_file() {
        let file = r#"[[order]]
at_s = 5
node = "raider"
do = "attack"
target = { vehicle = 1 }
weapon = "ATGM-class missile"
stay = true
label = "attacks HUNTER 1"
"#;
        let mut by_file = Loop::new(file, None);
        let want = by_file.run(120);
        assert!(want.iter().any(|e| e.what == "fired"), "the file's order fires: {want:?}");

        let mut early = Loop::new("", None);
        let mut body: Value = serde_json::from_str(ATTACK).expect("json");
        body["at_s"] = json!(5.0);
        assert_eq!(early.post(body), Reply::ok(json!({ "id": 1, "accepted": true })));
        assert_eq!(early.run(120), want);

        let mut now = Loop::new("", None);
        let mut got = now.run(4);
        assert_eq!(now.post(serde_json::from_str(ATTACK).expect("json")).status, 200);
        got.extend(now.run(116));
        assert_eq!(got, want);
        // and its record says what it did
        let st = now.ask(Ask::OrderState(1)).body;
        assert_eq!((st["source"].as_str(), st["do"].as_str(), st["taken_s"].as_f64()), (Some("api"), Some("attack"), Some(5.0)), "{st}");
        assert!(st["shots"].as_u64().is_some_and(|n| n >= 1), "{st}");
    }

    /// Each refusal the API names, with its code; nothing is queued.
    #[test]
    fn post_orders_refuses_each_case_saying_why() {
        let fleet: BTreeSet<u64> = [1, 2].into();
        let mut l = Loop::new("", Some(fleet));
        let cases = [
            (r#"{"node": "nobody", "do": "hold"}"#, "unknown_node"),
            (r#"{"node": "ghost", "do": "hold"}"#, "not_a_node"),
            (r#"{"node": "raider", "do": "hold", "radius_m": 300}"#, "keys"),
            (r#"{"node": "raider", "do": "hold", "fuel": 3}"#, "keys"),
            (r#"{"node": "raider", "do": "attack", "target": {"contact": "nobody"}}"#, "unknown_target"),
            (r#"{"node": "raider", "do": "attack", "target": {"vehicle": 9}}"#, "unknown_target"),
            (r#"{"node": "raider", "do": "attack", "target": {"vehicle": 1}, "weapon": "Hellfire"}"#, "weapon_not_carried"),
            (r#"{"node": "fisher", "do": "attack", "target": {"vehicle": 1}}"#, "weapon_not_carried"),
            (r#"{"node": "raider", "do": "orbit", "center": [-122.49, 37.81]}"#, "invalid"),
            (r#"{"node": "raider", "do": "explode"}"#, "unreadable"),
        ];
        for (body, code) in cases {
            let r = l.post(serde_json::from_str(body).expect("json"));
            assert_eq!((r.status, r.body["code"].as_str()), (422, Some(code)), "{body}: {}", r.body);
            assert!(r.body["refused"].as_str().is_some_and(|w| !w.is_empty()), "{body}: {}", r.body);
        }
        // a second gone by
        l.run(10);
        let r = l.post(json!({ "node": "raider", "do": "hold", "at_s": 3 }));
        assert_eq!(r.body["code"], "past", "{}", r.body);
        // under the legacy sensing model, no attack
        l.desk.legacy = true;
        let r = l.post(serde_json::from_str(ATTACK).expect("json"));
        assert_eq!(r.body["code"], "legacy_sensing", "{}", r.body);
        l.desk.legacy = false;
        assert_eq!(l.nodes.pending().count(), 0, "nothing refused is queued");
        // a node destroyed
        l.movers[0].out_of_play = true;
        l.movers[0].st.exists = false;
        let r = l.post(serde_json::from_str(ATTACK).expect("json"));
        assert_eq!(r.body["code"], "node_gone", "{}", r.body);
        // and one accepted, a neutral's hold
        let r = l.post(json!({ "node": "fisher", "do": "hold" }));
        assert_eq!(r, Reply::ok(json!({ "id": 1, "accepted": true })));
    }

    /// A node's own state and what it perceives: our vehicle within its reach as the bridge
    /// reported it, the other contact; an order's record from pending to ended.
    #[test]
    fn nodes_and_orders_say_what_each_node_does_and_perceives() {
        let mut l = Loop::new("", Some([1].into()));
        l.run(2);
        let v = l.ask(Ask::Nodes).body;
        let ids: Vec<&str> = v["nodes"].as_array().expect("nodes").iter().filter_map(|n| n["id"].as_str()).collect();
        assert_eq!(ids, ["raider", "fisher"], "a recorded contact takes no order");
        let raider = &v["nodes"][0];
        assert_eq!(raider["damage"], "undamaged");
        assert_eq!(raider["loadout"][0]["rounds_left"], raider["loadout"][0]["count"]);
        assert_eq!(raider["perceives"]["ours"][0]["uid"], 1);
        assert!(raider["perceives"]["ours"][0]["range_m"].as_f64().is_some_and(|r| (r - 1500.0).abs() < 5.0));
        assert_eq!(raider["perceives"]["contacts"][0]["id"], "fisher");
        assert!(raider["perceives"]["ours"][0].get("platform").is_none(), "what our vehicle is, not perceived");
        // a transit for the fishing boat: pending, running, ended done
        let id = l.post(json!({ "node": "fisher", "do": "transit", "route": [[-122.4500, 37.8120]] })).body["id"].as_u64().expect("id");
        assert_eq!(l.ask(Ask::OrderState(id)).body["state"], "pending");
        l.run(1);
        assert_eq!(l.ask(Ask::OrderState(id)).body["state"], "running");
        let fisher = &l.ask(Ask::Nodes).body["nodes"][1];
        assert_eq!((fisher["order"]["id"].as_u64(), fisher["order"]["do"].as_str()), (Some(id), Some("transit")));
        l.run(600);
        let st = l.ask(Ask::OrderState(id)).body;
        assert_eq!((st["state"].as_str(), st["why"].as_str()), (Some("ended"), Some("done")), "{st}");
        assert_eq!(l.ask(Ask::OrderState(99)).status, 404);
        assert_eq!(l.ask(Ask::Orders).body["orders"].as_array().map(Vec::len), Some(1));
    }

    /// A routes' answer, the loop answering from `l` on its own task.
    async fn call(token: Option<&str>, method: &str, path: &str, body: Option<&str>, configured: Option<&str>) -> (StatusCode, Value) {
        let (tx, mut rx) = mpsc::channel::<Request>(4);
        let server = tokio::spawn(async move {
            let mut l = Loop::new("", None);
            while let Some(req) = rx.recv().await {
                let r = l.ask(req.ask);
                let _ = req.reply.send(r);
            }
        });
        let mut b = Req::builder().method(method).uri(path).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(TOKEN_HEADER, t);
        }
        let req = b.body(body.map_or_else(Body::empty, |s| Body::from(s.to_string()))).expect("request");
        let res = router(configured.map(str::to_string), tx).oneshot(req).await.expect("response");
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1 << 20).await.expect("body");
        server.abort();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// No token, a wrong one, or none configured: every route refuses before reading anything.
    #[tokio::test]
    async fn every_request_needs_the_stacks_token() {
        let hold = Some(r#"{"node": "fisher", "do": "hold"}"#);
        for (method, path, body) in [("POST", "/orders", hold), ("GET", "/orders", None), ("GET", "/orders/1", None), ("GET", "/nodes", None), ("GET", "/elsewhere", None)] {
            for token in [None, Some("guess"), Some("contacts-test-tokeN")] {
                let (st, v) = call(token, method, path, body, Some(TOKEN)).await;
                assert_eq!(st, StatusCode::UNAUTHORIZED, "{method} {path} with {token:?}: {v}");
            }
            let (st, _) = call(Some(TOKEN), method, path, body, None).await;
            assert_eq!(st, StatusCode::UNAUTHORIZED, "{method} {path}: the API is closed without a token");
        }
        let (st, v) = call(Some(TOKEN), "POST", "/orders", hold, Some(TOKEN)).await;
        assert_eq!((st, v), (StatusCode::OK, json!({ "id": 1, "accepted": true })));
        let (st, v) = call(Some(TOKEN), "GET", "/nodes", None, Some(TOKEN)).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let (st, v) = call(Some(TOKEN), "POST", "/orders", Some("{not json"), Some(TOKEN)).await;
        assert_eq!((st, v["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("unreadable")));
    }
}
