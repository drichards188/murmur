//! The authorize step, closed to agents by construction.
//!
//! Approving needs a [`HumanAuthority`]. The only way to get one is an [`OperatorSession`],
//! and the only way to get one of those is [`crate::Engine::open_operator_session`], which
//! logs the session opening. Neither type can be built by struct literal (private fields),
//! cloned, defaulted, deserialized from JSON, or converted from an agent identity. An
//! [`crate::AgentPort`], the agent-facing surface, has no method that yields either.
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

/// An authenticated operator's session. The bridge opens one per operator login (its
/// operator HTTP endpoint is the only caller of `Engine::open_operator_session`) and keeps
/// it on the operator side; it is never handed to agent code.
#[derive(Debug)]
pub struct OperatorSession {
    authority: HumanAuthority,
}

impl OperatorSession {
    pub(crate) fn mint(operator: OperatorId, session: SessionId) -> Self {
        OperatorSession {
            authority: HumanAuthority { operator, session },
        }
    }

    pub fn authority(&self) -> &HumanAuthority {
        &self.authority
    }

    pub fn operator(&self) -> &OperatorId {
        &self.authority.operator
    }
}
