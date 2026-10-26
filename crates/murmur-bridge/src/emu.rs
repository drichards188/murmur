//! The link emulator's view of each vehicle's link (murmur-linkemu, Phase D), for the console.
//!
//! With `MURMUR_LINKEMU_URL` set (scripts/up.sh sets it when it runs the emulator), the bridge
//! polls the emulator's `GET /stats` every `POLL_S` seconds and keeps a minute of history per
//! link, so it can say what the emulator is doing to a link now (loss and added delay over the
//! last seconds, by cause), not only since start. It serves
//!
//! - `GET /api/links`: `{enabled, url, ok, age_s, error, summary, links: {uid: view}}`
//! - `GET /api/links/{uid}`: one link's view (404 if the emulator has no such link)
//!
//! and sets each vehicle's `link.emu` in the state push (`{state: ok | degraded | down,
//! cause}`), which the fleet row shows. So the console talks to the bridge only. Without the
//! variable nothing is polled and `/api/links` answers `enabled: false`.
//!
//! A view: the emulator's per-link record (profile, `nominal` numbers, `blocked` cause,
//! active scripted `events`, `depth_state`, terrain `los`, `satcom`) plus `recent` (each
//! direction's packets, drops, loss and drops by cause, mean added one-way latency, over the
//! shortest of 10, 30 or 60 s that saw at least `MIN_PKTS` uplink packets, so a slow acoustic
//! link still gets a figure), `total` (the same since the emulator started), and
//! `emu_rtt_ms`, the emulator's added delay up plus down: the round trip the link model
//! imposes, available even when a TIMESYNC answer never comes back.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::clock;
use crate::vehicle::Uid;
use crate::world::World;

const POLL_S: f64 = 2.0;
const HISTORY_S: f64 = 62.0;
const WINDOWS_S: [f64; 3] = [10.0, 30.0, 60.0];
/// Uplink packets a window needs before it is used for the recent figures.
const MIN_PKTS: u64 = 5;
/// Recent loss (both directions) at or above this marks a link degraded in the fleet row (the
/// coverage predictor's threshold too: a predicted and a measured link read alike).
pub const DEGRADED_LOSS: f64 = murmur_linkemu::coverage::DEGRADED_LOSS;
/// Recent loss at or above this counts as down even when the emulator does not block the
/// link outright (the predictor's too).
pub const DOWN_LOSS: f64 = murmur_linkemu::coverage::DOWN_LOSS;
/// No answer from the emulator for this long: the briefs are cleared.
const STALE_S: f64 = 10.0;
const TIMEOUT: Duration = Duration::from_secs(2);

/// The fleet row's view of the emulator on one link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EmuBrief {
    /// ok | degraded | down
    pub state: &'static str,
    /// Why: the blocking cause (terrain, no_sat, gate, cut, handover, ...), else the
    /// dominant recent drop cause (loss, queue, jam, event_loss, range, ...), else `event`
    /// while a scripted event acts on the link without (yet) costing much.
    pub cause: Option<String>,
}

/// One direction's counters at one poll.
#[derive(Debug, Clone, Default)]
struct DirCount {
    in_pkts: u64,
    dropped: BTreeMap<String, u64>,
    lat_n: u64,
    lat_sum_ms: f64,
}

impl DirCount {
    fn from_json(v: &Value) -> DirCount {
        let n = v["latency_n"].as_u64().unwrap_or(0);
        DirCount {
            in_pkts: v["in_pkts"].as_u64().unwrap_or(0),
            dropped: v["dropped"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, c)| c.as_u64().map(|c| (k.clone(), c)))
                        .collect()
                })
                .unwrap_or_default(),
            lat_n: n,
            lat_sum_ms: v["latency_ms_mean"].as_f64().unwrap_or(0.0) * n as f64,
        }
    }
}

type Sample = (f64, [DirCount; 2]);

fn round_to(x: f64, n: i32) -> f64 {
    let f = 10f64.powi(n);
    (x * f).round() / f
}

/// What happened on one direction between two polls.
fn dir_view(new: &DirCount, old: &DirCount) -> Value {
    let pkts = new.in_pkts.saturating_sub(old.in_pkts);
    let mut by = Map::new();
    let mut dropped = 0;
    for (k, &n) in &new.dropped {
        let d = n.saturating_sub(old.dropped.get(k).copied().unwrap_or(0));
        if d > 0 {
            by.insert(k.clone(), d.into());
            dropped += d;
        }
    }
    let ln = new.lat_n.saturating_sub(old.lat_n);
    json!({
        "pkts": pkts,
        "dropped": dropped,
        "loss": if pkts > 0 { json!(round_to(dropped as f64 / pkts as f64, 3)) } else { Value::Null },
        "by_cause": by,
        "latency_ms": if ln > 0 {
            json!(round_to((new.lat_sum_ms - old.lat_sum_ms) / ln as f64, 1))
        } else {
            Value::Null
        },
    })
}

/// The brief for the fleet row, from the emulator's record and the recent figures.
fn brief(raw: &Value, recent: &Value) -> EmuBrief {
    if let Some(b) = raw["blocked"].as_str() {
        return EmuBrief {
            state: "down",
            cause: Some(b.to_string()),
        };
    }
    let (mut pkts, mut dropped) = (0u64, 0u64);
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for d in ["up", "down"] {
        pkts += recent[d]["pkts"].as_u64().unwrap_or(0);
        dropped += recent[d]["dropped"].as_u64().unwrap_or(0);
        for (k, n) in recent[d]["by_cause"].as_object().into_iter().flatten() {
            *by.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
        }
    }
    let events = raw["events"].as_array().is_some_and(|e| !e.is_empty());
    let loss = if pkts > 0 {
        dropped as f64 / pkts as f64
    } else {
        0.0
    };
    if loss >= DEGRADED_LOSS {
        let top = by.into_iter().max_by_key(|(_, n)| *n).map(|(k, _)| k);
        return EmuBrief {
            // not blocked but nothing gets through (a jammer, terrain in ramp mode)
            state: if loss >= DOWN_LOSS { "down" } else { "degraded" },
            cause: top,
        };
    }
    if events {
        return EmuBrief {
            state: "degraded",
            cause: Some("event".into()),
        };
    }
    EmuBrief {
        state: "ok",
        cause: None,
    }
}

/// The figures the bridge's timing and telemetry plan use, from one link's view.
fn figures(v: &Value) -> crate::link::EmuLink {
    let s = |k: &str| v[k].as_str().map(str::to_string);
    let up = &v["recent"]["up"];
    crate::link::EmuLink {
        profile: s("profile"),
        active_profile: s("active_profile").or_else(|| s("profile")),
        bandwidth_bps: v["nominal"]["bandwidth_bps"]
            .as_f64()
            .map(|b| b.round() as u32),
        rtt_ms: v["emu_rtt_ms"].as_f64(),
        up_pkts: up["pkts"].as_u64().unwrap_or(0),
        up_queue_drops: up["by_cause"]["queue"].as_u64().unwrap_or(0),
    }
}

/// One link's view (see the module docs) from its latest record and its history.
fn view(raw: &Value, hist: &VecDeque<Sample>) -> (Value, EmuBrief) {
    let Some(now) = hist.back() else {
        return (raw.clone(), brief(raw, &Value::Null));
    };
    let mut old = hist.front().unwrap_or(now);
    for w in WINDOWS_S {
        let Some(cand) = hist.iter().find(|s| s.0 >= now.0 - w - 0.5) else {
            continue;
        };
        old = cand;
        if now.1[0].in_pkts.saturating_sub(cand.1[0].in_pkts) >= MIN_PKTS {
            break;
        }
    }
    let recent = json!({
        "window_s": round_to(now.0 - old.0, 1),
        "up": dir_view(&now.1[0], &old.1[0]),
        "down": dir_view(&now.1[1], &old.1[1]),
    });
    let zero = DirCount::default();
    let total = json!({
        "up": dir_view(&now.1[0], &zero),
        "down": dir_view(&now.1[1], &zero),
    });
    let rtt = match (
        recent["up"]["latency_ms"].as_f64(),
        recent["down"]["latency_ms"].as_f64(),
    ) {
        (Some(u), Some(d)) => json!(round_to(u + d, 0)),
        _ => Value::Null,
    };
    let b = brief(raw, &recent);
    let mut v = Map::new();
    for k in [
        "uid",
        "class",
        "profile",
        "active_profile",
        "platform",
        "domain",
        "blocked",
        "events",
        "depth_state",
        "los",
        "satcom",
        "nominal",
        "pos",
    ] {
        v.insert(k.into(), raw[k].clone());
    }
    v.insert("state".into(), b.state.into());
    v.insert("cause".into(), json!(b.cause));
    v.insert("recent".into(), recent);
    v.insert("total".into(), total);
    v.insert("emu_rtt_ms".into(), rtt);
    (Value::Object(v), b)
}

#[derive(Default)]
struct Inner {
    hist: HashMap<Uid, VecDeque<Sample>>,
    views: BTreeMap<Uid, Value>,
    summary: Value,
    last_ok: Option<f64>,
    error: Option<String>,
}

pub struct Emu {
    url: Option<String>,
    inner: RwLock<Inner>,
}

impl Emu {
    pub fn from_env() -> Arc<Emu> {
        let url = std::env::var("MURMUR_LINKEMU_URL")
            .ok()
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty());
        Arc::new(Emu {
            url,
            inner: RwLock::default(),
        })
    }

    pub fn enabled(&self) -> bool {
        self.url.is_some()
    }

    fn read<R>(&self, f: impl FnOnce(&Inner) -> R) -> R {
        f(&self.inner.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn write<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.write().unwrap_or_else(|e| e.into_inner()))
    }

    /// Takes one `/stats` answer: history, views, and the briefs to hand to the vehicles.
    pub(crate) fn ingest(&self, stats: &Value, now: f64) -> HashMap<Uid, (EmuBrief, crate::link::EmuLink)> {
        let mut briefs = HashMap::new();
        self.write(|s| {
            let mut summary = stats.clone();
            if let Some(o) = summary.as_object_mut() {
                o.remove("per_link");
            }
            s.summary = summary;
            s.last_ok = Some(now);
            s.error = None;
            for raw in stats["per_link"].as_array().into_iter().flatten() {
                let Some(uid) = raw["uid"].as_u64() else {
                    continue;
                };
                let h = s.hist.entry(uid).or_default();
                h.push_back((
                    now,
                    [
                        DirCount::from_json(&raw["up"]),
                        DirCount::from_json(&raw["down"]),
                    ],
                ));
                while h.front().is_some_and(|f| f.0 < now - HISTORY_S) {
                    h.pop_front();
                }
                let (v, b) = view(raw, h);
                briefs.insert(uid, (b, figures(&v)));
                s.views.insert(uid, v);
            }
        });
        briefs
    }

    /// Polls the emulator forever (only spawned when enabled).
    pub async fn run(self: Arc<Self>, world: Arc<World>) {
        let Some(url) = self.url.clone() else { return };
        let addr = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or("")
            .to_string();
        let mut interval = tokio::time::interval(Duration::from_secs_f64(POLL_S));
        let mut warned = false;
        loop {
            interval.tick().await;
            let now = clock::mono();
            match get_json(&addr, "/stats").await {
                Ok(stats) => {
                    let briefs = self.ingest(&stats, now);
                    for cell in world.vehicles() {
                        let (b, f) = briefs.get(&cell.uid).cloned().unzip();
                        cell.set_emu(b, f);
                    }
                    warned = false;
                }
                Err(e) => {
                    let stale = self.write(|s| {
                        s.error = Some(e.clone());
                        s.last_ok.is_none_or(|t| now - t > STALE_S)
                    });
                    if stale {
                        for cell in world.vehicles() {
                            cell.set_emu(None, None);
                        }
                    }
                    if !warned {
                        eprintln!("link emulator {url}: {e}");
                        warned = true;
                    }
                }
            }
        }
    }

    fn header(&self) -> Map<String, Value> {
        let now = clock::mono();
        self.read(|s| {
            let mut m = Map::new();
            m.insert("enabled".into(), self.enabled().into());
            m.insert("url".into(), json!(self.url));
            m.insert(
                "ok".into(),
                s.last_ok.is_some_and(|t| now - t <= STALE_S).into(),
            );
            m.insert(
                "age_s".into(),
                json!(s.last_ok.map(|t| round_to(now - t, 1))),
            );
            m.insert("error".into(), json!(s.error));
            m
        })
    }
}

pub fn router(emu: Arc<Emu>) -> Router {
    Router::new()
        .route("/api/links", get(all))
        .route("/api/links/{uid}", get(one))
        .with_state(emu)
}

async fn all(State(emu): State<Arc<Emu>>) -> Response {
    let mut m = emu.header();
    emu.read(|s| {
        m.insert("summary".into(), s.summary.clone());
        m.insert(
            "links".into(),
            Value::Object(
                s.views
                    .iter()
                    .map(|(u, v)| (u.to_string(), v.clone()))
                    .collect(),
            ),
        );
    });
    Json(Value::Object(m)).into_response()
}

async fn one(State(emu): State<Arc<Emu>>, Path(uid): Path<Uid>) -> Response {
    let mut m = emu.header();
    let Some(v) = emu.read(|s| s.views.get(&uid).cloned()) else {
        m.insert(
            "detail".into(),
            json!(if emu.enabled() {
                "the link emulator has no link for this uid"
            } else {
                "no link emulator (MURMUR_LINKEMU_URL unset)"
            }),
        );
        return (StatusCode::NOT_FOUND, Json(Value::Object(m))).into_response();
    };
    m.insert("link".into(), v);
    Json(Value::Object(m)).into_response()
}

/// One GET with `Connection: close`, read to the end (the emulator is local and answers
/// with a Content-Length body).
async fn get_json(addr: &str, path: &str) -> Result<Value, String> {
    let work = async {
        let mut s = TcpStream::connect(addr)
            .await
            .map_err(|e| format!("connect {addr}: {e}"))?;
        let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        s.read_to_end(&mut raw).await.map_err(|e| e.to_string())?;
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("incomplete HTTP response")?;
        let status = String::from_utf8_lossy(&raw[..split])
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        if status != 200 {
            return Err(format!("status {status}"));
        }
        serde_json::from_slice(&raw[split + 4..]).map_err(|e| format!("bad JSON: {e}"))
    };
    tokio::time::timeout(TIMEOUT, work)
        .await
        .map_err(|_| "timeout".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(uid: u64, up_in: u64, up_drop: &[(&str, u64)], lat_n: u64, lat: f64) -> Value {
        let dropped: Map<String, Value> = up_drop
            .iter()
            .map(|(k, n)| ((*k).to_string(), json!(n)))
            .collect();
        json!({
            "uid": uid, "class": "los", "profile": "los", "blocked": null, "events": [],
            "nominal": {"loss": 0.005, "latency_ms": 15.0},
            "up": {"in_pkts": up_in, "dropped": dropped, "latency_n": lat_n, "latency_ms_mean": lat},
            "down": {"in_pkts": up_in / 2, "dropped": {}, "latency_n": lat_n / 2, "latency_ms_mean": lat},
        })
    }

    #[test]
    fn recent_figures_are_differences_between_polls() {
        let emu = Emu {
            url: None,
            inner: RwLock::default(),
        };
        emu.ingest(&json!({"per_link": [rec(3, 100, &[("loss", 1)], 99, 15.0)]}), 0.0);
        // 100 more packets, 30 lost to a scripted event: degraded, cause event_loss
        let b = emu.ingest(
            &json!({"per_link": [rec(3, 200, &[("loss", 1), ("event_loss", 30)], 169, 15.0)]}),
            2.0,
        );
        assert_eq!(
            b[&3].0,
            EmuBrief {
                state: "degraded",
                cause: Some("event_loss".into())
            }
        );
        let v = emu.read(|s| s.views[&3].clone());
        assert_eq!(v["recent"]["up"]["pkts"], 100);
        assert_eq!(v["recent"]["up"]["loss"], 0.3);
        assert_eq!(v["recent"]["up"]["by_cause"]["event_loss"], 30);
        assert_eq!(v["recent"]["up"]["latency_ms"], 15.0);
        assert_eq!(v["total"]["up"]["dropped"], 31);
        assert_eq!(v["emu_rtt_ms"], 30.0);
        let f = &b[&3].1;
        assert_eq!(
            (f.rtt_ms, f.up_pkts, f.up_queue_drops),
            (Some(30.0), 100, 0)
        );
        assert_eq!(f.active_profile.as_deref(), Some("los")); // no gate: the profile itself
        // the old samples age out of the history
        emu.ingest(&json!({"per_link": [rec(3, 300, &[("loss", 1), ("event_loss", 30)], 269, 15.0)]}), 100.0);
        assert_eq!(emu.read(|s| s.hist[&3].len()), 1);
    }

    #[test]
    fn blocked_is_down_and_events_alone_are_degraded() {
        let mut r = rec(1, 10, &[], 10, 15.0);
        r["blocked"] = json!("terrain");
        assert_eq!(brief(&r, &Value::Null).state, "down");
        assert_eq!(brief(&r, &Value::Null).cause.as_deref(), Some("terrain"));
        r["blocked"] = Value::Null;
        r["events"] = json!(["jammer"]);
        assert_eq!(
            brief(&r, &Value::Null),
            EmuBrief {
                state: "degraded",
                cause: Some("event".into())
            }
        );
        r["events"] = json!([]);
        assert_eq!(brief(&r, &Value::Null).state, "ok");
    }

    #[test]
    fn slow_link_widens_its_window() {
        let mut h = VecDeque::new();
        // two uplink packets per 10 s: the 10 s window has too few, the 30 s one enough
        for i in 0..=30 {
            let t = f64::from(i) * 2.0;
            let c = DirCount {
                in_pkts: (i / 5) as u64 * 2,
                ..DirCount::default()
            };
            h.push_back((t, [c.clone(), c]));
        }
        let (v, _) = view(&rec(9, 0, &[], 0, 0.0), &h);
        assert_eq!(v["recent"]["window_s"], 30.0);
    }
}
