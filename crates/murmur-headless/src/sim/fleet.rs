//! Our vehicles: the stand-ins' own code (`murmur_standins::vehicle::Vehicle`), as the stand-in
//! engine runs them (`vehicle::run`), with the socket replaced: each 10 Hz step's frames are
//! taken with `Link::capture` and handed to the emulator, and what the emulator delivers goes
//! to `on_datagram`. Each vehicle draws from the episode seed (`Vehicle::seeded`), not the
//! wall clock. No tidewater conditions (no current) and open sky (no GNSS geometry thread):
//! docs/HEADLESS.md, "What the simulator leaves out".

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use murmur_standins::config::VehicleConfig;
use murmur_standins::vehicle::{TICK, Vehicle};
use tokio::net::UdpSocket;
use tokio::time::{Instant, MissedTickBehavior};

use super::emulator::Emulator;

pub struct Fleet {
    vehicles: Vec<Mutex<Vehicle>>,
}

fn lock(m: &Mutex<Vehicle>) -> MutexGuard<'_, Vehicle> {
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
    /// named ones with that much charge (a pack not full at take-off).
    pub fn new(
        cfgs: Vec<VehicleConfig>,
        seed: u64,
        battery_pct: &std::collections::BTreeMap<u32, f64>,
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
            vehicles.push(Mutex::new(v));
        }
        Ok(Fleet { vehicles })
    }

    pub fn len(&self) -> usize {
        self.vehicles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vehicles.is_empty()
    }

    /// One 10 Hz step of vehicle `i` and the frames it sent.
    pub fn step(&self, i: usize) -> Vec<Vec<u8>> {
        let mut v = lock(&self.vehicles[i]);
        v.step();
        v.link.take_captured()
    }

    /// A datagram from the bridge for vehicle `i`, and the frames it answered with.
    pub fn deliver(&self, i: usize, data: &[u8]) -> Vec<Vec<u8>> {
        let mut v = lock(&self.vehicles[i]);
        v.on_datagram(data);
        v.link.take_captured()
    }

    /// Read vehicle `i`.
    pub fn read<R>(&self, i: usize, f: impl FnOnce(&Vehicle) -> R) -> R {
        f(&lock(&self.vehicles[i]))
    }

    /// Change vehicle `i` (a scripted fault, outside the bridge).
    pub fn write<R>(&self, i: usize, f: impl FnOnce(&mut Vehicle) -> R) -> R {
        f(&mut lock(&self.vehicles[i]))
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

/// When the stand-in engine starts, from the situation's zero.
pub fn start_at(zero: Instant, s: f64) -> Instant {
    zero + Duration::from_secs_f64(s.max(0.0))
}
