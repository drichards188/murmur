//! Identifiers. Vehicles are keyed by fleet-unique `uid`, never by MAVLink system ID (a
//! one-byte, per-link number; see docs/DECISIONS.md).

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                $name(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(
    /// Fleet-unique vehicle identifier.
    Uid
);
string_id!(
    /// A human operator, as authenticated by the bridge.
    OperatorId
);
string_id!(
    /// An AI agent (Phase F). Being an agent grants nothing.
    AgentId
);

/// Intent number, assigned by the engine in order of creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntentId(pub u64);

impl fmt::Display for IntentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "i{}", self.0)
    }
}

/// Operator session number, assigned by the engine when a session is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub u64);
