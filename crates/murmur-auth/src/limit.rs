//! Failed-attempt limits for logins and step-ups.
//!
//! Per key (the operator id an attempt claims, known or not): after [`Limiter::max_failures`]
//! failures within [`Limiter::window`] the key is locked for [`Limiter::base_lock`], each
//! further lockout doubling it up to [`Limiter::max_lock`]. A success clears the key. While
//! locked, attempts are refused without checking the password (no work for the attacker to
//! make the bridge do, and no oracle). An attempt is reserved before its password is checked
//! ([`Limiter::begin`]): the failures in the window and the attempts still being checked
//! together never exceed [`Limiter::max_failures`], so concurrent attempts can't outrun the
//! lock. The table is bounded ([`Limiter::cap`]): when full, the entry used longest ago that
//! is neither locked nor being checked goes first, so a flood of made-up ids can neither grow
//! memory without bound nor unlock a real operator early.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Entry {
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
    lockouts: u32,
    last: Instant,
    /// Attempts reserved ([`Limiter::begin`]) and not yet ended.
    in_flight: usize,
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

/// How long to wait when attempts already being checked would reach the limit.
const BUSY: Duration = Duration::from_secs(1);

impl Limiter {
    /// `Err(retry after)` while `key` is locked.
    pub fn check(&mut self, key: &str, now: Instant) -> Result<(), Duration> {
        match self.entries.get(key).and_then(|e| e.locked_until) {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    /// Reserve an attempt on `key` before checking its password. `Err(retry after)` while
    /// the key is locked, or while its failures in the window and the attempts already
    /// being checked reach [`Limiter::max_failures`] (so no more than that many passwords
    /// are ever checked before a lock, however many arrive at once). Every reservation ends
    /// with [`Limiter::success`], [`Limiter::failure`] or [`Limiter::release`].
    pub fn begin(&mut self, key: &str, now: Instant) -> Result<(), Duration> {
        self.check(key, now)?;
        let window = self.window;
        let max = self.max_failures;
        let e = self.entry(key, now);
        e.failures.retain(|t| now.saturating_duration_since(*t) < window);
        if e.failures.len() + e.in_flight >= max {
            return Err(BUSY);
        }
        e.in_flight += 1;
        e.last = now;
        Ok(())
    }

    /// End a reservation that reached no verdict (the check could not run).
    pub fn release(&mut self, key: &str) {
        if let Some(e) = self.entries.get_mut(key) {
            e.in_flight = e.in_flight.saturating_sub(1);
        }
    }

    fn entry(&mut self, key: &str, now: Instant) -> &mut Entry {
        if !self.entries.contains_key(key) && self.entries.len() >= self.cap {
            self.evict(now);
        }
        self.entries.entry(key.to_string()).or_insert(Entry {
            failures: Vec::new(),
            locked_until: None,
            lockouts: 0,
            last: now,
            in_flight: 0,
        })
    }

    /// Record a failure (ending a reservation, if one was made); `Some(lock)` when it locked
    /// the key.
    pub fn failure(&mut self, key: &str, now: Instant) -> Option<Duration> {
        let (window, max, base, longest) = (self.window, self.max_failures, self.base_lock, self.max_lock);
        let e = self.entry(key, now);
        e.in_flight = e.in_flight.saturating_sub(1);
        e.last = now;
        e.failures.retain(|t| now.saturating_duration_since(*t) < window);
        e.failures.push(now);
        if e.failures.len() >= max {
            let factor = 1u32.checked_shl(e.lockouts.min(16)).unwrap_or(u32::MAX);
            let lock = base.saturating_mul(factor).min(longest);
            e.lockouts = e.lockouts.saturating_add(1);
            e.failures.clear();
            e.locked_until = Some(now + lock);
            return Some(lock);
        }
        None
    }

    /// A success: the key starts afresh (attempts still being checked stay reserved).
    pub fn success(&mut self, key: &str) {
        let Some(e) = self.entries.get_mut(key) else { return };
        let in_flight = e.in_flight.saturating_sub(1);
        if in_flight == 0 {
            self.entries.remove(key);
        } else {
            e.failures.clear();
            e.locked_until = None;
            e.lockouts = 0;
            e.in_flight = in_flight;
        }
    }

    fn evict(&mut self, now: Instant) {
        let victim = self
            .entries
            .iter()
            .filter(|(_, e)| e.in_flight == 0 && e.locked_until.is_none_or(|u| u <= now))
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

    /// Review fix 6: attempts reserved before their passwords are checked: however many
    /// arrive at once, no more than the limit are checked before the lock.
    #[test]
    fn concurrent_attempts_cannot_outrun_the_lock() {
        let mut l = Limiter::default();
        let t0 = Instant::now();
        let admitted = (0..40).filter(|_| l.begin("op-3", t0).is_ok()).count();
        assert_eq!(admitted, 5);
        // they fail as they finish: the fifth locks
        for i in 0..4 {
            assert_eq!(l.failure("op-3", t0 + Duration::from_millis(i)), None);
        }
        assert!(l.begin("op-3", t0).is_err(), "4 failures and 1 in flight");
        assert_eq!(l.failure("op-3", t0 + Duration::from_millis(5)), Some(Duration::from_secs(30)));
        assert!(l.begin("op-3", t0 + Duration::from_secs(1)).is_err(), "locked");
        // one that reached no verdict gives its place back; a success clears the key
        let mut l = Limiter::default();
        l.begin("op-1", t0).unwrap();
        l.release("op-1");
        for _ in 0..5 {
            l.begin("op-1", t0).unwrap();
        }
        assert!(l.begin("op-1", t0).is_err());
        l.success("op-1");
        assert!(l.begin("op-1", t0).is_ok(), "a success clears the failures, the others still count");
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
