//! Rules 30's check (docs/NODE-LOSS.md section 8, stage 2; evaluation only, never reachable from
//! the bridge's API or the MCP tools): each silent vehicle's region as the engine counts it now
//! (`Decision::silent_regions`, `murmur_decide::silent`), against where its stand-in really is.
//!
//! A silent vehicle's facts and the orders that may be running on it stay as they were at its
//! last report, so its region for an effect at `t` is the same whenever before `t` a decision
//! computes it: one check a second of the region now against the truth now covers every
//! prediction a decision during the silence makes.
//!
//! `plans`: `"bridge"` takes each vehicle's own lost-link failsafe as the bridge read it (the fact
//! `lost_link`, docs/NODE-LOSS.md stage 4; absent for a second or two after first contact, and
//! then not known); `"standins"` fills it from each stand-in's own parameters, mapped here as a
//! ground station reading them would map them ([`plan_of`]), a check on the bridge's read;
//! `"none"` clears it (not known: every action its autopilot has counts).

use murmur_decide::{Fact, LostLinkAction, LostLinkPlan, LostLinkSource, Uid as DUid};
use murmur_standins::behavior::Firmware;
use murmur_standins::vehicle::Vehicle;
use serde_json::{Value, json};

use super::Episode;

/// A stand-in's own lost-link failsafe as its parameters say (failsafe.rs runs them), mapped
/// onto [`LostLinkAction`], widening where the action depends on the mode it is in when the
/// failsafe fires (ArduPlane's FS_LONG_ACTN 0 continues a mission but returns from any other
/// mode: an rtl, whose region holds the mission's too).
pub fn plan_of(v: &Vehicle) -> LostLinkPlan {
    let p = |n: &str, d: f64| v.params.f(n, d);
    use LostLinkAction as L;
    let (action, timer) = match v.firmware {
        Firmware::Plane => {
            let action = if p("FS_GCS_ENABL", 0.0) == 0.0 {
                L::Off
            } else {
                match p("FS_LONG_ACTN", 0.0) as i64 {
                    2 | 3 => L::Glide,
                    _ => L::Rtl,
                }
            };
            (action, p("FS_LONG_TIMEOUT", 5.0))
        }
        Firmware::Copter => {
            let action = match p("FS_GCS_ENABLE", 0.0) as i64 {
                0 => L::Off,
                4 | 5 => L::Land,
                7 => L::Brake,
                _ => L::Rtl,
            };
            (action, p("FS_GCS_TIMEOUT", 5.0))
        }
        Firmware::Rover => {
            let action = if p("FS_GCS_ENABLE", 0.0) == 0.0 {
                L::Off
            } else {
                match p("FS_ACTION", 2.0) as i64 {
                    0 => L::Continue,
                    1 | 3 => L::Rtl,
                    5 => L::Disarm,
                    6 => L::Loiter,
                    _ => L::Hold,
                }
            };
            (action, p("FS_GCS_TIMEOUT", 5.0))
        }
        Firmware::Sub => {
            let action = match p("FS_GCS_ENABLE", 0.0) as i64 {
                2 => L::Disarm,
                3 => L::DepthHold,
                4 => L::Surface,
                _ => L::Off,
            };
            (action, p("FS_GCS_TIMEOUT", 5.0))
        }
        Firmware::Blimp => {
            let action = if p("FS_GCS_ENABLE", 0.0) == 0.0 { L::Off } else { L::Disarm };
            (action, p("FS_GCS_TIMEOUT", 5.0))
        }
        Firmware::Tracker => (L::Off, 0.0),
    };
    let circle_m = (v.firmware == Firmware::Plane).then(|| {
        let rtl = p("RTL_RADIUS", 0.0).abs();
        if rtl > 0.0 { rtl } else { p("WP_LOITER_RAD", 0.0).abs() }
    });
    LostLinkPlan {
        action,
        timer_s: (timer > 0.0).then_some(timer),
        source: LostLinkSource::Read,
        circle_m: circle_m.filter(|r| *r > 0.0),
    }
}

/// One stand-in as the check reads it.
struct Ours {
    uid: DUid,
    callsign: String,
    plan: LostLinkPlan,
    /// lat, lon, alt
    truth: (f64, f64, f64),
    mode: String,
    armed: bool,
}

impl Episode {
    /// Each silent vehicle now: its region as the engine counts it and where its stand-in is
    /// (`margin_m`: how far inside its region, negative outside), with the part of the region
    /// nearest the truth.
    pub fn silent(&self, plans: &str) -> Result<Value, String> {
        let _g = self.rt.enter();
        if !matches!(plans, "bridge" | "standins" | "none") {
            return Err(format!("silent: plans {plans:?} (bridge, standins, none)"));
        }
        let fleet: Vec<Ours> = (0..self.fleet.len())
            .map(|i| {
                self.fleet.read(i, |v| Ours {
                    uid: DUid::new(v.cfg.uid().to_string()),
                    callsign: v.cfg.callsign.clone(),
                    plan: plan_of(v),
                    truth: v.truth(),
                    mode: v.st.mode.to_string(),
                    armed: v.st.armed,
                })
            })
            .collect();
        let mut read = std::collections::BTreeMap::new();
        let regions = self.decision.silent_regions(|w| {
            for o in &fleet {
                if let Some(v) = w.vehicles.get_mut(&o.uid) {
                    read.insert(o.uid.clone(), v.lost_link.as_ref().map(|l| l.value.clone()));
                    match plans {
                        "standins" => {
                            let t = v.position.as_ref().map_or(w.t, |p| p.t);
                            v.lost_link = Some(Fact::new(o.plan.clone(), t));
                        }
                        "none" => v.lost_link = None,
                        _ => {}
                    }
                }
            }
        });
        let mut out = Vec::new();
        for Ours { uid: u, callsign, plan, truth: (lat, lon, alt), mode, armed } in &fleet {
            let Some(r) = regions.get(u) else { continue };
            let truth = murmur_decide::LonLat::new(*lon, *lat);
            let (edge, part) = r.edge_m(truth);
            let p = part.map(|i| &r.parts[i]);
            out.push(json!({
                "uid": u.as_str(),
                "callsign": callsign,
                "margin_m": -edge,
                "span_s": r.span_s,
                "reach_m": r.reach.pad_m,
                "hypothesis": p.map(|p| p.hypothesis),
                "kind": p.map(|p| p.kind.as_str()),
                "what": p.map(|p| p.what.clone()),
                "parts": r.parts.len(),
                "truth": [lon, lat],
                "alt_m": alt,
                "p0": r.p0,
                "mode": mode,
                "armed": armed,
                "plan": match plans {
                    "standins" => Some(plan.clone()),
                    "bridge" => read.get(u).cloned().flatten(),
                    _ => None,
                },
                // the bridge's read, beside the stand-in's own parameters (they should agree)
                "read_agrees": read.get(u).map(|r| r.as_ref().map(|r| (r.action, r.circle_m)) == Some((plan.action, plan.circle_m))),
            }));
        }
        Ok(json!({ "t": self.now_s(), "silent": out }))
    }
}
