//! Rule-based identification with human review in the bridge (rules 9; the engine's side is
//! `murmur_decide::ident`). Three paths in, all through the engine's identification ledger:
//!
//! - **Observed acts.** The sensing model (`crate::tracks::sensing::observe_act`) turns each
//!   abstract hostile act on the simulation channel that our sensors could see into a report;
//!   the loop hands each to `Engine::record_act` on the world now. The engine attributes it,
//!   logs it, and declares the author hostile by rule or flags it for review.
//! - **The weak-evidence rule.** Every tick, `Engine::step_identification` flags (and raises
//!   to suspect) tracks keeping company with a declared hostile. Never a declaration.
//! - **Operators.** `POST /api/tracks/{id}/declare` (an operator's declaration, as before
//!   rules 9, now logged as `ident.declared` under their session), `.../confirm` (a rule
//!   declaration confirmed), `.../revoke` (any declaration revoked: the engagements in play on
//!   the track stop first, their fallbacks commanded), `POST /api/review/{flag}/resolve`
//!   (a flag confirmed or dismissed). Each is a human act under the operator's own session,
//!   in a dev run (asserted) and a credentials run (signed in) alike; a hostile declaration
//!   or a confirmation steps up only where `[approval] step_up_acts` lists it (off by
//!   default). Agents have no route here and their MCP port no such tool.
//!
//! After every change the tracks module mirrors the ledger for display (the console, the
//! API, agents' reads); decisions never read that mirror: the engine fills each decision's
//! picture from its ledger.

use murmur_decide::{Affiliation, FlagResolution, FlagStatus, IdentOutcome};
use serde_json::{Value, json};

use super::{Actor, Core, Decision, DecisionError, engine_err};

/// The observed acts the inbox shows (the latest).
const ACTS_KEPT: usize = 20;

impl Decision {
    /// Mirror the engine's identification ledger into the track store (display only).
    pub(super) fn mirror_identification(&self, core: &Core) {
        if let Some(t) = self.tracks() {
            t.mirror(core.engine.identification());
        }
    }

    /// The fallbacks a revocation's stops released: commanded, as for any stop.
    fn follow_ident(&self, core: &mut Core, out: &IdentOutcome) {
        for f in &out.fallbacks {
            self.command_fallback(core, f.clone());
        }
    }

    fn ident_json(out: &IdentOutcome) -> Value {
        json!({
            "declaration": out.declaration,
            "revoked": out.revoked.map(|d| format!("D-{d}")),
            "stopped": out.fallbacks.iter().map(|f| f.intent.0).chain(out.voided.iter().map(|i| i.0)).collect::<Vec<_>>(),
        })
    }

    /// `POST /api/tracks/{id}/declare`: an operator declares a track's affiliation through the
    /// engine's ledger (`flag`: the review flag it answers). A declaration in force with
    /// another affiliation is revoked first, stopping the engagements in play on the track.
    pub fn declare(
        &self,
        by: &Actor,
        step: Option<crate::auth::StepUpChecked>,
        track: &str,
        affiliation: Affiliation,
        note: Option<String>,
        flag: Option<u64>,
    ) -> Result<Value, DecisionError> {
        let world = self.snapshot();
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let step = Self::mint_step(&mut core.engine, session, by, step)?;
            let out = core
                .engine
                .declare_track(session.authority(), step, track, affiliation, note, flag, &world)
                .map_err(ident_err)?;
            self.follow_ident(core, &out);
            self.mirror_identification(core);
            Self::ident_json(&out)
        };
        self.publish();
        Ok(out)
    }

    /// `POST /api/tracks/{id}/confirm`: any operator confirms the rule declaration in force.
    pub fn confirm(
        &self,
        by: &Actor,
        step: Option<crate::auth::StepUpChecked>,
        track: &str,
    ) -> Result<Value, DecisionError> {
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let step = Self::mint_step(&mut core.engine, session, by, step)?;
            let out = core
                .engine
                .confirm_declaration(session.authority(), step, track)
                .map_err(ident_err)?;
            self.mirror_identification(core);
            Self::ident_json(&out)
        };
        self.publish();
        Ok(out)
    }

    /// `POST /api/tracks/{id}/revoke`: any operator revokes the declaration in force; the
    /// engagements in play on the track are stopped first and their fallbacks commanded.
    pub fn revoke(&self, by: &Actor, track: &str, note: Option<String>) -> Result<Value, DecisionError> {
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            let out = core
                .engine
                .revoke_declaration(session.authority(), track, note)
                .map_err(ident_err)?;
            self.follow_ident(core, &out);
            self.mirror_identification(core);
            Self::ident_json(&out)
        };
        self.publish();
        Ok(out)
    }

    /// `POST /api/review/{flag}/resolve`: any operator confirms or dismisses a review flag.
    pub fn resolve_flag(
        &self,
        by: &Actor,
        flag: u64,
        resolution: FlagResolution,
        note: Option<String>,
    ) -> Result<Value, DecisionError> {
        let out = {
            let mut guard = self.lock();
            let core = &mut *guard;
            let session = Self::session(&mut core.engine, &mut core.sessions, &core.logins, by, &self.engage_operators, self.auth)?;
            core.engine
                .resolve_flag(session.authority(), flag, resolution, note)
                .map_err(ident_err)?;
            self.mirror_identification(core);
            json!({ "flag": flag, "resolution": resolution })
        };
        self.publish();
        Ok(out)
    }

    /// The loop's identification step: the acts our sensors observed since the last tick go
    /// to the engine (each attributed, logged, and declared by rule or flagged), then the
    /// weak-evidence rule runs on the world now. The ledger is mirrored for display.
    pub(super) fn step_identification(&self, core: &mut Core) {
        let Some(tracks) = self.tracks() else { return };
        let acts = tracks.take_acts();
        let world = self.snapshot();
        for report in acts {
            let id = report.act.clone();
            match core.engine.record_act(report.clone(), &world) {
                Ok(out) => {
                    if let Some(d) = &out.declaration {
                        println!("identification: act {id}: {}", d.describe());
                    } else if let Some(f) = &out.flag {
                        println!("identification: act {id}: flag F-{} ({}): {}", f.id, f.kind.as_str(), f.evidence);
                    }
                    core.acts.push(json!({
                        "act": report,
                        "candidates": out.candidates,
                        "verdict": out.verdict,
                        "declaration": out.declaration.as_ref().map(|d| format!("D-{}", d.id)),
                        "flag": out.flag.as_ref().map(|f| format!("F-{}", f.id)),
                    }));
                    let n = core.acts.len();
                    if n > ACTS_KEPT {
                        core.acts.drain(..n - ACTS_KEPT);
                    }
                }
                Err(e) => eprintln!("decision: act {id}: {e}"),
            }
        }
        match core.engine.step_identification(&world) {
            Ok(new) => {
                for f in new {
                    println!("identification: flag F-{f} (proximity)");
                }
            }
            Err(e) => eprintln!("decision: identification step: {e}"),
        }
        self.mirror_identification(core);
    }

    /// The latest observed acts and what the engine made of each (its attribution and
    /// verdict), newest first: with rule declaration off, an operator declares from these.
    pub(super) fn recorded_acts(&self, core: &Core) -> Vec<Value> {
        core.acts.iter().rev().cloned().collect()
    }

    /// `GET /api/review` (and the inbox's `review`): what the identification asks of a human,
    /// none of it blocking: every rule declaration in force not yet confirmed (confirm or
    /// revoke), every flag open or confirmed (confirm, dismiss, or declare the track), and the
    /// latest resolved ones.
    pub fn review_json(&self) -> Value {
        let core = self.lock();
        review_of(core.engine.identification())
    }
}

/// The review items of a ledger, newest first within each group.
pub(super) fn review_of(ledger: &murmur_decide::ident::Ledger) -> Value {
    let declarations: Vec<Value> = ledger
        .declarations()
        .filter(|d| d.unconfirmed_rule())
        .map(|d| {
            json!({
                "kind": "declaration",
                "id": format!("D-{}", d.id),
                "track": d.track,
                "t": d.t.0 as f64 / 1000.0,
                "affiliation": d.affiliation,
                "declared_by": d.label(),
                "basis": d.describe(),
                "declaration": d,
                "actions": ["confirm", "revoke"],
            })
        })
        .collect();
    let flag = |f: &murmur_decide::Flag, s: &FlagStatus| {
        json!({
            "kind": "flag",
            "id": format!("F-{}", f.id),
            "flag": f.id,
            "flag_kind": f.kind.as_str(),
            "tracks": f.tracks,
            "related": f.related,
            "act": f.act,
            "t": f.t.0 as f64 / 1000.0,
            "evidence": f.evidence,
            "status": s,
            "raises_suspect": f.kind.raises_suspect(),
            "actions": match s {
                FlagStatus::Open => vec!["confirm", "dismiss", "declare"],
                FlagStatus::Confirmed { .. } => vec!["dismiss", "declare"],
                _ => Vec::new(),
            },
        })
    };
    let mut open: Vec<Value> = ledger
        .flags()
        .filter(|(_, s)| s.in_force())
        .map(|(f, s)| flag(f, s))
        .collect();
    open.reverse();
    let mut resolved: Vec<Value> = ledger
        .flags()
        .filter(|(_, s)| !s.in_force())
        .map(|(f, s)| flag(f, s))
        .collect();
    resolved.reverse();
    resolved.truncate(20);
    let confirmed: Vec<Value> = ledger
        .declarations()
        .filter(|d| d.is_rule() && d.confirmed.is_some())
        .map(|d| json!({ "id": format!("D-{}", d.id), "track": d.track, "basis": d.describe() }))
        .collect();
    // a declarer's notice: their declaration revoked by someone else (the latest first)
    let mut revoked: Vec<Value> = ledger
        .revocations()
        .iter()
        .map(|r| {
            let declarer = match &r.declaration.by {
                murmur_decide::Declarer::Operator { operator, .. } => Some(operator.clone()),
                murmur_decide::Declarer::Rule { .. } => None,
            };
            json!({
                "id": format!("D-{}", r.declaration.id),
                "track": r.declaration.track,
                "declared_by": r.declaration.label(),
                "revoked_by": r.by,
                "t": r.t.0 as f64 / 1000.0,
                "note": r.note,
                // the operator to tell: the declarer, when someone else revoked it
                "notify": declarer.filter(|d| *d != r.by),
            })
        })
        .collect();
    revoked.reverse();
    revoked.truncate(20);
    json!({
        "declarations": declarations,
        "flags": open,
        "resolved": resolved,
        "confirmed": confirmed,
        "revocations": revoked,
        "counts": {
            "declarations": declarations.len(),
            "flags": open.len(),
        },
    })
}

/// An identification refused by the engine: 409 (nothing was logged), 401/403 as the
/// session errors say.
fn ident_err(e: murmur_decide::EngineError) -> DecisionError {
    match e {
        murmur_decide::EngineError::Identification(why) => DecisionError::new(409, why),
        e => engine_err(e),
    }
}
