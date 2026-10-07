//! Passkey login ceremonies waiting for an answer, per operator id claimed (rules 10,
//! murmur-i58).
//!
//! A login's options (`POST /api/auth/login/passkey {"operator"}`) are asked for before anyone
//! is signed in, so their issuance is limited per id: at most [`Ceremonies::per_id`] live
//! (issued, unanswered, younger than [`Ceremonies::ttl`]) per id. A request past that gets the
//! newest live one's options again instead of a new challenge ([`Ceremonies::full`]): no new
//! state, no work for the bridge (a real operator's challenge takes the decision lock), and
//! nothing refused or pushed out. So a stranger flooding an id can neither turn its operator
//! away nor evict the challenge the operator was given: the operator always gets a live
//! challenge their authenticator can answer (served again, it may have less of its lifetime
//! left; once it lapses the next request is a fresh one). Ids without a passkey, and ids that
//! don't exist, go through the same table with the same bounds (their options are decoys), so
//! the limit says nothing about which is which.
//!
//! What a stranger can still do: answer a challenge they were served (an answer, right or
//! wrong, takes it: it is answered once) and so fail the operator's answer to it, each such
//! answer a failure on the id's login limit ([`crate::Limiter`]); which they could do with
//! made-up answers anyway. That limit's lock is the one way to keep an operator from signing
//! in, as before (five failures in fifteen minutes: 30 s, doubling to 15 minutes).
//!
//! Bounded: `per_id` ceremonies per id, [`Ceremonies::cap`] ids. When the table is full, ids
//! with nothing live go first, then the id issued to longest ago: a flood of made-up ids can
//! push out a whole id, real or decoy alike, whose next request is then a fresh one (a retry,
//! for an operator mid-ceremony; it takes `cap` requests).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// One ceremony: its challenge, when it lapses, and what the caller keeps with it.
#[derive(Debug)]
pub struct Live<T> {
    pub challenge: [u8; 32],
    pub until: Instant,
    pub item: T,
}

#[derive(Debug)]
struct Waiting<T> {
    /// Oldest first.
    live: Vec<Live<T>>,
    /// When the id was last issued a new ceremony.
    last: Instant,
}

#[derive(Debug)]
pub struct Ceremonies<T> {
    ids: HashMap<String, Waiting<T>>,
    pub per_id: usize,
    pub ttl: Duration,
    pub cap: usize,
}

impl<T> Ceremonies<T> {
    pub fn new(per_id: usize, ttl: Duration, cap: usize) -> Self {
        Ceremonies {
            ids: HashMap::new(),
            per_id: per_id.max(1),
            ttl,
            cap: cap.max(1),
        }
    }

    /// `id`'s live ceremonies, the lapsed ones dropped.
    fn sweep(&mut self, id: &str, now: Instant) {
        if let Some(w) = self.ids.get_mut(id) {
            w.live.retain(|l| l.until > now);
        }
    }

    /// The newest live ceremony of `id`, when it has `per_id` of them (to be served again);
    /// None when a new one may be issued ([`Self::add`]).
    pub fn full(&mut self, id: &str, now: Instant) -> Option<&Live<T>> {
        self.sweep(id, now);
        let per_id = self.per_id;
        self.ids.get(id).filter(|w| w.live.len() >= per_id).and_then(|w| w.live.last())
    }

    /// Keep a ceremony issued to `id` now (the oldest given way if `id` was already full,
    /// which [`Self::full`] tells the caller first).
    pub fn add(&mut self, id: &str, now: Instant, challenge: [u8; 32], item: T) {
        self.sweep(id, now);
        if !self.ids.contains_key(id) && self.ids.len() >= self.cap {
            self.evict(now);
        }
        let per_id = self.per_id;
        let w = self.ids.entry(id.to_string()).or_insert(Waiting {
            live: Vec::new(),
            last: now,
        });
        while w.live.len() >= per_id {
            w.live.remove(0);
        }
        w.live.push(Live {
            challenge,
            until: now + self.ttl,
            item,
        });
        w.last = now;
    }

    /// The live ceremony of `id` with `challenge`, taken (answered once, right or wrong);
    /// None if there is none (another id's, answered already, lapsed, never issued).
    pub fn take(&mut self, id: &str, challenge: &[u8; 32], now: Instant) -> Option<T> {
        self.sweep(id, now);
        let w = self.ids.get_mut(id)?;
        let i = w.live.iter().position(|l| &l.challenge == challenge)?;
        Some(w.live.remove(i).item)
    }

    /// How many ceremonies of `id` are live.
    pub fn live(&mut self, id: &str, now: Instant) -> usize {
        self.sweep(id, now);
        self.ids.get(id).map_or(0, |w| w.live.len())
    }

    /// How many ids the table holds.
    pub fn ids(&self) -> usize {
        self.ids.len()
    }

    /// Room for one more id: every id with nothing live goes; if none, the id issued to
    /// longest ago.
    fn evict(&mut self, now: Instant) {
        self.ids.retain(|_, w| w.live.iter().any(|l| l.until > now));
        if self.ids.len() < self.cap {
            return;
        }
        let victim = self.ids.iter().min_by_key(|(_, w)| w.last).map(|(k, _)| k.clone());
        if let Some(k) = victim {
            self.ids.remove(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn c(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// Past the bound a request is served the newest live ceremony again: a flood neither
    /// refuses the operator nor pushes out the challenge they were given.
    #[test]
    fn past_the_bound_the_newest_is_served_again_and_nothing_is_pushed_out() {
        let mut t: Ceremonies<u8> = Ceremonies::new(4, Duration::from_secs(120), 100);
        let t0 = Instant::now();
        // the operator's own first
        assert!(t.full("op-2", t0).is_none());
        t.add("op-2", t0, c(1), 1);
        // a stranger's flood: three more issued, then the newest again and again
        for n in 2..=4 {
            assert!(t.full("op-2", t0).is_none());
            t.add("op-2", t0, c(n), n);
        }
        for _ in 0..1000 {
            let newest = t.full("op-2", t0 + Duration::from_secs(1)).expect("full");
            assert_eq!(newest.challenge, c(4));
        }
        assert_eq!(t.live("op-2", t0 + Duration::from_secs(1)), 4);
        // the operator's answer to their own still finds it
        assert_eq!(t.take("op-2", &c(1), t0 + Duration::from_secs(10)), Some(1));
        // answered once
        assert_eq!(t.take("op-2", &c(1), t0 + Duration::from_secs(10)), None);
        // a slot freed: the next request is a new one
        assert!(t.full("op-2", t0 + Duration::from_secs(10)).is_none());
        // another id's challenge is not this one's
        assert_eq!(t.take("op-3", &c(2), t0 + Duration::from_secs(10)), None);
        // lapsed: gone, and the id issues afresh
        assert_eq!(t.take("op-2", &c(2), t0 + Duration::from_secs(121)), None);
        assert_eq!(t.live("op-2", t0 + Duration::from_secs(121)), 0);
        assert!(t.full("op-2", t0 + Duration::from_secs(121)).is_none());
    }

    /// The table is bounded: ids with nothing live go first, then the one issued to longest
    /// ago (its next request is a fresh one).
    #[test]
    fn a_full_table_drops_idle_ids_first_then_the_oldest() {
        let mut t: Ceremonies<()> = Ceremonies::new(4, Duration::from_secs(120), 3);
        let t0 = Instant::now();
        t.add("op-2", t0, c(1), ());
        t.add("x1", t0 + Duration::from_secs(1), c(2), ());
        t.add("x2", t0 + Duration::from_secs(2), c(3), ());
        // x1's lapses: it goes before op-2, which still has one live
        t.take("x1", &c(2), t0 + Duration::from_secs(3)).unwrap();
        t.add("x3", t0 + Duration::from_secs(4), c(4), ());
        assert_eq!(t.ids(), 3);
        assert_eq!(t.live("op-2", t0 + Duration::from_secs(5)), 1);
        // all live: the id issued to longest ago (op-2) goes
        t.add("x4", t0 + Duration::from_secs(6), c(5), ());
        assert_eq!(t.ids(), 3);
        assert_eq!(t.live("op-2", t0 + Duration::from_secs(7)), 0);
        assert_eq!(t.live("x2", t0 + Duration::from_secs(7)), 1);
    }
}
