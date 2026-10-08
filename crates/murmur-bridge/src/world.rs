//! The world model: vehicles by uid, their links, and the fleet manifest that names them.
//! Readable by the API, the push loop and the task layer (and later the rules layer) without
//! touching the packet path: each link task writes only its own vehicle.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, SystemTime};

use mavlink::MessageData;
use serde_json::Value;

use crate::link::Link;
use crate::mav::{self, Kind, MavMessage, apm};
use crate::tasks::TaskManager;
use crate::vehicle::{Identity, Uid, Vehicle, VehicleCell};

/// A vehicle the manifest doesn't list (a stray MAVLink source on one of our ports) still
/// shows up, under a uid that can't collide with the launcher's 1..N.
pub const SYNTHETIC_UID_BASE: Uid = 1_000_000;
/// The telemetry plan is re-sent this often (cheap) so a rebooted vehicle picks it up again.
const PLAN_RESEND_TICKS: u64 = 30;
const MANIFEST_CHECK_TICKS: u64 = 5;
/// While a vehicle's home is unknown, HOME_POSITION is asked for this often. Streaming
/// vehicles get a home from their first GLOBAL_POSITION_INT long before; on HIGH_LATENCY2
/// (the minimal plan) no position message carries it.
const HOME_REQUEST_TICKS: u64 = 10;

pub struct Config {
    pub base_port: u16,
    pub http_port: u16,
    /// ports opened up front, from base_port; the manifest adds the rest
    pub default_ports: u16,
    pub manifest: PathBuf,
    pub platforms: PathBuf,
    /// the repo (MURMUR_ROOT): sim/ and logs/ are under it
    pub root: PathBuf,
}

impl Config {
    pub fn from_env() -> Config {
        fn num<T: std::str::FromStr>(key: &str, default: T) -> T {
            std::env::var(key)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(default)
        }
        // the repo root: MURMUR_ROOT, else where this crate was built from
        let root = std::env::var_os("MURMUR_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let root = root.canonicalize().unwrap_or(root);
        Config {
            base_port: num("MURMUR_BASE_PORT", 14560),
            http_port: num("MURMUR_HTTP_PORT", 8765),
            default_ports: num("MURMUR_MAX_VEHICLES", 32),
            manifest: std::env::var_os("MURMUR_FLEET_MANIFEST")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("sim/run/fleet.json")),
            platforms: root.join("sim/platforms.toml"),
            root,
        }
    }
}

/// sim/run/fleet.json as last read.
#[derive(Default)]
pub struct Manifest {
    mtime: Option<SystemTime>,
    pub identities: BTreeMap<Uid, Arc<Identity>>,
    /// udp port -> (uid, sysid)
    pub ports: HashMap<u16, (Uid, u8)>,
}

impl Manifest {
    pub fn parse(data: &Value) -> (Manifest, Vec<u16>) {
        let mut m = Manifest::default();
        let mut ports = Vec::new();
        for v in data
            .get("vehicles")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(entry) = v.as_object() else { continue };
            let Some(sysid) = entry.get("sysid").and_then(Value::as_u64) else {
                continue;
            };
            // manifests from before uid existed: sysid was unique then
            let uid = entry.get("uid").and_then(Value::as_u64).unwrap_or(sysid);
            m.identities
                .insert(uid, Arc::new(Identity::from_manifest(entry)));
            if let Some(port) = entry
                .get("port")
                .and_then(Value::as_u64)
                .and_then(|p| u16::try_from(p).ok())
            {
                m.ports.insert(port, (uid, sysid as u8));
                ports.push(port);
            }
        }
        (m, ports)
    }

    /// The manifest entry for the port a vehicle talks on, if its sysid matches; else a
    /// synthetic uid, stable per port.
    pub fn resolve(&self, port: u16, sysid: u8) -> Uid {
        match self.ports.get(&port) {
            Some(&(uid, s)) if s == sysid => uid,
            _ => SYNTHETIC_UID_BASE + Uid::from(port),
        }
    }

    pub fn sysid_of(&self, uid: Uid) -> Option<u8> {
        self.ports
            .values()
            .find(|(u, _)| *u == uid)
            .map(|(_, s)| *s)
    }
}

pub struct World {
    pub cfg: Config,
    vehicles: RwLock<BTreeMap<Uid, Arc<VehicleCell>>>,
    links: Mutex<BTreeMap<u16, Arc<Link>>>,
    manifest: Mutex<Manifest>,
    wanted_ports: Mutex<BTreeSet<u16>>,
    pub tasks: TaskManager,
    /// The water sampler's drift tracks (`crate::estimate`).
    pub water: crate::estimate::WaterTracks,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl World {
    pub fn new(cfg: Config) -> Arc<World> {
        let wanted = (0..cfg.default_ports)
            .filter_map(|i| cfg.base_port.checked_add(i))
            .collect();
        Arc::new(World {
            cfg,
            vehicles: RwLock::default(),
            links: Mutex::default(),
            manifest: Mutex::default(),
            wanted_ports: Mutex::new(wanted),
            tasks: TaskManager::default(),
            water: crate::estimate::WaterTracks::default(),
        })
    }

    /// All vehicles, in uid order.
    pub fn vehicles(&self) -> Vec<Arc<VehicleCell>> {
        self.vehicles
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    /// Tests: a vehicle that didn't come in over a UDP link.
    #[cfg(test)]
    pub fn insert_vehicle(&self, cell: Arc<VehicleCell>) {
        self.vehicles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cell.uid, cell);
    }

    pub fn manifest(&self) -> MutexGuard<'_, Manifest> {
        lock(&self.manifest)
    }

    /// Re-read the manifest when it changes (a new fleet was launched).
    pub fn load_manifest(&self) {
        let Ok(mtime) = std::fs::metadata(&self.cfg.manifest).and_then(|m| m.modified()) else {
            return;
        };
        let mut manifest = lock(&self.manifest);
        if manifest.mtime == Some(mtime) {
            return;
        }
        manifest.mtime = Some(mtime);
        let Some(data) = std::fs::read(&self.cfg.manifest)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        else {
            return;
        };
        let (mut fresh, ports) = Manifest::parse(&data);
        fresh.mtime = Some(mtime);
        lock(&self.wanted_ports).extend(ports);
        for cell in self.vehicles() {
            let identity = fresh.identities.get(&cell.uid).cloned();
            cell.write(|v| v.identity = identity);
        }
        *manifest = fresh;
    }

    /// First autopilot heartbeat on a link: the vehicle gets its uid from the manifest entry
    /// for that port (checked against the sysid it sends).
    pub fn on_new_vehicle(&self, link: &Arc<Link>, sysid: u8) -> Arc<VehicleCell> {
        self.load_manifest();
        let (uid, identity) = {
            let m = lock(&self.manifest);
            let uid = m.resolve(link.port, sysid);
            (uid, m.identities.get(&uid).cloned())
        };
        let who = match identity.as_ref().and_then(|i| i.str("callsign")) {
            Some(cs) => cs.to_string(),
            None => format!("sysid {sysid} (uid {uid})"),
        };
        let class = identity.as_ref().and_then(|i| i.str("link"));
        link.stats.lock().unwrap_or_else(|e| e.into_inner()).timesync =
            crate::link::TimesyncPolicy::for_class(class);
        let cell = Arc::new(VehicleCell::new(
            uid,
            sysid,
            link.clone(),
            link.stats.clone(),
            identity,
        ));
        self.vehicles
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(uid, cell.clone());
        println!("{who} connected on udp {}", link.port);
        cell
    }

    /// Open a UDP link for every wanted port not yet open (one per vehicle, so per-vehicle
    /// link stats and link emulation stay honest).
    pub async fn ensure_ports(self: &Arc<Self>) {
        let missing: Vec<u16> = {
            let links = lock(&self.links);
            lock(&self.wanted_ports)
                .iter()
                .copied()
                .filter(|p| !links.contains_key(p))
                .collect()
        };
        let mut opened = Vec::new();
        for port in missing {
            match Link::bind(port).await {
                Ok(link) => {
                    lock(&self.links).insert(port, link.clone());
                    tokio::spawn(link.run(self.clone()));
                    opened.push(port);
                }
                Err(e) => {
                    // dropped from the wanted set so it isn't retried (and reported) every second
                    eprintln!("cannot listen on udp {port}: {e}");
                    lock(&self.wanted_ports).remove(&port);
                }
            }
        }
        if let (Some(first), Some(last)) = (opened.first(), opened.last()) {
            let total = lock(&self.links).len();
            println!(
                "listening on {} more udp ports ({first}-{last}); {total} total",
                opened.len()
            );
        }
    }

    /// Every UDP link, in port order.
    pub fn links(&self) -> Vec<Arc<Link>> {
        lock(&self.links).values().cloned().collect()
    }

    /// TIMESYNC pings, parameter reads (`params_to_read`), the telemetry plan and with it
    /// each link's GCS heartbeat period, once a second. The heartbeat itself goes out from
    /// its own thread (`crate::heartbeat`), so nothing here can hold it up.
    pub async fn housekeeping(self: Arc<Self>) {
        let mut tick: u64 = 0;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            if tick.is_multiple_of(MANIFEST_CHECK_TICKS) {
                self.load_manifest();
            }
            self.ensure_ports().await;
            for link in self.links() {
                let Some(v) = link.vehicle.get() else {
                    continue;
                };
                let (kind, rates_sent, air, unread, identity, submerged, may_dive, no_home) = v
                    .read(|s| {
                        let air = s.identity.as_ref().and_then(|i| i.str("domain")) == Some("air")
                            || s.kind.is_some_and(Kind::flying);
                        let submerged = s
                            .alt_msl
                            .is_some_and(|a| a < -crate::telemetry::SUBMERGED_M);
                        let unread = params_to_read(s);
                        // an AUV on its acoustic modem goes quiet when it dives: expected while
                        // it runs a mission (or is already under), not a lost link
                        let acoustic =
                            s.identity.as_ref().and_then(|i| i.str("link")) == Some("acoustic");
                        let below = s
                            .alt_msl
                            .is_some_and(|a| a < -0.5 * crate::telemetry::SUBMERGED_M);
                        let mission =
                            s.armed && matches!(s.mode.as_deref(), Some("AUTO" | "GUIDED"));
                        let may_dive = acoustic && (below || mission);
                        (
                            s.kind,
                            s.rates_sent,
                            air,
                            unread,
                            s.identity.clone(),
                            submerged,
                            may_dive,
                            s.home.is_none(),
                        )
                    });
                let now = crate::clock::mono();
                let (choice, sent, every) = {
                    let mut st = link.stats.lock().unwrap_or_else(|e| e.into_inner());
                    st.dive_expected = may_dive;
                    if st
                        .emu_link
                        .as_ref()
                        .is_some_and(crate::telemetry::queue_pressure)
                    {
                        st.queue_hold_until = Some(now + crate::telemetry::QUEUE_HOLD_S);
                    }
                    let choice = crate::telemetry::choose(&crate::telemetry::PlanInput {
                        kind,
                        air,
                        link_class: identity.as_ref().and_then(|i| i.str("link")),
                        space_class: identity.as_ref().and_then(|i| i.str("space_class")),
                        submerged: submerged || st.high_latency,
                        emu: st.emu_link.as_ref(),
                        queue_hold: st.queue_hold_until.is_some_and(|t| now < t),
                        uniform: crate::telemetry::uniform_from_env(),
                    });
                    st.plan = Some(choice.clone());
                    (choice, st.plan_sent.clone(), st.timesync.every_s.max(1))
                };
                link.set_gcs_heartbeat_s(choice.gcs_heartbeat_s);
                if tick.is_multiple_of(every) {
                    link.send_timesync();
                }
                if no_home && kind.is_some() && tick.is_multiple_of(HOME_REQUEST_TICKS) {
                    let id = apm::HOME_POSITION_DATA::ID as f32;
                    v.send(&mav::command_long(
                        v.sysid,
                        apm::MavCmd::MAV_CMD_REQUEST_MESSAGE,
                        0,
                        [id, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                    ));
                }
                for id in unread {
                    v.send(&MavMessage::PARAM_REQUEST_READ(
                        apm::PARAM_REQUEST_READ_DATA {
                            param_index: -1,
                            target_system: v.sysid,
                            target_component: mav::AUTOPILOT_COMPID,
                            param_id: mav::char_array(id),
                        },
                    ));
                }
                // the plan: on a change at once (stopping what is no longer wanted), else
                // re-sent now and then so a rebooted vehicle picks it up again. Nothing for
                // a silent link (an AUV under water can't stream and ignores the requests).
                let want = choice.messages(kind, air);
                let changed = sent.as_deref() != Some(want.as_slice());
                if kind.is_some()
                    && choice.tier != crate::telemetry::Tier::Silent
                    && (!rates_sent || changed || tick.is_multiple_of(PLAN_RESEND_TICKS))
                {
                    let prior = if rates_sent { sent.as_deref() } else { None };
                    for (msg_id, us) in crate::telemetry::interval_requests(&want, prior) {
                        let p = [msg_id as f32, us, 0.0, 0.0, 0.0, 0.0, 0.0];
                        v.send(&mav::command_long(
                            v.sysid,
                            apm::MavCmd::MAV_CMD_SET_MESSAGE_INTERVAL,
                            0,
                            p,
                        ));
                    }
                    if changed && rates_sent {
                        println!(
                            "uid {}: telemetry {} for {} ({})",
                            v.uid,
                            choice.tier.name(),
                            choice
                                .label
                                .or(choice.profile.as_deref())
                                .unwrap_or("unknown link"),
                            choice.reason
                        );
                    }
                    link.stats
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .plan_sent = Some(want);
                    v.write(|s| s.rates_sent = true);
                }
            }
            tick += 1;
        }
    }
}

/// Parameters the bridge reads at link-up (and asks for again each second until answered):
/// Q_ENABLE tells a QuadPlane from a plane; WP_LOITER_RAD and RTL_RADIUS give the circle a
/// fixed wing flies around home in RTL (when the RTL task counts as done).
fn params_to_read(v: &Vehicle) -> Vec<&'static str> {
    let mut ids = Vec::new();
    if !matches!(v.kind, Some(Kind::Plane | Kind::Vtol)) {
        return ids;
    }
    if v.kind == Some(Kind::Plane) && v.is_vtol.is_none() {
        ids.push("Q_ENABLE");
    }
    if v.loiter_radius_m.is_none() {
        ids.push("WP_LOITER_RAD");
    }
    if v.rtl_radius_m.is_none() {
        ids.push("RTL_RADIUS");
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_wing_params_read_until_answered() {
        let mut v = Vehicle::default();
        v.kind = Some(Kind::Plane);
        assert_eq!(
            params_to_read(&v),
            ["Q_ENABLE", "WP_LOITER_RAD", "RTL_RADIUS"]
        );
        v.is_vtol = Some(false);
        v.loiter_radius_m = Some(150.0);
        assert_eq!(params_to_read(&v), ["RTL_RADIUS"]);
        v.kind = Some(Kind::Vtol);
        v.rtl_radius_m = Some(0.0);
        assert!(params_to_read(&v).is_empty());
        v.kind = Some(Kind::Sub);
        v.loiter_radius_m = None;
        assert!(params_to_read(&v).is_empty());
    }

    #[test]
    fn manifest_resolves_uid_by_port_and_sysid() {
        let data = serde_json::json!({"vehicles": [
            {"uid": 1, "sysid": 1, "port": 14560, "callsign": "HAWK 1"},
            {"uid": 251, "sysid": 1, "port": 14810, "callsign": "SEA 9"},
        ]});
        let (m, ports) = Manifest::parse(&data);
        assert_eq!(ports, vec![14560, 14810]);
        assert_eq!(m.resolve(14560, 1), 1);
        assert_eq!(m.resolve(14810, 1), 251); // same sysid, other link: other vehicle
        assert_eq!(m.resolve(14810, 7), SYNTHETIC_UID_BASE + 14810); // a stray source
        assert_eq!(m.resolve(15000, 1), SYNTHETIC_UID_BASE + 15000);
        assert_eq!(m.identities[&251].str("callsign"), Some("SEA 9"));
        assert_eq!(m.identities[&251].0.get("domain"), Some(&Value::Null));
        assert_eq!(m.sysid_of(251), Some(1));
    }
}
