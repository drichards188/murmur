//! Failed-attempt limits for logins and step-ups.
//!
//! Per key (the operator id an attempt claims, known or not): after [`Limiter::max_failures`]
//! failures within [`Limiter::window`] the key is locked for [`Limiter::base_lock`], each
//! further lockout doubling it up to [`Limiter::max_lock`]. A success clears the key. While
//! locked, attempts are refused without checking the password (no work for the attacker to
//! make the bridge do, and no oracle). The table is bounded ([`Limiter::cap`]): when full,
//! the entry used longest ago that is not locked goes first, so a flood of made-up ids can
//! neither grow memory without bound nor unlock a real operator early.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Entry {
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
    lockouts: u32,
    last: Instant,
}

#[derive(Debug)]
pub struct Limiter {
    entries: HashMap<String, Entry>,
    pub max_failures: usize,
    pub window: Duration,
    pub base_lock: Duration,
    pub max_lock: Duration,
    pub cap: usize,
}

impl Default for Limiter {
    /// 5 failures in 15 minutes lock a key for 30 s, then 60 s, 120 s, ... up to 15 minutes.
    fn default() -> Self {
        Limiter {
            entries: HashMap::new(),
            max_failures: 5,
            window: Duration::from_secs(15 * 60),
            base_lock: Duration::from_secs(30),
            max_lock: Duration::from_secs(15 * 60),
            cap: 10_000,
        }
    }
}

impl Limiter {
    /// `Err(retry after)` while `key` is locked.
    pub fn check(&mut self, key: &str, now: Instant) -> Result<(), Duration> {
        match self.entries.get(key).and_then(|e| e.locked_until) {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    /// Record a failure; `Some(lock)` when it locked the key.
    pub fn failure(&mut self, key: &str, now: Instant) -> Option<Duration> {
        if !self.entries.contains_key(key) && self.entries.len() >= self.cap {
            self.evict(now);
        }
        let window = self.window;
        let e = self.entries.entry(key.to_string()).or_insert(Entry {
            failures: Vec::new(),
            locked_until: None,
            lockouts: 0,
            last: now,
        });
        e.last = now;
        e.failures.retain(|t| now.saturating_duration_since(*t) < window);
        e.failures.push(now);
        if e.failures.len() >= self.max_failures {
            let factor = 1u32.checked_shl(e.lockouts.min(16)).unwrap_or(u32::MAX);
            let lock = self.base_lock.saturating_mul(factor).min(self.max_lock);
            e.lockouts = e.lockouts.saturating_add(1);
            e.failures.clear();
            e.locked_until = Some(now + lock);
            return Some(lock);
        }
        None
    }

    /// A success: the key starts afresh.
    pub fn success(&mut self, key: &str) {
        self.entries.remove(key);
    }

    fn evict(&mut self, now: Instant) {
        let victim = self
            .entries
            .iter()
            .filter(|(_, e)| e.locked_until.is_none_or(|u| u <= now))
            .min_by_key(|(_, e)| e.last)
            .map(|(k, _)| k.clone());
        if let Some(k) = victim {
            self.entries.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn five_failures_lock_then_the_lock_doubles() {
        let mut l = Limiter::default();
        let t0 = Instant::now();
        for i in 0..4 {
            assert_eq!(l.failure("op-1", t0 + Duration::from_secs(i)), None);
            assert!(l.check("op-1", t0 + Duration::from_secs(i)).is_ok());
        }
        assert_eq!(l.failure("op-1", t0 + Duration::from_secs(4)), Some(Duration::from_secs(30)));
        assert!(l.check("op-1", t0 + Duration::from_secs(5)).is_err());
        // another id is not affected
        assert!(l.check("op-2", t0 + Duration::from_secs(5)).is_ok());
        let t1 = t0 + Duration::from_secs(40);
        assert!(l.check("op-1", t1).is_ok());
        for i in 0..4 {
            assert_eq!(l.failure("op-1", t1 + Duration::from_secs(i)), None);
        }
        assert_eq!(l.failure("op-1", t1 + Duration::from_secs(4)), Some(Duration::from_secs(60)));
        // a success clears it
        l.success("op-1");
        assert!(l.check("op-1", t1 + Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn failures_outside_the_window_do_not_count() {
        let mut l = Limiter::default();
        let t0 = Instant::now();
        for i in 0..4 {
            l.failure("op-1", t0 + Duration::from_secs(i));
        }
        assert_eq!(l.failure("op-1", t0 + Duration::from_secs(16 * 60)), None);
    }

    #[test]
    fn a_full_table_never_drops_a_locked_key() {
        let mut l = Limiter {
            cap: 3,
            ..Limiter::default()
        };
        let t0 = Instant::now();
        for _ in 0..5 {
            l.failure("op-1", t0);
        }
        for i in 0..10 {
            l.failure(&format!("made-up-{i}"), t0 + Duration::from_secs(1));
        }
        assert!(l.check("op-1", t0 + Duration::from_secs(2)).is_err());
        assert!(l.entries.len() <= 3);
    }
}
