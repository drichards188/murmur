//! One stand-in: autopilot state, its behavior, its link, and the task that runs it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mavlink::dialects::ardupilotmega::{MavMessage, MavSeverity};
use tokio::net::UdpSocket;
use tokio::time::MissedTickBehavior;

use crate::behavior::{self, Behavior, Tick};
use crate::conditions::Slot;
use crate::config::VehicleConfig;
use crate::geo::Gauss;
use crate::kinematics::{Out, State};
use crate::link::{self, Link};
use crate::mission::Mission;
use crate::{messages, telemetry};

pub const PHYSICS_HZ: u32 = 10;

pub struct Vehicle {
    pub cfg: VehicleConfig,
    pub behavior: Box<dyn Behavior>,
    pub st: State,
    pub mission: Mission,
    pub link: Link,
    pub started: Instant,
    pub tick: u64,
    pub water: Option<Arc<Slot>>,
    rng: Gauss,
    out: Vec<Out>,
}

impl Vehicle {
    pub fn new(
        cfg: VehicleConfig,
        sock: UdpSocket,
        water: Option<Arc<Slot>>,
    ) -> Result<Vehicle, String> {
        let behavior = behavior::create(&cfg.spec.behavior)
            .ok_or_else(|| format!("{}: unknown behavior {:?}", cfg.callsign, cfg.spec.behavior))?;
        let [lat, lon, alt, hdg] = cfg.home;
        let passive = if behavior.mode_number("MANUAL").is_some() {
            "MANUAL"
        } else {
            behavior.hold_mode()
        };
        let mut st = State {
            lat,
            lon,
            alt,
            heading: hdg,
            speed: 0.0,
            vz: 0.0,
            ve: 0.0,
            vn: 0.0,
            home: (lat, lon, alt),
            armed: false,
            mode: passive,
            hold_point: (lat, lon),
            aground: false,
        };
        behavior.spawn(&cfg.spec, &mut st);
        let seed = u64::from(cfg.uid()).wrapping_mul(0x1000_0000_01B3) ^ wall_nanos();
        let link = Link::new(sock, cfg.sysid());
        Ok(Vehicle {
            cfg,
            behavior,
            st,
            mission: Mission::default(),
            link,
            started: Instant::now(),
            tick: 0,
            water,
            rng: Gauss::new(seed),
            out: Vec::new(),
        })
    }

    pub fn sysid(&self) -> u8 {
        self.cfg.sysid()
    }

    pub fn airborne(&self) -> bool {
        self.behavior.airborne(&self.st)
    }

    pub fn set_mode(&mut self, name: &'static str) {
        if name != self.st.mode {
            self.st.mode = name;
            self.st.hold_point = (self.st.lat, self.st.lon);
        }
    }

    pub fn text(&mut self, severity: MavSeverity, s: &str) {
        self.link.send(&messages::statustext(severity, s));
    }

    /// One physics step and the telemetry due on this tick.
    pub fn step(&mut self) {
        let dt = 1.0 / f64::from(PHYSICS_HZ);
        let sample = if self.behavior.marine() {
            self.water.as_ref().and_then(|w| w.latest())
        } else {
            None
        };
        let mut out = std::mem::take(&mut self.out);
        self.behavior.step(&mut Tick {
            spec: &self.cfg.spec,
            st: &mut self.st,
            mission: &mut self.mission,
            water: sample.as_ref(),
            rng: &mut self.rng,
            out: &mut out,
            dt,
        });
        for o in out.drain(..) {
            self.link.send(&messages::out(o));
        }
        self.out = out;
        if let Some(w) = &self.water {
            w.report(self.st.lat, self.st.lon, self.st.depth());
        }
        telemetry::send(self);
        self.tick += 1;
    }

    pub fn on_datagram(&mut self, data: &[u8]) {
        for (_, msg) in link::parse(data) {
            self.handle(&msg);
        }
    }

    pub fn handle(&mut self, msg: &MavMessage) {
        crate::commands::handle(self, msg);
    }
}

fn wall_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

pub const TICK: Duration = Duration::from_millis(1000 / PHYSICS_HZ as u64);

/// Run one vehicle forever: 10 Hz physics and telemetry, and answer whatever the GCS sends
/// as soon as it arrives. `phase` (within one tick) offsets this vehicle's ticks from the
/// others': real autopilots are not in lockstep, and thousands of links sending in the same
/// millisecond overflow the host's loopback queue (seen at 3,000 vehicles).
pub async fn run(mut v: Vehicle, phase: Duration) {
    let mut every = tokio::time::interval_at(tokio::time::Instant::now() + phase, TICK);
    every.set_missed_tick_behavior(MissedTickBehavior::Burst);
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            _ = every.tick() => v.step(),
            r = v.link.sock.recv(&mut buf) => {
                // errors (e.g. ICMP port unreachable while the bridge is down) are not fatal
                if let Ok(n) = r {
                    v.on_datagram(&buf[..n]);
                }
            }
        }
    }
}
