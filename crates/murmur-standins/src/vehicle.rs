//! One stand-in: autopilot state, its behavior, its link, and the task that runs it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use mavlink::MessageData;
use mavlink::dialects::ardupilotmega::{HOME_POSITION_DATA, MavMessage, MavSeverity};
use tokio::net::UdpSocket;
use tokio::time::MissedTickBehavior;

use crate::battery::{self, Battery};
use crate::behavior::{self, Behavior, Firmware, Tick};
use crate::conditions::{Sample, Slot};
use crate::config::VehicleConfig;
use crate::failsafe::Failsafes;
use crate::geo::{Gauss, M_PER_DEG, wrap180};
use crate::kinematics::{Out, State};
use crate::link::{self, Inbound, Link};
use crate::mission::{Mission, NavCfg};
use crate::nav::Nav;
use crate::params::Params;
use crate::telemetry::Streams;
use crate::{messages, telemetry, truth};

pub const PHYSICS_HZ: u32 = 10;

pub struct Vehicle {
    pub cfg: VehicleConfig,
    pub behavior: Box<dyn Behavior>,
    pub firmware: Firmware,
    /// the autopilot's own estimate (see nav.rs for the truth)
    pub st: State,
    pub mission: Mission,
    pub params: Params,
    pub streams: Streams,
    pub nav: Nav,
    pub battery: Battery,
    pub fs: Failsafes,
    pub link: Link,
    pub started: Instant,
    pub tick: u64,
    pub water: Option<Arc<Slot>>,
    /// rad/s over the last tick (ATTITUDE)
    pub yaw_rate: f64,
    /// came down without power
    pub crashed: bool,
    /// when the next acoustic status is due (sim seconds)
    pub acoustic_next: f64,
    /// MISSION_CURRENT last sent, so events don't repeat it
    announced_seq: Option<u16>,
    /// warned once that the GCS used MISSION_REQUEST (ArduPilot does the same)
    pub(crate) mission_request_warned: bool,
    pub(crate) mission_item_warned: bool,
    /// next PARAM_VALUE of a PARAM_REQUEST_LIST stream
    pub(crate) param_stream: Option<usize>,
    truth_next: f64,
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
            demand: 0.0,
        };
        behavior.spawn(&cfg.spec, &mut st);
        let seed = u64::from(cfg.uid()).wrapping_mul(0x1000_0000_01B3) ^ wall_nanos();
        let mut rng = Gauss::new(seed);
        let firmware = behavior.firmware();
        let mut params = Params::seed(firmware, &cfg.spec, cfg.sysid());
        params.extend(behavior.extra_params(&cfg.spec));
        let nav = Nav::new(&mut rng, (lat, lon, alt));
        let link = Link::new(sock, cfg.sysid());
        let mut mission = Mission::default();
        mission.cfg.firmware = firmware;
        Ok(Vehicle {
            cfg,
            behavior,
            firmware,
            st,
            mission,
            params,
            streams: Streams::default(),
            nav,
            battery: Battery::default(),
            fs: Failsafes::default(),
            link,
            started: Instant::now(),
            tick: 0,
            water,
            yaw_rate: 0.0,
            crashed: false,
            acoustic_next: 0.0,
            announced_seq: None,
            mission_request_warned: false,
            mission_item_warned: false,
            param_stream: None,
            truth_next: 0.0,
            rng,
            out: Vec::new(),
        })
    }

    pub fn sysid(&self) -> u8 {
        self.cfg.sysid()
    }

    pub fn airborne(&self) -> bool {
        self.behavior.airborne(&self.st)
    }

    /// Seconds since boot on the physics clock.
    pub fn now(&self) -> f64 {
        self.tick as f64 / f64::from(PHYSICS_HZ)
    }

    pub fn time_boot_ms(&self) -> u64 {
        self.tick * 1000 / u64::from(PHYSICS_HZ)
    }

    pub fn capacity_mah(&self) -> f64 {
        self.params.f("BATT_CAPACITY", 0.0)
    }

    pub fn flat(&self) -> bool {
        self.battery.flat(self.capacity_mah())
    }

    /// Where the vehicle really is: its estimate minus the navigation error.
    pub fn truth(&self) -> (f64, f64, f64) {
        let lat = self.st.lat - self.nav.err_n / M_PER_DEG;
        let lon = self.st.lon - self.nav.err_e / (M_PER_DEG * self.st.lat.to_radians().cos());
        (lat, lon, self.st.alt)
    }

    pub fn set_mode(&mut self, name: &'static str) {
        if name != self.st.mode {
            self.st.mode = name;
            self.st.hold_point = (self.st.lat, self.st.lon);
            if let Some(armed) = self.behavior.mode_arms(name) {
                self.st.armed = armed;
            }
            if name == "AUTO" {
                let restart = self.params.get("MIS_RESTART") == Some(1.0);
                self.mission.start_or_resume(restart);
                if self.firmware != Firmware::Plane {
                    // Rover / Sub reset to the default speed on entering AUTO
                    self.mission.speed = None;
                }
            }
        }
    }

    /// A mode change the autopilot makes itself (failsafe, mission end): only to a mode this
    /// firmware has and the behavior accepts.
    pub fn try_mode(&mut self, name: &'static str) -> bool {
        let Some(name) = self
            .behavior
            .mode_number(name)
            .and_then(|n| self.behavior.mode_name(n))
        else {
            return false;
        };
        if !self.behavior.accepts_mode(name) {
            return false;
        }
        self.set_mode(name);
        true
    }

    pub fn text(&mut self, severity: MavSeverity, s: &str) {
        self.link.send(&messages::statustext(severity, s));
    }

    /// The text ArduPilot sends on arming / disarming (AP_Arming_*::arm / disarm).
    pub fn arm_text(&self, armed: bool) -> &'static str {
        match (self.firmware, armed) {
            (Firmware::Plane | Firmware::Rover, true) => "Throttle armed",
            (Firmware::Plane | Firmware::Rover, false) => "Throttle disarmed",
            (_, true) => "Arming motors",
            (_, false) => "Disarming motors",
        }
    }

    /// Acceptance radius for waypoints, from this firmware's parameter.
    pub fn wp_radius(&self) -> f64 {
        match self.firmware {
            Firmware::Sub | Firmware::Copter => self.params.f("WP_RADIUS_M", 2.0),
            _ => self.params.f("WP_RADIUS", 10.0),
        }
    }

    /// One physics step and the telemetry due on this tick.
    pub fn step(&mut self) {
        let dt = 1.0 / f64::from(PHYSICS_HZ);
        let now = self.now();
        let spec = &self.cfg.spec;
        self.mission.cfg = NavCfg {
            firmware: self.firmware,
            now,
            wp_radius: self.wp_radius(),
            min_radius: if self.firmware == Firmware::Plane {
                spec.min_turn_radius_m()
            } else {
                0.0
            },
            max_speed: spec.max_mps.max(spec.cruise_mps),
        };
        let sample = if self.behavior.wants_conditions() {
            self.water.as_ref().and_then(|w| w.latest())
        } else {
            None
        };
        self.check_failsafes(now);

        // a flat battery drives nothing: the behavior sees an unpowered vehicle
        let flat = self.flat();
        let armed = self.st.armed;
        let was_airborne = self.airborne();
        if flat {
            self.st.armed = false;
        }
        let heading = self.st.heading;
        let mut out = std::mem::take(&mut self.out);
        self.behavior.step(&mut Tick {
            spec: &self.cfg.spec,
            params: &self.params,
            st: &mut self.st,
            mission: &mut self.mission,
            water: sample.as_ref(),
            rng: &mut self.rng,
            out: &mut out,
            dt,
        });
        if flat {
            self.st.armed = armed;
            if was_airborne && !self.airborne() && !self.crashed {
                self.crashed = true;
                self.st.speed = 0.0;
                self.st.armed = false;
            }
        }
        self.yaw_rate = wrap180(self.st.heading - heading).to_radians() / dt;
        out.append(&mut self.mission.out);
        self.emit(&mut out);
        if let Some(home) = self.mission.new_home.take() {
            self.st.home = home;
            self.streams.once.push(HOME_POSITION_DATA::ID);
        }

        self.navigate(dt, now, sample.as_ref(), &mut out);
        self.emit(&mut out);
        self.out = out;
        if let Some(w) = &self.water {
            let (lat, lon, _) = self.truth();
            w.report(lat, lon, self.st.depth());
        }
        self.drain_battery(dt);
        crate::commands::upload_tick(self, now);
        crate::commands::param_stream_tick(self);
        telemetry::send(self);
        if self.nav.tracked && now >= self.truth_next {
            self.truth_next = now + truth::PERIOD_S;
            truth::record(self);
        }
        self.tick += 1;
    }

    pub(crate) fn emit(&mut self, out: &mut Vec<Out>) {
        for o in out.drain(..) {
            match o {
                Out::Text(sev, s) => self.text(sev, &s),
                Out::MissionCurrent(seq) => {
                    if self.announced_seq != Some(seq) {
                        self.announced_seq = Some(seq);
                        self.link.send(&messages::mission_current(seq));
                    }
                }
                Out::ItemReached(seq) => self.link.send(&messages::mission_item_reached(seq)),
                Out::SetMode(name) => {
                    self.try_mode(name);
                }
            }
        }
    }

    /// Navigation error while there's no GPS; the estimate (self.st) carries it.
    fn navigate(&mut self, dt: f64, now: f64, sample: Option<&Sample>, out: &mut Vec<Out>) {
        let sky = self.behavior.gps(&self.st).0 as u8
            >= mavlink::dialects::ardupilotmega::GpsFixType::GPS_FIX_TYPE_3D_FIX as u8;
        let truth = self.truth();
        let (dn, de) = self.nav.step(
            dt,
            now,
            sky,
            (self.st.ve, self.st.vn),
            self.st.depth(),
            sample,
            truth,
            &mut self.rng,
            out,
        );
        if dn != 0.0 || de != 0.0 {
            self.st.lat += dn / M_PER_DEG;
            self.st.lon += de / (M_PER_DEG * self.st.lat.to_radians().cos());
        }
    }

    fn drain_battery(&mut self, dt: f64) {
        let load = battery::load(self.firmware, &self.cfg.spec, &self.st, self.airborne());
        let load = if self.flat() { 0.0 } else { load };
        let cap = self.capacity_mah();
        self.battery
            .step(dt, load, cap, self.cfg.spec.endurance_h());
    }

    pub fn on_datagram(&mut self, data: &[u8]) {
        for m in link::parse_all(data) {
            match m {
                Inbound::Msg(h, msg) => self.handle_from(&h, &msg),
                other => crate::commands::handle_unknown(self, &other),
            }
        }
    }

    /// A message from the usual GCS (255/190).
    pub fn handle(&mut self, msg: &MavMessage) {
        let h = mavlink::MavHeader {
            system_id: messages::GCS_SYSID,
            component_id: messages::GCS_COMPID,
            sequence: 0,
        };
        self.handle_from(&h, msg);
    }

    pub fn handle_from(&mut self, header: &mavlink::MavHeader, msg: &MavMessage) {
        crate::commands::handle(self, header, msg);
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
