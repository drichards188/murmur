//! The authorize step, closed to agents by construction.
//!
//! Approving needs a [`HumanAuthority`]. The only way to get one is an [`OperatorSession`],
//! and the only way to get one of those is [`crate::Engine::open_operator_session`], which
//! logs the session opening. Neither type can be built by struct literal (private fields),
//! cloned, defaulted, deserialized from JSON, or converted from an agent identity. An
//! [`crate::AgentPort`], the agent-facing surface, has no method that yields either.
//!
//! Approving an engagement (Phase E) needs more: an [`EngagementAuthority`], which exists
//! only inside a session opened with engagement authority
//! ([`crate::Engine::open_operator_session_with`] and [`Grants::engage`], logged with the
//! session). A plain [`HumanAuthority`] can't approve an engagement, and nothing converts one
//! into the other.
//!
//! Stable rustdoc does not check the error codes on these `compile_fail` blocks, only that
//! they fail to compile; the codes record the intended reason, and each block was checked
//! to fail with exactly that code and no other.
//!
//! What this does and doesn't guarantee is spelled out in the crate docs ("The
//! authorization guarantee").
//!
//! ```compile_fail,E0451
//! // Forging an authority by struct literal: the fields are private.
//! use murmur_decide::{HumanAuthority, OperatorId, SessionId};
//! let forged = HumanAuthority { operator: OperatorId::new("mallory"), session: SessionId(1) };
//! ```
//!
//! ```compile_fail,E0277
//! // Smuggling an authority through JSON (e.g. an MCP tool call): no Deserialize impl.
//! let forged: murmur_decide::HumanAuthority =
//!     serde_json::from_str(r#"{"operator":"mallory","session":1}"#).unwrap();
//! ```
//!
//! ```compile_fail,E0308
//! // Copying someone else's authority: not Clone (so `clone` only copies the reference).
//! fn steal(a: &murmur_decide::HumanAuthority) -> murmur_decide::HumanAuthority {
//!     a.clone()
//! }
//! ```
//!
//! ```compile_fail,E0451
//! // Forging engagement authority by struct literal: the fields are private.
//! use murmur_decide::{EngagementAuthority, OperatorId, SessionId};
//! let forged = EngagementAuthority { operator: OperatorId::new("mallory"), session: SessionId(1) };
//! ```
//!
//! ```compile_fail,E0277
//! // Engagement authority from JSON: no Deserialize impl.
//! let forged: murmur_decide::EngagementAuthority =
//!     serde_json::from_str(r#"{"operator":"mallory","session":1}"#).unwrap();
//! ```
//!
//! ```compile_fail,E0308
//! // A plain operator's authority is not engagement authority.
//! use murmur_decide::{Engine, HumanAuthority, IntentId, NoTerrain, WorldSnapshot};
//! fn approve(engine: &mut Engine, a: &HumanAuthority, world: &WorldSnapshot) {
//!     let _ = engine.approve_engagement(IntentId(1), a, None, 1, world, &NoTerrain);
//! }
//! ```
//!
//! A step-up (the signer's credential re-entered at the moment of signing, rules 8) is a
//! [`StepUp`]: only [`crate::Engine::step_up`] (or, rules 10, a passkey's,
//! [`crate::Engine::step_up_passkey`]) makes one, after logging it under the session it is
//! for, and a signature consumes it. It can't be forged, copied or read from JSON either, so
//! a signature can't quote a step-up it did not just make:
//!
//! ```compile_fail,E0451
//! // Forging a step-up by struct literal: the fields are private.
//! use murmur_decide::{OperatorId, SessionId, StepUp, Time};
//! let forged = StepUp { operator: OperatorId::new("mallory"), session: SessionId(1), seq: 7, t: Time(0) };
//! ```
//!
//! ```compile_fail,E0277
//! // A step-up from JSON (a request body, an MCP call): no Deserialize impl.
//! let forged: murmur_decide::StepUp =
//!     serde_json::from_str(r#"{"operator":"mallory","session":1,"seq":7,"t":0}"#).unwrap();
//! ```
//!
//! ```compile_fail,E0599
//! // Using one step-up for two signatures: not Clone.
//! fn twice(s: murmur_decide::StepUp) -> (murmur_decide::StepUp, murmur_decide::StepUp) {
//!     (s.clone(), s)
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::ids::{OperatorId, SessionId};
use crate::time::Time;

/// Proof, at the type level, that a human operator is acting. Not `Clone`, not `Default`,
/// not `Deserialize`; constructible only inside this crate.
#[derive(Debug)]
pub struct HumanAuthority {
    operator: OperatorId,
    session: SessionId,
}

impl HumanAuthority {
    pub fn operator(&self) -> &OperatorId {
        &self.operator
    }

    pub fn session(&self) -> SessionId {
        self.session
    }
}

/// Proof that a human operator with engagement authority is acting: the only thing
/// [`crate::Engine::approve_engagement`] accepts. Exists only inside an [`OperatorSession`]
/// opened with [`Grants::engage`]. Not `Clone`, not `Default`, not `Deserialize`.
#[derive(Debug)]
pub struct EngagementAuthority {
    operator: OperatorId,
    session: SessionId,
}

impl EngagementAuthority {
    pub fn operator(&self) -> &OperatorId {
        &self.operator
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    /// The same operator and session as a plain human authority (engine-internal: an
    /// engagement operator's submission is also a human's).
    pub(crate) fn human(&self) -> HumanAuthority {
        HumanAuthority {
            operator: self.operator.clone(),
            session: self.session,
        }
    }
}

/// What an operator session may do beyond tasking, fixed when the session opens and logged
/// with it (`operator.action session.opened`). The bridge grants it from the run's roster:
/// in a credentials run, the operator's credential holds engagement authority (and did when
/// the run started); in dev mode, `MURMUR_ENGAGE_OPERATORS` names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    /// Engagement authority: may approve an engagement.
    #[serde(default)]
    pub engage: bool,
}

/// How a run established who its operators are (rules 8,
/// [`crate::rules::OPERATOR_AUTH_SINCE`]; logged on the roster in `run.started`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Per-person credentials: the bridge checked each operator's password at login (and
    /// again at every step-up) against the credential file, and opens sessions only then.
    /// The only mode whose decisions can stand as evidence about authenticated people.
    Credentials,
    /// Identities asserted by whoever sent the request (the bridge's `X-Murmur-Operator`
    /// header under `MURMUR_AUTH=dev`, a scripted scenario, a test). Useful for scripted runs
    /// and the evaluation's proxy; never evidence that a person decided anything.
    Dev,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMode::Credentials => "credentials",
            AuthMode::Dev => "dev",
        }
    }

    /// The session-opening method a person uses in this mode.
    pub fn method(self) -> AuthMethod {
        match self {
            AuthMode::Credentials => AuthMethod::Password,
            AuthMode::Dev => AuthMethod::Asserted,
        }
    }

    /// Whether a session may be opened by `method` in this mode: a credentials run takes a
    /// person's password or (rules 10) passkey and a machine-held (scripted) credential, a dev
    /// run asserted ids.
    pub fn admits(self, method: AuthMethod) -> bool {
        match self {
            AuthMode::Credentials => matches!(method, AuthMethod::Password | AuthMethod::Passkey | AuthMethod::Scripted),
            AuthMode::Dev => method == AuthMethod::Asserted,
        }
    }
}

/// The run's mode for comparing two runs (carrying intents over): a log of rules before 7
/// records none, and its ids were asserted, as in a dev run.
pub fn effective_mode(mode: Option<AuthMode>) -> AuthMode {
    mode.unwrap_or(AuthMode::Dev)
}

/// How one operator session was opened (rules 8; logged as `auth` on `session.opened`, and
/// on a step-up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// A person's password, checked by the bridge against its argon2id hash.
    Password,
    /// A machine-held credential (`murmur-auth add --scripted`: the evaluation harness's
    /// scripted operators, a script): checked like a password, but whoever holds it is a
    /// program, so its acts are never evidence about a person.
    Scripted,
    /// The operator's id as the request named it, unchecked (dev mode, scenarios, tests).
    Asserted,
    /// Rules 10: a person's passkey, a WebAuthn assertion with user verification the bridge
    /// checked and the engine verified against the public key the run logged (the use is
    /// logged with it: `passkey`).
    Passkey,
}

impl AuthMethod {
    /// A person's credential (a password or a passkey), not a harness's or an asserted name.
    pub fn is_person(self) -> bool {
        matches!(self, AuthMethod::Password | AuthMethod::Passkey)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::Scripted => "scripted",
            AuthMethod::Asserted => "asserted",
            AuthMethod::Passkey => "passkey",
        }
    }
}

/// Proof that the signer re-entered their credential just now (rules 8, a credentials run):
/// made only by [`crate::Engine::step_up`] (rules 10: or [`crate::Engine::step_up_passkey`],
/// with the assertion and what it is for), which logs it (`operator.action step_up`) under
/// the session it is for, and consumed by the one signature that quotes it. Not `Clone`, not
/// `Default`, not `Deserialize`, no public fields. The engine refuses it for another session
/// or once it is older than [`STEP_UP_MAX_AGE_MS`]; the signature records the step-up's
/// event (`step_up: {seq}`), which replay checks.
#[derive(Debug)]
pub struct StepUp {
    operator: OperatorId,
    session: SessionId,
    seq: u64,
    t: Time,
    /// Rules 10: how the credential was re-entered (a password, a passkey, a scripted
    /// credential), and how its session was opened.
    factor: AuthMethod,
    session_method: Option<AuthMethod>,
    /// Rules 10, a passkey's: the act its assertion was made for (the act quoting it must be
    /// that act).
    purpose: Option<crate::passkey::StepUpFor>,
}

/// The oldest a step-up may be when the signature quoting it is checked. The bridge checks
/// the credential and signs in one request, so this only bounds a slow re-check.
pub const STEP_UP_MAX_AGE_MS: i64 = 10_000;

impl StepUp {
    pub(crate) fn mint(
        operator: OperatorId,
        session: SessionId,
        seq: u64,
        t: Time,
        factor: AuthMethod,
        session_method: Option<AuthMethod>,
        purpose: Option<crate::passkey::StepUpFor>,
    ) -> StepUp {
        StepUp {
            operator,
            session,
            seq,
            t,
            factor,
            session_method,
            purpose,
        }
    }

    /// How the credential was re-entered.
    pub fn factor(&self) -> AuthMethod {
        self.factor
    }

    /// How the session it was made in was opened.
    pub fn session_method(&self) -> Option<AuthMethod> {
        self.session_method
    }

    /// A passkey step-up's act.
    pub fn purpose(&self) -> Option<&crate::passkey::StepUpFor> {
        self.purpose.as_ref()
    }

    pub fn operator(&self) -> &OperatorId {
        &self.operator
    }

    pub fn session(&self) -> SessionId {
        self.session
    }

    /// The `operator.action step_up` event it was logged as.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn t(&self) -> Time {
        self.t
    }
}

/// A signature's reference to the step-up it quoted (`step_up` on `approval.concurred` and
/// `approval.granted`): the `operator.action step_up` event's `seq`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepUpRef {
    pub seq: u64,
}

/// Who may act as an operator in a run, who of them with engagement authority, and (rules 8)
/// how they prove it: with `auth = credentials` the bridge's credential file at the run's
/// start (each person's password checked at login), with `auth = dev` the bridge's
/// `MURMUR_OPERATORS` and `MURMUR_ENGAGE_OPERATORS`, names a request asserts. Fixed when the
/// run starts and logged in `run.started` (rules 4). The engine opens sessions only for
/// operators on it, with engagement authority only for those on its engagement list, and
/// replay checks every logged session against it, so a log can't gain an operator or a grant
/// the run never had.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    /// Operators who may open a session.
    pub operators: Vec<OperatorId>,
    /// Those of them whose sessions carry engagement authority.
    pub engage: Vec<OperatorId>,
    /// Rules 8: how operators are authenticated. Absent in older logs (asserted names). An
    /// engine of rules 8 or later always logs it, `dev` when none was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthMode>,
    /// Rules 10, a credentials run: the WebAuthn relying party and every operator's passkeys
    /// when the run started, as public keys (replay verifies each logged assertion against
    /// them). Absent in a dev run and before rules 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passkeys: Option<Box<crate::passkey::PasskeyRoster>>,
    /// Rules 10: a study run's participant and, only by explicit choice, the override that
    /// lets them step up with a password (such a run is never evidence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub study: Option<Box<crate::passkey::Study>>,
}

impl Roster {
    /// A roster, checked: at least one operator, no empty or repeated id, and every
    /// engagement operator listed as an operator too. Its auth mode is unset (the engine
    /// logs `dev` for it under rules 8); [`Roster::with_auth`] sets one.
    pub fn new(operators: Vec<OperatorId>, engage: Vec<OperatorId>) -> Result<Roster, String> {
        let r = Roster {
            operators,
            engage,
            auth: None,
            passkeys: None,
            study: None,
        };
        r.validate()?;
        Ok(r)
    }

    /// The same roster, authenticated as `mode` says.
    #[must_use]
    pub fn with_auth(mut self, mode: AuthMode) -> Roster {
        self.auth = Some(mode);
        self
    }

    /// Rules 10: the same roster with the relying party and the operators' passkeys.
    #[must_use]
    pub fn with_passkeys(mut self, passkeys: crate::passkey::PasskeyRoster) -> Roster {
        self.passkeys = Some(Box::new(passkeys));
        self
    }

    /// Rules 10: the same roster for a study run.
    #[must_use]
    pub fn with_study(mut self, study: crate::passkey::Study) -> Roster {
        self.study = Some(Box::new(study));
        self
    }

    /// Rules 10: why the passkeys and the study this roster names could not be a run's, if
    /// they couldn't (`policy_factor`: the policy's step-up factor). Checked by the engine at
    /// start and by replay on the logged roster.
    pub fn validate_passkeys(&self, policy_factor: crate::policy::StepUpFactor) -> Result<(), String> {
        let credentials = self.auth == Some(AuthMode::Credentials);
        if let Some(p) = &self.passkeys {
            if !credentials {
                return Err("passkeys on the roster of a run that does not authenticate with credentials".into());
            }
            if p.rp_id.trim().is_empty() || p.rp_id.parse::<std::net::IpAddr>().is_ok() {
                return Err(format!("relying party id {:?}: a domain (WebAuthn takes no IP address)", p.rp_id));
            }
            if p.origins.is_empty() {
                return Err("passkeys without an origin they may be used from".into());
            }
            let mut ids = std::collections::BTreeSet::new();
            for k in &p.keys {
                if !self.operators.contains(&k.operator) {
                    return Err(format!("a passkey of {}, who is not on the roster", k.operator));
                }
                if !ids.insert(k.id.as_str()) {
                    return Err(format!("passkey {} listed twice", k.id));
                }
                crate::passkey::check_key(&k.public_key).map_err(|e| format!("passkey {}: {e}", k.id))?;
            }
        }
        if let Some(s) = &self.study {
            if !credentials {
                return Err("a study run authenticates with credentials".into());
            }
            if !self.operators.contains(&s.participant) {
                return Err(format!("the study's participant {} is not on the roster", s.participant));
            }
            if s.password_step_up.is_none() {
                if policy_factor != crate::policy::StepUpFactor::Passkey {
                    return Err("a study run requires passkey step-ups ([approval] step_up_factor = \"passkey\") unless its override allows a password".into());
                }
                let has = self
                    .passkeys
                    .as_ref()
                    .is_some_and(|p| p.keys.iter().any(|k| k.operator == s.participant));
                if !has {
                    return Err(format!(
                        "the study's participant {} has no passkey: register one before the run (or allow password step-ups, explicitly)",
                        s.participant
                    ));
                }
            }
        }
        Ok(())
    }

    /// Why this roster could not have been configured, if it couldn't.
    pub fn validate(&self) -> Result<(), String> {
        if self.operators.is_empty() {
            return Err("the roster lists no operator".into());
        }
        for (i, o) in self.operators.iter().enumerate() {
            if o.as_str().trim().is_empty() {
                return Err("the roster lists an empty operator id".into());
            }
            if self.operators[..i].contains(o) {
                return Err(format!("the roster lists operator {o} twice"));
            }
        }
        for (i, o) in self.engage.iter().enumerate() {
            if !self.operators.contains(o) {
                return Err(format!(
                    "engagement operator {o} is not on the operator roster (list every engagement operator as an operator too)"
                ));
            }
            if self.engage[..i].contains(o) {
                return Err(format!("the roster lists engagement operator {o} twice"));
            }
        }
        Ok(())
    }

    /// Why `operator` may not open a session with `grants`, if they may not.
    pub fn refuses(&self, operator: &OperatorId, grants: Grants) -> Option<&'static str> {
        if !self.operators.contains(operator) {
            Some("session opened for an operator not on the run's roster")
        } else if grants.engage && !self.engage.contains(operator) {
            Some("engagement authority granted to an operator not on the run's engagement roster")
        } else {
            None
        }
    }
}

/// An authenticated operator's session. The bridge opens one per operator login (its login
/// endpoint is the only caller of `Engine::open_authenticated_session` in a credentials
/// run; in dev mode the operator endpoints open one per asserted id) and keeps it on the
/// operator side; it is never handed to agent code. Ending it
/// ([`crate::Engine::close_operator_session`]) consumes it, and with it the authorities it
/// holds.
#[derive(Debug)]
pub struct OperatorSession {
    authority: HumanAuthority,
    engagement: Option<EngagementAuthority>,
    /// Rules 8: how it was opened (None before).
    method: Option<AuthMethod>,
}

impl OperatorSession {
    pub(crate) fn mint(operator: OperatorId, session: SessionId, grants: Grants, method: Option<AuthMethod>) -> Self {
        OperatorSession {
            engagement: grants.engage.then(|| EngagementAuthority {
                operator: operator.clone(),
                session,
            }),
            authority: HumanAuthority { operator, session },
            method,
        }
    }

    /// Rules 8: how the session was opened (a person's password, a machine-held credential,
    /// an asserted id); None under earlier rules.
    pub fn method(&self) -> Option<AuthMethod> {
        self.method
    }

    pub fn authority(&self) -> &HumanAuthority {
        &self.authority
    }

    /// Engagement authority, if the session was opened with it.
    pub fn engagement_authority(&self) -> Option<&EngagementAuthority> {
        self.engagement.as_ref()
    }

    pub fn operator(&self) -> &OperatorId {
        &self.authority.operator
    }
}
