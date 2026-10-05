//! Login sessions: a random token in the operator's cookie, its SHA-256 here, and two
//! expiries: idle (no request for that long) and absolute (since the login, whatever the
//! activity). The table never holds a token, so a memory dump of the bridge does not hand out
//! sessions. Lookups hash the token and compare the stored digest in constant time.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::AuthError;
use crate::keys::{ct_eq, digest, token};

/// Why a session ended on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    Idle,
    Absolute,
}

impl Expiry {
    pub fn as_str(self) -> &'static str {
        match self {
            Expiry::Idle => "expired (idle)",
            Expiry::Absolute => "expired (session lifetime)",
        }
    }
}

#[derive(Debug)]
struct Entry<T> {
    digest: [u8; 32],
    created: Instant,
    last: Instant,
    value: T,
}

/// What a token is.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup<T> {
    /// A live session (its idle timer restarted).
    Valid(T),
    /// It was a session, and it has just expired: removed, the value handed back so the
    /// caller can close what it held.
    Expired(T, Expiry),
    /// Not a session (never was, or already ended).
    Unknown,
}

#[derive(Debug)]
pub struct SessionTable<T> {
    entries: HashMap<[u8; 32], Entry<T>>,
    pub idle: Duration,
    pub absolute: Duration,
}

impl<T: Clone> SessionTable<T> {
    pub fn new(idle: Duration, absolute: Duration) -> Self {
        SessionTable {
            entries: HashMap::new(),
            idle,
            absolute,
        }
    }

    /// A new session holding `value`: its token (for the cookie; not kept here).
    pub fn create(&mut self, value: T, now: Instant) -> Result<String, AuthError> {
        let t = token()?;
        let d = digest(&t);
        self.entries.insert(
            d,
            Entry {
                digest: d,
                created: now,
                last: now,
                value,
            },
        );
        Ok(t)
    }

    fn expiry(&self, e: &Entry<T>, now: Instant) -> Option<Expiry> {
        if now.saturating_duration_since(e.created) >= self.absolute {
            Some(Expiry::Absolute)
        } else if now.saturating_duration_since(e.last) >= self.idle {
            Some(Expiry::Idle)
        } else {
            None
        }
    }

    /// The session a token names, restarting its idle timer; an expired one is removed.
    pub fn lookup(&mut self, token: &str, now: Instant) -> Lookup<T> {
        let d = digest(token);
        let expired = match self.entries.get(&d) {
            Some(e) if ct_eq(&e.digest, &d) => self.expiry(e, now),
            _ => return Lookup::Unknown,
        };
        match expired {
            Some(why) => match self.entries.remove(&d) {
                Some(e) => Lookup::Expired(e.value, why),
                None => Lookup::Unknown,
            },
            None => match self.entries.get_mut(&d) {
                Some(e) => {
                    e.last = now;
                    Lookup::Valid(e.value.clone())
                }
                None => Lookup::Unknown,
            },
        }
    }

    /// [`SessionTable::lookup`] without restarting the idle timer (a status query is not
    /// activity: a console asking who is signed in must not keep an unattended session
    /// alive). An expired session is still removed.
    pub fn peek(&mut self, token: &str, now: Instant) -> Lookup<T> {
        let d = digest(token);
        let expired = match self.entries.get(&d) {
            Some(e) if ct_eq(&e.digest, &d) => self.expiry(e, now),
            _ => return Lookup::Unknown,
        };
        match expired {
            Some(why) => match self.entries.remove(&d) {
                Some(e) => Lookup::Expired(e.value, why),
                None => Lookup::Unknown,
            },
            None => match self.entries.get(&d) {
                Some(e) => Lookup::Valid(e.value.clone()),
                None => Lookup::Unknown,
            },
        }
    }

    /// When the session a token names ends if nothing else happens (idle or absolute,
    /// whichever comes first), without touching it.
    pub fn ends_at(&self, token: &str) -> Option<Instant> {
        let d = digest(token);
        self.entries
            .get(&d)
            .map(|e| (e.last + self.idle).min(e.created + self.absolute))
    }

    /// End the session a token names (logout).
    pub fn remove(&mut self, token: &str) -> Option<T> {
        let d = digest(token);
        match self.entries.get(&d) {
            Some(e) if ct_eq(&e.digest, &d) => self.entries.remove(&d).map(|e| e.value),
            _ => None,
        }
    }

    /// Remove every expired session, handing back what each held and why it ended.
    pub fn sweep(&mut self, now: Instant) -> Vec<(T, Expiry)> {
        let gone: Vec<([u8; 32], Expiry)> = self
            .entries
            .iter()
            .filter_map(|(k, e)| self.expiry(e, now).map(|why| (*k, why)))
            .collect();
        gone.into_iter()
            .filter_map(|(k, why)| self.entries.remove(&k).map(|e| (e.value, why)))
            .collect()
    }

    /// Remove every session whose value `end` picks (a credential removed or changed).
    pub fn end_where(&mut self, end: impl Fn(&T) -> bool) -> Vec<T> {
        let gone: Vec<[u8; 32]> = self
            .entries
            .iter()
            .filter(|(_, e)| end(&e.value))
            .map(|(k, _)| *k)
            .collect();
        gone.into_iter()
            .filter_map(|k| self.entries.remove(&k).map(|e| e.value))
            .collect()
    }

    /// Every live session's value (for status pages; no tokens).
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.values().map(|e| &e.value)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn table() -> SessionTable<&'static str> {
        SessionTable::new(Duration::from_secs(60), Duration::from_secs(300))
    }

    #[test]
    fn a_session_lives_while_used_and_ends_idle_or_at_its_lifetime() {
        let mut t = table();
        let t0 = Instant::now();
        let a = t.create("op-1", t0).unwrap();
        assert_eq!(t.lookup(&a, t0 + Duration::from_secs(50)), Lookup::Valid("op-1"));
        // used at 50 s: alive at 100 s
        assert_eq!(t.lookup(&a, t0 + Duration::from_secs(100)), Lookup::Valid("op-1"));
        // idle 60 s
        assert_eq!(
            t.lookup(&a, t0 + Duration::from_secs(160)),
            Lookup::Expired("op-1", Expiry::Idle)
        );
        // gone for good: a replayed token is unknown
        assert_eq!(t.lookup(&a, t0 + Duration::from_secs(161)), Lookup::Unknown);
        // used every 50 s, it still ends at 300 s
        let b = t.create("op-2", t0).unwrap();
        for s in (50..300).step_by(50) {
            assert_eq!(t.lookup(&b, t0 + Duration::from_secs(s)), Lookup::Valid("op-2"));
        }
        assert_eq!(
            t.lookup(&b, t0 + Duration::from_secs(300)),
            Lookup::Expired("op-2", Expiry::Absolute)
        );
    }

    #[test]
    fn logout_and_removal_end_sessions_and_tokens_are_not_guessable() {
        let mut t = table();
        let t0 = Instant::now();
        let a = t.create("op-1", t0).unwrap();
        let b = t.create("op-2", t0).unwrap();
        let c = t.create("op-1", t0).unwrap();
        assert_ne!(a, c, "every login gets its own token");
        assert_eq!(t.lookup("not-a-token", t0), Lookup::Unknown);
        assert_eq!(t.remove(&a), Some("op-1"));
        assert_eq!(t.lookup(&a, t0), Lookup::Unknown, "a logged-out token is dead");
        assert_eq!(t.end_where(|v| *v == "op-1"), vec!["op-1"]);
        assert_eq!(t.lookup(&c, t0), Lookup::Unknown);
        assert_eq!(t.lookup(&b, t0), Lookup::Valid("op-2"));
        let swept = t.sweep(t0 + Duration::from_secs(61));
        assert_eq!(swept, vec![("op-2", Expiry::Idle)]);
        assert!(t.is_empty());
    }

    #[test]
    fn peeking_is_not_activity() {
        let mut t = table();
        let t0 = Instant::now();
        let a = t.create("op-1", t0).unwrap();
        assert_eq!(t.peek(&a, t0 + Duration::from_secs(50)), Lookup::Valid("op-1"));
        assert_eq!(
            t.peek(&a, t0 + Duration::from_secs(61)),
            Lookup::Expired("op-1", Expiry::Idle),
            "a status query did not restart the idle timer"
        );
    }
}
