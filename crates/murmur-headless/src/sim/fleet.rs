//! Our vehicles: the stand-ins' own code (`murmur_standins::vehicle::Vehicle`), as the stand-in
//! engine runs them (`vehicle::run`), with the socket replaced: each 10 Hz step's frames are
//! taken with `Link::capture` and handed to the emulator, and what the emulator delivers goes
//! to `on_datagram`. Each vehicle draws from the episode seed (`Vehicle::seeded`), not the
//! wall clock. Open sky (no GNSS geometry thread: `Sky::LEGACY`), and no tidewater conditions
//! unless the world gives water (`water.rs`): docs/HEADLESS.md, "What the simulator leaves out".
//!
//! The faults of world v3's H4 that live in the vehicle (`faults.rs`) act here: a battery's
//! draw times a rate (`Drain`), a reboot (`reboot`: a fresh autopilot, silent for its boot), a
//! GNSS environment of jammers and spoofers (`set_gnss`, the stand-ins' own `gnss::Env`).
//! A vehicle none of them touches steps exactly as before.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use murmur_standins::conditions::Slot;
use murmur_standins::config::VehicleConfig;
use murmur_standins::gnss;
use murmur_standins::vehicle::{TICK, Vehicle};
use tokio::net::UdpSocket;
use tokio::time::{Instant, MissedTickBehavior};

use super::emulator::Emulator;
use super::water::Water;

/// A battery fault's new draw: the pack drains `rate` times as fast from the vehicle's own
/// second `from_s` until `until_s` (none: to the end).
#[derive(Debug, Clone, Copy)]
pub struct Drain {
    pub rate: f64,
    pub from_s: f64,
    pub until_s: Option<f64>,
}

/// One vehicle and what the faults hold for it.
struct Held {
    v: Vehicle,
    drain: Option<Drain>,
    /// Rebooting: nothing goes out or comes in until then.
    silent_until: Option<Instant>,
}

pub struct Fleet {
    vehicles: Vec<Mutex<Held>>,
    /// Gone from the world (an engagement's platform spent by its own effect, `Spec::effects`):
    /// it neither steps nor sends, and answers nothing.
    gone: Vec<AtomicBool>,
    /// The episode seed (a rebooted autopilot's draws start from it again, offset per reboot).
    seed: u64,
    reboots: Mutex<u64>,
}

fn lock(m: &Mutex<Held>) -> MutexGuard<'_, Held> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A socket for the stand-in's `Link` that is never used for a packet (its frames are
/// captured): unbound, so it takes no port. Inside a tokio runtime.
fn unbound() -> Result<UdpSocket, String> {
    let raw = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)
        .map_err(|e| e.to_string())?;
    raw.set_nonblocking(true).map_err(|e| e.to_string())?;
    UdpSocket::from_std(raw.into()).map_err(|e| e.to_string())
}

impl Fleet {
    /// Every stand-in of the episode, seeded, its frames captured; `battery_pct` starts the
    /// named ones with that much charge (a pack not full at take-off). With `water` (the world's,
    /// `spec.water`), each vehicle that takes conditions gets its slot and a first sample at its
    /// spawn at `t0` (scenario unix s), as the stand-in engine awaits its first poll.
    pub fn new(
        cfgs: Vec<VehicleConfig>,
        seed: u64,
        battery_pct: &std::collections::BTreeMap<u32, f64>,
        water: Option<(&Water, f64)>,
    ) -> Result<Fleet, String> {
        let mut vehicles = Vec::with_capacity(cfgs.len());
        for cfg in cfgs {
            let uid = cfg.uid();
            let mut v = Vehicle::seeded(cfg, unbound()?, None, seed)?;
            v.link.capture();
            if let Some(pct) = battery_pct.get(&uid) {
                let cap = v.capacity_mah();
                v.battery.consumed_mah = cap * (1.0 - pct.clamp(0.0, 100.0) / 100.0);
            }
            if let Some((w, t0)) = water
                && v.behavior.wants_conditions()
            {
                let slot = Arc::new(Slot::default());
                slot.report(v.st.lat, v.st.lon, v.st.depth());
                slot.put(w.sample(v.st.lat, v.st.lon, t0));
                v.water = Some(slot);
            }
            vehicles.push(Mutex::new(Held { v, drain: None, silent_until: None }));
        }
        let gone = vehicles.iter().map(|_| AtomicBool::new(false)).collect();
        Ok(Fleet { vehicles, gone, seed, reboots: Mutex::new(0) })
    }

    /// Vehicle `i` leaves the world: from now on it neither steps nor sends nor answers (a
    /// loitering munition spent by its own effect). Its last state stays readable for truth.
    pub fn remove(&self, i: usize) {
        self.gone[i].store(true, Ordering::Relaxed);
    }

    /// Whether vehicle `i` has left the world ([`Self::remove`]).
    pub fn is_gone(&self, i: usize) -> bool {
        self.gone[i].load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        self.vehicles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vehicles.is_empty()
    }

    fn silent(s: &Held) -> bool {
        s.silent_until.is_some_and(|t| Instant::now() < t)
    }

    /// One 10 Hz step of vehicle `i` and the frames it sent (none while it reboots, none once
    /// it is gone).
    pub fn step(&self, i: usize) -> Vec<Vec<u8>> {
        if self.is_gone(i) {
            return Vec::new();
        }
        let mut s = lock(&self.vehicles[i]);
        match s.drain {
            None => s.v.step(),
            Some(d) => {
                let before = s.v.battery.consumed_mah;
                s.v.step();
                let now = s.v.now();
                if now >= d.from_s && d.until_s.is_none_or(|u| now < u) {
                    // the same load times the rate: the pack's draw and the current the
                    // autopilot's sensor reads
                    let cap = s.v.capacity_mah();
                    let used = s.v.battery.consumed_mah - before;
                    s.v.battery.consumed_mah = (before + used * d.rate).clamp(0.0, cap.max(0.0));
                    s.v.battery.current_a *= d.rate;
                }
            }
        }
        let frames = s.v.link.take_captured();
        if Self::silent(&s) { Vec::new() } else { frames }
    }

    /// A datagram from the bridge for vehicle `i`, and the frames it answered with (unheard
    /// while it reboots, or once it is gone).
    pub fn deliver(&self, i: usize, data: &[u8]) -> Vec<Vec<u8>> {
        if self.is_gone(i) {
            return Vec::new();
        }
        let mut s = lock(&self.vehicles[i]);
        if Self::silent(&s) {
            return Vec::new();
        }
        s.v.on_datagram(data);
        s.v.link.take_captured()
    }

    /// Read vehicle `i`.
    pub fn read<R>(&self, i: usize, f: impl FnOnce(&Vehicle) -> R) -> R {
        f(&lock(&self.vehicles[i]).v)
    }

    /// Change vehicle `i` (a scripted fault, outside the bridge).
    pub fn write<R>(&self, i: usize, f: impl FnOnce(&mut Vehicle) -> R) -> R {
        f(&mut lock(&self.vehicles[i]).v)
    }

    /// A battery fault's new draw on vehicle `i`, from its own second now.
    pub fn set_drain(&self, i: usize, rate: f64, duration_s: Option<f64>) {
        let mut s = lock(&self.vehicles[i]);
        let now = s.v.now();
        s.drain = Some(Drain { rate, from_s: now, until_s: duration_s.map(|d| now + d) });
    }

    /// Every vehicle's GNSS environment (jammers and spoofers: the stand-ins' own `gnss::Env`).
    pub fn set_gnss(&self, env: &Arc<gnss::Env>) {
        for m in &self.vehicles {
            lock(m).v.gnss = Some(env.clone());
        }
    }

    /// The GNSS environment the vehicles share now (none: open sky, no interference).
    pub fn gnss(&self) -> Option<Arc<gnss::Env>> {
        self.vehicles.first().and_then(|m| lock(m).v.gnss.clone())
    }

    /// Vehicle `i`'s autopilot restarts (`faults.rs` says what is modelled on what): a fresh
    /// `Vehicle` from its config where it truly is now, its time since boot from 0, its stream
    /// rates, failsafes, navigation filter and link counters new; its parameters, its stored
    /// mission and its pack carried over. On the ground or the water it boots disarmed in its
    /// boot mode, home where it is, the mission from its start; in the air (ArduPilot's
    /// watchdog restore) armed, in its mode, with its home and its place in the mission. It is
    /// silent, sending nothing and hearing nothing, until `silent_until`.
    pub fn reboot(&self, i: usize, silent_until: Instant) -> Result<serde_json::Value, String> {
        let n = {
            let mut r = self.reboots.lock().unwrap_or_else(|e| e.into_inner());
            *r += 1;
            *r
        };
        let mut s = lock(&self.vehicles[i]);
        let old = &mut s.v;
        let airborne = old.airborne();
        let (lat, lon, alt) = old.truth();
        let mut cfg = old.cfg.clone();
        cfg.home = [lat, lon, alt, old.st.heading];
        let seed = self.seed ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut v = Vehicle::seeded(cfg, unbound()?, old.water.clone(), seed)?;
        v.link.capture();
        v.params = std::mem::take(&mut old.params);
        v.battery = old.battery.clone();
        v.battery.level = murmur_standins::battery::Level::Ok;
        v.gnss = old.gnss.clone();
        v.crashed = old.crashed;
        let mut st = old.st.clone();
        // the restarted filter starts at the fix, where the vehicle is
        st.lat = lat;
        st.lon = lon;
        if airborne {
            v.mission = std::mem::take(&mut old.mission);
            v.mission.upload = None;
        } else {
            let items = std::mem::take(&mut old.mission.items);
            v.mission.install(items);
            v.mission.fence = std::mem::take(&mut old.mission.fence);
            v.mission.rally = std::mem::take(&mut old.mission.rally);
            st.armed = false;
            st.mode = v.st.mode;
            st.hold_point = (lat, lon);
            st.home = (lat, lon, alt);
            st.demand = 0.0;
        }
        v.st = st;
        let out = serde_json::json!({
            "uid": v.cfg.uid(),
            "airborne": airborne,
            "mode": v.st.mode,
            "armed": v.st.armed,
        });
        s.v = v;
        s.drain = None;
        s.silent_until = Some(silent_until);
        Ok(out)
    }
}

/// Vehicle `i`'s task, as the stand-in engine's `vehicle::run`: 10 Hz from `start` plus its
/// phase (`TICK * i / n`, so vehicles are not in lockstep), every step's frames to the
/// emulator. On the paused clock no tick is ever late, so the engine's stall skip never acts.
pub async fn run(fleet: Arc<Fleet>, emu: Arc<Emulator>, i: usize, start: Instant) {
    let n = u32::try_from(fleet.len().max(1)).unwrap_or(u32::MAX);
    let phase = TICK * u32::try_from(i).unwrap_or(0) / n;
    let mut every = tokio::time::interval_at(start + phase, TICK);
    every.set_missed_tick_behavior(MissedTickBehavior::Burst);
    loop {
        every.tick().await;
        let frames = fleet.step(i);
        emu.uplink(i, frames);
    }
}

/// The conditions poller (crates/murmur-standins `conditions::start`): about once a second from
/// the stand-ins' start, a sample for every vehicle that takes them, at where it last said it
/// was, from the world's water at scenario time `epoch` plus the situation's second.
pub async fn feed_water(fleet: Arc<Fleet>, water: Arc<Water>, start: Instant, zero: Instant, epoch: f64) {
    let mut every = tokio::time::interval_at(start + Duration::from_secs(1), Duration::from_secs(1));
    every.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let t = epoch + Instant::now().saturating_duration_since(zero).as_secs_f64();
        for i in 0..fleet.len() {
            let slot = fleet.read(i, |v| v.water.clone());
            if let Some(slot) = slot
                && let Some([lat, lon, _]) = slot.position()
            {
                slot.put(water.sample(lat, lon, t));
            }
        }
    }
}

/// When the stand-in engine starts, from the situation's zero.
pub fn start_at(zero: Instant, s: f64) -> Instant {
    zero + Duration::from_secs_f64(s.max(0.0))
}
