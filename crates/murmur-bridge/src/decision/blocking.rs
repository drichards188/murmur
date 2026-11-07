//! What a person alone is holding up: the human steps the engagement chain waits on with
//! nothing else in its way (display only, never a decision input, never logged).
//!
//! A rule declaration nobody confirmed blocks every nomination on its track while the policy's
//! `nominate_unconfirmed` is off (the shipped policy), and a track an act against ours was
//! flagged on, not declared, waits for an operator's declaration. The console listed both as
//! review items "not blocking", and a live run's watch officer could only say so in a note
//! (hostile-act-engagement, AI=claude: D-1 waited 38 s, the officer's "I can't nominate ...
//! until you confirm" a plain line in the Situation panel). This says it where the person
//! looks: `GET /api/inbox` and `GET /api/review` carry `blocking`, one item for each review
//! item that alone holds up a nomination, naming the armed platforms, the track and since when;
//! the MCP `get_inbox` answer carries the same, so the watch officer can name the item.
//!
//! **How it knows "only this".** The nomination form's own dry run (the checks a nomination
//! would get now, `Engine::dry_run_inputs` and `rules::evaluate`, as `engage_options` runs
//! them, nothing logged), with the form's defaults ([`EFFECT_RADIUS_M`], the policy's window,
//! fallback return), on the armed platforms nearest the track ([`MAX_PLATFORMS`]): an item
//! holds up a platform's nomination when `target_declared` fails and every other check passes
//! (an engagement is consequential, so a check not evaluated or unverified would block it too
//! and the item would not be alone). The cases:
//!
//! - a rule declaration of a hostile in force and not confirmed (`D-n`): confirming it is
//!   what lets the nomination go ahead;
//! - a flag in force on an act against ours or a track we protect (attribution below the
//!   threshold, conflicting evidence, one observation where the policy wants two: `F-n`), on a
//!   track no declaration is in force on: only an operator's hostile declaration of the track
//!   lets a nomination go ahead (confirming the flag records the concern and declares nothing).
//!
//! Not here: an engagement waiting for its signatures. It is already an Inbox card with its
//! signatures, live checks and expiry, and its signature is the decision itself, not a step
//! before it: an "urgent" banner on it would press on the very act Murmur measures.
//!
//! **Cost.** A background task beside the decision loop, every [`REFRESH_S`] seconds (the live
//! bridge's, `main.rs`; the headless simulator does not run it), and only while a candidate
//! item is in force: one world snapshot, the decision lock only to fill each dry run's inputs
//! (as `engage_options`), the checks outside it, at most [`MAX_PLATFORMS`] per track. The
//! console's and agents' reads only read the result, checked against the ledger under the lock
//! they already take, so an item confirmed a moment ago is never shown blocking.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use murmur_decide::{
    Affiliation, Against, CheckResult, Clock, EngageWindow, Fallback, FlagKind, IntentId, IntentSpec,
    Outcome as CheckOutcome, RuleId, Task, WorldSnapshot,
};
use serde::Serialize;
use serde_json::{Value, json};

use super::snapshot::bridge_uid;
use super::{Core, DUid, Decision};

/// How often the bridge looks again, seconds: a new blocker shows within this.
pub const REFRESH_S: u64 = 2;

/// The nomination judged: the nomination form's default effect radius (and the MCP
/// `get_nomination_options` default), metres.
pub const EFFECT_RADIUS_M: f64 = 75.0;

/// At most this many armed platforms are judged per track, nearest first: a far one that
/// alone could take it is missed (no banner), never shown wrongly.
pub const MAX_PLATFORMS: usize = 12;

/// An armed platform a nomination on the track would pass with, but for the item.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Platform {
    pub uid: u64,
    pub callsign: Option<String>,
}

/// One review item that alone holds up a nomination on one track.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Blocking {
    /// The review item a person acts on: `D-<n>` or `F-<n>`.
    pub id: String,
    /// `declaration` or `flag`.
    pub kind: &'static str,
    pub track: String,
    /// What it holds up (only nominations today).
    pub holds_up: &'static str,
    /// The armed platforms whose nomination passes every other check, nearest first.
    pub platforms: Vec<Platform>,
    /// When the item was raised (the review item's `t`), engine seconds.
    pub raised_t: f64,
    /// Since when it has held up the nomination (first found so), engine seconds.
    pub since_t: f64,
    /// The act behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    /// What waits for a person, in a line.
    pub waits_for: String,
    /// What a person's act does to it.
    pub unblocks: String,
    /// Who may act on it.
    pub who: &'static str,
    /// The sentence the console shows: "Holding up: REAPER 1's nomination on T-0008. Every
    /// other check passes."
    pub text: String,
    /// The failing check's own words (`target_declared`).
    pub failing: String,
}

/// An item in force that could hold up a nomination on `track`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Candidate {
    pub id: String,
    pub kind: CandidateKind,
    pub track: String,
    pub raised_t: f64,
    pub act: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CandidateKind {
    /// A rule declaration of a hostile, not confirmed.
    Declaration,
    /// A flag on an act, the track not declared.
    Flag { flag: FlagKind, victim: Option<String> },
}

/// The items in force that could hold up a nomination: every rule declaration of a hostile
/// nobody confirmed, and every flag in force on an act (not a victim's or a proximity flag) for
/// each of its tracks that no declaration is in force on. `acts`: the bridge's latest observed
/// acts (`Core::acts`), to name a flagged act's victim; `name`: a fleet uid's callsign.
pub(crate) fn candidates(
    ledger: &murmur_decide::ident::Ledger,
    acts: &[Value],
    name: &dyn Fn(&DUid) -> Option<String>,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = ledger
        .declarations()
        .filter(|d| d.unconfirmed_rule() && d.affiliation == Affiliation::Hostile)
        .map(|d| Candidate {
            id: format!("D-{}", d.id),
            kind: CandidateKind::Declaration,
            track: d.track.clone(),
            raised_t: d.t.0 as f64 / 1000.0,
            act: match &d.by {
                murmur_decide::Declarer::Rule { evidence, .. } => Some(evidence.act.clone()),
                murmur_decide::Declarer::Operator { .. } => None,
            },
        })
        .collect();
    for (f, s) in ledger.flags() {
        if !s.in_force() || f.act.is_none() {
            continue;
        }
        if !matches!(f.kind, FlagKind::Attribution | FlagKind::Conflict | FlagKind::Uncorroborated) {
            continue;
        }
        let victim = f.act.as_deref().and_then(|a| victim_of(acts, a, name));
        for track in &f.tracks {
            if ledger.declaration(track).is_some() {
                continue;
            }
            out.push(Candidate {
                id: format!("F-{}", f.id),
                kind: CandidateKind::Flag { flag: f.kind, victim: victim.clone() },
                track: track.clone(),
                raised_t: f.t.0 as f64 / 1000.0,
                act: f.act.clone(),
            });
        }
    }
    out
}

/// Whom act `act` was against, in words ("REAPER 2", "track T-0003"), from the bridge's
/// latest observed acts.
fn victim_of(acts: &[Value], act: &str, name: &dyn Fn(&DUid) -> Option<String>) -> Option<String> {
    let report = acts.iter().find(|a| a["act"]["act"] == act)?;
    let against: Against = serde_json::from_value(report["act"]["against"].clone()).ok()?;
    Some(match against {
        Against::Vehicle { uid } => name(&uid).unwrap_or_else(|| format!("our vehicle {uid}")),
        Against::Track { track } => format!("track {track}"),
        Against::Unknown => return None,
    })
}

/// Whether a nomination's checks fail on the target's declaration alone: `target_declared`
/// fails and every other check passes. An engagement is consequential: a check not evaluated
/// or unverified blocks it as a failure does, so it is not "only the declaration".
pub(crate) fn only_declaration_fails(checks: &[CheckResult]) -> Option<&str> {
    let mut declared = None;
    for c in checks {
        match (c.rule, c.outcome) {
            (RuleId::TargetDeclared, CheckOutcome::Fail) => declared = Some(c.detail.as_str()),
            (_, CheckOutcome::Pass) => {}
            _ => return None,
        }
    }
    declared
}

/// What a candidate's platforms make of it: one [`Blocking`] when a platform passes every
/// other check, none otherwise. `held`: each judged platform's checks, nearest first;
/// `since`: when it was first found so (the caller keeps that across looks).
pub(crate) fn judge(c: &Candidate, held: &[(Platform, Vec<CheckResult>)], since_t: f64) -> Option<Blocking> {
    let mut failing = None;
    let platforms: Vec<Platform> = held
        .iter()
        .filter_map(|(p, checks)| {
            let detail = only_declaration_fails(checks)?;
            failing.get_or_insert_with(|| detail.to_string());
            Some(p.clone())
        })
        .collect();
    let failing = failing?;
    let names: Vec<String> = platforms
        .iter()
        .map(|p| p.callsign.clone().unwrap_or_else(|| format!("vehicle {}", p.uid)))
        .collect();
    let text = match names.as_slice() {
        [one] => format!("Holding up: {one}'s nomination on {}. Every other check passes.", c.track),
        many => format!(
            "Holding up: a nomination on {} with {}. Every other check passes for each.",
            c.track,
            or_list(many)
        ),
    };
    let act = c.act.as_deref().unwrap_or("an act");
    let (kind, waits_for, unblocks) = match &c.kind {
        CandidateKind::Declaration => (
            "declaration",
            format!("{} declared hostile by the engine's rule on {act}, not confirmed by an operator", c.track),
            format!(
                "Confirming {} lets a nomination on {} go ahead (the policy needs an operator's confirmation of a \
                 rule declaration); revoking it ends the declaration. On its card in the Inbox's Review or on {}'s \
                 track card.",
                c.id, c.track, c.track
            ),
        ),
        CandidateKind::Flag { flag, victim } => {
            let on = victim.as_ref().map_or_else(String::new, |v| format!(" against {v}"));
            let why = match flag {
                FlagKind::Attribution => "attributed below the rule's threshold",
                FlagKind::Conflict => "attributed, but the track's evidence conflicts",
                _ => "attributed on one observation where the policy wants two",
            };
            (
                "flag",
                format!("{act}{on} {why}: the engine declared nothing on {}", c.track),
                format!(
                    "Only an operator's hostile declaration of {} (its track card) lets a nomination go ahead. \
                     Confirming {} records that the concern stands and declares nothing; dismissing it records that \
                     it does not.",
                    c.track, c.id
                ),
            )
        }
    };
    Some(Blocking {
        id: c.id.clone(),
        kind,
        track: c.track.clone(),
        holds_up: "nomination",
        platforms,
        raised_t: c.raised_t,
        since_t,
        act: c.act.clone(),
        waits_for,
        unblocks,
        who: "any operator",
        text,
        failing,
    })
}

fn or_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} or {last}", init.join(", ")),
    }
}

/// The last look's result, and since when each item has held up (by item and track).
#[derive(Default)]
pub(crate) struct Cache {
    items: Vec<Blocking>,
    since: BTreeMap<(String, String), f64>,
}

impl Cache {
    /// The items of a new look: each keeps its first `since_t`. Whether anything shown changed.
    fn set(&mut self, items: Vec<Blocking>) -> bool {
        let keys: BTreeSet<(String, String)> = items.iter().map(|b| (b.id.clone(), b.track.clone())).collect();
        self.since.retain(|k, _| keys.contains(k));
        let changed = items != self.items;
        self.items = items;
        changed
    }

    fn since(&mut self, id: &str, track: &str, now: f64) -> f64 {
        *self.since.entry((id.to_string(), track.to_string())).or_insert(now)
    }
}

impl Decision {
    /// The look beside the decision loop: every [`REFRESH_S`] seconds (the live bridge's,
    /// `main.rs`). Display only.
    pub async fn run_blocking(self: Arc<Self>) {
        let mut every = tokio::time::interval(std::time::Duration::from_secs(REFRESH_S));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            self.refresh_blocking().await;
        }
    }

    /// Look again (see the module): the items that alone hold up a nomination now, kept for the
    /// reads; the console is told at once when they changed.
    pub(crate) async fn refresh_blocking(&self) {
        let found = self.find_blocking().await;
        let changed = self.blocking.lock().unwrap_or_else(|e| e.into_inner()).set(found);
        if changed {
            self.publish();
        }
    }

    async fn find_blocking(&self) -> Vec<Blocking> {
        // the callsigns read first, outside the decision lock (as `engage_options` does)
        let names: BTreeMap<DUid, String> = self
            .fleet()
            .iter()
            .filter_map(|(u, c)| Some((super::duid(*u), super::engage::identity_of(c).callsign?)))
            .collect();
        let callsign = |u: &DUid| names.get(u).cloned();
        let cands = {
            let core = self.lock();
            candidates(core.engine.identification(), &core.acts, &callsign)
        };
        if cands.is_empty() {
            return Vec::new();
        }
        let world = self.snapshot();
        let now = self.clock.now();
        let window = EngageWindow {
            from: None,
            until: now.plus_ms(i64::from(self.rules.engage_rules().default_window_s) * 1000),
        };
        // each candidate's track, if in the picture, and the armed platforms nearest it
        let mut jobs: Vec<(usize, Vec<IntentSpec>)> = Vec::new();
        for (k, c) in cands.iter().enumerate() {
            let Some(t) = world.tracks.get(&c.track) else { continue };
            let specs: Vec<IntentSpec> = nearest_armed(&world, t.position.value)
                .into_iter()
                .map(|uid| IntentSpec {
                    id: IntentId(0),
                    created_t: now,
                    source: murmur_decide::Source::Human { id: murmur_decide::OperatorId::new("dry-run") },
                    task: Task::Engage {
                        track: c.track.clone(),
                        effect_radius_m: EFFECT_RADIUS_M,
                        window: Some(window),
                        fallback: Fallback::Return,
                        weapon: None,
                    },
                    uids: vec![uid],
                    constraints: murmur_decide::Constraints::default(),
                    rationale: None,
                })
                .collect();
            if !specs.is_empty() {
                jobs.push((k, specs));
            }
        }
        if jobs.is_empty() {
            return Vec::new();
        }
        // the terrain the checks ask for (the reach check's effect point, rules 17's way there)
        self.prefetch_specs(&world, jobs.iter().flat_map(|(_, s)| s.iter())).await;
        // each dry run's inputs under the lock, as the engine fills them; the checks outside it
        let mut inputs: Vec<(usize, IntentSpec, WorldSnapshot)> = Vec::new();
        for (k, specs) in jobs {
            for spec in specs {
                let w = {
                    let core = self.lock();
                    core.engine.dry_run_inputs(&spec, &world, &self.terrain.oracle())
                };
                inputs.push((k, spec, w));
            }
        }
        let rules = self.rules.clone();
        let judged = tokio::task::spawn_blocking(move || {
            inputs
                .into_iter()
                .map(|(k, spec, w)| {
                    let uid = spec.uids[0].clone();
                    (k, uid, murmur_decide::rules::evaluate(&spec, &w, &rules))
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        let mut by: BTreeMap<usize, Vec<(Platform, Vec<CheckResult>)>> = BTreeMap::new();
        for (k, uid, checks) in judged {
            let Some(b) = bridge_uid(&uid) else { continue };
            by.entry(k).or_default().push((Platform { uid: b, callsign: callsign(&uid) }, checks));
        }
        let now_s = now.0 as f64 / 1000.0;
        let mut cache = self.blocking.lock().unwrap_or_else(|e| e.into_inner());
        by.into_iter()
            .filter_map(|(k, held)| {
                let c = &cands[k];
                // a since kept only for what is found holding up
                judge(c, &held, now_s).map(|mut b| {
                    b.since_t = cache.since(&b.id, &b.track, now_s);
                    b
                })
            })
            .collect()
    }

    /// Terrain for these dry runs, as `prefetch_engagements` fetches it for pending
    /// nominations: one world snapshot, every point at once.
    async fn prefetch_specs<'a>(&self, world: &WorldSnapshot, specs: impl Iterator<Item = &'a IntentSpec>) {
        if !self.terrain.configured() || self.terrain.backing_off() {
            return;
        }
        let mut points = Vec::new();
        let mut along = Vec::new();
        for s in specs {
            let w = world.subset_for(s);
            if murmur_decide::engage::surface_needed(s, &w)
                && let Some(p) = murmur_decide::engage::terrain_point(s, &w, &self.rules)
            {
                points.push(p);
            }
            along.extend(murmur_decide::route::along_points(s, &w, &self.rules));
        }
        if !points.is_empty() {
            self.terrain.prefetch_near(&points).await;
        }
        if !along.is_empty() {
            self.terrain.prefetch(&along).await;
        }
    }

    /// The items holding up a nomination as of the last look, less any the ledger no longer
    /// holds in force as they were (confirmed, revoked, resolved or declared since): read
    /// under the lock the caller already holds.
    pub(super) fn blocking_now(&self, core: &Core) -> Vec<Blocking> {
        let ledger = core.engine.identification();
        let items = self.blocking.lock().unwrap_or_else(|e| e.into_inner()).items.clone();
        items
            .into_iter()
            .filter(|b| match b.kind {
                "declaration" => ledger
                    .declaration(&b.track)
                    .is_some_and(|d| format!("D-{}", d.id) == b.id && d.unconfirmed_rule()),
                _ => {
                    ledger.declaration(&b.track).is_none()
                        && ledger
                            .flags()
                            .any(|(f, s)| format!("F-{}", f.id) == b.id && s.in_force())
                }
            })
            .collect()
    }
}

/// The armed vehicles nearest `at` with a position, at most [`MAX_PLATFORMS`].
fn nearest_armed(world: &WorldSnapshot, at: murmur_decide::LonLat) -> Vec<DUid> {
    let mut armed: Vec<(f64, DUid)> = world
        .vehicles
        .iter()
        .filter(|(_, v)| v.platform.as_ref().is_some_and(|p| p.armed))
        .filter_map(|(u, v)| Some((murmur_decide::geo::haversine_m(v.position.as_ref()?.value, at), u.clone())))
        .collect();
    armed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    armed.truncate(MAX_PLATFORMS);
    armed.into_iter().map(|(_, u)| u).collect()
}

/// What the reads add (`GET /api/inbox`, `GET /api/review`, MCP `get_inbox`): `blocking` and
/// `counts.blocking`, only when something blocks, so an answer without it is as it was (the
/// headless simulator, which does not look, answers byte for byte as before).
pub(super) fn add_to(v: &mut Value, items: &[Blocking]) {
    if items.is_empty() {
        return;
    }
    v["blocking"] = json!(items);
    if v["counts"].is_object() {
        v["counts"]["blocking"] = json!(items.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use murmur_decide::Outcome;

    fn check(rule: RuleId, outcome: Outcome) -> CheckResult {
        CheckResult { rule, uid: None, outcome, detail: format!("{} {outcome:?}", rule.as_str()), fix: None, warning: None }
    }

    fn passing() -> Vec<CheckResult> {
        vec![
            check(RuleId::PlatformArmed, Outcome::Pass),
            check(RuleId::TargetDeclared, Outcome::Pass),
            check(RuleId::TargetRecency, Outcome::Pass),
        ]
    }

    fn with(rule: RuleId, outcome: Outcome) -> Vec<CheckResult> {
        passing()
            .into_iter()
            .map(|c| if c.rule == rule { check(rule, outcome) } else { c })
            .collect()
    }

    fn reaper() -> Platform {
        Platform { uid: 3, callsign: Some("REAPER 1".into()) }
    }

    fn declaration() -> Candidate {
        Candidate {
            id: "D-1".into(),
            kind: CandidateKind::Declaration,
            track: "T-0008".into(),
            raised_t: 100.0,
            act: Some("A-2".into()),
        }
    }

    #[test]
    fn only_the_declaration_failing_holds_up() {
        let checks = with(RuleId::TargetDeclared, Outcome::Fail);
        assert!(only_declaration_fails(&checks).is_some());
        let b = judge(&declaration(), &[(reaper(), checks)], 105.0).expect("blocking");
        assert_eq!(b.id, "D-1");
        assert_eq!(b.platforms, vec![reaper()]);
        assert_eq!(b.text, "Holding up: REAPER 1's nomination on T-0008. Every other check passes.");
        assert!(b.unblocks.starts_with("Confirming D-1"), "{}", b.unblocks);
        assert_eq!(b.since_t, 105.0);
    }

    #[test]
    fn another_check_failing_or_not_evaluated_is_not_blocking() {
        let mut two = with(RuleId::TargetDeclared, Outcome::Fail);
        two.push(check(RuleId::PlatformFree, Outcome::Fail));
        assert!(only_declaration_fails(&two).is_none());
        let mut unknown = with(RuleId::TargetDeclared, Outcome::Fail);
        unknown.push(check(RuleId::Reach, Outcome::NotEvaluated));
        assert!(only_declaration_fails(&unknown).is_none(), "an engagement is consequential");
        let mut unverified = with(RuleId::TargetDeclared, Outcome::Fail);
        unverified.push(check(RuleId::Reserves, Outcome::Unverified));
        assert!(only_declaration_fails(&unverified).is_none());
        assert!(judge(&declaration(), &[(reaper(), two)], 1.0).is_none());
    }

    #[test]
    fn a_passing_nomination_or_no_platform_is_not_blocking() {
        assert!(only_declaration_fails(&passing()).is_none(), "confirmed: nothing fails");
        assert!(judge(&declaration(), &[(reaper(), passing())], 1.0).is_none());
        assert!(judge(&declaration(), &[], 1.0).is_none(), "no armed platform judged");
    }

    #[test]
    fn several_platforms_and_a_flag_say_so() {
        let hunter = Platform { uid: 5, callsign: None };
        let checks = with(RuleId::TargetDeclared, Outcome::Fail);
        let flag = Candidate {
            id: "F-1".into(),
            kind: CandidateKind::Flag { flag: FlagKind::Attribution, victim: Some("HUNTER 2".into()) },
            track: "T-0008".into(),
            raised_t: 50.0,
            act: Some("A-1".into()),
        };
        let mut other = checks.clone();
        other.push(check(RuleId::PlatformFree, Outcome::Fail));
        let b = judge(&flag, &[(reaper(), checks.clone()), (hunter.clone(), checks), (Platform { uid: 7, callsign: None }, other)], 60.0)
            .expect("blocking");
        assert_eq!(b.platforms, vec![reaper(), hunter], "the one failing another check is left out");
        assert_eq!(b.text, "Holding up: a nomination on T-0008 with REAPER 1 or vehicle 5. Every other check passes for each.");
        assert_eq!(b.kind, "flag");
        assert!(b.waits_for.contains("A-1 against HUNTER 2 attributed below the rule's threshold"), "{}", b.waits_for);
        assert!(b.unblocks.contains("declares nothing"), "{}", b.unblocks);
    }

    #[test]
    fn the_cache_keeps_since_and_says_what_changed() {
        let mut c = Cache::default();
        assert_eq!(c.since("D-1", "T-1", 10.0), 10.0);
        assert_eq!(c.since("D-1", "T-1", 20.0), 10.0, "kept");
        let b = judge(&declaration(), &[(reaper(), with(RuleId::TargetDeclared, Outcome::Fail))], 10.0).expect("b");
        assert!(c.set(vec![b.clone()]));
        assert!(!c.set(vec![b]), "the same items: no change");
        assert!(c.set(Vec::new()));
        assert_eq!(c.since("D-1", "T-1", 30.0), 30.0, "dropped once it stopped holding up");
    }
}
