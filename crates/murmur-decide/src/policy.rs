//! Risk-tier policy (Phase C.3): `sim/policy.toml`, first match wins.
//!
//! A `[[tier]]` entry matches when every condition in its `when` table holds (an empty
//! `when` always matches). If nothing matches, the decision is `human`: fail closed.
//! Conditions that can't be evaluated resolve in the conservative direction: a vehicle
//! with unknown platform counts as armed, and with no area set loaded (or no fresh
//! position or home) every route counts as leaving the approved area.
//!
//! One floor holds whatever the file says: an intent with an `Unverified` check (a rule
//! that couldn't verify its condition, such as reserves with unknown endurance) is never
//! auto-approved. If the first matching tier is `auto`, the decision becomes `human` with
//! rule `"(unverified)"`. The `unverified = [...]` condition lets the file route such
//! intents explicitly, with its own reason.
//!
//! **Who authorizes a human tier.** A human tier sends an agent's proposal to the inbox,
//! where only a human approves it. A human operator's own submission is already a human
//! decision: when `submitter_authorizes` holds for the matched tier (per tier, else
//! `[approval] submitter_authorizes`, default true), the submission itself is the approval
//! ([`PolicyDecision::submitter_authorizes`]; the engine records it as
//! `approval.at_submission`). Set it false on a tier to make even the submitter's own intent
//! wait in the inbox; `approvers` and `proposer_may_approve` (below) make the inbox approval
//! two-person. It never applies to an agent-sourced intent. In a logged policy without the key (logs written before it
//! existed) it reads as false, which is how those runs behaved.
//!
//! **Two-person approval** (Phase E). A human tier may set `approvers = N` (N distinct
//! operators must sign before the intent is approved) and `proposer_may_approve = false`
//! (the human who proposed it can't be one of them). An engagement has a floor whatever the
//! file says: it is never auto-approved and never approved at submission, its proposer never
//! approves it, and at least two distinct approvers sign it (DoD Directive 3000.09: appropriate human
//! judgment over the use of force); only an operator session with engagement authority may
//! sign it (`crate::Engine::approve_engagement`). The floor is two approvers
//! ([`ENGAGE_MIN_APPROVERS`]); a file may ask for more, never fewer. An agent's order on a
//! vehicle with an engagement in play is never auto (`(engagement in play)`).
//!
//! **Step-up** (rules 8, [`crate::rules::OPERATOR_AUTH_SINCE`]). A human tier may set
//! `step_up = true`: the authorization is consequential, so in a credentials run every
//! approval of it re-enters the approver's credential at that moment
//! ([`PolicyDecision::step_up`]; the engine refuses a signature without a [`crate::StepUp`]),
//! and the submitter's own request is never the approval (it waits in the inbox). An
//! engagement always has it, whatever the file says. Rules 10 ([`crate::rules::PASSKEY_SINCE`],
//! policy schema 7): `[approval] step_up_factor` says what a person's step-up must be
//! (`password`, the default, takes the password or a passkey; `passkey` only a passkey bound to
//! the act) and `login_factor` how people sign in.
//!
//! **A supplement** (docs/SCENARIO-SETTINGS.md section 3). A policy file may say `extends =
//! "<file>"` (relative to its own directory) and set only what it changes; [`Policy::load`]
//! merges it over that file before reading either: tables merge key by key, a value or an array
//! replaces the one it inherits, and each of its `[[tier]]` entries names `before = "<an
//! inherited tier>"` and goes in just before it. It refuses a key set to the value it inherits
//! (a copy, which would hide what the file changes), a `before` naming no inherited tier, a tier
//! name already there, a cycle, and more than [`MAX_LAYERS`] files in one chain, each refusal
//! naming the file and the key. Neither `extends` nor `before` is part of the merged policy, so
//! a supplement logs exactly what the whole file it stands for would: what is logged is the
//! policy, not the files. [`Policy::load_layers`] also says which files it was built from
//! (`murmur-policy show`; from rules 33 `run.started`'s `policy_layers`).
//!
//! **The scenario's settings and their floor** (rules 33, [`crate::rules::SCENARIO_SETTINGS_SINCE`],
//! docs/SCENARIO-SETTINGS.md section 3). `[watch]` ([`crate::watch::WatchSettings`]),
//! `[rules.silent]` and `[rules.engage] min_identity_level` and `default_effect_radius_m` are
//! written out with their defaults when a file leaves them out, and logged from rules 33. A floor
//! no file loosens, refused at load and by replay on the logged copy: the engagement identity bar
//! (`min_identity_confidence`) at least the watch's `bar`, the engagement identity level
//! `identify`, no watch domain at `detect`; and the engagement floor [`decide`] holds whatever the
//! file says (never auto, never approved at submission, never by its proposer, two approvers at
//! least). A loosening only when named ([`LOOSENABLE`]): a file that sets
//! `identification.nominate_unconfirmed = true`, or `rules.engage.min_identity_confidence` below
//! what it extends gives (a file that extends none: below the code's 0.6), says so in `loosened =
//! { "<key>" = "<why>" }`, and an entry that names nothing this file loosens is refused.
//!
//! **A salvo's ceiling** (rules 35, [`crate::rules::SALVO_SINCE`], policy schema 10, docs/SALVO.md).
//! `[rules.engage] max_rounds_per_plan` (standing 1, the floor) and `salvo_spacing_s` (standing 5)
//! are written out with the engagement's other keys and logged from rules 35; a ceiling above
//! what a file extends is a loosening it names (`rules.engage.max_rounds_per_plan`), and a run of
//! older rules refuses either set to another value ([`Policy::fits_rules`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::time::Time;

use crate::areas::Area;
use crate::ids::Uid;
use crate::intent::{IntentSpec, Source, Task, TaskKind};
use crate::route::{Segment, route_for};
use crate::rules::{self, OPERATOR_AUTH_SINCE, Outcome, RuleId, RulesConfig};
use crate::world::WorldSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Approved by policy, no human needed.
    Auto,
    /// Waits in the inbox for a human.
    Human,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Human,
    Agent,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// Task kind is one of these.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Vec<TaskKind>>,
    /// Some target is (true) / no target is (false) on an armed platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed: Option<bool>,
    /// The task path leaves (true) / stays inside (false) the approved area.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaves_approved_area: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceKind>,
    /// Some check of one of these rules came out `unverified` (e.g. `["reserves"]`:
    /// endurance unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unverified: Option<Vec<RuleId>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TierRule {
    pub name: String,
    #[serde(default)]
    pub when: When,
    pub tier: Tier,
    pub reason: String,
    /// Human tiers: does a human's own submission authorize it (true), or does it wait for
    /// an approval like an agent's proposal (false)? Unset: `[approval]
    /// submitter_authorizes`. Ignored on auto tiers and for agent-sourced intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submitter_authorizes: Option<bool>,
    /// Human tiers: how many distinct operators must approve (default 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvers: Option<u32>,
    /// Human tiers: may the human who proposed the intent be one of its approvers (default
    /// true; an engagement is always false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposer_may_approve: Option<bool>,
    /// Human tiers, rules 8 (policy schema 5): a consequential authorization. Each approval
    /// needs a step-up, the approver's credential re-entered at the moment of approving, in
    /// a credentials run; the submitter's own request never stands as the approval
    /// (`submitter_authorizes` is false whatever this tier says). An engagement always has
    /// it, whatever this file says. Default false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_up: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalConfig {
    /// Pending approvals expire after this many seconds.
    pub expire_s: u32,
    /// Default for tiers that don't set `submitter_authorizes`, and for the `(unverified)`
    /// and `(none)` fallbacks. [`Policy::from_toml`] fills in true when the file leaves it
    /// out; a policy deserialized from an old log keeps it unset, which means false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitter_authorizes: Option<bool>,
    /// Per task kind: `[approval.kind.<kind>]` overrides `expire_s` or sets another
    /// deadline (`until = "tasking_lead"` for collect). Absent in logs written before it
    /// existed, which is how those runs behaved.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub kind: BTreeMap<TaskKind, KindApproval>,
    /// Rules 8 (policy schema 5): acts besides approvals that re-enter the operator's
    /// credential in a credentials run (a step-up): `declare_hostile` (an operator declares a
    /// track hostile), `accept_unknown` (an operator accepts a lost vehicle for an
    /// engagement). Absent: none (approvals step up per tier, `step_up`, and every engagement
    /// signature always does).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_up_acts: Option<Vec<StepUpAct>>,
    /// Rules 10 (policy schema 7): what a person's step-up must be in a credentials run.
    /// `password` (absent: the default) takes the password again or a passkey; `passkey`
    /// takes only a passkey (a WebAuthn assertion with user verification, bound to the act).
    /// A scripted credential (harness-held, never a person's) steps up with what it holds. A
    /// study run requires `passkey` unless its explicit override says otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_up_factor: Option<StepUpFactor>,
    /// Rules 10 (policy schema 7): how a person signs in. `either` (absent: the default): the
    /// password or a passkey; `password` or `passkey`: only that one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_factor: Option<LoginFactor>,
}

/// What a person's step-up must be (rules 10, `[approval] step_up_factor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepUpFactor {
    /// The password again, or a passkey (at least as strong).
    Password,
    /// A passkey only.
    Passkey,
}

impl StepUpFactor {
    pub fn as_str(self) -> &'static str {
        match self {
            StepUpFactor::Password => "password",
            StepUpFactor::Passkey => "passkey",
        }
    }
}

/// How a person may sign in (rules 10, `[approval] login_factor`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginFactor {
    Either,
    Password,
    Passkey,
}

impl LoginFactor {
    pub fn as_str(self) -> &'static str {
        match self {
            LoginFactor::Either => "either",
            LoginFactor::Password => "password",
            LoginFactor::Passkey => "passkey",
        }
    }

    /// Whether a person may open a session by `method` (a password or a passkey).
    pub fn admits(self, method: crate::authority::AuthMethod) -> bool {
        use crate::authority::AuthMethod;
        match (self, method) {
            (LoginFactor::Either, AuthMethod::Password | AuthMethod::Passkey) => true,
            (LoginFactor::Password, AuthMethod::Password) | (LoginFactor::Passkey, AuthMethod::Passkey) => true,
            // a scripted credential is a harness's: it signs in as it is held
            (_, AuthMethod::Scripted) => true,
            _ => false,
        }
    }
}

/// An act other than an approval that a policy may mark consequential (rules 8,
/// `[approval] step_up_acts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepUpAct {
    /// Declaring a track hostile (what makes it a target).
    DeclareHostile,
    /// Accepting a lost vehicle's unknown position for an engagement's keep-out.
    AcceptUnknown,
    /// Rules 9 (policy schema 6): confirming a track the hostile-act rule declared (it makes
    /// the declaration exempt in keep-outs and nominable whatever `nominate_unconfirmed`
    /// says). Listed only by choice: off by default.
    ConfirmDeclaration,
}

impl Default for ApprovalConfig {
    fn default() -> Self {
        ApprovalConfig {
            expire_s: 120,
            submitter_authorizes: None,
            kind: BTreeMap::new(),
            step_up_acts: None,
            step_up_factor: None,
            login_factor: None,
        }
    }
}

/// `[approval.kind.<kind>]`: how long a pending intent of that kind waits for a human.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindApproval {
    /// Seconds, instead of `[approval] expire_s` (also the fallback when `until` can't be
    /// computed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<Until>,
}

/// A deadline set by the task rather than a fixed wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Until {
    /// Collect: the last moment the chosen pass can still be tasked, the collect time minus
    /// the class's tasking lead time ([`crate::space::tasking_deadline`]). An approval
    /// after it would fail the lead-time check anyway; expiring there tells the operator
    /// the real deadline.
    TaskingLead,
    /// Engage: the engagement's window closes (or the fixed wait, if that comes first). An
    /// approval after the window would fail its reach check anyway.
    Window,
}

/// Why a pending intent's deadline is what it is, when it is not a fixed wait (logged in
/// `approval.requested`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadlineBasis {
    pub until: Until,
    /// The deadline in scenario time (engine `expires_t` is this mapped back through the
    /// segment's scenario map).
    pub scenario_t: Time,
}

/// When an intent sent to the inbox at engine time `now` expires: `[approval.kind.<kind>]`
/// if the policy has one, else `[approval] expire_s`. A collect with `until =
/// "tasking_lead"` and a matched option in `answer` expires at the tasking deadline (scenario
/// time, mapped to engine time through `map`); without one it falls back to the fixed wait.
pub fn approval_deadline(
    policy: &Policy,
    task: &crate::intent::Task,
    answer: Option<&crate::space::SpaceAnswer>,
    map: &crate::space::ScenarioMap,
    now: Time,
) -> (Time, Option<DeadlineBasis>) {
    let k = policy.approval.kind.get(&task.kind());
    let secs = k
        .and_then(|k| k.expire_s)
        .unwrap_or(policy.approval.expire_s);
    let fixed = now.plus_ms(i64::from(secs) * 1000);
    if k.and_then(|k| k.until) == Some(Until::Window)
        && let Task::Engage { window, .. } = task
    {
        // the window the nomination named, or the default from its creation (the engine
        // calls this just after creating it, so `now` stands in for the creation time)
        let until = window.map_or_else(
            || now.plus_ms(i64::from(policy.rules.engage_rules().default_window_s) * 1000),
            |w| w.until,
        );
        if until < fixed {
            return (
                until,
                Some(DeadlineBasis {
                    until: Until::Window,
                    scenario_t: until,
                }),
            );
        }
        return (fixed, None);
    }
    if k.and_then(|k| k.until) == Some(Until::TaskingLead) {
        let tol = policy.rules.space_rules().collect_match_tolerance_s;
        if let Some(s) = answer.and_then(|a| crate::space::tasking_deadline(task, a, tol)) {
            return (
                map.engine(s),
                Some(DeadlineBasis {
                    until: Until::TaskingLead,
                    scenario_t: s,
                }),
            );
        }
    }
    (fixed, None)
}

/// The whole policy file. Logged in full at run start; replay decides with the logged copy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    #[serde(default)]
    pub approval: ApprovalConfig,
    #[serde(default)]
    pub rules: RulesConfig,
    #[serde(rename = "tier")]
    pub tiers: Vec<TierRule>,
    /// `[queue]` (rules 5, policy schema 3): how long orders for unreachable vehicles are
    /// held, per kind, and when a vehicle counts as reachable. Absent: the defaults
    /// ([`crate::QueueConfig::default`]); absent from logs written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<crate::queue::QueueConfig>,
    /// `[identification]` (rules 9, policy schema 5): rule declaration on or off, the
    /// attribution threshold, whether a rule-declared track may be nominated before a
    /// confirmation (see [`crate::ident::IdentConfig`]). Absent: off, rules 7's behaviour;
    /// absent from logs written before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identification: Option<crate::ident::IdentConfig>,
    /// `[watch]` (rules 33, policy schema 8): the scenario's settings for the watch
    /// ([`crate::watch::WatchSettings`]); written out with the standing values when a file has
    /// none. Absent from logs of older rules ([`Policy::as_logged`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<crate::watch::WatchSettings>,
    /// `loosened` (rules 33, policy schema 8): each key a file sets looser than what it extends,
    /// with the reason it gives ([`LOOSENABLE`]). Absent from logs of older rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loosened: Option<BTreeMap<String, String>>,
    /// `[group]` (rules 34, policy schema 9): a group order's figures where its proposal names
    /// none ([`crate::group::GroupPolicy`]). Logged from rules 34 only, with the standing values
    /// when the file has none ([`Policy::as_logged`]); a run of older rules refuses a file whose
    /// figures differ from the standing ones ([`Policy::fits_rules`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<crate::group::GroupPolicy>,
}

/// The keys a policy file may set looser than the file it extends (a file that extends none:
/// than the code's defaults) only by naming them in `loosened` with a reason: a rule-declared
/// track nominated before an operator confirms it, a lower engagement identity bar, and (rules 35)
/// a higher ceiling on the rounds one signed plan may release and a shorter least spacing between
/// them (how fast one decision releases).
pub const LOOSENABLE: [&str; 4] = [
    "identification.nominate_unconfirmed",
    "rules.engage.min_identity_confidence",
    "rules.engage.max_rounds_per_plan",
    "rules.engage.salvo_spacing_s",
];

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("policy: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("policy {0}: {1}")]
    Io(String, std::io::Error),
    #[error("policy: {0}")]
    Invalid(String),
    /// A file that extends another, or one it extends, refused: the file (as its child names
    /// it) and why, starting with the key.
    #[error("policy {file}: {why}")]
    Layer { file: String, why: String },
}

/// The most files one policy may be built from: the file given and those it extends, in turn
/// (a scenario's supplement on the watch policy on the standing values is three).
pub const MAX_LAYERS: usize = 4;

/// One file a policy was built from ([`Policy::load_layers`]): its path as written (the file
/// given as it was given, each further one as the `extends` naming it says) and the SHA-256 of
/// its bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    pub path: String,
    pub sha256: String,
}

impl Layer {
    /// The layer as `run.started` logs it (rules 33, `policy_layers`): never an absolute path,
    /// which would name the machine and the checkout. A file given by an absolute path is named
    /// relative to the working directory when it is under it, else by its file name; the
    /// SHA-256 says which file it was either way.
    pub fn for_log(&self) -> Layer {
        let p = Path::new(&self.path);
        if !p.is_absolute() {
            return self.clone();
        }
        let shown = std::env::current_dir()
            .ok()
            .and_then(|cwd| p.strip_prefix(cwd).ok().map(Path::to_path_buf))
            .or_else(|| p.file_name().map(PathBuf::from))
            .map_or_else(|| self.path.clone(), |q| q.display().to_string());
        Layer {
            path: shown,
            sha256: self.sha256.clone(),
        }
    }
}

impl Policy {
    pub fn from_toml(text: &str) -> Result<Self, PolicyError> {
        let p: Policy = toml::from_str(text)?;
        // rules 33: a file that extends none is compared with the code's defaults
        let table: toml::Table = toml::from_str(text)?;
        check_loosened(&table, None, table.get("loosened"), SAFE_VALUES).map_err(PolicyError::Invalid)?;
        Self::finish(p)
    }

    /// What every file gets once read: its defaults written out, its bounds checked, synced.
    fn finish(mut p: Policy) -> Result<Self, PolicyError> {
        // The file default: a human's own submission authorizes it. Written out explicitly
        // so the logged copy says so (an unset value in a log means false).
        p.approval.submitter_authorizes.get_or_insert(true);
        // rules 33: the scenario's settings, the standing values where the file sets none,
        // written out so a run logs what it decided under (as_logged drops them under older
        // rules); the engagement's only where the file has a [rules.engage]
        p.watch.get_or_insert_with(crate::watch::WatchSettings::default);
        p.rules.silent.get_or_insert_with(crate::silent::SilentRules::default);
        if let Some(e) = p.rules.engage.as_mut() {
            e.min_identity_level.get_or_insert(crate::identify::Level::Identify);
            e.default_effect_radius_m.get_or_insert(crate::engage::DEFAULT_EFFECT_RADIUS_M);
            // rules 35: the salvo's ceiling and spacing (as_logged drops them under older rules)
            e.max_rounds_per_plan.get_or_insert(crate::engage::STANDING_MAX_ROUNDS);
            e.salvo_spacing_s.get_or_insert(crate::engage::STANDING_SALVO_SPACING);
        }
        p.validate()?;
        p.sync();
        Ok(p)
    }

    /// The policy as a run deciding under its own rules version logs it in `run.started` and
    /// decides with it ([`crate::Engine::start`]): without what older rules never had (rules
    /// 9's `[identification]`, rules 10's step-up and login factors, rules 34's `[group]` and a
    /// tier's `transit`; no build of theirs logged one), synced.
    pub fn as_logged(mut self) -> Self {
        if !self.rules.rule_id() {
            self.identification = None;
        }
        if !self.rules.passkeys() {
            self.approval.step_up_factor = None;
            self.approval.login_factor = None;
        }
        // rules 34: a group order's figures, logged from rules 34 only (the standing ones where
        // the file has none)
        if self.rules.group_orders() {
            self.group.get_or_insert_with(crate::group::GroupPolicy::default);
        } else {
            self.group = None;
            // and a tier's transit, which older rules refuse before the policy is asked: a run
            // asked for them logs the tiers their builds logged
            for t in &mut self.tiers {
                if let Some(ks) = t.when.kind.as_mut() {
                    ks.retain(|k| *k != TaskKind::Transit);
                }
            }
        }
        // rules 33: the scenario's settings, logged from rules 33 only (a run of older rules
        // logs the policy it always did, and decides as it always did); from 33 always logged
        if self.rules.scenario_settings() {
            self.watch.get_or_insert_with(crate::watch::WatchSettings::default);
        } else {
            self.watch = None;
            self.loosened = None;
            self.rules.silent = None;
            if let Some(e) = self.rules.engage.as_mut() {
                e.min_identity_level = None;
                e.default_effect_radius_m = None;
            }
        }
        // rules 35: the salvo's ceiling and spacing, logged from rules 35 only
        if !self.rules.salvo()
            && let Some(e) = self.rules.engage.as_mut()
        {
            e.max_rounds_per_plan = None;
            e.salvo_spacing_s = None;
        }
        self.sync();
        self
    }

    /// Copy what the rules read from other sections into [`RulesConfig`] (rules 9: the
    /// `[identification]` settings the engagement checks read; rules 33: `[watch]`). The engine
    /// and replay call it on the policy they decide with.
    pub fn sync(&mut self) {
        self.rules.identification = self.identification.clone().unwrap_or_default();
        self.rules.watch = self.watch.clone().unwrap_or_default();
    }

    /// The watch settings a run of this policy decides under, as MCP `get_policy` and
    /// `murmur-policy show` serve them: the policy's `[watch]` from rules 33 (the standing values
    /// where it has none); under older rules what those rules meant,
    /// [`crate::watch::WatchSettings::legacy`], with the scenario's `[watch.area]`.
    pub fn effective_watch(&self) -> crate::watch::WatchSettings {
        let own = self.watch.clone().unwrap_or_default();
        if self.rules.scenario_settings() {
            own
        } else {
            crate::watch::WatchSettings {
                area: own.area,
                ..crate::watch::WatchSettings::legacy().clone()
            }
        }
    }

    /// Whether a run under this policy's rules version can decide with it as written: under rules
    /// below 33 nothing reads the scenario's settings, so a policy whose `[watch]` (the area
    /// aside), `[rules.silent]` or `[rules.engage] min_identity_level` or
    /// `default_effect_radius_m` differ from the standing values is refused, saying which, rather
    /// than run on values it would not use. [`crate::Engine::start`] asks it before it logs
    /// anything.
    pub fn fits_rules(&self) -> Result<(), PolicyError> {
        self.fits_salvo()?;
        // rules 34: a group order's figures are read from rules 34 only
        if !self.rules.group_orders()
            && self.group.as_ref().is_some_and(|g| *g != crate::group::GroupPolicy::default())
        {
            return Err(PolicyError::Invalid(format!(
                "group.min_share differs from the standing value, and decision rules {} take no group order (rules {} do): run under rules {} (MURMUR_RULES), or leave [group] out",
                self.rules.rules_version,
                crate::rules::GROUP_ORDER_SINCE,
                crate::rules::GROUP_ORDER_SINCE,
            )));
        }
        if self.rules.scenario_settings() {
            return Ok(());
        }
        let mut differ = self
            .watch
            .as_ref()
            .map(|w| w.differs_from(&crate::watch::WatchSettings::default()))
            .unwrap_or_default();
        if self.rules.silent.as_ref().is_some_and(|s| *s != crate::silent::SilentRules::default()) {
            differ.push("rules.silent.drift_mps".into());
        }
        if let Some(e) = &self.rules.engage {
            if e.min_identity_level() != crate::identify::Level::Identify {
                differ.push("rules.engage.min_identity_level".into());
            }
            if e.default_effect_radius() != crate::engage::DEFAULT_EFFECT_RADIUS_M {
                differ.push("rules.engage.default_effect_radius_m".into());
            }
        }
        if differ.is_empty() {
            return Ok(());
        }
        let (one, it) = if differ.len() == 1 { ("differs", "it") } else { ("differ", "them") };
        Err(PolicyError::Invalid(format!(
            "{} {one} from the standing values (sim/policy.toml), and decision rules {} do not read the scenario's settings (rules {} do): run under rules {} (MURMUR_RULES), or leave {it} as sim/policy.toml has {it}",
            differ.join(", "),
            self.rules.rules_version,
            crate::rules::SCENARIO_SETTINGS_SINCE,
            crate::rules::SCENARIO_SETTINGS_SINCE,
        )))
    }

    /// Rules 35: under rules below 35 nothing reads the salvo's ceiling or spacing, so a policy
    /// that sets either to another value than the standing one is refused, saying which (as
    /// [`Policy::fits_rules`] refuses rules 33's settings under older rules).
    fn fits_salvo(&self) -> Result<(), PolicyError> {
        if self.rules.salvo() {
            return Ok(());
        }
        let Some(e) = &self.rules.engage else {
            return Ok(());
        };
        let mut differ = Vec::new();
        if e.max_rounds() != crate::engage::STANDING_MAX_ROUNDS {
            differ.push("rules.engage.max_rounds_per_plan");
        }
        if e.salvo_spacing() != crate::engage::STANDING_SALVO_SPACING {
            differ.push("rules.engage.salvo_spacing_s");
        }
        if differ.is_empty() {
            return Ok(());
        }
        let (one, it) = if differ.len() == 1 { ("differs", "it") } else { ("differ", "them") };
        Err(PolicyError::Invalid(format!(
            "{} {one} from the standing values (sim/policy.toml), and decision rules {} do not read a salvo's settings (rules {} do): run under rules {} (MURMUR_RULES), or leave {it} as sim/policy.toml has {it}",
            differ.join(", "),
            self.rules.rules_version,
            crate::rules::SALVO_SINCE,
            crate::rules::SALVO_SINCE,
        )))
    }

    /// `[group]`, or its standing values (rules 34).
    pub fn group(&self) -> crate::group::GroupPolicy {
        self.group.clone().unwrap_or_default()
    }

    /// `[identification]`, or its defaults (rule declaration off).
    pub fn identification(&self) -> crate::ident::IdentConfig {
        self.identification.clone().unwrap_or_default()
    }

    /// Every bound a policy file must meet (what [`Policy::from_toml`] checks after parsing).
    /// Replay runs it on the logged policy too, so a log can't carry thresholds no file
    /// could. Rules 33's floor is here (the engagement identity bar at least the watch's, its
    /// level `identify`, no watch domain at `detect`, a loosening only of a [`LOOSENABLE`] key
    /// with a reason); the engagement floor (two approvers, never auto, never at submission,
    /// never the proposer) is not a bound here: `policy::decide` applies it whatever the file
    /// says.
    pub fn validate(&self) -> Result<(), PolicyError> {
        let p = self;
        if p.tiers.is_empty() {
            return Err(PolicyError::Invalid("no [[tier]] rules".into()));
        }
        if p.approval.expire_s == 0 {
            return Err(PolicyError::Invalid("approval.expire_s must be > 0".into()));
        }
        for (kind, k) in &p.approval.kind {
            if k.expire_s == Some(0) {
                return Err(PolicyError::Invalid(format!(
                    "approval.kind.{}.expire_s must be > 0",
                    kind.as_str()
                )));
            }
            if k.until == Some(Until::TaskingLead) && *kind != TaskKind::Collect {
                return Err(PolicyError::Invalid(format!(
                    "approval.kind.{}: until = \"tasking_lead\" applies to collect only",
                    kind.as_str()
                )));
            }
            if k.until == Some(Until::Window) && *kind != TaskKind::Engage {
                return Err(PolicyError::Invalid(format!(
                    "approval.kind.{}: until = \"window\" applies to engage only",
                    kind.as_str()
                )));
            }
        }
        let r = &p.rules;
        if !(r.orbit_radius_margin.is_finite() && (1.0..=5.0).contains(&r.orbit_radius_margin)) {
            return Err(PolicyError::Invalid(
                "rules.orbit_radius_margin must be within 1..5".into(),
            ));
        }
        if !(r.reserve_pct.is_finite() && (0.0..=100.0).contains(&r.reserve_pct)) {
            return Err(PolicyError::Invalid(
                "rules.reserve_pct must be 0..100".into(),
            ));
        }
        // freshness: what every rule (and an engagement's keep-outs) trusts a fact for
        if !(1..=120).contains(&r.max_fact_age_s) {
            return Err(PolicyError::Invalid(format!(
                "rules.max_fact_age_s must be within 1..120, got {}",
                r.max_fact_age_s
            )));
        }
        if let Some(n) = r.fact_age_intervals
            && !(1..=10).contains(&n)
        {
            return Err(PolicyError::Invalid(format!(
                "rules.fact_age_intervals must be within 1..10, got {n}"
            )));
        }
        if !(r.fixed_wing_default_min_turn_radius_m.is_finite()
            && (5.0..=2000.0).contains(&r.fixed_wing_default_min_turn_radius_m))
        {
            return Err(PolicyError::Invalid(
                "rules.fixed_wing_default_min_turn_radius_m must be within 5..2000".into(),
            ));
        }
        if !(1..=86_400).contains(&r.task_duration_s) {
            return Err(PolicyError::Invalid(
                "rules.task_duration_s must be within 1..86400".into(),
            ));
        }
        if let Some(s) = &r.space {
            s.validate().map_err(PolicyError::Invalid)?;
        }
        if let Some(e) = &r.engage {
            e.validate().map_err(PolicyError::Invalid)?;
        }
        if let Some(g) = &p.group {
            g.validate().map_err(PolicyError::Invalid)?;
        }
        if let Some(q) = &p.queue {
            q.validate().map_err(PolicyError::Invalid)?;
        }
        if let Some(i) = &p.identification {
            i.validate().map_err(PolicyError::Invalid)?;
        }
        // rules 33: the scenario's settings, and the floor no file loosens
        if let Some(s) = &r.silent {
            s.validate().map_err(PolicyError::Invalid)?;
        }
        if let Some(w) = &p.watch {
            w.validate().map_err(PolicyError::Invalid)?;
            let bar = r.engage_rules().min_identity_confidence;
            if bar < w.bar {
                return Err(PolicyError::Invalid(format!(
                    "rules.engage.min_identity_confidence = {bar} is below watch.bar = {}: an engagement's target is identified at least as surely as the watch counts a contact identified (the floor, no override)",
                    w.bar
                )));
            }
        }
        if let Some(lo) = &p.loosened {
            for (key, why) in lo {
                if !LOOSENABLE.contains(&key.as_str()) {
                    return Err(PolicyError::Invalid(format!(
                        "loosened: {key:?} is not a key a policy loosens ({})",
                        LOOSENABLE.join(", ")
                    )));
                }
                if why.trim().is_empty() {
                    return Err(PolicyError::Invalid(format!("loosened: {key:?} gives no reason")));
                }
            }
        }
        for t in &p.tiers {
            if t.approvers == Some(0) {
                return Err(PolicyError::Invalid(format!(
                    "tier {}: approvers must be at least 1",
                    t.name
                )));
            }
            if t.step_up == Some(true) && t.tier != Tier::Human {
                return Err(PolicyError::Invalid(format!(
                    "tier {}: step_up applies to human tiers (an auto tier has no approval to step up)",
                    t.name
                )));
            }
        }
        Ok(())
    }

    /// Rules 33: each [`LOOSENABLE`] key this policy (a merged one, as logged) sets looser than
    /// the code's defaults that its `loosened` does not name. A file names each loosening it
    /// makes and its children inherit the names, so a merged policy names every loosening it
    /// holds; replay holds a log of rules 33 to it (the logged copy is checked, not the files).
    pub fn unnamed_loosenings(&self) -> Vec<&'static str> {
        let [nominate, confidence, rounds, spacing] = LOOSENABLE;
        let named = |k: &str| self.loosened.as_ref().is_some_and(|l| l.get(k).is_some_and(|why| !why.trim().is_empty()));
        let mut out = Vec::new();
        if self.identification.as_ref().is_some_and(crate::ident::IdentConfig::nominate_unconfirmed) && !named(nominate) {
            out.push(nominate);
        }
        let safe = crate::engage::EngageRules::default().min_identity_confidence;
        if self.rules.engage.as_ref().is_some_and(|e| e.min_identity_confidence < safe) && !named(confidence) {
            out.push(confidence);
        }
        // rules 35: a ceiling above one round a plan
        if self.rules.engage.as_ref().is_some_and(|e| e.max_rounds() > crate::engage::STANDING_MAX_ROUNDS) && !named(rounds) {
            out.push(rounds);
        }
        // ... and rounds released closer together than the standing 5 s
        if self.rules.engage.as_ref().is_some_and(|e| e.salvo_spacing() < crate::engage::STANDING_SALVO_SPACING) && !named(spacing) {
            out.push(spacing);
        }
        out
    }

    /// The policy file at `path`, merged over the files it extends if it says `extends`.
    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        Self::load_layers(path).map(|(p, _)| p)
    }

    /// [`Policy::load`], and the files the policy was built from: `path` first, then each file
    /// it extends, in turn. A file that extends none is read exactly as it always was.
    pub fn load_layers(path: &Path) -> Result<(Self, Vec<Layer>), PolicyError> {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|e| PolicyError::Io(shown.clone(), e))?;
        let table: toml::Table = toml::from_str(&text)?;
        let mut layers = vec![Layer {
            path: shown.clone(),
            sha256: crate::hash::sha256_hex(text.as_bytes()),
        }];
        if !table.contains_key("extends") {
            no_before(&table, &shown)?;
            check_loosened(&table, None, table.get("loosened"), SAFE_VALUES).map_err(|why| refused(&shown, why))?;
            return Ok((Self::from_toml(&text)?, layers));
        }
        let own = std::fs::canonicalize(path).map_err(|e| PolicyError::Io(shown.clone(), e))?;
        let merged = layered(table, path, &shown, &mut layers, &mut vec![own])?;
        let p: Policy = toml::Value::Table(merged).try_into().map_err(|e: toml::de::Error| PolicyError::Layer {
            file: shown.clone(),
            why: format!("merged with the files it extends: {}", e.to_string().trim_end().replace('\n', " ")),
        })?;
        let p = Self::finish(p).map_err(|e| PolicyError::Layer {
            file: shown,
            why: format!(
                "merged with the files it extends: {}",
                match e {
                    PolicyError::Invalid(why) => why,
                    e => e.to_string(),
                }
            ),
        })?;
        Ok((p, layers))
    }

    /// `[queue]`, or its defaults.
    pub fn queue(&self) -> crate::queue::QueueConfig {
        self.queue.clone().unwrap_or_default()
    }

    /// Rules 8: whether `act` re-enters the operator's credential (`[approval] step_up_acts`;
    /// nothing before rules 8).
    pub fn steps_up(&self, act: StepUpAct) -> bool {
        self.rules.rules_version >= OPERATOR_AUTH_SINCE
            && self.approval.step_up_acts.as_ref().is_some_and(|a| a.contains(&act))
    }

    /// Rules 10: what a person's step-up must be (`[approval] step_up_factor`; a password, or
    /// a passkey, before rules 10 and when the key is absent).
    pub fn step_up_factor(&self) -> StepUpFactor {
        if self.rules.passkeys() {
            self.approval.step_up_factor.unwrap_or(StepUpFactor::Password)
        } else {
            StepUpFactor::Password
        }
    }

    /// Rules 10: how a person signs in (`[approval] login_factor`; the password only before
    /// rules 10, either when the key is absent).
    pub fn login_factor(&self) -> LoginFactor {
        if self.rules.passkeys() {
            self.approval.login_factor.unwrap_or(LoginFactor::Either)
        } else {
            LoginFactor::Password
        }
    }

    /// Whether a human's own submission authorizes an intent routed to `tier` (`None`: the
    /// `(unverified)` and `(none)` fallbacks, which use the `[approval]` default).
    pub fn submitter_authorizes(&self, tier: Option<&TierRule>) -> bool {
        tier.and_then(|t| t.submitter_authorizes)
            .or(self.approval.submitter_authorizes)
            .unwrap_or(false)
    }
}

// ---- supplements: one file over another ----------------------------------------------------------

fn refused(file: &str, why: String) -> PolicyError {
    PolicyError::Layer { file: file.to_string(), why }
}

/// The policy file `table` (read from `path`, named `shown`) merged over the file it extends,
/// itself merged over its own, and so on: the standing values first, each supplement over
/// them. `layers` gains each file read; `chain` holds the files of this chain so far
/// (canonical paths), the file being read last.
fn layered(
    mut table: toml::Table,
    path: &Path,
    shown: &str,
    layers: &mut Vec<Layer>,
    chain: &mut Vec<PathBuf>,
) -> Result<toml::Table, PolicyError> {
    let Some(extends) = table.remove("extends") else {
        no_before(&table, shown)?;
        check_loosened(&table, None, table.get("loosened"), SAFE_VALUES).map_err(|why| refused(shown, why))?;
        return Ok(table);
    };
    let toml::Value::String(rel) = extends else {
        return Err(refused(shown, "`extends` names the file this one supplements, a path relative to it".into()));
    };
    if layers.len() >= MAX_LAYERS {
        return Err(refused(
            shown,
            format!("`extends` = {rel:?}: a policy is built from {MAX_LAYERS} files at most"),
        ));
    }
    let parent = path.parent().unwrap_or(Path::new(".")).join(&rel);
    let text = std::fs::read_to_string(&parent).map_err(|e| refused(shown, format!("`extends` = {rel:?}: {e}")))?;
    let canonical = std::fs::canonicalize(&parent).map_err(|e| refused(shown, format!("`extends` = {rel:?}: {e}")))?;
    if let Some(i) = chain.iter().position(|c| *c == canonical) {
        let cycle: Vec<String> = chain[i..].iter().chain([&canonical]).map(|c| c.display().to_string()).collect();
        return Err(refused(shown, format!("`extends` = {rel:?}: a cycle ({})", cycle.join(" extends "))));
    }
    let base: toml::Table =
        toml::from_str(&text).map_err(|e| refused(&rel, e.to_string().trim_end().replace('\n', " ")))?;
    layers.push(Layer {
        path: rel.clone(),
        sha256: crate::hash::sha256_hex(text.as_bytes()),
    });
    chain.push(canonical);
    let mut merged = layered(base, &parent, &rel, layers, chain)?;
    let inherited = merged.clone();
    let own_loosened = table.get("loosened").cloned();
    for (key, value) in table {
        if key == "tier" {
            insert_tiers(&mut merged, value, shown, &rel)?;
        } else {
            merge_key(&mut merged, key.clone(), value, &key, shown, &rel)?;
        }
    }
    // rules 33: what this file loosens against what it extends, each named with its reason
    check_loosened(&merged, Some(&inherited), own_loosened.as_ref(), &rel).map_err(|why| refused(shown, why))?;
    Ok(merged)
}

/// What a file that extends none is compared with: the code's own values.
const SAFE_VALUES: &str = "the code's default";

/// Each [`LOOSENABLE`] key `policy` (a merged policy table) sets looser than `parent` (None: the
/// code's defaults), with what it sets and what it loosens, in [`LOOSENABLE`]'s order.
fn loosenings(policy: &toml::Table, parent: Option<&toml::Table>) -> Vec<(&'static str, String, String)> {
    fn at<'a>(t: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
        let mut parts = path.split('.');
        let mut v = t.get(parts.next()?)?;
        for p in parts {
            v = v.as_table()?.get(p)?;
        }
        Some(v)
    }
    let flag = |t: Option<&toml::Table>, path: &str| t.and_then(|t| at(t, path)).and_then(toml::Value::as_bool).unwrap_or(false);
    let number = |t: Option<&toml::Table>, path: &str, default: f64| {
        t.and_then(|t| at(t, path))
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(default)
    };
    let mut out = Vec::new();
    let [nominate, confidence, rounds, spacing] = LOOSENABLE;
    if flag(Some(policy), nominate) && !flag(parent, nominate) {
        out.push((nominate, "true".to_string(), "false".to_string()));
    }
    let safe = crate::engage::EngageRules::default().min_identity_confidence;
    let (own, theirs) = (number(Some(policy), confidence, safe), number(parent, confidence, safe));
    if own < theirs {
        out.push((confidence, own.to_string(), theirs.to_string()));
    }
    // rules 35: more rounds one signed plan may release than what it extends allows
    let one = f64::from(crate::engage::STANDING_MAX_ROUNDS);
    let (own, theirs) = (number(Some(policy), rounds, one), number(parent, rounds, one));
    if own > theirs {
        out.push((rounds, own.to_string(), theirs.to_string()));
    }
    // ... and rounds released closer together
    let five = crate::engage::STANDING_SALVO_SPACING;
    let (own, theirs) = (number(Some(policy), spacing, five), number(parent, spacing, five));
    if own < theirs {
        out.push((spacing, own.to_string(), theirs.to_string()));
    }
    out
}

/// Rules 33: a file names each loosening it makes in its own `loosened` (`own`), with a reason,
/// and names nothing else. `policy` is the file merged over what it extends, `parent` what it
/// extends merged (None: a file that extends none, compared with the code's defaults), named
/// `inherited` in the refusal.
fn check_loosened(
    policy: &toml::Table,
    parent: Option<&toml::Table>,
    own: Option<&toml::Value>,
    inherited: &str,
) -> Result<(), String> {
    let named: BTreeMap<String, toml::Value> = match own {
        None => BTreeMap::new(),
        Some(toml::Value::Table(t)) => t.clone().into_iter().collect(),
        Some(v) => {
            return Err(format!(
                "`loosened` = {v}: a table of reasons by key, loosened = {{ \"<key>\" = \"<why>\" }}"
            ));
        }
    };
    let made = loosenings(policy, parent);
    for (key, sets, gives) in &made {
        match named.get(*key) {
            Some(toml::Value::String(why)) if !why.trim().is_empty() => {}
            Some(_) => return Err(format!("`loosened.\"{key}\"`: the reason, a sentence")),
            None => {
                return Err(format!(
                    "`{key}` = {sets} is looser than {inherited} ({gives}): a file that loosens a key names it with its reason, loosened = {{ \"{key}\" = \"<why>\" }}"
                ));
            }
        }
    }
    if let Some(key) = named.keys().find(|k| !made.iter().any(|(m, _, _)| m == k)) {
        return Err(if LOOSENABLE.contains(&key.as_str()) {
            format!("`loosened` names {key:?}, which this file does not set looser than {inherited}: a loosening is named only where one is made")
        } else {
            format!("`loosened` names {key:?}, which is not a key a policy loosens ({})", LOOSENABLE.join(", "))
        });
    }
    Ok(())
}

/// A file that extends none has no tier to place among inherited ones.
fn no_before(table: &toml::Table, shown: &str) -> Result<(), PolicyError> {
    let tiers = table.get("tier").and_then(toml::Value::as_array).into_iter().flatten();
    if let Some(t) = tiers.filter_map(toml::Value::as_table).find(|t| t.contains_key("before")) {
        return Err(refused(
            shown,
            format!(
                "tier {}: `before` places a tier among the ones a file inherits, and this file extends none",
                tier_name(t).unwrap_or("(no name)")
            ),
        ));
    }
    Ok(())
}

/// Set `key` (at `at`, its dotted path) of the inherited table to `value`: a table into a table
/// key by key, anything else in place of what it inherits. A value equal to the inherited one
/// is refused: a supplement sets only what it changes.
fn merge_key(
    base: &mut toml::Table,
    key: String,
    value: toml::Value,
    at: &str,
    shown: &str,
    inherited: &str,
) -> Result<(), PolicyError> {
    if let (Some(toml::Value::Table(b)), toml::Value::Table(over)) = (base.get_mut(&key), &value) {
        for (k, v) in over.clone() {
            let path = format!("{at}.{k}");
            merge_key(b, k, v, &path, shown, inherited)?;
        }
        return Ok(());
    }
    if base.get(&key).is_some_and(|b| same(b, &value)) {
        return Err(refused(
            shown,
            format!("`{at}` = {value} is what {inherited} gives it already: a supplement sets only what it changes"),
        ));
    }
    base.insert(key, value);
    Ok(())
}

/// A supplement's `[[tier]]` entries, each just before the inherited tier its `before` names,
/// in the file's order (`before` dropped).
fn insert_tiers(base: &mut toml::Table, value: toml::Value, shown: &str, inherited: &str) -> Result<(), PolicyError> {
    let toml::Value::Array(new) = value else {
        return Err(refused(shown, "`tier`: a supplement adds tiers as [[tier]] entries".into()));
    };
    let Some(toml::Value::Array(tiers)) = base.get_mut("tier") else {
        return Err(refused(shown, format!("[[tier]]: {inherited} has no tiers to place one before")));
    };
    let names: Vec<String> = tiers
        .iter()
        .filter_map(|t| t.as_table().and_then(tier_name))
        .map(str::to_string)
        .collect();
    for t in new {
        let toml::Value::Table(mut t) = t else {
            return Err(refused(shown, "`tier`: a supplement adds tiers as [[tier]] entries".into()));
        };
        let Some(name) = tier_name(&t).map(str::to_string) else {
            return Err(refused(shown, "[[tier]]: a tier without a name".into()));
        };
        let before = match t.remove("before") {
            Some(toml::Value::String(b)) => b,
            Some(_) => return Err(refused(shown, format!("tier {name}: `before` names an inherited tier"))),
            None => {
                return Err(refused(
                    shown,
                    format!("tier {name}: a supplement's tier says where it goes, before = \"<a tier of {inherited}>\""),
                ));
            }
        };
        if tiers.iter().any(|x| x.as_table().and_then(tier_name) == Some(name.as_str())) {
            return Err(refused(shown, format!("tier {name}: there is a tier of that name already")));
        }
        if !names.contains(&before) {
            return Err(refused(
                shown,
                format!("tier {name}: `before` = {before:?} names no tier of {inherited}"),
            ));
        }
        let at = tiers
            .iter()
            .position(|x| x.as_table().and_then(tier_name) == Some(before.as_str()))
            .unwrap_or(tiers.len());
        tiers.insert(at, toml::Value::Table(t));
    }
    Ok(())
}

fn tier_name(t: &toml::Table) -> Option<&str> {
    t.get("name").and_then(toml::Value::as_str)
}

/// The same value as the policy reads it: an integer and a float of one value are the same
/// number (`reserve_pct = 20` is `20.0`).
fn same(a: &toml::Value, b: &toml::Value) -> bool {
    use toml::Value::{Array, Float, Integer, Table};
    match (a, b) {
        (Integer(i), Float(f)) | (Float(f), Integer(i)) => *i as f64 == *f,
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Table(x), Table(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

/// The facts the policy conditions were evaluated on, kept with the decision so the
/// inbox can show why a human is needed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyFacts {
    /// For collect and cover (no vehicles): `armed` is empty and `leaves_approved_area`
    /// false ("no path"); a satellite collect over any area is routed by `kind`.
    pub kind: TaskKind,
    pub source: SourceKind,
    /// Targets on armed platforms (or with unknown platform, counted as armed).
    pub armed: Vec<Uid>,
    pub leaves_approved_area: bool,
    pub area_detail: String,
    /// Checks a human must judge (outcome `unverified`), as `"rule uid: detail"`. Shown in
    /// the inbox as the reason the intent needs a human.
    pub unverified: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub tier: Tier,
    /// Name of the matching `[[tier]]` entry, or `"(none)"`.
    pub rule: String,
    pub reason: String,
    pub facts: PolicyFacts,
    /// Tier human, a human submitted it, and the tier lets the submitter authorize: the
    /// submission is the approval (`approval.at_submission`), no inbox. Always false for
    /// auto tiers and agent-sourced intents. Absent (false) in logs written before it existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submitter_authorizes: bool,
    /// Distinct operators who must approve (two-person approval: 2). Absent (1) in logs
    /// written before it existed.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub approvers: u32,
    /// The proposer may not approve. Always true for an engagement.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proposer_excluded: bool,
    /// Rules 8: every approval re-enters the approver's credential (a step-up), in a
    /// credentials run, and the submission is never the approval. Always true for an
    /// engagement; a human tier's `step_up` otherwise. Absent (false) before rules 8.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub step_up: bool,
}

fn one() -> u32 {
    1
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_one(n: &u32) -> bool {
    *n == 1
}

/// Does any vehicle's route (transit leg, task path, return leg; see [`crate::route`])
/// leave the approved area, and why. Every point must be inside an approved area and no
/// leg may touch an approved boundary. A vehicle whose route can't be built (no fresh
/// position, no home) counts as leaving.
pub fn leaves_approved_area(
    spec: &IntentSpec,
    world: &WorldSnapshot,
    cfg: &RulesConfig,
) -> (bool, String) {
    let mut routes = Vec::new();
    for uid in &spec.uids {
        let Some(v) = world.vehicles.get(uid) else {
            return (
                true,
                format!("{uid}: not in the world model (counted as leaving)"),
            );
        };
        match route_for(spec, v, world, cfg) {
            Ok(route) if route.is_empty() => {}
            Ok(route) => routes.push((uid, route)),
            Err(e) => return (true, format!("{uid}: {e} (counted as leaving)")),
        }
    }
    if routes.is_empty() {
        return (false, "no path".into());
    }
    let Some(areas) = &world.areas else {
        return (true, "no area set loaded (counted as leaving)".into());
    };
    let approved: Vec<&Area> = areas.approved().collect();
    if approved.is_empty() {
        return (true, "no approved area defined (counted as leaving)".into());
    }
    let inside = |p| approved.iter().any(|a| a.contains(p));
    // Conservative: a leg touching any approved boundary counts as leaving, even if it
    // passes into an adjoining approved area.
    let crosses = |a, b| approved.iter().any(|ar| ar.segment_touches_boundary(a, b));
    // The task path is the same for every vehicle: check it once.
    if let Some(path) = routes.first().and_then(|(_, r)| r.task.as_ref()) {
        for (i, p) in path.points.iter().enumerate() {
            if !inside(*p) {
                return (
                    true,
                    format!("task path: waypoint {} is outside the approved area", i + 1),
                );
            }
        }
        for (i, a, b) in path.legs() {
            if crosses(a, b) {
                return (
                    true,
                    format!(
                        "task path: leg from waypoint {} crosses the approved-area boundary",
                        i + 1
                    ),
                );
            }
        }
    }
    for (uid, route) in &routes {
        for seg in &route.segments() {
            if let Some(why) = segment_leaves(seg, &inside, &crosses) {
                return (true, format!("{uid}: {why}"));
            }
        }
    }
    (
        false,
        "inside the approved area (transit, task path, return)".into(),
    )
}

/// Rules 34: whether a group order's share of any member sent or waiting leaves the approved
/// area, and why (each member's own route, as [`leaves_approved_area`] judges one order's).
fn group_leaves(spec: &IntentSpec, g: &crate::group::GroupEval, world: &WorldSnapshot, cfg: &RulesConfig) -> (bool, String) {
    let mut any_path = false;
    for uid in g.counted() {
        let Some(ms) = g.member_spec(spec, &uid) else { continue };
        let (leaves, why) = leaves_approved_area(&ms, world, cfg);
        if leaves {
            return (true, why);
        }
        any_path |= why != "no path";
    }
    if any_path {
        (false, "inside the approved area (each member's transit, share and return)".into())
    } else {
        (false, "no path".into())
    }
}

fn segment_leaves(
    seg: &Segment,
    inside: &dyn Fn(crate::geo::LonLat) -> bool,
    crosses: &dyn Fn(crate::geo::LonLat, crate::geo::LonLat) -> bool,
) -> Option<String> {
    let name = seg.name();
    for (p, label) in [(seg.from, &seg.from_label), (seg.to, &seg.to_label)] {
        if !inside(p) {
            return Some(format!("{name}: {label} is outside the approved area"));
        }
    }
    if crosses(seg.from, seg.to) {
        return Some(format!("{name} crosses the approved-area boundary"));
    }
    None
}

pub fn decide(spec: &IntentSpec, world: &WorldSnapshot, policy: &Policy) -> PolicyDecision {
    // rules 34: a group order is routed on the members sent or waiting, never on one left out
    // (which is not sent: a member an engagement holds is left out, so it never stops it)
    let group = crate::group::is_group(spec, &policy.rules).then(|| crate::group::evaluate(spec, world, &policy.rules));
    let counted: Option<std::collections::BTreeSet<crate::ids::Uid>> = group.as_ref().map(|g| g.counted().into_iter().collect());
    let counts = |u: &crate::ids::Uid| counted.as_ref().is_none_or(|c| c.contains(u));
    let mut d = decide_by_file(spec, world, policy, group.as_ref());
    // rules 27: a lay order is signed as an engagement is (a lay exists from rules 27 only)
    if !spec.kind().needs_engagement_authority() {
        // An agent's order on a vehicle with an engagement in play (pending, approved or
        // dispatched) goes to a human: a hold or an rtl would otherwise run on its own and
        // supersede, so stop, the engagement. An operator's own order is a human decision.
        let engaged: BTreeMap<&crate::ids::Uid, &crate::ids::IntentId> = world.engaged.iter().filter(|(u, _)| counts(u)).collect();
        let waiting: BTreeMap<&crate::ids::Uid, &crate::ids::IntentId> = world.waiting.iter().filter(|(u, _)| counts(u)).collect();
        if spec.source.is_agent() && d.tier == Tier::Auto && !engaged.is_empty() {
            let list: Vec<String> = engaged
                .iter()
                .map(|(u, i)| format!("{u} ({i})"))
                .collect();
            d.reason = format!(
                "tier {} is auto, but an agent's order on a vehicle with an engagement in play needs a human: {}",
                d.rule,
                list.join(", ")
            );
            d.rule = "(engagement in play)".into();
            d.tier = Tier::Human;
            d.submitter_authorizes = false;
        }
        // Rules 5: likewise on a vehicle with an order a human approved waiting in the queue:
        // an agent's order dispatched or queued there would supersede the human's.
        if policy.rules.rules_version >= 5
            && spec.source.is_agent()
            && d.tier == Tier::Auto
            && !waiting.is_empty()
        {
            let list: Vec<String> = waiting
                .iter()
                .map(|(u, i)| format!("{u} ({i})"))
                .collect();
            d.reason = format!(
                "tier {} is auto, but an agent's order on a vehicle with a human-approved order waiting in the queue needs a human: {}",
                d.rule,
                list.join(", ")
            );
            d.rule = "(human order waiting)".into();
            d.tier = Tier::Human;
            d.submitter_authorizes = false;
        }
        return d;
    }
    // The engagement floor, whatever the file says: a human always, never at submission,
    // never the proposer, and two distinct approvers at least (two-person rule).
    if d.tier == Tier::Auto {
        d.reason = format!(
            "tier {} is auto, but an engagement always needs human approval (DoD Directive 3000.09)",
            d.rule
        );
        d.rule = "(engage)".into();
        d.tier = Tier::Human;
    }
    // rules 27: a minefield's floor is an engagement's, and says so
    if spec.kind() == TaskKind::Lay && d.rule != "engagement" {
        d.reason = format!(
            "{}; a minefield is laid on an engagement's two signatures: its mines choose their victims later, so the judgement is made now (DoD Directive 3000.09)",
            d.reason
        );
    }
    d.submitter_authorizes = false;
    d.proposer_excluded = true;
    d.approvers = d.approvers.max(ENGAGE_MIN_APPROVERS);
    // rules 8: every signature on an engagement re-enters the signer's credential
    d.step_up = policy.rules.rules_version >= OPERATOR_AUTH_SINCE;
    d
}

/// The fewest distinct approvers an engagement may have, whatever the policy file says.
pub const ENGAGE_MIN_APPROVERS: u32 = 2;

fn decide_by_file(spec: &IntentSpec, world: &WorldSnapshot, policy: &Policy, group: Option<&crate::group::GroupEval>) -> PolicyDecision {
    // rules 34: a group's facts are its members' sent or waiting
    let counted: Option<std::collections::BTreeSet<Uid>> = group.map(|g| g.counted().into_iter().collect());
    let counts = |u: &Uid| counted.as_ref().is_none_or(|c| c.contains(u));
    let armed: Vec<Uid> = spec
        .uids
        .iter()
        .filter(|u| counts(u))
        .filter(|u| {
            world
                .vehicles
                .get(*u)
                .and_then(|v| v.platform.as_ref())
                .is_none_or(|p| p.armed)
        })
        .cloned()
        .collect();
    let (leaves, area_detail) = match group {
        Some(g) => group_leaves(spec, g, world, &policy.rules),
        None => leaves_approved_area(spec, world, &policy.rules),
    };
    // The same pure rules the engine ran on the same inputs: which checks need a human (a
    // group's: those of its members sent or waiting).
    let checks: Vec<rules::CheckResult> = match group {
        Some(g) => g
            .results
            .iter()
            .filter(|r| r.class.is_none() && r.uid.as_ref().is_none_or(counts))
            .cloned()
            .collect(),
        None => rules::evaluate(spec, world, &policy.rules),
    };
    let facts = PolicyFacts {
        kind: spec.kind(),
        source: match spec.source {
            Source::Human { .. } => SourceKind::Human,
            Source::Agent { .. } => SourceKind::Agent,
            // rules 29: the previous watch's order, never decided by the policy (a dequeue asks
            // none); a person's, should anything ask
            Source::Handover { .. } => SourceKind::Human,
        },
        armed,
        leaves_approved_area: leaves,
        area_detail,
        unverified: rules::unverified(&checks),
    };
    let unverified_rules: Vec<RuleId> = checks
        .iter()
        .filter(|c| c.outcome == Outcome::Unverified)
        .map(|c| c.rule)
        .collect();
    let human_source = facts.source == SourceKind::Human;
    for rule in &policy.tiers {
        let w = &rule.when;
        // rules 34: a tier's kind list routes a transit only where it names one (every shipped
        // policy that names patrol names transit beside it)
        let matches = w.kind.as_ref().is_none_or(|ks| ks.contains(&facts.kind))
            && w.armed.is_none_or(|a| a == !facts.armed.is_empty())
            && w.leaves_approved_area
                .is_none_or(|l| l == facts.leaves_approved_area)
            && w.source.is_none_or(|s| s == facts.source)
            && w.unverified
                .as_ref()
                .is_none_or(|rs| rs.iter().any(|r| unverified_rules.contains(r)));
        if matches {
            if rule.tier == Tier::Auto && !facts.unverified.is_empty() {
                return PolicyDecision {
                    tier: Tier::Human,
                    rule: "(unverified)".into(),
                    reason: format!(
                        "tier {} is auto, but a check could not be verified; a human must judge it",
                        rule.name
                    ),
                    facts,
                    submitter_authorizes: human_source && policy.submitter_authorizes(None),
                    approvers: 1,
                    proposer_excluded: false,
                    step_up: false,
                };
            }
            let human = rule.tier == Tier::Human;
            // rules 8: a consequential tier's approval re-enters the credential, and is never
            // the submission itself
            let step_up = human
                && rule.step_up == Some(true)
                && policy.rules.rules_version >= OPERATOR_AUTH_SINCE;
            return PolicyDecision {
                tier: rule.tier,
                rule: rule.name.clone(),
                reason: rule.reason.clone(),
                facts,
                submitter_authorizes: human
                    && human_source
                    && !step_up
                    && policy.submitter_authorizes(Some(rule)),
                approvers: if human { rule.approvers.unwrap_or(1).max(1) } else { 1 },
                proposer_excluded: human && rule.proposer_may_approve == Some(false),
                step_up,
            };
        }
    }
    PolicyDecision {
        tier: Tier::Human,
        rule: "(none)".into(),
        reason: "no policy rule matched; fail closed to a human".into(),
        facts,
        submitter_authorizes: human_source && policy.submitter_authorizes(None),
        approvers: 1,
        proposer_excluded: false,
        step_up: false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn table(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn a_number_is_the_same_written_either_way() {
        let v = |s: &str| table(&format!("v = {s}")).remove("v").unwrap();
        assert!(same(&v("20"), &v("20.0")) && same(&v("20.0"), &v("20")));
        assert!(!same(&v("20"), &v("20.5")));
        assert!(same(&v("[1, 2.0]"), &v("[1.0, 2]")) && !same(&v("[1, 2]"), &v("[2, 1]")));
        assert!(same(&v("{ a = 1, b = [\"x\"] }"), &v("{ b = [\"x\"], a = 1.0 }")));
        assert!(!same(&v("{ a = 1 }"), &v("{ a = 1, b = 2 }")));
        assert!(!same(&v("\"20\""), &v("20")));
    }

    #[test]
    fn tables_merge_key_by_key_and_the_rest_replaces() {
        let mut base = table("[a]\nx = 1\ny = [1, 2]\n[a.inner]\np = 1\nq = 2\n");
        let over = table("[a]\ny = [3]\nz = \"new\"\n[a.inner]\nq = 5\n");
        for (k, v) in over {
            merge_key(&mut base, k.clone(), v, &k, "over.toml", "base.toml").unwrap();
        }
        assert_eq!(base, table("[a]\nx = 1\ny = [3]\nz = \"new\"\n[a.inner]\np = 1\nq = 5\n"));
        // a copy, however deep, is refused by its dotted key
        let copy = toml::Value::Table(table("inner = { p = 1.0 }"));
        let e = merge_key(&mut base, "a".into(), copy, "a", "over.toml", "base.toml").unwrap_err().to_string();
        assert!(e.starts_with("policy over.toml: `a.inner.p` = 1.0 is what base.toml gives it already"), "{e}");
    }

    #[test]
    fn tiers_go_before_the_ones_they_name_in_the_files_order() {
        let mut base = table("[[tier]]\nname = \"one\"\n[[tier]]\nname = \"two\"\n");
        let over = table(
            "[[tier]]\nname = \"a\"\nbefore = \"two\"\n[[tier]]\nname = \"b\"\nbefore = \"two\"\n[[tier]]\nname = \"c\"\nbefore = \"one\"\n",
        );
        insert_tiers(&mut base, over["tier"].clone(), "over.toml", "base.toml").unwrap();
        let tiers = base["tier"].as_array().unwrap();
        let names: Vec<&str> = tiers.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["c", "one", "a", "b", "two"]);
        assert!(tiers.iter().all(|t| !t.as_table().unwrap().contains_key("before")));
    }
}
