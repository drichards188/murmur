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

use serde::{Deserialize, Serialize};

use crate::ids::{OperatorId, SessionId};

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
}

/// What an operator session may do beyond tasking, fixed when the session opens and logged
/// with it (`operator.action session.opened`). The bridge grants it from its operator
/// configuration (`MURMUR_ENGAGE_OPERATORS`, a placeholder for real authentication).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grants {
    /// Engagement authority: may approve an engagement.
    #[serde(default)]
    pub engage: bool,
}

/// An authenticated operator's session. The bridge opens one per operator login (its
/// operator HTTP endpoint is the only caller of `Engine::open_operator_session`) and keeps
/// it on the operator side; it is never handed to agent code.
#[derive(Debug)]
pub struct OperatorSession {
    authority: HumanAuthority,
    engagement: Option<EngagementAuthority>,
}

impl OperatorSession {
    pub(crate) fn mint(operator: OperatorId, session: SessionId, grants: Grants) -> Self {
        OperatorSession {
            engagement: grants.engage.then(|| EngagementAuthority {
                operator: operator.clone(),
                session,
            }),
            authority: HumanAuthority { operator, session },
        }
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
