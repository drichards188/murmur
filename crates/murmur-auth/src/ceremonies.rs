//! Passkey login ceremonies waiting for an answer, per operator id claimed (rules 10,
//! murmur-i58).
//!
//! A login's options (`POST /api/auth/login/passkey {"operator"}`) are asked for before anyone
//! is signed in, so their issuance is limited per id: at most [`Ceremonies::per_id`] live
//! (issued, unanswered, younger than [`Ceremonies::ttl`]) per id. A request past that gets the
//! newest live one's options again instead of a new challenge ([`Ceremonies::full`]): no new
//! state, no work for the bridge (a real operator's challenge takes the decision lock), and
//! nothing refused or pushed out by requests for the same id. Ids without a passkey, and ids
//! that don't exist, go through the same table with the same bounds (their options are
//! decoys), so the limit says nothing about which is which.
//!
//! Bounded: `per_id` ceremonies per id, [`Ceremonies::cap`] ids. When the table is full the id
//! issued to longest ago goes, whoever it is (keeping roster ids would show, by how they are
//! served after a flood, which ids are on the roster), in constant time: ids are kept in the
//! order they were last issued to. So a sustained flood of made-up ids (`cap` of them within a
//! ceremony's lifetime) can push out the challenge an operator is answering; the bridge refuses
//! an answer to a challenge that is not live without counting it against the operator (it
//! can't succeed, so it is no guess), and the operator asks again.

use std::collections::{HashMap, VecDeque};
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
    /// The number of the id's latest issue (its place in [`Ceremonies::order`]).
    issue: u64,
}

#[derive(Debug)]
pub struct Ceremonies<T> {
    ids: HashMap<String, Waiting<T>>,
    /// Ids by the issue that put them there, oldest first. An entry whose number is not its
    /// id's latest issue is stale: skipped when evicting, dropped when the queue is compacted
    /// (when stale entries outnumber the ids), so every operation is amortized constant time.
    order: VecDeque<(String, u64)>,
    next: u64,
    pub per_id: usize,
    pub ttl: Duration,
    pub cap: usize,
}

impl<T> Ceremonies<T> {
    pub fn new(per_id: usize, ttl: Duration, cap: usize) -> Self {
        Ceremonies {
            ids: HashMap::new(),
            order: VecDeque::new(),
            next: 0,
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
        if !self.ids.contains_key(id) {
            while self.ids.len() >= self.cap && self.evict_one() {}
        }
        let issue = self.next;
        self.next += 1;
        let per_id = self.per_id;
        let w = self.ids.entry(id.to_string()).or_insert(Waiting { live: Vec::new(), issue });
        while w.live.len() >= per_id {
            w.live.remove(0);
        }
        w.live.push(Live {
            challenge,
            until: now + self.ttl,
            item,
        });
        w.issue = issue;
        self.order.push_back((id.to_string(), issue));
        if self.order.len() > 2 * self.cap.max(self.ids.len()) + 16 {
            let ids = &self.ids;
            self.order.retain(|(k, n)| ids.get(k).is_some_and(|w| w.issue == *n));
        }
    }

    /// Drop the id issued to longest ago; false if there is none.
    fn evict_one(&mut self) -> bool {
        while let Some((k, n)) = self.order.pop_front() {
            if self.ids.get(&k).is_some_and(|w| w.issue == n) {
                self.ids.remove(&k);
                return true;
            }
        }
        false
    }

    /// The live ceremony of `id` with `challenge`, taken (answered once, right or wrong);
    /// None if there is none (another id's, answered already, lapsed, pushed out, never issued).
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

    /// How many entries the issue order holds (bounded by about twice the ids kept).
    pub fn order_len(&self) -> usize {
        self.order.len()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn c(n: u8) -> [u8; 32] {
        [n; 32]
    }

    /// Past the bound a request is served the newest live ceremony again: a flood for the id
    /// neither refuses the operator nor pushes out the challenge they were given.
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

    /// The table is bounded: the id issued to longest ago goes, whoever it is; an id issued to
    /// again moves to the back.
    #[test]
    fn a_full_table_drops_the_id_issued_to_longest_ago() {
        let mut t: Ceremonies<()> = Ceremonies::new(4, Duration::from_secs(120), 3);
        let t0 = Instant::now();
        t.add("op-2", t0, c(1), ());
        t.add("x1", t0 + Duration::from_secs(1), c(2), ());
        t.add("x2", t0 + Duration::from_secs(2), c(3), ());
        // op-2 issued to again: now x1 is the oldest
        t.add("op-2", t0 + Duration::from_secs(3), c(4), ());
        t.add("x3", t0 + Duration::from_secs(4), c(5), ());
        assert_eq!(t.ids(), 3);
        assert_eq!(t.live("x1", t0 + Duration::from_secs(5)), 0);
        assert_eq!(t.live("op-2", t0 + Duration::from_secs(5)), 2);
        // then x2, then op-2
        t.add("x4", t0 + Duration::from_secs(6), c(6), ());
        assert_eq!(t.live("x2", t0 + Duration::from_secs(7)), 0);
        t.add("x5", t0 + Duration::from_secs(8), c(7), ());
        assert_eq!(t.live("op-2", t0 + Duration::from_secs(9)), 0);
        assert_eq!(t.ids(), 3);
    }

    /// Eviction walks no table: a flood of distinct ids far past the cap keeps the ids and the
    /// issue order bounded, and repeated issues to a few ids don't grow the order either.
    #[test]
    fn a_flood_stays_bounded_without_scanning() {
        let cap = 1_000;
        let mut t: Ceremonies<()> = Ceremonies::new(4, Duration::from_secs(120), cap);
        let t0 = Instant::now();
        for k in 0..200_000u32 {
            t.add(&format!("x{k}"), t0, c(1), ());
            assert!(t.ids() <= cap);
            assert!(t.order_len() <= 2 * cap + 17, "{}", t.order_len());
        }
        for k in 0..200_000u32 {
            t.add(&format!("y{}", k % 7), t0, c(2), ());
            assert!(t.order_len() <= 2 * cap + 17, "{}", t.order_len());
        }
        assert_eq!(t.live("y3", t0), 4);
        assert_eq!(t.ids(), cap);
    }
}
