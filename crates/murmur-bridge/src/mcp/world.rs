//! The world as agents read it: the facts the rules read (the decision layer's
//! `WorldSnapshot`, each fact with its observation time) plus what the console shows, every
//! value with `as_of`, `age_s` and, for vehicle facts, `stale` against the freshness limit
//! the rules apply to that vehicle's link.

use std::collections::HashMap;

use axum::http::Method;
use murmur_decide::{AgentId, Fact, Time, VehicleFacts};
use serde::Serialize;
use serde_json::{Value, json};

use super::Mcp;
use super::tools::STALE_NOTE;
use crate::clock::{self, round};
use crate::decision::snapshot::duid;
use crate::decision::DecisionError;
use crate::tasks::{OpSnapshot, caps};
use crate::vehicle::{Uid, VehicleSnapshot};

fn age(now_s: f64, t_s: f64) -> f64 {
    round((now_s - t_s).max(0.0), 1)
}

/// `{value, as_of, age_s, stale}` for one decision fact; unknown facts say so and count as
/// stale (the rules treat "not known" as a reason to fail).
fn fact<T: Serialize>(f: Option<&Fact<T>>, t: Time, limit_ms: i64) -> Value {
    match f {
        None => json!({
            "value": null, "as_of": null, "age_s": null, "stale": true,
            "note": "not known: the rules fail closed on it",
        }),
        Some(f) => {
            let age_ms = (t.0 - f.t.0).max(0);
            json!({
                "value": f.value,
                "as_of": f.t.0 as f64 / 1000.0,
                "age_s": round(age_ms as f64 / 1000.0, 1),
                "stale": age_ms > limit_ms,
            })
        }
    }
}

/// Merge `extra` fields into a fact object.
fn with(mut v: Value, extra: Value) -> Value {
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, val) in e {
            o.insert(k.clone(), val.clone());
        }
    }
    v
}

#[allow(clippy::too_many_arguments)]
fn vehicle_json(
    mcp: &Mcp,
    v: &VehicleSnapshot,
    facts: Option<&VehicleFacts>,
    t: Time,
    now_wall: f64,
    op: Option<&OpSnapshot>,
) -> Value {
    let rules = mcp.decision.rules();
    let (limit_ms, _) = rules.fact_age_limit_ms(Some(v.link.expected_interval_ms));
    let identity = v.identity.as_deref();
    let id = |k: &str| identity.and_then(|i| i.0.get(k)).cloned().unwrap_or(Value::Null);
    let platform = facts.and_then(|f| f.platform.as_ref());
    let tasks: Vec<&str> = v
        .kind
        .and_then(caps)
        .map(|c| c.tasks.iter().map(|t| t.name()).collect())
        .unwrap_or_default();
    let position = facts.and_then(|f| f.position.as_ref());
    // the link's last packet; a link that counts no packets (test doubles) falls back to
    // the heartbeat the autopilot fact rests on
    let last_rx_t = v.link.last_rx_t.or_else(|| {
        facts
            .and_then(|f| f.autopilot.as_ref())
            .map(|f| f.t.0 as f64 / 1000.0)
    });
    let link_age = last_rx_t.map(|last| age(now_wall, last));
    let lost_after = v.link.stale_after_s();
    json!({
        "uid": v.uid,
        "sysid": v.sysid,
        "callsign": id("callsign"),
        "platform": {
            "id": id("platform"),
            "name": id("name"),
            "short": id("short"),
            "role": id("role"),
            "domain": id("domain"),
            "link": id("link"),
            "armed": platform.map(|p| p.armed),
            "cruise_mps": platform.and_then(|p| p.cruise_mps),
            "endurance_h": platform.and_then(|p| p.endurance_h),
            "min_turn_radius_m": platform.and_then(|p| p.min_turn_radius_m),
            "cruise_alt_m": id("cruise_alt_m"),
            "standin": matches!(id("sim").as_str(), Some("standin")),
            "tasks": tasks,
        },
        "autopilot": with(
            fact(facts.and_then(|f| f.autopilot.as_ref()), t, limit_ms),
            json!({ "mode": v.mode, "mav_armed": v.armed }),
        ),
        "position": with(
            fact(position, t, limit_ms),
            json!({
                "value": null,
                "lon": position.map(|p| p.value.lon),
                "lat": position.map(|p| p.value.lat),
                "alt_msl_m": v.alt_msl,
                "alt_rel_m": v.alt_rel,
                "heading_deg": v.heading,
                "groundspeed_mps": v.groundspeed,
                "airspeed_mps": v.airspeed,
            }),
        ),
        "nav": with(
            fact(facts.and_then(|f| f.nav_ready.as_ref()), t, limit_ms),
            json!({
                "value": null,
                "ready": facts.and_then(|f| f.nav_ready.as_ref()).map(|f| f.value),
                "source": facts.and_then(|f| f.nav_source.as_ref()).map(|f| f.value.as_str()),
                "gps_fix": v.gps_fix,
                "satellites": v.satellites,
                // the navigation filter flags its GPS as glitching (a jammed or spoofed fix
                // looks like this): the same fact an engagement's release reads, with its age
                "gps_glitch": fact(facts.and_then(|f| f.gps_glitch.as_ref()), t, limit_ms),
            }),
        ),
        "battery": with(
            fact(facts.and_then(|f| f.battery_pct.as_ref()), t, limit_ms),
            json!({ "value": null, "pct": facts.and_then(|f| f.battery_pct.as_ref()).map(|f| f.value) }),
        ),
        "home": with(
            fact(facts.and_then(|f| f.home.as_ref()), t, limit_ms),
            json!({
                "value": null,
                "lon": facts.and_then(|f| f.home.as_ref()).map(|f| f.value.lon),
                "lat": facts.and_then(|f| f.home.as_ref()).map(|f| f.value.lat),
            }),
        ),
        "link": {
            "last_rx_t": last_rx_t,
            "age_s": link_age,
            "lost": link_age.is_none_or(|a| a > lost_after),
            "lost_after_s": lost_after,
            "expected_interval_ms": v.link.expected_interval_ms,
            "loss_pct": v.link.loss_pct,
            "rtt_ms": v.link.rtt_ms,
        },
        "freshness_limit_s": limit_ms as f64 / 1000.0,
        "statustext": v.statustext.iter().map(|s| json!({
            "t": s.t, "age_s": age(now_wall, s.t), "severity": s.severity, "text": s.text,
        })).collect::<Vec<_>>(),
        "tracking": v.tracking,
        // while the link is silent: where the console shows the vehicle probably is. A
        // display model, not a fact; the decision core does not read it (`crate::estimate`)
        "estimate": v.estimate.as_ref().map(|e| crate::estimate::agent_view(e, now_wall)),
        "current_op": op.map(|o| json!({
            "intent_id": o.intent_id,
            "task": o.task.name(),
            "state": o.state,
            "detail": o.detail,
            "progress": o.progress,
            "since_t": o.history.last().map(|h| h.t),
        })),
    })
}

fn vehicles(mcp: &Mcp, only: Option<&[u64]>) -> (f64, Vec<Value>) {
    let snap = mcp.decision.world_now();
    let now_wall = clock::wall();
    let now_mono = clock::mono();
    let ops: HashMap<Uid, OpSnapshot> = mcp
        .world
        .tasks
        .current_ops()
        .into_iter()
        .map(|o| (o.uid, o))
        .collect();
    let mut out = Vec::new();
    // silent vehicles carry the console's estimate too (display only, said so in the view);
    // what an estimate reads besides its vehicle is read before any vehicle's lock
    let cells: Vec<_> = mcp
        .world
        .vehicles()
        .into_iter()
        .filter(|c| only.is_none_or(|u| u.contains(&c.uid)))
        .collect();
    let links: Vec<_> = cells.iter().map(|c| c.link_snapshot(now_mono)).collect();
    let estimates = crate::estimate::FrameContext::new(&mcp.world, now_wall, &links);
    for (cell, link) in cells.iter().zip(links) {
        let v = cell.read(|v| {
            let mut s = v.snapshot(cell.uid, cell.sysid, link, now_mono);
            s.estimate = estimates.estimate(cell.uid, v, &s.link);
            s
        });
        let facts = snap.vehicles.get(&duid(cell.uid));
        out.push(vehicle_json(
            mcp,
            &v,
            facts,
            snap.t,
            now_wall,
            ops.get(&cell.uid),
        ));
    }
    (now_wall, out)
}

/// The facts a vehicle's view dates, by key: `stale` in the compact view names these (and
/// `gps_glitch`, the navigation filter's flag).
const DATED: [&str; 5] = ["autopilot", "position", "nav", "battery", "home"];

/// A vehicle as `get_fleet` with `compact` shows it: what changes and what a supervisor
/// picks a vehicle by, taken from the full view so the two never disagree. The platform's
/// limits, home, link figures, status texts and the silent-link estimate are left to
/// `get_vehicle` or `get_fleet` without `compact`.
fn compact_vehicle(v: &Value) -> Value {
    let mut stale: Vec<&str> = DATED
        .into_iter()
        .filter(|k| v[*k]["stale"].as_bool().unwrap_or(true))
        .collect();
    // the GPS glitch flag is a fact of its own, with its own age (an engagement's release
    // reads it): a stale flag says nothing about the fix now
    if v["nav"]["gps_glitch"]["stale"].as_bool().unwrap_or(true) {
        stale.push("gps_glitch");
    }
    let op = &v["current_op"];
    json!({
        "uid": v["uid"],
        "callsign": v["callsign"],
        "platform": v["platform"]["id"],
        "domain": v["platform"]["domain"],
        "armed": v["platform"]["armed"],
        "mode": v["autopilot"]["mode"],
        "lon": v["position"]["lon"],
        "lat": v["position"]["lat"],
        "alt_rel_m": v["position"]["alt_rel_m"],
        "position_age_s": v["position"]["age_s"],
        "nav_ready": v["nav"]["ready"],
        "nav_source": v["nav"]["source"],
        "gps_glitch": v["nav"]["gps_glitch"]["value"],
        "battery_pct": v["battery"]["pct"],
        "link_lost": v["link"]["lost"],
        "link_age_s": v["link"]["age_s"],
        "task": if op.is_null() {
            Value::Null
        } else {
            json!({ "intent_id": op["intent_id"], "task": op["task"], "state": op["state"] })
        },
        "stale": stale,
    })
}

/// `get_fleet`: every vehicle's full view, or with `compact` one short entry each.
pub fn fleet(mcp: &Mcp, only: Option<&[u64]>, compact: bool) -> Value {
    let (t, mut vehicles) = vehicles(mcp, only);
    if compact {
        vehicles = vehicles.iter().map(compact_vehicle).collect();
    }
    let rules = mcp.decision.rules();
    let note = if compact {
        format!(
            "{STALE_NOTE} compact: one entry per vehicle; stale names its facts older than \
             the vehicle's freshness limit (or not known), which the rules refuse; \
             get_vehicle (or get_fleet without compact, with uids) has the full facts and \
             each one's age"
        )
    } else {
        STALE_NOTE.to_string()
    };
    json!({
        "t": t,
        "count": vehicles.len(),
        "compact": compact,
        "freshness": {
            "max_fact_age_s": rules.max_fact_age_s,
            "fact_age_intervals": rules.fact_age_intervals,
            "note": "a vehicle's freshness limit is max_fact_age_s, or fact_age_intervals times its link's expected report interval if larger (an acoustic link reports every 20 s)",
        },
        "note": note,
        "vehicles": vehicles,
    })
}

/// `get_vehicle`.
pub fn vehicle(mcp: &Mcp, uid: u64) -> Option<Value> {
    let (t, mut vehicles) = vehicles(mcp, Some(&[uid]));
    let v = vehicles.pop()?;
    Some(json!({ "t": t, "note": STALE_NOTE, "vehicle": v }))
}

/// `get_platforms`: the catalog, read off the async runtime (it reads a file).
pub async fn platforms(mcp: &Mcp) -> Result<Value, DecisionError> {
    let world = mcp.world.clone();
    let fail = |detail: String| DecisionError {
        status: 500,
        detail,
        intent: None,
    };
    let mut v = tokio::task::spawn_blocking(move || crate::platforms::catalog(&world))
        .await
        .map_err(|e| fail(e.to_string()))?
        .map_err(fail)?;
    v["t"] = json!(clock::wall());
    v["note"] = json!(
        "Catalog limits, not telemetry. armed means the platform carries an effect (policy: \
         every task on it needs a human), not the MAVLink arm state. in_fleet lists the \
         vehicles of each type by uid."
    );
    Ok(v)
}

/// `get_areas`.
pub fn areas(mcp: &Mcp) -> Value {
    let set = mcp.decision.areas();
    let now = clock::wall();
    json!({
        "t": now,
        "source": set.source,
        "as_of": set.loaded_t.0 as f64 / 1000.0,
        "age_s": age(now, set.loaded_t.0 as f64 / 1000.0),
        "note": "approved: where the operator has approved operations (a route leaving it needs a human). no_go: no route may enter (the geofence check fails). engagement_zone: an engagement's whole effect circle must lie inside one. no_strike: an engagement's effect circle must stay clear of every one. Polygons are rings of [lon, lat].",
        "areas": set.areas,
    })
}

/// `get_policy`.
pub fn policy(mcp: &Mcp) -> Value {
    json!({
        "t": clock::wall(),
        "note": "First matching tier wins; no match means human. An intent with any unverified check is never auto-approved. submitter_authorizes concerns human operators only: an agent's proposal in a human tier always waits in the inbox for an operator.",
        "policy": mcp.decision.policy_json(),
    })
}

/// Age fields for an intent's JSON (`created_t` and `expires_t` are engine milliseconds).
fn intent_ages(v: &mut Value, now: f64) {
    if let Some(ms) = v["created_t"].as_i64() {
        v["age_s"] = json!(age(now, ms as f64 / 1000.0));
    }
    if let Some(ms) = v["expires_t"].as_i64() {
        v["expires_in_s"] = json!(round(ms as f64 / 1000.0 - now, 1));
    }
}

/// `get_intents`: newest first.
pub fn intents(mcp: &Mcp, agent: &AgentId, state: Option<&str>, mine: bool, limit: usize) -> Value {
    let now = clock::wall();
    let all = mcp.decision.intents_json();
    let intents: Vec<Value> = all["intents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| state.is_none_or(|s| i["state"].as_str() == Some(s)))
        .filter(|i| {
            !mine
                || (i["source"]["kind"].as_str() == Some("agent")
                    && i["source"]["id"].as_str() == Some(agent.as_str()))
        })
        .take(limit)
        .cloned()
        .map(|mut i| {
            intent_ages(&mut i, now);
            i
        })
        .collect();
    json!({
        "t": now,
        "count": intents.len(),
        "audit": all["audit"],
        "note": "Newest first. An intent's checks and policy decision were computed on the facts logged with it; its state is current.",
        "intents": intents,
    })
}

/// `get_nominations`: the engagement nominations, newest first, with their state, checks,
/// preview, signatures, watch verdict, effect, stop and assessment.
pub fn nominations(mcp: &Mcp, agent: &AgentId, mine: bool, state: Option<&str>) -> Value {
    let now = clock::wall();
    let mut list: Vec<Value> = mcp
        .decision
        .nominations_json()
        .into_iter()
        .filter(|i| state.is_none_or(|s| i["state"].as_str() == Some(s)))
        .filter(|i| {
            !mine
                || (i["source"]["kind"].as_str() == Some("agent")
                    && i["source"]["id"].as_str() == Some(agent.as_str()))
        })
        .collect();
    for i in &mut list {
        intent_ages(i, now);
    }
    json!({
        "t": now,
        "count": list.len(),
        "note": "Newest first. An engagement is an abstract effect (a point, a radius, a time). It needs the human signatures listed in engage.approvers_needed by operators with engagement authority, never its proposer; after approval the picture is held every second and any change voids it.",
        "nominations": list,
    })
}

/// `get_inbox`: pending approvals and alerts, each with its age.
pub fn inbox(mcp: &Mcp) -> Value {
    let now = clock::wall();
    let mut v = mcp.decision.inbox_json();
    if let Some(pending) = v["pending"].as_array_mut() {
        for i in pending {
            intent_ages(i, now);
        }
    }
    if let Some(alerts) = v["alerts"].as_array_mut() {
        for a in alerts {
            if let Some(since) = a["since_t"].as_f64() {
                a["age_s"] = json!(age(now, since));
            }
        }
    }
    v["t"] = json!(now);
    v["note"] = json!(
        "Pending intents wait for a human operator; an agent cannot decide them. Alerts are \
         derived from the current state and clear when their cause does."
    );
    v
}

/// `get_tracks`: contacts that are not ours, as the bridge's own sensors see them (Phase
/// C2). The same JSON as `/api/tracks` (perception only: that endpoint never carries
/// truth), each track with `as_of`, `age_s` and `stale`, and the estimate carried to now
/// (`pos_sigma_now_m`, `confidence_now`, `predicted`).
pub async fn tracks(
    mcp: &Mcp,
    affiliation: Option<&str>,
    domain: Option<&str>,
    include_stale: bool,
) -> Result<Value, DecisionError> {
    let v = mcp.api(Method::GET, "/api/tracks", None).await?;
    let now = v["t"].as_f64().unwrap_or_else(clock::wall);
    let stale_after = v["stale_after_s"].as_f64().unwrap_or(10.0);
    let tracks: Vec<Value> = v["tracks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|t| affiliation.is_none_or(|a| t["affiliation"].as_str() == Some(a)))
        .filter(|t| domain.is_none_or(|d| t["domain"].as_str() == Some(d)))
        .map(|mut t| {
            let last = t["last_seen_t"].as_f64().unwrap_or(now);
            let age_s = age(now, last);
            let stale = age_s > stale_after;
            let sigma = t["pos_sigma_m"].as_f64().unwrap_or(0.0)
                + t["sigma_growth_mps"].as_f64().unwrap_or(0.0) * age_s;
            let decay = t["confidence_decay_s"].as_f64().unwrap_or(60.0).max(1.0);
            let conf = t["confidence"].as_f64().unwrap_or(0.0) * (-age_s / decay).exp();
            let predicted = match (t["position"].as_array(), t["velocity_mps"].as_array()) {
                (Some(p), Some(vel)) if p.len() == 2 && vel.len() == 2 => {
                    let (lon, lat) = (p[0].as_f64().unwrap_or(0.0), p[1].as_f64().unwrap_or(0.0));
                    let (e, n) = (vel[0].as_f64().unwrap_or(0.0), vel[1].as_f64().unwrap_or(0.0));
                    json!([
                        round(lon + e * age_s / (111_320.0 * lat.to_radians().cos()), 6),
                        round(lat + n * age_s / 111_320.0, 6),
                    ])
                }
                _ => t["position"].clone(),
            };
            t["as_of"] = json!(last);
            t["age_s"] = json!(age_s);
            t["stale"] = json!(stale);
            t["pos_sigma_now_m"] = json!(round(sigma, 1));
            t["confidence_now"] = json!(round(conf, 3));
            t["predicted"] = predicted;
            t
        })
        .filter(|t| include_stale || t["stale"] != json!(true))
        .collect();
    Ok(json!({
        "t": now,
        "count": tracks.len(),
        "stale_after_s": stale_after,
        "note": "Perception only: every track is what our own platforms' sensors reported, \
                 fused by the bridge; nobody here knows what a contact really is. position \
                 is the last fix (as_of), predicted carries it along the estimated velocity \
                 to now, pos_sigma_now_m is the 1-sigma uncertainty now (it grows while \
                 unseen) and confidence_now the existence confidence now (it decays). \
                 affiliation is an estimate from identity evidence, with its confidence and \
                 reason; hostile appears only when declared (declared: by a human operator, \
                 or by the engine's hostile-act rule on an observed act against ours, with \
                 its evidence and whether an operator confirmed it); review lists the review \
                 flags in force (a track near or moving with a declared hostile is raised to \
                 suspect for review, never declared on that). \
                 identity.kind is the best classification with its confidence, the sensors \
                 that support it and any conflict (an AIS identity that does not fit what \
                 was seen). sources lists which of our vehicles and sensors hold the track. \
                 lost lists the contacts the tracker dropped (no detection for 180 s) that \
                 every engagement keep-out still counts, from their last fix (since_t), with \
                 the regions counted now; only an operator with engagement authority may \
                 accept one for an engagement.",
        "tracks": tracks,
        // rules 16: read-only, as the console shows them
        "lost": v["lost"].clone(),
    }))
}

/// `get_recent_events`: newest first.
pub fn events(mcp: &Mcp, uid: Option<u64>, limit: usize) -> Value {
    let now = clock::wall();
    let events: Vec<Value> = mcp
        .decision
        .recent_events(uid, limit)
        .into_iter()
        .map(|e| {
            json!({
                "t": e.t, "age_s": age(now, e.t), "uid": e.uid, "callsign": e.callsign,
                "kind": e.kind, "text": e.text,
            })
        })
        .collect();
    json!({
        "t": now,
        "count": events.len(),
        "note": "What the audit log recorded as vehicle.event since the bridge started (the last 500), newest first: mode changes, failsafes and autopilot status text.",
        "events": events,
    })
}
