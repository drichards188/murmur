//! Rule-based identification with human review (rules 9, [`crate::rules::RULE_ID_SINCE`]).
//!
//! Until rules 9 hostile was only ever declared by a human (DECISIONS.md, "Hostile is
//! declared, never inferred", 10-02). From rules 9, with `[identification] rule_declaration`
//! on, the engine itself declares a track hostile by one deterministic rule over logged
//! facts, and flags what it is unsure of for a human to review:
//!
//! - **The hostile-act rule** ([`HOSTILE_ACT_RULE`]). The bridge's sensing model reports an
//!   abstract hostile act against one of our vehicles or a protected track (no weapon is
//!   modelled: an act is a time, a victim and where whoever did it appeared to be), observed
//!   by our sensors with their position error ([`ActReport`]). The engine attributes it to
//!   the tracks it holds ([`attribute`]: each track's likelihood of having been at the
//!   observed location, against the chance that the actor is a contact we hold no track
//!   of), logs the report, the track picture and the attribution (`ident.act`), and decides
//!   ([`verdict`]): declare the best track hostile by rule when its attribution reaches the
//!   policy's threshold and nothing conflicts (the track is not declared otherwise by an
//!   operator, an operator did not revoke a declaration of it, its identity evidence is not
//!   neutral or friendly with high confidence), else flag it for review.
//! - **Weak evidence is flagged, never declared.** A track that keeps company with a declared
//!   hostile (within `escort_m`, its velocity within `escort_speed_mps` of the hostile's) is
//!   flagged ([`escort`]) and shown suspect to readers (the console, agents): that is how
//!   neutrals and civilians get misclassified, so it never declares anything on its own, and
//!   it never hides the track's own identity evidence from a decision (the rule's conflict
//!   check reads the estimate as the tracker has it now).
//! - **Corroboration** (`[identification] corroboration`): `none` declares on one
//!   observation; `second_observation` waits for a second independent one (another
//!   observer's fix of the same act, or a second act attributed to the same track at the
//!   threshold), flagging the act until then.
//! - **Review.** A rule declaration is a non-blocking review item: any operator may confirm it
//!   (`ident.confirmed`) or revoke it (`ident.revoked`, which stops every engagement in play
//!   on the track first, in the same step). A flag may be confirmed or dismissed
//!   (`ident.flag_resolved`), or answered by an operator's own declaration that names it.
//!   An operator may declare any track at any time, as before (`ident.declared` by an
//!   operator).
//!
//! What stays human, by type: releasing an effect (two operators with engagement authority
//! on one frozen plan), accepting a lost friendly, and every confirmation, revocation and
//! declaration by an operator take a [`crate::HumanAuthority`] or an
//! [`crate::EngagementAuthority`]; an agent's port has none of these methods. A rule
//! declaration is the engine's own act over logged facts, never an agent's, and replay
//! recomputes it.
//!
//! **The basis travels with the decision.** The engine fills every track's
//! [`TrackFacts::declaration`] from its ledger ([`Ledger::fill`]) before any decision reads
//! the picture, so the checks, the picture token and the frozen plan carry how the target
//! was declared (operator or rule, and the rule's evidence). A track declared by rule and not
//! confirmed stays protected in another engagement's keep-out (it is exempt only when it is
//! the target): conservative, while an operator's declaration or a confirmed one is exempt as
//! before. Whether a rule-declared track may be nominated before a confirmation is the
//! policy's (`nominate_unconfirmed`, default no).
//!
//! Every function here is pure; the engine logs inputs and results and replay recomputes
//! them, and checks every decision's track picture against the ledger it keeps from the log
//! ([`Ledger::check_facts`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::audit::EventBody;
use crate::geo::{LonLat, haversine_m};
use crate::ids::{IntentId, OperatorId, SessionId, Uid};
use crate::time::Time;
use crate::world::{Affiliation, Fact, TrackFacts};

/// The one identification rule: a hostile act against ours, attributed to a track.
pub const HOSTILE_ACT_RULE: &str = "hostile_act";

/// How a rule declaration names its declarer in `declared_by`: `rule:<rule>`.
pub const RULE_PREFIX: &str = "rule:";

/// At most this many candidate tracks are logged with an act (the most likely ones).
pub const MAX_CANDIDATES: usize = 5;

/// A candidate below this attribution is not named in a flag.
pub const FLAG_FLOOR: f64 = 0.1;

// ---- policy -------------------------------------------------------------------------------

/// `[identification]` in the policy file (policy schema 5, rules 9). Every key optional; an
/// unknown key fails to load. Absent, or `rule_declaration = false`: the engine declares
/// nothing and flags nothing (rules 7's behaviour); an operator still declares.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdentConfig {
    /// The engine declares a track hostile by the hostile-act rule, and flags what it is
    /// unsure of. Default false.
    pub rule_declaration: bool,
    /// The attribution an act must reach for a rule declaration, 0.5..0.999. Default 0.9.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_threshold: Option<f64>,
    /// A track declared by rule may be nominated for an engagement before an operator
    /// confirms the declaration. Default false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nominate_unconfirmed: Option<bool>,
    /// A track shown neutral or assumed friend at least this confidently conflicts with an
    /// act attributed to it (flagged, not declared), 0.3..1. Default 0.5: any neutral or
    /// friend estimate held better than even (a fishing boat without AIS classified at 0.625
    /// or more, the tracker's affiliation confidence being 0.8 of its classification).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_confidence: Option<f64>,
    /// How likely the actor is a contact we hold no track of, as a density of untracked
    /// contacts per square kilometre (the attribution's alternative), 0.01..100. Default 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untracked_per_km2: Option<f64>,
    /// A track within this distance of a declared hostile, moving with it, is flagged and
    /// raised to suspect (never declared), metres, 50..2000. Default 300.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escort_m: Option<f64>,
    /// "Moving with it": the two velocity estimates within this much of each other, m/s,
    /// 0.1..10. Default 1.5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escort_speed_mps: Option<f64>,
    /// What a rule declaration needs besides one observation attributed at the threshold:
    /// `none` (default) or `second_observation` (another observer's fix of the same act, or a
    /// second act attributed to the same track at the threshold that passed every check but
    /// corroboration itself, within `corroboration_window_s`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corroboration: Option<Corroboration>,
    /// A second act corroborates only if it was observed at most this long before, seconds,
    /// 10..3600. Default 600.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corroboration_window_s: Option<f64>,
}

/// `[identification] corroboration`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corroboration {
    /// One observation attributed at the threshold declares.
    #[default]
    None,
    /// A second independent observation must agree: another observer's fix of the same act,
    /// or a second act, attributed to the same track at the threshold.
    SecondObservation,
}

impl Corroboration {
    pub fn as_str(self) -> &'static str {
        match self {
            Corroboration::None => "none",
            Corroboration::SecondObservation => "second_observation",
        }
    }
}

impl IdentConfig {
    pub fn threshold(&self) -> f64 {
        self.attribution_threshold.unwrap_or(0.9)
    }

    pub fn nominate_unconfirmed(&self) -> bool {
        self.nominate_unconfirmed.unwrap_or(false)
    }

    pub fn conflict_confidence(&self) -> f64 {
        self.conflict_confidence.unwrap_or(0.5)
    }

    pub fn corroboration(&self) -> Corroboration {
        self.corroboration.unwrap_or_default()
    }

    pub fn corroboration_window_s(&self) -> f64 {
        self.corroboration_window_s.unwrap_or(600.0)
    }

    pub fn untracked_per_km2(&self) -> f64 {
        self.untracked_per_km2.unwrap_or(1.0)
    }

    pub fn escort_m(&self) -> f64 {
        self.escort_m.unwrap_or(300.0)
    }

    pub fn escort_speed_mps(&self) -> f64 {
        self.escort_speed_mps.unwrap_or(1.5)
    }

    /// Every value inside a bound, so a policy file can tighten the rule but not make it
    /// declare on a coin flip (a threshold below 0.5) or switch its conflict check off.
    pub fn validate(&self) -> Result<(), String> {
        let within = |v: f64, lo: f64, hi: f64, n: &str| {
            if v.is_finite() && v >= lo && v <= hi {
                Ok(())
            } else {
                Err(format!("identification.{n} must be within {lo}..{hi}, got {v}"))
            }
        };
        within(self.threshold(), 0.5, 0.999, "attribution_threshold")?;
        within(self.conflict_confidence(), 0.3, 1.0, "conflict_confidence")?;
        within(self.untracked_per_km2(), 0.01, 100.0, "untracked_per_km2")?;
        within(self.escort_m(), 50.0, 2000.0, "escort_m")?;
        within(self.escort_speed_mps(), 0.1, 10.0, "escort_speed_mps")?;
        within(self.corroboration_window_s(), 10.0, 3600.0, "corroboration_window_s")?;
        Ok(())
    }
}

// ---- acts ---------------------------------------------------------------------------------

/// Whom a hostile act was against.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Against {
    /// One of our vehicles.
    Vehicle { uid: Uid },
    /// A track we hold (the victim as our sensors associate it).
    Track { track: String },
    /// Something we hold no track of.
    Unknown,
}

impl Against {
    pub fn describe(&self) -> String {
        match self {
            Against::Vehicle { uid } => format!("our vehicle {uid}"),
            Against::Track { track } => format!("track {track}"),
            Against::Unknown => "something we hold no track of".into(),
        }
    }
}

/// An abstract hostile act as our sensors observed it (the bridge's sensing model; no weapon
/// of any kind is modelled): whom it was against, when, which of our sensors saw it, and where
/// whoever did it appeared to be with that position's 1-sigma error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActReport {
    /// The bridge's number for it (`A-1`, ...), unique in a run.
    pub act: String,
    pub against: Against,
    pub observed_t: Time,
    /// Our sensors that observed it, as `"<uid>:<sensor>"`.
    pub sensors: Vec<String>,
    /// Where the act's author appeared to be, and that position's 1-sigma error, metres (the
    /// best observer's fix).
    pub location: LonLat,
    pub sigma_m: f64,
    /// Further fixes of the same act by other observers, each one observer's best sensor
    /// (corroboration; fusing them is later work).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<Fix>,
    /// The channel it came in on: the bridge's simulation channel (`sim:contacts`, its token
    /// checked) or the scenario runner (`scenario`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

/// One observer's fix of an act's author.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fix {
    /// The observing vehicle (its uid).
    pub observer: String,
    /// Its sensors that saw the act, as `"<uid>:<sensor>"`.
    pub sensors: Vec<String>,
    pub location: LonLat,
    pub sigma_m: f64,
}

/// What corroborated a rule declaration (`[identification] corroboration =
/// "second_observation"`): another observer's fix of the same act, or an earlier act.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corroborating {
    /// The act it rests on (the same act for a fix).
    pub act: String,
    /// The observer whose fix agreed (a fix of the same act); none for an earlier act.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observer: Option<String>,
    pub attribution: f64,
}

/// One track the act may be attributed to: how far its prediction was from the observed
/// location, the combined 1-sigma, and the attribution (0..1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub track: String,
    pub distance_m: f64,
    pub sigma_m: f64,
    pub confidence: f64,
}

/// Why an act was flagged rather than declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    /// Attributed below the threshold (several tracks close together, a loose fix).
    Attribution,
    /// Attributed, but the track's evidence conflicts (neutral or friendly with high
    /// confidence, declared otherwise by an operator, a declaration an operator revoked).
    Conflict,
    /// No track it could be attributed to.
    Untracked,
    /// Its victim can't be said to be ours or protected (no track of it).
    Victim,
    /// Keeps company with a declared hostile: weak evidence; the track is shown suspect.
    Proximity,
    /// Attributed at the threshold on one observation, and the policy wants a second
    /// (`corroboration = "second_observation"`).
    Uncorroborated,
}

impl FlagKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FlagKind::Attribution => "attribution",
            FlagKind::Conflict => "conflict",
            FlagKind::Untracked => "untracked",
            FlagKind::Victim => "victim",
            FlagKind::Proximity => "proximity",
            FlagKind::Uncorroborated => "uncorroborated",
        }
    }

    /// A flag of this kind shows its track suspect while it is in force (display: the console
    /// and agents' reads; decisions read the track's own evidence and its flags).
    pub fn raises_suspect(self) -> bool {
        self == FlagKind::Proximity
    }
}

/// What the engine does with an act ([`verdict`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ActVerdict {
    /// Declared hostile by rule (`ident.declared` follows at once).
    Declare {
        track: String,
        attribution: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corroborated_by: Option<Corroborating>,
    },
    /// Flagged for review (`ident.flagged` follows at once).
    Flag {
        kind: FlagKind,
        tracks: Vec<String>,
        why: String,
    },
    /// Recorded only (the rule is off, the track is already declared hostile, the victim is a
    /// declared hostile).
    Recorded { why: String },
}

/// A review flag: what the engine is unsure of, for a human.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Flag {
    pub id: u64,
    pub t: Time,
    pub kind: FlagKind,
    /// The tracks it is about (an act's candidates, the one keeping company), possibly none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<String>,
    /// The act it comes from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    /// Proximity: the declared hostile it keeps company with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related: Option<String>,
    pub evidence: String,
}

/// A track's affiliation estimate and its confidence.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub affiliation: Affiliation,
    pub confidence: f64,
}

/// What [`escort`] finds: an undeclared track keeping company with a declared hostile.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Escort {
    pub track: String,
    pub hostile: String,
    pub evidence: String,
    pub estimate: Estimate,
}

/// How an operator resolved a flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagResolution {
    /// Agreed with it (a proximity flag keeps the track suspect).
    Confirmed,
    /// Disagreed (a proximity flag no longer raises the track).
    Dismissed,
}

/// A flag's standing in the ledger.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FlagStatus {
    Open,
    Confirmed { by: OperatorId, t: Time },
    Dismissed { by: OperatorId, t: Time },
    /// An operator declared one of its tracks, naming it.
    Declared { by: OperatorId, t: Time, track: String },
}

impl FlagStatus {
    /// Still in force: open, or confirmed.
    pub fn in_force(&self) -> bool {
        matches!(self, FlagStatus::Open | FlagStatus::Confirmed { .. })
    }

    pub fn name(&self) -> &'static str {
        match self {
            FlagStatus::Open => "open",
            FlagStatus::Confirmed { .. } => "confirmed",
            FlagStatus::Dismissed { .. } => "dismissed",
            FlagStatus::Declared { .. } => "declared",
        }
    }
}

// ---- declarations ---------------------------------------------------------------------------

/// The evidence a rule declaration rests on: the act, whom it was against, when and by which
/// of our sensors it was observed, and its attribution against the threshold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleEvidence {
    pub act: String,
    pub against: Against,
    pub observed_t: Time,
    pub sensors: Vec<String>,
    pub attribution: f64,
    pub threshold: f64,
    /// The second observation that agreed (`corroboration = "second_observation"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corroborated_by: Option<Corroborating>,
}

/// Who or what declared a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Declarer {
    /// An operator, under their session (any operator may declare).
    Operator {
        operator: OperatorId,
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// The review flag this declaration answers, if it names one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        flag: Option<u64>,
        /// Rules 8: the step-up a hostile declaration quoted (`[approval] step_up_acts` lists
        /// `declare_hostile`, a credentials run).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_up: Option<crate::authority::StepUpRef>,
    },
    /// The engine's rule, on the evidence of an act (never an agent's).
    Rule { rule: String, evidence: RuleEvidence },
}

/// An operator's confirmation of a rule declaration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Confirmation {
    pub by: OperatorId,
    pub session: SessionId,
    pub t: Time,
}

/// A declaration of a track's affiliation, as the ledger holds it while it is in force (and
/// as the engine fills it into the track's facts for every decision).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    /// Its number in the run (`D-<id>`).
    pub id: u64,
    pub track: String,
    pub affiliation: Affiliation,
    pub t: Time,
    pub by: Declarer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<Confirmation>,
}

impl Declaration {
    /// What `declared_by` says: the operator, or `rule:<rule>`.
    pub fn label(&self) -> String {
        match &self.by {
            Declarer::Operator { operator, .. } => operator.to_string(),
            Declarer::Rule { rule, .. } => format!("{RULE_PREFIX}{rule}"),
        }
    }

    pub fn is_rule(&self) -> bool {
        matches!(self.by, Declarer::Rule { .. })
    }

    /// Declared by rule and not confirmed by an operator.
    pub fn unconfirmed_rule(&self) -> bool {
        self.is_rule() && self.confirmed.is_none()
    }

    /// The basis in a sentence: "declared hostile by op-1", or "declared hostile by rule
    /// hostile_act on act A-1 (against our vehicle 3, observed by 3:eo_ir at t=..., attribution
    /// 0.97 >= 0.90), confirmed by op-2" (or "not confirmed by an operator").
    pub fn describe(&self) -> String {
        let aff = self.affiliation.as_str();
        match &self.by {
            Declarer::Operator { operator, .. } => format!("declared {aff} by {operator} (D-{})", self.id),
            Declarer::Rule { rule, evidence } => {
                let confirmed = match &self.confirmed {
                    Some(c) => format!("confirmed by {}", c.by),
                    None => "not confirmed by an operator".into(),
                };
                let corroborated = match &evidence.corroborated_by {
                    Some(c) => match &c.observer {
                        Some(o) => format!(", corroborated by {o}'s fix at {:.2}", c.attribution),
                        None => format!(", corroborated by act {} at {:.2}", c.act, c.attribution),
                    },
                    None => String::new(),
                };
                format!(
                    "declared {aff} by rule {rule} (D-{}) on act {} against {}, observed by {} at t={}, attribution {:.2} >= {:.2}{corroborated}; {confirmed}",
                    self.id,
                    evidence.act,
                    evidence.against.describe(),
                    evidence.sensors.join(", "),
                    evidence.observed_t.0,
                    evidence.attribution,
                    evidence.threshold
                )
            }
        }
    }
}

/// Whether a track counts as a declared hostile for an engagement's keep-out exemption: an
/// operator's hostile declaration, or a rule's an operator confirmed (rules 9; before, any
/// declaration, which was always an operator's). A track declared by rule and not confirmed
/// stays protected unless it is the target.
pub fn exempt_in_keep_out(t: &TrackFacts) -> bool {
    t.affiliation.value == Affiliation::Hostile
        && t.declared_by.is_some()
        && !t.declaration.as_ref().is_some_and(|d| d.unconfirmed_rule())
}

// ---- the rule -------------------------------------------------------------------------------

fn round4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Attribute an act to the tracks in the picture (`now`: the picture's time). Each track is
/// predicted to the observation's time along its velocity, with its uncertainty grown to
/// then; its likelihood is the bivariate Gaussian density of the observed location under the
/// combined uncertainty (`sqrt(sigma_obs^2 + sigma_track^2)`), against a uniform density of
/// contacts we hold no track of (`untracked_per_km2`). The attribution of a track is its
/// likelihood over the sum of all of them and the untracked one: several tracks close
/// together share it. The most likely [`MAX_CANDIDATES`] within five combined sigmas are
/// returned, most likely first. The act's victim, when it is a track, is never its author.
pub fn attribute(
    report: &ActReport,
    tracks: &BTreeMap<String, TrackFacts>,
    now: Time,
    cfg: &IdentConfig,
) -> Vec<Candidate> {
    attribute_fix(report, report.location, report.sigma_m, tracks, now, cfg)
}

/// [`attribute`] for one fix of the act (its location and 1-sigma).
pub fn attribute_fix(
    report: &ActReport,
    location: LonLat,
    sigma_m: f64,
    tracks: &BTreeMap<String, TrackFacts>,
    now: Time,
    cfg: &IdentConfig,
) -> Vec<Candidate> {
    let untracked = cfg.untracked_per_km2() * 1e-6;
    let victim = match &report.against {
        Against::Track { track } => Some(track.as_str()),
        _ => None,
    };
    let mut likely: Vec<(String, f64, f64, f64)> = Vec::new();
    for (id, t) in tracks {
        if victim == Some(id.as_str()) {
            continue;
        }
        let p = crate::engage::track_at(t, report.observed_t);
        let sigma_t = crate::engage::track_sigma_at(t, now, report.observed_t);
        let s2 = sigma_m * sigma_m + sigma_t * sigma_t;
        let d = haversine_m(p, location);
        if s2.is_nan() || s2 <= 0.0 || d > 5.0 * s2.sqrt() {
            continue;
        }
        let l = (-(d * d) / (2.0 * s2)).exp() / (std::f64::consts::TAU * s2);
        likely.push((id.clone(), d, s2.sqrt(), l));
    }
    let total: f64 = untracked + likely.iter().map(|c| c.3).sum::<f64>();
    let mut out: Vec<Candidate> = likely
        .into_iter()
        .map(|(track, d, s, l)| Candidate {
            track,
            distance_m: round1(d),
            sigma_m: round1(s),
            confidence: round4(l / total),
        })
        .collect();
    out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence).then(a.track.cmp(&b.track)));
    out.truncate(MAX_CANDIDATES);
    out
}

/// What the rule makes of an attributed act, on the picture as the ledger fills it
/// (`tracks`: declarations from the ledger, every other identity field the tracker's own,
/// never masked by a flag), our fleet (`fleet`: the vehicles a victim may be) and the ledger
/// itself (`now`: the picture's time, for a second act's corroboration).
#[allow(clippy::too_many_arguments)]
pub fn verdict(
    report: &ActReport,
    candidates: &[Candidate],
    tracks: &BTreeMap<String, TrackFacts>,
    fleet: &BTreeSet<Uid>,
    ledger: &Ledger,
    now: Time,
    cfg: &IdentConfig,
) -> ActVerdict {
    if !cfg.rule_declaration {
        return ActVerdict::Recorded {
            why: "rule declaration is off: an operator declares".into(),
        };
    }
    // the victim: one of our vehicles, or a track we protect (any but a declared hostile;
    // a suspect too: an act against an undeclared contact is an act against someone we
    // protect)
    match &report.against {
        Against::Vehicle { uid } if !fleet.contains(uid) => {
            return ActVerdict::Flag {
                kind: FlagKind::Victim,
                tracks: named(candidates),
                why: format!("its victim, vehicle {uid}, is not one of our fleet: it can't be said to be ours"),
            };
        }
        Against::Vehicle { .. } => {}
        Against::Track { track } => match tracks.get(track) {
            Some(v) if v.affiliation.value == Affiliation::Hostile && v.declared_by.is_some() => {
                return ActVerdict::Recorded {
                    why: format!("against {track}, itself declared hostile: not an act against ours"),
                };
            }
            Some(_) => {}
            None => {
                return ActVerdict::Flag {
                    kind: FlagKind::Victim,
                    tracks: named(candidates),
                    why: format!("its victim, track {track}, is not in the picture: it can't be said to be ours or protected"),
                };
            }
        },
        Against::Unknown => {
            return ActVerdict::Flag {
                kind: FlagKind::Victim,
                tracks: named(candidates),
                why: "against something we hold no track of: it can't be said to be ours or protected".into(),
            };
        }
    }
    let threshold = cfg.threshold();
    let Some(best) = candidates.first() else {
        return ActVerdict::Flag {
            kind: FlagKind::Untracked,
            tracks: Vec::new(),
            why: format!(
                "no track within five sigmas of where it was observed ({:.0} m 1-sigma): its author is no track we hold",
                report.sigma_m
            ),
        };
    };
    if best.confidence < threshold {
        let others: Vec<String> = candidates
            .iter()
            .skip(1)
            .filter(|c| c.confidence >= FLAG_FLOOR)
            .map(|c| format!("{} at {:.2}", c.track, c.confidence))
            .collect();
        let also = if others.is_empty() {
            String::new()
        } else {
            format!("; also {}", others.join(", "))
        };
        return ActVerdict::Flag {
            kind: if best.confidence < FLAG_FLOOR {
                FlagKind::Untracked
            } else {
                FlagKind::Attribution
            },
            tracks: named(candidates),
            why: format!(
                "attributed to {} at {:.2} ({:.0} m from it, {:.0} m 1-sigma), below the threshold {threshold:.2}{also}",
                best.track, best.confidence, best.distance_m, best.sigma_m
            ),
        };
    }
    let Some(t) = tracks.get(&best.track) else {
        return ActVerdict::Flag {
            kind: FlagKind::Untracked,
            tracks: Vec::new(),
            why: format!("attributed to {}, no longer in the picture", best.track),
        };
    };
    if let Some(d) = ledger.declaration(&best.track) {
        if d.affiliation == Affiliation::Hostile {
            return ActVerdict::Recorded {
                why: format!("{} is already {}", best.track, d.describe()),
            };
        }
        return ActVerdict::Flag {
            kind: FlagKind::Conflict,
            tracks: vec![best.track.clone()],
            why: format!(
                "attributed to {} at {:.2}, but it is {}: an operator's declaration outranks the rule",
                best.track,
                best.confidence,
                d.describe()
            ),
        };
    }
    if ledger.revoked_by_operator(&best.track) {
        return ActVerdict::Flag {
            kind: FlagKind::Conflict,
            tracks: vec![best.track.clone()],
            why: format!(
                "attributed to {} at {:.2}, but an operator revoked a hostile declaration of it: a human decides",
                best.track, best.confidence
            ),
        };
    }
    // its own identity evidence, as the tracker has it now (a flag never masks it; missing
    // evidence, an unclassified track, does not conflict: the act is the evidence)
    let conflict = cfg.conflict_confidence();
    let own = Estimate {
        affiliation: t.affiliation.value,
        confidence: t.affiliation_confidence,
    };
    if matches!(own.affiliation, Affiliation::Neutral | Affiliation::AssumedFriend)
        && own.confidence >= conflict
    {
        return ActVerdict::Flag {
            kind: FlagKind::Conflict,
            tracks: vec![best.track.clone()],
            why: format!(
                "attributed to {} at {:.2}, but its identity evidence shows it {} at {:.2} (>= {conflict:.2}): conflicting evidence",
                best.track,
                best.confidence,
                own.affiliation.as_str(),
                own.confidence
            ),
        };
    }
    // corroboration: a second independent observation attributed to the same track
    let corroborated_by = match cfg.corroboration() {
        Corroboration::None => None,
        Corroboration::SecondObservation => {
            let by_fix = report.fixes.iter().find_map(|f| {
                let c = attribute_fix(report, f.location, f.sigma_m, tracks, now, cfg);
                c.first()
                    .filter(|c| c.track == best.track && c.confidence >= threshold)
                    .map(|c| Corroborating {
                        act: report.act.clone(),
                        observer: Some(f.observer.clone()),
                        attribution: c.confidence,
                    })
            });
            let window = cfg.corroboration_window_s();
            let Some(c) = by_fix.or_else(|| ledger.corroborating_act(&best.track, threshold, &report.act, report.observed_t, window)) else {
                return ActVerdict::Flag {
                    kind: FlagKind::Uncorroborated,
                    tracks: vec![best.track.clone()],
                    why: format!(
                        "attributed to {} at {:.2} on one observation; the policy wants a second (another observer's fix of it, or another act within {window:.0} s) at {threshold:.2}",
                        best.track, best.confidence
                    ),
                };
            };
            Some(c)
        }
    };
    ActVerdict::Declare {
        track: best.track.clone(),
        attribution: best.confidence,
        corroborated_by,
    }
}

/// The candidates a flag names (at least [`FLAG_FLOOR`]).
fn named(candidates: &[Candidate]) -> Vec<String> {
    candidates
        .iter()
        .filter(|c| c.confidence >= FLAG_FLOOR)
        .map(|c| c.track.clone())
        .collect()
}

/// The declaration a rule makes of an act's verdict (`id`: the next declaration number).
pub fn rule_declaration(
    id: u64,
    t: Time,
    report: &ActReport,
    track: &str,
    attribution: f64,
    corroborated_by: Option<Corroborating>,
    cfg: &IdentConfig,
) -> Declaration {
    Declaration {
        id,
        track: track.to_string(),
        affiliation: Affiliation::Hostile,
        t,
        by: Declarer::Rule {
            rule: HOSTILE_ACT_RULE.into(),
            evidence: RuleEvidence {
                act: report.act.clone(),
                against: report.against.clone(),
                observed_t: report.observed_t,
                sensors: report.sensors.clone(),
                attribution,
                threshold: cfg.threshold(),
                corroborated_by,
            },
        },
        confirmed: None,
    }
}

/// The flag an act's verdict raises (`id`: the next flag number).
pub fn act_flag(id: u64, t: Time, report: &ActReport, kind: FlagKind, tracks: &[String], why: &str) -> Flag {
    Flag {
        id,
        t,
        kind,
        tracks: tracks.to_vec(),
        act: Some(report.act.clone()),
        related: None,
        evidence: format!(
            "act {} against {}, observed by {} at t={}: {why}; not declared",
            report.act,
            report.against.describe(),
            report.sensors.join(", "),
            report.observed_t.0
        ),
    }
}

/// The weak-evidence rule: undeclared tracks keeping company with a declared hostile (in the
/// same domain, within `escort_m` of it now, both velocity estimates known and within
/// `escort_speed_mps` of each other), each pair once, in id order. Never a declaration: a
/// flag, which shows the track suspect to readers.
pub fn escort(
    tracks: &BTreeMap<String, TrackFacts>,
    ledger: &Ledger,
    now: Time,
    cfg: &IdentConfig,
) -> Vec<Escort> {
    if !cfg.rule_declaration {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (hid, h) in tracks {
        if !ledger
            .declaration(hid)
            .is_some_and(|d| d.affiliation == Affiliation::Hostile)
        {
            continue;
        }
        let Some(hv) = h.velocity_mps else { continue };
        for (xid, x) in tracks {
            if xid == hid || ledger.declaration(xid).is_some() || ledger.pair_flagged(xid, hid) {
                continue;
            }
            // keeping company: in the same domain (an aircraft high over a boat is not)
            if x.domain != h.domain {
                continue;
            }
            let Some(xv) = x.velocity_mps else { continue };
            let d = haversine_m(crate::engage::track_at(x, now), crate::engage::track_at(h, now));
            let rel = (xv[0] - hv[0]).hypot(xv[1] - hv[1]);
            if d <= cfg.escort_m() && rel <= cfg.escort_speed_mps() {
                // its own estimate, as the tracker has it
                let estimate = Estimate {
                    affiliation: x.affiliation.value,
                    confidence: x.affiliation_confidence,
                };
                out.push(Escort {
                    track: xid.clone(),
                    hostile: hid.clone(),
                    evidence: format!(
                        "{xid} ({} at {:.2}) keeps company with declared hostile {hid}: {d:.0} m from it (within {:.0} m), its velocity within {rel:.1} m/s of it (within {:.1}): weak evidence (how neutrals get misclassified), shown suspect, never declared on this",
                        estimate.affiliation.as_str(),
                        estimate.confidence,
                        cfg.escort_m(),
                        cfg.escort_speed_mps()
                    ),
                    estimate,
                });
            }
        }
    }
    out
}

/// The facts a proximity flag rests on: the two tracks, as the decision picture had them.
pub fn escort_inputs(
    tracks: &BTreeMap<String, TrackFacts>,
    track: &str,
    hostile: &str,
) -> BTreeMap<String, TrackFacts> {
    tracks
        .iter()
        .filter(|(id, _)| id.as_str() == track || id.as_str() == hostile)
        .map(|(id, t)| (id.clone(), t.clone()))
        .collect()
}

/// The track picture an identification decision read (an act's attribution, a proximity
/// flag's two tracks), with the ledger's identification filled in, as logged.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IdentInputs {
    pub t: Time,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tracks: BTreeMap<String, TrackFacts>,
    /// An act against a vehicle: our fleet then (the vehicles a victim may be).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub vehicles: BTreeSet<Uid>,
}

// ---- the ledger ------------------------------------------------------------------------------

/// The identification ledger: the declarations in force (by track), every flag with its
/// standing, the acts seen, and what an operator revoked. The engine keeps it as it logs, and
/// replay keeps the same from the log ([`Ledger::step_body`]).
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    declarations: BTreeMap<String, Declaration>,
    flags: BTreeMap<u64, (Flag, FlagStatus)>,
    acts: BTreeMap<String, SeenAct>,
    last_declaration: u64,
    last_flag: u64,
    revoked_by_operator: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
    revocations: Vec<Revocation>,
}

/// An act the ledger has seen: its best candidate and that attribution (none: no track), and
/// whether it may corroborate a later one: its verdict passed every check but corroboration
/// (a rule declaration, or a flag only for want of a second observation), so a victim not ours
/// or conflicting evidence never counts.
#[derive(Clone, Debug, PartialEq)]
pub struct SeenAct {
    pub t: Time,
    /// When our sensors observed it (its report's time).
    pub observed: Time,
    pub best: Option<(String, f64)>,
    pub eligible: bool,
}

/// A declaration an operator revoked, with who, when and why (the declarer's notice).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Revocation {
    pub declaration: Declaration,
    pub by: OperatorId,
    pub t: Time,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Ledger {
    /// The declaration in force on `track`.
    pub fn declaration(&self, track: &str) -> Option<&Declaration> {
        self.declarations.get(track)
    }

    pub fn declarations(&self) -> impl Iterator<Item = &Declaration> {
        self.declarations.values()
    }

    pub fn flag(&self, id: u64) -> Option<&(Flag, FlagStatus)> {
        self.flags.get(&id)
    }

    pub fn flags(&self) -> impl Iterator<Item = &(Flag, FlagStatus)> {
        self.flags.values()
    }

    pub fn seen_act(&self, act: &str) -> bool {
        self.acts.contains_key(act)
    }

    /// An earlier act whose best candidate was `track` at `threshold` or more (not `except`),
    /// eligible itself ([`SeenAct::eligible`]) and observed at most `window_s` before `now`
    /// (the corroborated act's observation): a second act corroborating a rule declaration
    /// (the latest such).
    pub fn corroborating_act(
        &self,
        track: &str,
        threshold: f64,
        except: &str,
        now: Time,
        window_s: f64,
    ) -> Option<Corroborating> {
        let window_ms = (window_s * 1000.0).round() as i64;
        self.acts
            .iter()
            .filter(|(id, a)| {
                id.as_str() != except
                    && a.eligible
                    && a.observed <= now
                    && now.ms_since(a.observed) <= window_ms
                    && a.best.as_ref().is_some_and(|(t, c)| t == track && *c >= threshold)
            })
            .max_by_key(|(_, a)| a.observed)
            .map(|(id, a)| Corroborating {
                act: id.clone(),
                observer: None,
                attribution: a.best.as_ref().map_or(0.0, |b| b.1),
            })
    }

    /// The declarations operators revoked, oldest first (for the declarer's notice).
    pub fn revocations(&self) -> &[Revocation] {
        &self.revocations
    }

    pub fn next_declaration(&self) -> u64 {
        self.last_declaration + 1
    }

    pub fn next_flag(&self) -> u64 {
        self.last_flag + 1
    }

    pub fn revoked_by_operator(&self, track: &str) -> bool {
        self.revoked_by_operator.contains(track)
    }

    fn pair_flagged(&self, track: &str, hostile: &str) -> bool {
        self.pairs.contains(&(track.to_string(), hostile.to_string()))
    }

    /// The flags in force (open or confirmed) naming `track`, by id.
    pub fn flags_on(&self, track: &str) -> Vec<u64> {
        self.flags
            .values()
            .filter(|(f, s)| s.in_force() && f.tracks.iter().any(|t| t == track))
            .map(|(f, _)| f.id)
            .collect()
    }

    /// Since when a flag in force shows `track` suspect (the earliest), if one does (display
    /// only: decisions read the track's own evidence).
    pub fn suspect_since(&self, track: &str) -> Option<Time> {
        self.flags
            .values()
            .filter(|(f, s)| s.in_force() && f.kind.raises_suspect() && f.tracks.iter().any(|t| t == track))
            .map(|(f, _)| f.t)
            .min()
    }

    /// The ledger after one logged event (at its time `t`), as logged (replay checks the
    /// events themselves).
    pub fn step(&mut self, t: Time, body: &EventBody) {
        match body {
            EventBody::IdentAct {
                act,
                candidates,
                verdict,
                ..
            } => {
                let eligible = matches!(
                    verdict,
                    ActVerdict::Declare { .. }
                        | ActVerdict::Flag {
                            kind: FlagKind::Uncorroborated,
                            ..
                        }
                );
                self.acts.insert(
                    act.act.clone(),
                    SeenAct {
                        t,
                        observed: act.observed_t,
                        best: candidates.first().map(|c| (c.track.clone(), c.confidence)),
                        eligible,
                    },
                );
            }
            EventBody::IdentDeclared { declaration, .. } => {
                self.last_declaration = self.last_declaration.max(declaration.id);
                if let Declarer::Operator {
                    operator,
                    flag: Some(f),
                    ..
                } = &declaration.by
                    && let Some((_, status)) = self.flags.get_mut(f)
                {
                    *status = FlagStatus::Declared {
                        by: operator.clone(),
                        t: declaration.t,
                        track: declaration.track.clone(),
                    };
                }
                self.revoked_by_operator.remove(&declaration.track);
                self.declarations
                    .insert(declaration.track.clone(), (**declaration).clone());
            }
            EventBody::IdentConfirmed {
                track, by, session, ..
            } => {
                if let Some(d) = self.declarations.get_mut(track) {
                    d.confirmed = Some(Confirmation {
                        by: by.clone(),
                        session: *session,
                        t,
                    });
                }
            }
            EventBody::IdentRevoked { track, by, note, .. } => {
                if let Some(d) = self.declarations.remove(track) {
                    self.revoked_by_operator.insert(track.clone());
                    self.revocations.push(Revocation {
                        declaration: d,
                        by: by.clone(),
                        t,
                        note: note.clone(),
                    });
                }
            }
            EventBody::IdentFlagged { flag, .. } => {
                self.last_flag = self.last_flag.max(flag.id);
                if flag.kind == FlagKind::Proximity
                    && let (Some(t), Some(h)) = (flag.tracks.first(), &flag.related)
                {
                    self.pairs.insert((t.clone(), h.clone()));
                }
                self.flags.insert(flag.id, (flag.clone(), FlagStatus::Open));
            }
            EventBody::IdentFlagResolved {
                flag, resolution, by, ..
            } => {
                if let Some((_, status)) = self.flags.get_mut(flag) {
                    *status = match resolution {
                        FlagResolution::Confirmed => FlagStatus::Confirmed { by: by.clone(), t },
                        FlagResolution::Dismissed => FlagStatus::Dismissed { by: by.clone(), t },
                    };
                }
            }
            _ => {}
        }
    }

    /// Fill the picture's identification from the ledger (what every decision reads, rules
    /// 9): a declared track's `declared_by`, `declaration`, its affiliation as declared (at the
    /// declaration's time, confidence 1); every track's flags in force. An undeclared track
    /// keeps the tracker's own affiliation (a proximity flag never masks it). What the bridge
    /// sent for the declaration fields is replaced: the ledger is the record.
    pub fn fill(&self, tracks: &mut BTreeMap<String, TrackFacts>) {
        for (id, t) in tracks.iter_mut() {
            self.fill_one(id, t);
        }
    }

    /// [`Self::fill`] for one track's facts (rules 16: a lost contact's, at its drop).
    pub fn fill_one(&self, id: &str, t: &mut TrackFacts) {
        let (declared_by, declaration, affiliation, confidence) = self.expected(id, t);
        t.declared_by = declared_by;
        t.declaration = declaration;
        t.affiliation = affiliation;
        t.affiliation_confidence = confidence;
        t.review_flags = self.flags_on(id);
    }

    /// What [`Self::fill`] puts in a track's identification fields.
    #[allow(clippy::type_complexity)]
    fn expected(
        &self,
        id: &str,
        t: &TrackFacts,
    ) -> (Option<String>, Option<Box<Declaration>>, Fact<Affiliation>, f64) {
        match self.declarations.get(id) {
            Some(d) => (
                Some(d.label()),
                Some(Box::new(d.clone())),
                Fact::new(d.affiliation, d.t),
                1.0,
            ),
            None => (None, None, t.affiliation.clone(), t.affiliation_confidence),
        }
    }

    /// Whether a logged picture's identification is the ledger's (rules 9): every track's
    /// `declared_by`, `declaration`, flags in force, and its affiliation where the ledger sets
    /// it (declared: as declared, confidence 1). `Err` names the first track that differs and
    /// how.
    pub fn check_facts(&self, tracks: &BTreeMap<String, TrackFacts>) -> Result<(), String> {
        for (id, t) in tracks {
            let (declared_by, declaration, affiliation, confidence) = self.expected(id, t);
            let flags = self.flags_on(id);
            if t.declared_by != declared_by || t.declaration != declaration {
                return Err(format!(
                    "{id}: declared by {:?} ({}), the declarations in force say {:?} ({})",
                    t.declared_by,
                    serde_json::to_string(&t.declaration).unwrap_or_default(),
                    declared_by,
                    serde_json::to_string(&declaration).unwrap_or_default()
                ));
            }
            if t.review_flags != flags {
                return Err(format!(
                    "{id}: review flags {:?}, the flags in force are {flags:?}",
                    t.review_flags
                ));
            }
            if t.affiliation != affiliation || t.affiliation_confidence != confidence {
                return Err(format!(
                    "{id}: affiliation {} at {} ({:.2}), the ledger gives {} at {} ({confidence:.2})",
                    t.affiliation.value.as_str(),
                    t.affiliation.t.0,
                    t.affiliation_confidence,
                    affiliation.value.as_str(),
                    affiliation.t.0
                ));
            }
        }
        Ok(())
    }
}

/// Rules 9: the engagements in play whose last picture held `track` exempt from its keep-out
/// of tracks and would fail it without the declaration (`before`: the ledger before the
/// revocation, `after`: after it), each with its approval snapshot and that picture's world
/// as logged. What a revocation of a bystander's declaration stops, besides the engagements
/// on the track itself. Rules 16: the picture's lost contacts are refilled too, so a declared
/// hostile the tracker dropped (exempt as it was live) is a bystander as well. Each engagement
/// comes with its platform (rules 20: a munition's strike sets when the keep-out is judged).
pub fn revocation_bystanders<'a>(
    in_play: impl Iterator<
        Item = (
            IntentId,
            &'a str,
            Option<&'a crate::ids::Uid>,
            &'a crate::engage::EngageSnapshot,
            &'a crate::world::WorldSnapshot,
        ),
    >,
    track: &str,
    before: &Ledger,
    after: &Ledger,
    cfg: &crate::rules::RulesConfig,
) -> Vec<IntentId> {
    let mut out: Vec<IntentId> = in_play
        .filter(|(_, target, _, _, _)| *target != track)
        .filter(|(_, _, uid, snap, w)| {
            let refill = |l: &Ledger| {
                let mut w = (*w).clone();
                l.fill(&mut w.tracks);
                for (id, c) in w.lost_contacts.iter_mut() {
                    l.fill_one(id, &mut c.facts);
                }
                w
            };
            let (pre, post) = (refill(before), refill(after));
            let pass = |w: &crate::world::WorldSnapshot| {
                crate::engage::picture_keep_out_tracks(*uid, snap, w, cfg).outcome == crate::rules::Outcome::Pass
            };
            pass(&pre) && !pass(&post)
        })
        .map(|(id, _, _, _, _)| id)
        .collect();
    out.sort();
    out
}

/// The engagements in play (pending, approved, dispatched) on `track`, by id: what a
/// revocation of its declaration stops, in id order.
pub fn engagements_on<'a>(
    intents: impl Iterator<Item = (IntentId, &'a crate::intent::IntentSpec, crate::intent::State)>,
    track: &str,
) -> Vec<IntentId> {
    use crate::intent::State;
    let mut out: Vec<IntentId> = intents
        .filter(|(_, s, state)| {
            matches!(state, State::PendingApproval | State::Approved | State::Dispatched)
                && crate::engage::params(s).is_some_and(|p| p.0 == track)
        })
        .map(|(id, _, _)| id)
        .collect();
    out.sort();
    out
}
