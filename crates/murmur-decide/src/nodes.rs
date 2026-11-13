//! Rules 30 ([`crate::rules::NODE_LOSS_SINCE`], bd murmur-zb1d, docs/NODE-LOSS.md sections 2, 4
//! and 6): losing one of our vehicles. From the C2's seat a destroyed vehicle and a cut link look
//! the same at first: silence. A vehicle whose position is stale is **silent** (nothing new is
//! logged: every decision's facts show it); one silent with logged evidence of a strike is
//! **presumed lost** (`node.presumed`, a flag that decides nothing); one an operator with
//! engagement authority confirmed lost on the silence they saw is **lost** (`node.lost`): it leaves
//! the friendly keep-outs, takes no order, its orders end and the lost-link values it is owed
//! close. Any report from it revokes the loss (`node.loss_revoked`, cause `heard`) before a
//! decision reads that report; any operator may revoke it too (it only restricts).
//!
//! This module is the ledger the engine and replay both keep of it, built from the log's events
//! alone ([`NodeLedger::step`]), and the judgements on a vehicle's facts both make: whether it is
//! silent ([`silent`]), heard again since a report ([`heard_since`]), and what evidence presumes
//! it lost ([`NodeLedger::evidence`]). Evidence (section 6): an observed hostile act against it
//! (`ident.act`, its victim the truth's at the launch: a by-construction shortcut,
//! docs/AUTONOMOUS-NODES.md 2.7 item 2) or its own `struck` report (a hit only, murmur-aset), each
//! within [`PRESUME_WINDOW_S`] of its last report; damage it reported before going silent (rules
//! 25's fact); spent by its own effect (rules 12 and 15, decision D6: spent and lost stay
//! separate). Silence alone never presumes anything.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::audit::EventBody;
use crate::engage::VehicleIdentity;
use crate::ids::{OperatorId, SessionId, Uid};
use crate::rules::RulesConfig;
use crate::time::Time;
use crate::world::{LostNode, VehicleFacts};

/// How far from a vehicle's last report a strike may be (an observed act against it, its struck
/// report) and still presume it lost, s (GUESS: a weapon's flight and the last reports' spacing).
pub const PRESUME_WINDOW_S: f64 = 30.0;

/// The vehicle event kind of a strike report (the bridge's, from the truth side's impact: a hit).
pub const STRUCK_EVENT: &str = "struck";

/// The op state a dispatched order's op on a vehicle confirmed lost ends in.
pub const LOST_OP: &str = "lost";

/// The reason an intent that names a vehicle confirmed lost is voided (or ends) with.
pub const LOST_REASON: &str = "vehicle confirmed lost";

/// Whether `v` is silent at `t`: it never reported a position, or its last one is stale by the
/// link-aware age limit the checks use ([`RulesConfig::fact_age_limit_ms`], as `rules::fresh` and
/// [`crate::engage::lost_since_report`] judge it).
pub fn silent(v: &VehicleFacts, t: Time, cfg: &RulesConfig) -> bool {
    match &v.position {
        None => true,
        Some(p) => t.ms_since(p.t) > cfg.fact_age_limit_ms(v.report_interval_ms).0,
    }
}

/// Why `v` counts as heard again at `t` since its report at `since` (none: it had reported no
/// position) when it was last heard at `heard` (its heartbeat, position or navigation status:
/// [`VehicleFacts::heard_t`]), if it does: a position newer than `since`, its position fresh, or
/// anything heard after `heard`. Any report under the uid counts, whatever vehicle sends it (fail
/// closed: the protection comes back). None: still silent since then.
pub fn heard_since(v: &VehicleFacts, since: Option<Time>, heard: Option<Time>, t: Time, cfg: &RulesConfig) -> Option<String> {
    if let Some(p) = &v.position {
        if since.is_none_or(|s| p.t > s) {
            return Some(format!("it reported a position at {} ms, after the report at {}", p.t.0, at(since)));
        }
        let (limit, _) = cfg.fact_age_limit_ms(v.report_interval_ms);
        if t.ms_since(p.t) <= limit {
            return Some(format!("its position is fresh ({:.1} s old)", t.ms_since(p.t) as f64 / 1000.0));
        }
    }
    if let Some(h) = v.heard_t()
        && heard.is_none_or(|b| h > b)
    {
        return Some(format!("it was heard at {} ms, after {}", h.0, at(heard)));
    }
    None
}

fn at(t: Option<Time>) -> String {
    t.map_or("none".to_string(), |t| format!("{} ms", t.0))
}

/// What kind of evidence presumes one of ours lost (section 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// An observed hostile act against it (`ident.act`).
    Act,
    /// Its own struck report (`vehicle.event` struck).
    Struck,
    /// Damage it reported of itself (rules 25's `damage` fact).
    Damage,
    /// Spent by its own effect (`restore.closed` expended, or carried over spent).
    Spent,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::Act => "act",
            EvidenceKind::Struck => "struck",
            EvidenceKind::Damage => "damage",
            EvidenceKind::Spent => "spent",
        }
    }
}

/// One item of evidence, cited by the event it is in (`seq`; none: a fact of the vehicle's own,
/// logged with the presumption), with its time and what it says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub kind: EvidenceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub t: Time,
    pub detail: String,
}

/// A presumption in force (`node.presumed`): its last report then (`since`), when it was last
/// heard then (`heard`), the evidence and the world's time.
#[derive(Clone, Debug, PartialEq)]
pub struct Presumption {
    pub since: Option<Time>,
    pub heard: Option<Time>,
    pub evidence: Vec<Evidence>,
    pub t: Time,
}

/// A loss in force: confirmed in this log (`node.lost`, `from` none) or carried over from an
/// earlier one (`run.started`'s `carried_from`, `nodes_lost`; `from` that log). Lost since its
/// report at `since` (none: it never reported a position), last heard at `heard` then, confirmed
/// by `by` at `t`, on the vehicle `vehicle` (platform, callsign and system id, as the bridge knew
/// it), with the lost-link values its confirmation closed (`restore.closed` lost: owed back again
/// if it is heard). What a later start carries ([`NodeLedger::marks`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LostMark {
    pub uid: Uid,
    #[serde(default)]
    pub since: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<Time>,
    pub by: OperatorId,
    pub t: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vehicle: Option<VehicleIdentity>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, f64>,
}

/// How a loss was revoked (`node.loss_revoked`'s `cause`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Revocation {
    /// The vehicle was heard again: its facts at `world_t` (replay recomputes it), logged before
    /// any decision reads the report.
    Heard {
        world_t: Time,
        vehicle: Box<VehicleFacts>,
        detail: String,
    },
    /// An operator revoked it, under their own session, saying why (any operator may: it only
    /// restricts).
    Operator {
        operator: OperatorId,
        session: SessionId,
        note: String,
    },
}

impl Revocation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Revocation::Heard { .. } => "heard",
            Revocation::Operator { .. } => "operator",
        }
    }
}

/// The ledger of our vehicles' standing as the log has it: the strikes seen against each (acts and
/// struck reports, with their seq), the spent marks in force, the presumptions and the losses in
/// force. The engine steps it with every event it writes, replay with every event it reads.
#[derive(Clone, Debug, Default)]
pub struct NodeLedger {
    strikes: BTreeMap<Uid, Vec<Evidence>>,
    spent: BTreeMap<Uid, Evidence>,
    presumed: BTreeMap<Uid, Presumption>,
    lost: BTreeMap<Uid, LostMark>,
}

impl NodeLedger {
    /// The ledger after one logged event (`seq` at `t`), as logged: replay checks it.
    pub fn step(&mut self, seq: u64, t: Time, body: &EventBody) {
        match body {
            // what the start carried (newest log first: its standing stands)
            EventBody::RunStarted { carried_from, .. } => {
                for c in carried_from.iter().filter(|c| c.error.is_none()) {
                    for sp in &c.spent {
                        self.spent.entry(sp.uid.clone()).or_insert_with(|| Evidence {
                            kind: EvidenceKind::Spent,
                            seq: Some(seq),
                            t: sp.spent_t.unwrap_or(t),
                            detail: format!(
                                "spent by its own effect (engagement {} of {}, carried over)",
                                sp.by,
                                sp.from.as_deref().unwrap_or(&c.log)
                            ),
                        });
                    }
                    for m in &c.nodes_lost {
                        let mut m = m.clone();
                        m.from.get_or_insert_with(|| c.log.clone());
                        self.lost.entry(m.uid.clone()).or_insert(m);
                    }
                }
            }
            EventBody::IdentAct { act, .. } => {
                if let crate::ident::Against::Vehicle { uid } = &act.against {
                    self.strikes.entry(uid.clone()).or_default().push(Evidence {
                        kind: EvidenceKind::Act,
                        seq: Some(seq),
                        t: act.observed_t,
                        detail: format!("hostile act {} against it, observed by {}", act.act, act.sensors.join(", ")),
                    });
                }
            }
            EventBody::VehicleEvent { uid, kind, text } if kind == STRUCK_EVENT => {
                self.strikes.entry(uid.clone()).or_default().push(Evidence {
                    kind: EvidenceKind::Struck,
                    seq: Some(seq),
                    t,
                    detail: format!("its own report: {text}"),
                });
            }
            EventBody::RestoreClosed {
                uid: Some(u),
                closed_by: crate::audit::RestoreClosure::Expended { intent },
                ..
            } => {
                self.spent.insert(
                    u.clone(),
                    Evidence {
                        kind: EvidenceKind::Spent,
                        seq: Some(seq),
                        t,
                        detail: format!("spent by its own effect (engagement {intent})"),
                    },
                );
            }
            EventBody::RestoreClosed {
                uid: Some(u),
                values,
                closed_by: crate::audit::RestoreClosure::Lost { .. },
                ..
            } => {
                if let Some(m) = self.lost.get_mut(u) {
                    m.values = values.clone();
                }
            }
            EventBody::PlatformUnspent { uid, .. } => {
                self.spent.remove(uid);
            }
            EventBody::NodePresumed {
                uid,
                since,
                evidence,
                world_t,
                vehicle,
            } => {
                self.presumed.insert(
                    uid.clone(),
                    Presumption {
                        since: *since,
                        heard: vehicle.heard_t(),
                        evidence: evidence.clone(),
                        t: *world_t,
                    },
                );
            }
            EventBody::NodePresumptionCleared { uid, .. } => {
                self.presumed.remove(uid);
            }
            EventBody::NodeLost {
                uid,
                since,
                by,
                vehicle,
                identity,
                ..
            } => {
                self.presumed.remove(uid);
                self.lost.insert(
                    uid.clone(),
                    LostMark {
                        uid: uid.clone(),
                        since: *since,
                        heard: vehicle.heard_t(),
                        by: by.clone(),
                        t,
                        from: None,
                        vehicle: identity.clone(),
                        values: BTreeMap::new(),
                    },
                );
            }
            EventBody::NodeLossRevoked { uid, .. } => {
                self.lost.remove(uid);
            }
            _ => {}
        }
    }

    /// The evidence that presumes `uid` lost on its facts `v` (logged with the presumption): the
    /// strikes within [`PRESUME_WINDOW_S`] of its last report, damage it reported, and a spent
    /// mark in force. Empty: nothing presumes it lost (silence alone never does).
    pub fn evidence(&self, uid: &Uid, v: &VehicleFacts) -> Vec<Evidence> {
        let mut out = Vec::new();
        let window = (PRESUME_WINDOW_S * 1000.0).round() as i64;
        if let Some(last) = v.position.as_ref().map(|p| p.t) {
            out.extend(
                self.strikes
                    .get(uid)
                    .into_iter()
                    .flatten()
                    .filter(|e| e.t.ms_since(last).abs() <= window)
                    .cloned(),
            );
        }
        if let Some(d) = v.damage.as_ref().filter(|d| d.value.mobility || d.value.firepower) {
            out.push(Evidence {
                kind: EvidenceKind::Damage,
                seq: None,
                t: d.t,
                detail: format!("it reported {}", d.value.describe()),
            });
        }
        if let Some(s) = self.spent.get(uid) {
            out.push(s.clone());
        }
        out
    }

    /// The presumption in force on `uid`, if any.
    pub fn presumption(&self, uid: &Uid) -> Option<&Presumption> {
        self.presumed.get(uid)
    }

    /// Every presumption in force.
    pub fn presumptions(&self) -> &BTreeMap<Uid, Presumption> {
        &self.presumed
    }

    /// The loss in force on `uid`, if any (this log's or carried over).
    pub fn lost_mark(&self, uid: &Uid) -> Option<&LostMark> {
        self.lost.get(uid)
    }

    /// Every loss in force, this log's (`from` none) and those carried over: what a later start
    /// carries (`carried_from`'s `nodes_lost`).
    pub fn marks(&self) -> Vec<LostMark> {
        self.lost.values().cloned().collect()
    }

    /// The losses a decision reads (its `nodes_lost` fact): each in force on a vehicle of
    /// `vehicles` that has not been heard since on those facts at `t` (one heard is revoked before
    /// the decision is logged), a carried one with the vehicle under the uid now (`now`, the
    /// bridge's identities as the decision read them; it holds only that very vehicle).
    pub fn lost_for(
        &self,
        vehicles: &BTreeMap<Uid, VehicleFacts>,
        now: &BTreeMap<Uid, VehicleIdentity>,
        t: Time,
        cfg: &RulesConfig,
    ) -> BTreeMap<Uid, LostNode> {
        self.lost
            .iter()
            .filter_map(|(u, m)| {
                let v = vehicles.get(u)?;
                if heard_since(v, m.since, m.heard, t, cfg).is_some() {
                    return None;
                }
                Some((u.clone(), m.node(now.get(u).cloned())))
            })
            .collect()
    }

    /// The losses in force that `v` (the facts a decision or step reads for `uid`) shows heard
    /// again, with why: what the engine revokes before that decision is logged.
    pub fn heard(&self, uid: &Uid, v: &VehicleFacts, t: Time, cfg: &RulesConfig) -> Option<(LostMark, String)> {
        let m = self.lost.get(uid)?;
        heard_since(v, m.since, m.heard, t, cfg).map(|why| (m.clone(), why))
    }
}

impl LostMark {
    /// The decision fact for it ([`LostNode`]): a carried one with the vehicle it was confirmed on
    /// and `now`, the vehicle under the uid now.
    pub fn node(&self, now: Option<VehicleIdentity>) -> LostNode {
        let carried = self.from.is_some();
        LostNode {
            since: self.since,
            by: self.by.clone(),
            t: self.t,
            from: self.from.clone(),
            lost_on: if carried { self.vehicle.clone() } else { None },
            now: if carried { now } else { None },
        }
    }
}

impl LostNode {
    /// Whether it holds the vehicle under the uid now: one confirmed in this run does; one carried
    /// over from an earlier run only when the vehicle under the uid is shown to be the one lost
    /// (platform, callsign and system id, never the home: [`VehicleIdentity::same_vehicle`]).
    pub fn holds(&self) -> bool {
        self.other_vehicle().is_none()
    }

    /// Why a carried loss does not hold the vehicle under the uid now, if it does not.
    pub fn other_vehicle(&self) -> Option<String> {
        self.from.as_ref()?;
        match &self.lost_on {
            None => Some("the vehicle that was lost is not known".into()),
            Some(was) => was
                .same_vehicle(self.now.as_ref())
                .err()
                .map(|why| format!("the vehicle under the uid now is not shown to be the one lost: {why}")),
        }
    }

    /// In words, for a check's detail.
    pub fn describe(&self) -> String {
        let carried = self.from.as_ref().map(|f| format!(", carried over from {f}")).unwrap_or_default();
        format!("confirmed lost by {} at {} ms (silent since {}){carried}", self.by, self.t.0, at(self.since))
    }
}
