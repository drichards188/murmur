//! The per-direction link model. Pure: time is passed in as nanoseconds, so a fixed seed and
//! the same input timing give the same decisions, with no sockets or clocks involved.

use crate::config::{DownWhen, RawProfile};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    /// Vehicle to bridge.
    Up,
    /// Bridge to vehicle.
    Down,
}

impl Dir {
    pub fn idx(self) -> usize {
        match self {
            Dir::Up => 0,
            Dir::Down => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// Random and bursty channel loss.
    Loss,
    /// Scripted extra loss.
    EventLoss,
    /// Scripted jammer circle.
    Jam,
    /// Scripted outage.
    Cut,
    /// Range from the ground station.
    Range,
    /// No line of sight (terrain).
    Terrain,
    /// Link unavailable for the vehicle's depth (acoustic at surface, SATCOM submerged).
    Gate,
    /// Drop-tail queue or in-flight cap full.
    Queue,
    /// Bridge reply with no known vehicle address yet.
    NoPeer,
    /// Socket send failed.
    Socket,
}

pub const CAUSES: [Cause; 10] = [
    Cause::Loss,
    Cause::EventLoss,
    Cause::Jam,
    Cause::Cut,
    Cause::Range,
    Cause::Terrain,
    Cause::Gate,
    Cause::Queue,
    Cause::NoPeer,
    Cause::Socket,
];

impl Cause {
    pub fn name(self) -> &'static str {
        match self {
            Cause::Loss => "loss",
            Cause::EventLoss => "event_loss",
            Cause::Jam => "jam",
            Cause::Cut => "cut",
            Cause::Range => "range",
            Cause::Terrain => "terrain",
            Cause::Gate => "gate",
            Cause::Queue => "queue",
            Cause::NoPeer => "no_peer",
            Cause::Socket => "socket",
        }
    }
    pub fn idx(self) -> usize {
        CAUSES.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// A resolved link profile.
#[derive(Clone, Debug)]
pub struct Params {
    pub loss_good: f64,
    pub loss_bad: f64,
    pub p_gb: f64,
    pub p_bg: f64,
    pub latency_ns: u64,
    pub jitter_ns: u64,
    /// Indexed by `Dir::idx`; 0 is unlimited.
    pub bw_bps: [u64; 2],
    pub queue_bytes: [usize; 2],
    pub duplicate: f64,
    pub reorder: bool,
    pub max_inflight: usize,
    /// (ok_m, max_m, exponent)
    pub range: Option<(f64, f64, f64)>,
    pub down_when: Option<DownWhen>,
    pub propagation_mps: Option<f64>,
}

impl Params {
    pub fn from_profile(p: &RawProfile) -> Result<Params, String> {
        let avg = p.loss.unwrap_or(0.0);
        let good = p.loss_good.unwrap_or(0.0);
        let bad = p.loss_bad.unwrap_or(1.0);
        for (n, v) in [
            ("loss", avg),
            ("loss_good", good),
            ("loss_bad", bad),
            ("duplicate", p.duplicate.unwrap_or(0.0)),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{n} {v} outside 0..1"));
            }
        }
        if bad <= good {
            return Err("loss_bad must exceed loss_good".into());
        }
        let (p_gb, p_bg) = gilbert_elliott(avg, p.burst_len.unwrap_or(1.0), good, bad);
        let up_bw = p.bandwidth_bps.unwrap_or(0);
        let down_bw = p.down_bandwidth_bps.unwrap_or(up_bw);
        let queue = |bw: u64| match p.queue_bytes {
            Some(q) => q,
            None if bw == 0 => usize::MAX,
            None => ((bw / 8 / 4) as usize).max(1500),
        };
        let range = match (p.range_ok_km, p.range_max_km) {
            (Some(ok), Some(max)) if max > ok => {
                Some((ok * 1000.0, max * 1000.0, p.range_exp.unwrap_or(2.0)))
            }
            (None, None) => None,
            _ => return Err("range_ok_km and range_max_km must both be set, max > ok".into()),
        };
        Ok(Params {
            loss_good: good,
            loss_bad: bad,
            p_gb,
            p_bg,
            latency_ns: ms_to_ns(p.latency_ms.unwrap_or(0.0)),
            jitter_ns: ms_to_ns(p.jitter_ms.unwrap_or(0.0)),
            bw_bps: [up_bw, down_bw],
            queue_bytes: [queue(up_bw), queue(down_bw)],
            duplicate: p.duplicate.unwrap_or(0.0),
            reorder: p.reorder.unwrap_or(false),
            max_inflight: p.max_inflight.unwrap_or(2048),
            range,
            down_when: p.down_when,
            propagation_mps: p.propagation_mps,
        })
    }
}

fn ms_to_ns(ms: f64) -> u64 {
    (ms.max(0.0) * 1e6) as u64
}

/// Transition probabilities (good to bad, bad to good) of a Gilbert-Elliott chain whose
/// stationary loss is `avg` and whose bad bursts last `burst` packets on average.
pub fn gilbert_elliott(avg: f64, burst: f64, good: f64, bad: f64) -> (f64, f64) {
    if avg <= good {
        return (0.0, 1.0);
    }
    let pi = ((avg - good) / (bad - good)).min(1.0);
    if pi >= 1.0 {
        return (1.0, 0.0);
    }
    // A burst of independent losses at rate p already averages 1/(1-p) packets, so shorter
    // requested bursts collapse to the Bernoulli case.
    let min_burst = 1.0 / (1.0 - pi);
    let p_bg = 1.0 / burst.max(min_burst);
    let p_gb = (pi * p_bg / (1.0 - pi)).min(1.0);
    (p_gb, p_bg)
}

/// Conditions at the moment a packet arrives, worked out by the link from events, position
/// and gating. `blocked` drops the packet before it takes any airtime.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cond {
    pub blocked: Option<Cause>,
    pub event_loss: f64,
    pub jam_loss: f64,
    pub range_loss: f64,
    pub extra_latency_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Drop(Cause),
    Forward { at_ns: u64, dup_at_ns: Option<u64> },
}

pub struct DirState {
    rng: ChaCha8Rng,
    bad: bool,
    busy_until_ns: u64,
    last_at_ns: u64,
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl DirState {
    pub fn new(seed: u64, uid: u32, dir: Dir) -> DirState {
        let s = splitmix64(seed ^ splitmix64(u64::from(uid) << 1 | dir.idx() as u64));
        DirState {
            rng: ChaCha8Rng::seed_from_u64(s),
            bad: false,
            busy_until_ns: 0,
            last_at_ns: 0,
        }
    }

    /// Bytes waiting behind the packet in service at `now_ns`.
    pub fn backlog_bytes(&self, now_ns: u64, bw_bps: u64) -> u64 {
        if bw_bps == 0 {
            return 0;
        }
        let ns = self.busy_until_ns.saturating_sub(now_ns);
        (u128::from(ns) * u128::from(bw_bps) / 8_000_000_000u128) as u64
    }

    /// Decide the fate of one packet of `len` bytes arriving at `now_ns`. Exactly seven random
    /// numbers are consumed per call whatever the outcome, so one packet's fate does not
    /// shift the random stream seen by the next.
    pub fn decide(
        &mut self,
        p: &Params,
        dir: Dir,
        c: &Cond,
        now_ns: u64,
        len: usize,
        inflight: usize,
    ) -> Verdict {
        let r: [f64; 7] = [
            self.rng.random(),
            self.rng.random(),
            self.rng.random(),
            self.rng.random(),
            self.rng.random(),
            self.rng.random(),
            self.rng.random(),
        ];
        self.bad = if self.bad {
            r[0] >= p.p_bg
        } else {
            r[0] < p.p_gb
        };
        let ch_loss = if self.bad { p.loss_bad } else { p.loss_good };

        if let Some(cause) = c.blocked {
            return Verdict::Drop(cause);
        }
        let bw = p.bw_bps[dir.idx()];
        let backlog = self.backlog_bytes(now_ns, bw);
        if inflight >= p.max_inflight
            || (backlog > 0 && backlog + len as u64 > p.queue_bytes[dir.idx()] as u64)
        {
            return Verdict::Drop(Cause::Queue);
        }
        // The packet takes its airtime even if the channel then loses it.
        let depart = if bw == 0 {
            now_ns
        } else {
            let start = self.busy_until_ns.max(now_ns);
            let tx = (len as u128 * 8_000_000_000u128 / u128::from(bw)) as u64;
            self.busy_until_ns = start + tx;
            start + tx
        };
        if r[1] < ch_loss {
            return Verdict::Drop(Cause::Loss);
        }
        if r[2] < c.jam_loss {
            return Verdict::Drop(Cause::Jam);
        }
        if r[3] < c.event_loss {
            return Verdict::Drop(Cause::EventLoss);
        }
        if r[4] < c.range_loss {
            return Verdict::Drop(Cause::Range);
        }
        let jitter = (r[5] * 2.0 - 1.0) * p.jitter_ns as f64;
        let delay = (p.latency_ns as f64 + c.extra_latency_ns as f64 + jitter).max(0.0) as u64;
        let mut at = depart + delay;
        if !p.reorder {
            at = at.max(self.last_at_ns);
        }
        self.last_at_ns = at;
        let dup = r[6] < p.duplicate;
        Verdict::Forward {
            at_ns: at,
            dup_at_ns: dup.then_some(at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(f: impl FnOnce(&mut RawProfile)) -> Params {
        let mut p = RawProfile::default();
        f(&mut p);
        Params::from_profile(&p).expect("params")
    }

    fn run(p: &Params, seed: u64, n: usize, gap_ns: u64, len: usize) -> Vec<Verdict> {
        let mut d = DirState::new(seed, 1, Dir::Up);
        (0..n)
            .map(|i| d.decide(p, Dir::Up, &Cond::default(), i as u64 * gap_ns, len, 0))
            .collect()
    }

    #[test]
    fn loss_converges_to_configured_value() {
        for target in [0.01, 0.1, 0.3] {
            let p = params(|p| {
                p.loss = Some(target);
                p.burst_len = Some(4.0);
            });
            let v = run(&p, 7, 200_000, 1_000_000, 50);
            let lost = v
                .iter()
                .filter(|x| matches!(x, Verdict::Drop(Cause::Loss)))
                .count();
            let rate = lost as f64 / v.len() as f64;
            assert!(
                (rate - target).abs() < 0.01 + target * 0.05,
                "target {target} got {rate}"
            );
        }
    }

    #[test]
    fn bursts_have_the_configured_mean_length() {
        let p = params(|p| {
            p.loss = Some(0.1);
            p.burst_len = Some(5.0);
        });
        let v = run(&p, 11, 400_000, 1_000_000, 50);
        let (mut bursts, mut lost, mut cur) = (0u64, 0u64, 0u64);
        for x in &v {
            if matches!(x, Verdict::Drop(_)) {
                cur += 1;
                lost += 1;
            } else if cur > 0 {
                bursts += 1;
                cur = 0;
            }
        }
        let mean = lost as f64 / bursts as f64;
        assert!((mean - 5.0).abs() < 0.4, "mean burst {mean}");
        // And an unbursty link of the same average has a mean near 1/(1-p).
        let q = params(|p| p.loss = Some(0.1));
        let v = run(&q, 11, 400_000, 1_000_000, 50);
        let (mut bursts, mut lost, mut cur) = (0u64, 0u64, 0u64);
        for x in &v {
            if matches!(x, Verdict::Drop(_)) {
                cur += 1;
                lost += 1;
            } else if cur > 0 {
                bursts += 1;
                cur = 0;
            }
        }
        let mean = lost as f64 / bursts as f64;
        assert!((mean - 1.0 / 0.9).abs() < 0.05, "mean burst {mean}");
    }

    #[test]
    fn same_seed_same_decisions_different_seed_differs() {
        let p = params(|p| {
            p.loss = Some(0.2);
            p.burst_len = Some(3.0);
            p.latency_ms = Some(20.0);
            p.jitter_ms = Some(5.0);
            p.duplicate = Some(0.05);
        });
        let a = run(&p, 42, 5000, 2_000_000, 60);
        let b = run(&p, 42, 5000, 2_000_000, 60);
        let c = run(&p, 43, 5000, 2_000_000, 60);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn latency_and_jitter_stay_in_bounds_without_reordering() {
        let p = params(|p| {
            p.latency_ms = Some(100.0);
            p.jitter_ms = Some(30.0);
        });
        let mut last = 0;
        for (i, v) in run(&p, 5, 10_000, 5_000_000, 40).into_iter().enumerate() {
            let Verdict::Forward { at_ns, .. } = v else {
                panic!("dropped")
            };
            let sent = i as u64 * 5_000_000;
            assert!(at_ns >= sent + 70_000_000 && at_ns <= sent + 130_000_000);
            assert!(at_ns >= last, "reordered");
            last = at_ns;
        }
    }

    #[test]
    fn reorder_when_allowed() {
        let p = params(|p| {
            p.latency_ms = Some(100.0);
            p.jitter_ms = Some(90.0);
            p.reorder = Some(true);
        });
        let mut last = 0;
        let mut reordered = false;
        for v in run(&p, 5, 2000, 5_000_000, 40) {
            let Verdict::Forward { at_ns, .. } = v else {
                panic!()
            };
            reordered |= at_ns < last;
            last = at_ns;
        }
        assert!(reordered);
    }

    #[test]
    fn bandwidth_cap_causes_queuing_delay_then_tail_drop() {
        // 8 kbit/s = 1000 B/s; 100 B packets take 100 ms; queue holds 500 B.
        let p = params(|p| {
            p.bandwidth_bps = Some(8000);
            p.queue_bytes = Some(500);
        });
        let mut d = DirState::new(1, 1, Dir::Up);
        let mut delays = Vec::new();
        let mut drops = 0;
        // Offered 10x the capacity: one 100 B packet every 10 ms.
        for i in 0..200u64 {
            let now = i * 10_000_000;
            match d.decide(&p, Dir::Up, &Cond::default(), now, 100, 0) {
                Verdict::Forward { at_ns, .. } => delays.push(at_ns - now),
                Verdict::Drop(Cause::Queue) => drops += 1,
                other => panic!("{other:?}"),
            }
        }
        assert!(
            delays[0] == 100_000_000,
            "first packet pays only its own airtime"
        );
        let max = delays.iter().max().copied().unwrap_or(0);
        // The 500 B queue (the packet in service included) is 500 ms of line time.
        assert!(
            (450_000_000..=500_000_000).contains(&max),
            "max delay {max}"
        );
        // Goodput is the line rate: about 1000 B/s over 2 s is 20 packets.
        assert!(
            (18..=24).contains(&delays.len()),
            "delivered {}",
            delays.len()
        );
        assert_eq!(delays.len() + drops, 200);
    }

    #[test]
    fn under_capacity_there_is_no_queuing() {
        let p = params(|p| p.bandwidth_bps = Some(80_000));
        let mut d = DirState::new(1, 1, Dir::Up);
        for i in 0..100u64 {
            let now = i * 100_000_000;
            let Verdict::Forward { at_ns, .. } =
                d.decide(&p, Dir::Up, &Cond::default(), now, 100, 0)
            else {
                panic!()
            };
            assert_eq!(at_ns - now, 10_000_000);
        }
    }

    #[test]
    fn inflight_cap_bounds_memory() {
        let p = params(|p| p.max_inflight = Some(10));
        let mut d = DirState::new(1, 1, Dir::Up);
        assert_eq!(
            d.decide(&p, Dir::Up, &Cond::default(), 0, 50, 10),
            Verdict::Drop(Cause::Queue)
        );
    }

    #[test]
    fn blocked_conditions_and_event_loss() {
        let p = params(|_| {});
        let mut d = DirState::new(1, 1, Dir::Up);
        let c = Cond {
            blocked: Some(Cause::Cut),
            ..Cond::default()
        };
        assert_eq!(
            d.decide(&p, Dir::Up, &c, 0, 50, 0),
            Verdict::Drop(Cause::Cut)
        );
        let c = Cond {
            event_loss: 0.3,
            ..Cond::default()
        };
        let n = 100_000;
        let lost = (0..n)
            .filter(|i| {
                matches!(
                    d.decide(&p, Dir::Up, &c, *i as u64 * 1000, 50, 0),
                    Verdict::Drop(Cause::EventLoss)
                )
            })
            .count();
        assert!((lost as f64 / n as f64 - 0.3).abs() < 0.01);
    }

    #[test]
    fn duplicates_follow_the_configured_rate() {
        let p = params(|p| p.duplicate = Some(0.1));
        let dups = run(&p, 3, 100_000, 1_000_000, 50)
            .iter()
            .filter(|v| {
                matches!(
                    v,
                    Verdict::Forward {
                        dup_at_ns: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert!((dups as f64 / 100_000.0 - 0.1).abs() < 0.01);
    }

    #[test]
    fn rejects_bad_parameters() {
        let p = RawProfile {
            loss: Some(1.5),
            ..RawProfile::default()
        };
        assert!(Params::from_profile(&p).is_err());
        let p = RawProfile {
            range_ok_km: Some(10.0),
            ..RawProfile::default()
        };
        assert!(Params::from_profile(&p).is_err());
    }
}
