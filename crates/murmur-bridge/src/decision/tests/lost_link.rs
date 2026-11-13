//! Rules 30's vehicle fact `lost_link` (docs/NODE-LOSS.md section 7, the owner's decision D4)
//! against the stand-ins' own autopilots: each one launched as `sim/swarm.py` launches it (its
//! catalog entry, its `standin_params`, the lost-link timer of its link profile), on station in
//! its hold mode, wired to a rig's world in memory. The claim: the plan the bridge reads is the
//! vehicle's. The fact's timer is the stand-in's own parameter, and with its GCS heartbeat cut
//! the stand-in's failsafe does what the fact says, after that long and not before.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock, Weak};

use murmur_decide::{AutopilotKind, LostLinkAction, LostLinkPlan, LostLinkSource, NODE_LOSS_SINCE};
use murmur_standins::config::{Spec, VehicleConfig};
use murmur_standins::vehicle::Vehicle as Standin;

use super::*;
use crate::link::{LinkStats, Transmit};
use crate::mav::{self, Kind, MavMessage, apm};
use crate::telemetry::{self, PlanInput, Tier};
use crate::vehicle::VehicleCell;

/// The links file every fleet runs on, and Saturation's (its LOS radios' timer is a minute).
const LINKS: &str = "sim/links.toml";
const SATURATION_LINKS: &str = "sim/situations/links/swarm-saturation-bay-region.toml";
/// The stand-ins' system id on their links.
const SYSID: u8 = 7;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The bridge's side of an in-memory link to a stand-in: what the bridge sends goes in at once
/// (encoded as the GCS, 255/190), and the stand-in's answers reach the bridge's cell.
struct ToStandin {
    standin: Arc<Mutex<Standin>>,
    cell: OnceLock<Weak<VehicleCell>>,
    seq: AtomicU8,
    /// The parameters the bridge asked the stand-in for (PARAM_REQUEST_READ), in order.
    asked: Arc<Mutex<Vec<String>>>,
}

impl ToStandin {
    fn deliver(&self, datagram: &[u8]) {
        let frames = {
            let mut s = lock(&self.standin);
            s.on_datagram(datagram);
            s.link.take_captured()
        };
        if let Some(cell) = self.cell.get().and_then(Weak::upgrade) {
            to_bridge(&cell, &frames);
        }
    }
}

impl Transmit for ToStandin {
    fn send(&self, msg: &MavMessage) {
        if let MavMessage::PARAM_REQUEST_READ(r) = msg {
            lock(&self.asked).push(mav::chars_to_string(&r.param_id));
        }
        self.deliver(&mav::encode(msg, self.seq.fetch_add(1, Ordering::Relaxed)));
    }
    fn send_raw(&self, frame: &[u8]) {
        self.deliver(frame);
    }
    fn port(&self) -> u16 {
        0
    }
}

/// The stand-in's frames into the bridge's cell, as its receive loop takes them.
fn to_bridge(cell: &VehicleCell, frames: &[Vec<u8>]) {
    for datagram in frames {
        for f in mav::decode(datagram) {
            cell.link_on_message(&f.msg);
            cell.handle(&f.msg, &f.raw, clock::mono());
        }
    }
}

/// A stand-in wired to a rig's world.
struct Wired {
    standin: Arc<Mutex<Standin>>,
    cell: Arc<VehicleCell>,
    stats: Arc<Mutex<LinkStats>>,
    /// The GCS's heartbeat, once a second while set (cleared: the link is cut).
    gcs: Arc<AtomicBool>,
    asked: Arc<Mutex<Vec<String>>>,
    uid: Uid,
    /// Its steps and the GCS's heartbeat.
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Wired {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Wired {
    /// The fact in the rig's snapshot now.
    fn fact(&self, rig: &Rig) -> Option<LostLinkPlan> {
        rig.d
            .snapshot()
            .vehicles
            .get(&snapshot::duid(self.uid))
            .and_then(|f| f.lost_link.as_ref())
            .map(|f| f.value.clone())
    }

    /// One of the stand-in's own parameters.
    fn param(&self, name: &str) -> Option<f64> {
        lock(&self.standin).params.get(name).map(f64::from)
    }

    /// The stand-in's mode and armed state.
    fn state(&self) -> (&'static str, bool) {
        let s = lock(&self.standin);
        (s.st.mode, s.st.armed)
    }

    /// What carries its link now, for the telemetry plan (as the world's link loop chooses it).
    fn plan(&self, class: &str, submerged: bool) {
        lock(&self.stats).plan = Some(telemetry::choose(&PlanInput {
            link_class: Some(class),
            submerged,
            ..PlanInput::default()
        }));
    }

    /// Tick the rig once a second (tokio time) until the fact comes, at most `limit_s`.
    async fn fact_within(&self, rig: &Rig, limit_s: u64) -> Option<LostLinkPlan> {
        for _ in 0..limit_s {
            rig.d.tick();
            sleep(Duration::from_secs(1)).await;
            if let Some(f) = self.fact(rig) {
                return Some(f);
            }
        }
        None
    }

    /// Cut the GCS's heartbeat and watch the stand-in's failsafe: its mode and armed state half
    /// a second before `timer_s` has passed since the last heartbeat it heard, and a second
    /// after.
    async fn cut(&self, timer_s: f64) -> ((&'static str, bool), (&'static str, bool)) {
        self.gcs.store(false, Ordering::SeqCst);
        sleep(Duration::from_millis(50)).await;
        let last = lock(&self.standin).fs.gcs_last.expect("it heard its GCS");
        let until = |t: f64| {
            let s = self.standin.clone();
            async move {
                while lock(&s).now() < t {
                    sleep(Duration::from_millis(100)).await;
                }
            }
        };
        until(last + timer_s - 0.5).await;
        let before = self.state();
        until(last + timer_s + 1.0).await;
        (before, self.state())
    }
}

/// A rig deciding under rules 30 (the rules being built, docs/NODE-LOSS.md section 8), or under
/// the default rules.
fn rig_rules(rules: u32) -> Rig {
    let mut p = Policy::load(&root().join("sim/policy.toml")).expect("policy");
    p.rules.rules_version = rules;
    rig_with(p)
}

/// The firmware parameter `sim/swarm.py` writes a link profile's `lost_link_s` into
/// (`LOST_LINK_PARAM`, by the firmware the behavior imitates), none for a tracker.
fn timer_param(behavior: &str) -> Option<&'static str> {
    match behavior {
        "plane" | "vtol" => Some("FS_LONG_TIMEOUT"),
        "tracker" => None,
        _ => Some("FS_GCS_TIMEOUT"),
    }
}

/// The platform `platform` launched as `sim/swarm.py` launches a stand-in on `links`: its
/// catalog entry, its `standin_params`, then its link profile's lost-link timer
/// (`standin_params`); on station at `at` ([lat, lon, alt, heading]), armed in its hold mode,
/// wired to `rig`'s world as `uid`.
async fn wire(rig: &Rig, uid: Uid, platform: &str, links: &str, at: [f64; 4]) -> Wired {
    let catalog: toml::Table = std::fs::read_to_string(root().join("sim/platforms.toml"))
        .expect("catalog")
        .parse()
        .expect("catalog parses");
    let entry = catalog[platform].as_table().expect("a platform").clone();
    let spec: Spec = serde_json::from_value(serde_json::to_value(&entry).expect("json")).expect("spec");
    let mut params: std::collections::BTreeMap<String, f64> = entry
        .get("standin_params")
        .and_then(toml::Value::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_float().or_else(|| v.as_integer().map(|i| i as f64))?)))
                .collect()
        })
        .unwrap_or_default();
    let link = entry.get("link").and_then(toml::Value::as_str);
    let space_class = entry.get("space_class").and_then(toml::Value::as_str);
    let timers = crate::estimate::LostLink::from_toml(&std::fs::read_to_string(root().join(links)).expect("links"))
        .expect("links parse");
    if let (Some(name), Some((_, t))) = (timer_param(&spec.behavior), timers.for_vehicle(Some(platform), link, space_class)) {
        params.insert(name.to_string(), t);
    }
    let cfg = VehicleConfig {
        uid: Some(uid as u32),
        sysid: u32::from(SYSID),
        callsign: format!("{platform} {uid}"),
        port: 0,
        home: [at[0] - 0.002, at[1], 0.0, at[3]],
        spec,
        params,
        surface_m: None,
        moored: false,
        start_at: Some(at),
    };
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let mut s = Standin::seeded(cfg, sock, None, 1).expect("stand-in");
    s.link.capture();
    let standin = Arc::new(Mutex::new(s));
    let asked = Arc::new(Mutex::new(Vec::new()));
    let tx = Arc::new(ToStandin {
        standin: standin.clone(),
        cell: OnceLock::new(),
        seq: AtomicU8::new(0),
        asked: asked.clone(),
    });
    let stats = Arc::new(Mutex::new(LinkStats::default()));
    let mut m = serde_json::Map::new();
    m.insert("platform".into(), json!(platform));
    m.insert("callsign".into(), json!(format!("{platform} {uid}")));
    let cell = Arc::new(VehicleCell::new(
        uid,
        SYSID,
        tx.clone(),
        stats.clone(),
        Some(Arc::new(Identity::from_manifest(&m))),
    ));
    let _ = tx.cell.set(Arc::downgrade(&cell));
    rig.world.insert_vehicle(cell.clone());
    let gcs = Arc::new(AtomicBool::new(true));
    // its 10 Hz steps (heartbeat and events) to the bridge, and the GCS's heartbeat to it
    let task = {
        let (standin, cell, gcs, tx) = (standin.clone(), cell.clone(), gcs.clone(), tx.clone());
        tokio::spawn(async move {
            let heartbeat = MavMessage::HEARTBEAT(apm::HEARTBEAT_DATA {
                mavtype: apm::MavType::MAV_TYPE_GCS,
                autopilot: apm::MavAutopilot::MAV_AUTOPILOT_INVALID,
                ..Default::default()
            });
            let mut tick = tokio::time::interval(murmur_standins::vehicle::TICK);
            let mut n = 0u64;
            loop {
                tick.tick().await;
                if n.is_multiple_of(10) && gcs.load(Ordering::SeqCst) {
                    tx.send(&heartbeat);
                }
                n += 1;
                let frames = {
                    let mut s = lock(&standin);
                    s.step();
                    s.link.take_captured()
                };
                to_bridge(&cell, &frames);
            }
        })
    };
    let w = Wired {
        standin,
        cell,
        stats,
        gcs,
        asked,
        uid,
        task,
    };
    w.plan(link.unwrap_or("los"), false);
    w
}

/// Where the stand-ins are put, at `alt` m (aircraft at 150, subs 20 m down, the rest on the
/// surface).
fn station(alt: f64) -> [f64; 4] {
    [SEA.0, SEA.1, alt, 90.0]
}

const AIR: f64 = 150.0;
const SURFACE: f64 = 0.0;
const DEPTH: f64 = -20.0;

/// One platform per firmware (and both kinds of ArduSub and of ArduRover's hulls): the fact the
/// bridge reads is its own parameters', and its failsafe, its GCS cut, does what the fact says
/// after the fact's timer and not before. The Puma runs on Saturation's links file, whose LOS
/// timer is a minute.
#[tokio::test(start_paused = true)]
async fn the_plan_read_is_the_vehicles_own() {
    use LostLinkAction::*;
    struct Case {
        platform: &'static str,
        links: &'static str,
        alt: f64,
        /// a mode it is put in before the read (else its hold mode)
        mode: Option<&'static str>,
        action: LostLinkAction,
        /// what its failsafe makes of its mode and armed state, from its hold
        after: Option<(&'static str, bool)>,
        /// its own parameters that make the timer (summed)
        timer: &'static [&'static str],
        asks: &'static [&'static str],
    }
    let plane = &["FS_GCS_ENABL", "FS_LONG_ACTN", "FS_LONG_TIMEOUT", "Q_OPTIONS"][..];
    let rover = &["FS_GCS_ENABLE", "FS_ACTION", "FS_OPTIONS", "FS_GCS_TIMEOUT", "FS_TIMEOUT"][..];
    let cases = [
        // ArduPlane: LOITER, RTL after FS_LONG_TIMEOUT (Saturation's minute)
        Case { platform: "puma", links: SATURATION_LINKS, alt: AIR, mode: None, action: Rtl, after: Some(("RTL", true)), timer: &["FS_LONG_TIMEOUT"], asks: plane },
        // a QuadPlane is ArduPlane too
        Case { platform: "fvr90", links: LINKS, alt: AIR, mode: None, action: Rtl, after: Some(("RTL", true)), timer: &["FS_LONG_TIMEOUT"], asks: plane },
        // ArduCopter: BRAKE, RTL
        Case { platform: "x10", links: LINKS, alt: AIR, mode: None, action: Rtl, after: Some(("RTL", true)), timer: &["FS_GCS_TIMEOUT"], asks: &["FS_GCS_ENABLE", "FS_OPTIONS", "FS_GCS_TIMEOUT"] },
        // ArduRover, a boat in LOITER: FS_ACTION's RTL after FS_GCS_TIMEOUT and FS_TIMEOUT
        Case { platform: "magura", links: LINKS, alt: SURFACE, mode: Some("LOITER"), action: Rtl, after: Some(("RTL", true)), timer: &["FS_GCS_TIMEOUT", "FS_TIMEOUT"], asks: rover },
        // ArduRover, a UGV in HOLD: its failsafe leaves HOLD alone
        Case { platform: "ugv_recon", links: LINKS, alt: SURFACE, mode: None, action: Continue, after: None, timer: &["FS_GCS_TIMEOUT", "FS_TIMEOUT"], asks: rover },
        // ArduSub, an AUV under water: it surfaces after two minutes
        Case { platform: "remus", links: LINKS, alt: DEPTH, mode: None, action: Surface, after: Some(("SURFACE", true)), timer: &["FS_GCS_TIMEOUT"], asks: &["FS_GCS_ENABLE", "FS_GCS_TIMEOUT"] },
        // ArduSub, a tethered ROV: it disarms
        Case { platform: "bluerov", links: LINKS, alt: DEPTH, mode: None, action: Disarm, after: Some(("POSHOLD", false)), timer: &["FS_GCS_TIMEOUT"], asks: &["FS_GCS_ENABLE", "FS_GCS_TIMEOUT"] },
        // Blimp: its failsafe is off
        Case { platform: "lta", links: LINKS, alt: AIR, mode: None, action: Off, after: None, timer: &[], asks: &["FS_GCS_ENABLE", "FS_GCS_TIMEOUT"] },
        // AntennaTracker: it has none (it says so)
        Case { platform: "tracker", links: LINKS, alt: SURFACE, mode: None, action: Off, after: None, timer: &[], asks: &["FS_GCS_ENABLE"] },
        // a drifting buoy imitates ArduRover, but cannot drive
        Case { platform: "buoy", links: LINKS, alt: SURFACE, mode: None, action: Off, after: None, timer: &[], asks: rover },
    ];
    for (i, c) in cases.iter().enumerate() {
        let rig = rig_rules(NODE_LOSS_SINCE);
        let w = wire(&rig, 400 + i as Uid, c.platform, c.links, station(c.alt)).await;
        if let Some(m) = c.mode {
            assert!(lock(&w.standin).try_mode(m), "{}: {m}", c.platform);
        }
        let start = w.state();
        let fact = w.fact_within(&rig, 10).await.unwrap_or_else(|| panic!("{}: no fact", c.platform));
        let timer = (!c.timer.is_empty()).then(|| c.timer.iter().map(|n| w.param(n).expect("its parameter")).sum::<f64>());
        let mut plan = fact.clone();
        let circle = plan.circle_m.take();
        assert_eq!(
            plan,
            LostLinkPlan { action: c.action, timer_s: timer, source: LostLinkSource::Read, circle_m: None },
            "{}: the plan read",
            c.platform
        );
        // a fixed wing's own circle, once read (RTL_RADIUS, else WP_LOITER_RAD): where it holds
        // and arrives home, which a silent one's region circles at (murmur_decide::silent)
        if let Some(r) = circle {
            let rtl = w.param("RTL_RADIUS").unwrap_or(0.0).abs();
            let want = if rtl > 0.0 { rtl } else { w.param("WP_LOITER_RAD").unwrap_or(0.0).abs() };
            assert_eq!(r, want, "{}: its circle", c.platform);
        }
        assert_eq!(*lock(&w.asked), c.asks, "{}: what the bridge asked", c.platform);
        // the stand-in's own failsafe does what the fact says, after that long
        let (before, after) = w.cut(timer.unwrap_or(5.0)).await;
        assert_eq!(before, start, "{}: before its timer", c.platform);
        assert_eq!(after, c.after.unwrap_or(start), "{}: after its timer", c.platform);
    }
}

/// Saturation's Puma: its links file gives its LOS radio a one-minute lost-link timer, which
/// `sim/swarm.py` writes into the stand-in's FS_LONG_TIMEOUT, and the bridge reads 60 s.
#[tokio::test(start_paused = true)]
async fn saturations_puma_reads_its_minute() {
    let text = std::fs::read_to_string(root().join(SATURATION_LINKS)).expect("links");
    let timers = crate::estimate::LostLink::from_toml(&text).expect("parse");
    assert_eq!(timers.for_vehicle(Some("puma"), Some("los"), None), Some(("los".to_string(), 60.0)));
    let rig = rig_rules(NODE_LOSS_SINCE);
    let w = wire(&rig, 410, "puma", SATURATION_LINKS, station(AIR)).await;
    assert_eq!(w.param("FS_LONG_TIMEOUT"), Some(60.0));
    let fact = w.fact_within(&rig, 10).await.expect("the fact");
    assert_eq!(fact.timer_s, Some(60.0));
    assert_eq!(fact.source, LostLinkSource::Read);
}

/// A run under rules 29 asks its vehicles for nothing it did not ask before and has no such
/// fact.
#[tokio::test(start_paused = true)]
async fn rules_29_asks_for_nothing_new() {
    let rig = rig_rules(murmur_decide::HANDOVER_SINCE);
    assert!(!rig.d.rules.node_loss());
    let w = wire(&rig, 411, "puma", SATURATION_LINKS, station(AIR)).await;
    assert!(w.fact_within(&rig, 10).await.is_none());
    assert!(lock(&w.asked).is_empty(), "{:?}", lock(&w.asked));
    assert!(w.cell.read(|v| v.lost_link.reported.is_empty()));
}

/// An AUV under water (its telemetry plan `silent`: the acoustic modem carries no read) has its
/// plan configured, the parameters Murmur gives an AUV and its link profile's timer, and nothing
/// is asked of it; surfaced, it is read, and the read takes over.
#[tokio::test(start_paused = true)]
async fn configured_only_where_the_link_cannot_carry_a_read() {
    let rig = rig_rules(NODE_LOSS_SINCE);
    let w = wire(&rig, 412, "remus", LINKS, station(SURFACE)).await;
    w.plan("acoustic", true);
    assert_eq!(lock(&w.stats).plan.as_ref().map(|p| p.tier), Some(Tier::Silent));
    let fact = w.fact_within(&rig, 10).await.expect("configured");
    assert_eq!(
        fact,
        LostLinkPlan { action: LostLinkAction::Surface, timer_s: Some(120.0), source: LostLinkSource::Configured, circle_m: None }
    );
    for _ in 0..5 {
        rig.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert!(lock(&w.asked).is_empty(), "nothing asked under water: {:?}", lock(&w.asked));
    // at the surface its mast radio carries a read
    w.plan("acoustic", false);
    assert_eq!(lock(&w.stats).plan.as_ref().map(|p| p.tier), Some(Tier::Minimal));
    for _ in 0..5 {
        rig.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(
        w.fact(&rig),
        Some(LostLinkPlan { action: LostLinkAction::Surface, timer_s: Some(120.0), source: LostLinkSource::Read, circle_m: None })
    );
    assert_eq!(*lock(&w.asked), ["FS_GCS_ENABLE", "FS_GCS_TIMEOUT"]);
}

/// After a reboot the bridge sees (the time since boot went back) it reads the failsafe again:
/// a timer changed meanwhile is the one the fact gives.
#[tokio::test(start_paused = true)]
async fn a_reboot_reads_it_again() {
    let rig = rig_rules(NODE_LOSS_SINCE);
    let w = wire(&rig, 413, "puma", SATURATION_LINKS, station(AIR)).await;
    assert_eq!(w.fact_within(&rig, 10).await.and_then(|f| f.timer_s), Some(60.0));
    let asked = lock(&w.asked).len();
    // nothing more is asked while it stays up
    for _ in 0..5 {
        rig.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(lock(&w.asked).len(), asked);
    lock(&w.standin).params.preset("FS_LONG_TIMEOUT", 90.0);
    let time = |ms: u32| MavMessage::SYSTEM_TIME(apm::SYSTEM_TIME_DATA { time_boot_ms: ms, ..Default::default() });
    w.cell.handle(&time(600_000), &[], clock::mono());
    w.cell.handle(&time(2_000), &[], clock::mono());
    assert_eq!(w.cell.read(|v| v.boot_epoch), 1, "a reboot seen");
    for _ in 0..5 {
        rig.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert_eq!(w.fact(&rig).and_then(|f| f.timer_s), Some(90.0));
    assert_eq!(lock(&w.asked).len(), asked * 2, "{:?}", lock(&w.asked));
}

/// The fact is what the vehicle last reported, whoever asked: an engagement's write and its
/// read-back move it. And the read waits while an engagement's job runs on the link (rules 6:
/// one at a time).
#[tokio::test(start_paused = true)]
async fn the_fact_follows_the_vehicle_and_the_read_waits_for_an_engagement() {
    let rig = rig_rules(NODE_LOSS_SINCE);
    let w = wire(&rig, 414, "puma", SATURATION_LINKS, station(AIR)).await;
    // an engagement's job holds the link: nothing is asked
    let job = w.cell.param_job();
    for _ in 0..5 {
        rig.d.tick();
        sleep(Duration::from_secs(1)).await;
    }
    assert!(lock(&w.asked).is_empty(), "{:?}", lock(&w.asked));
    assert!(w.fact(&rig).is_none());
    drop(job);
    assert_eq!(w.fact_within(&rig, 10).await.and_then(|f| f.timer_s), Some(60.0));
    // a write (a restore's) and its read-back
    let restore = crate::decision::preload::start_restore(w.cell.clone(), [("FS_LONG_TIMEOUT".to_string(), 5.0)].into());
    for _ in 0..10 {
        sleep(Duration::from_secs(1)).await;
        if restore.take().is_some() {
            break;
        }
    }
    assert_eq!(w.param("FS_LONG_TIMEOUT"), Some(5.0));
    assert_eq!(w.fact(&rig).and_then(|f| f.timer_s), Some(5.0));
}

/// The parameters configured where the link cannot carry a read are those the stand-ins are
/// seeded with for their firmware (`crates/murmur-standins/src/params.rs`, as SITL's
/// `sim/params/*.parm`), and the plan they give is each firmware's RTL, an AUV's surfacing, a
/// tethered ROV's disarm, the airship's and the tracker's none.
#[test]
fn configured_parameters_are_the_stand_ins_own() {
    use murmur_standins::behavior::Firmware as Fw;
    use murmur_standins::params::Params;
    use LostLinkAction::*;
    let spec = |behavior: &str, link: &str| Spec {
        name: "t".into(),
        behavior: behavior.into(),
        cruise_mps: 10.0,
        max_mps: 12.0,
        climb_mps: None,
        turn_dps: None,
        loiter_radius_m: None,
        endurance_h: None,
        link: Some(link.into()),
    };
    let cases = [
        (Kind::Plane, AutopilotKind::Plane, Fw::Plane, "plane", "los", "LOITER", Rtl),
        (Kind::Copter, AutopilotKind::Copter, Fw::Copter, "copter", "mesh", "BRAKE", Rtl),
        (Kind::Boat, AutopilotKind::Boat, Fw::Rover, "boat", "los", "LOITER", Rtl),
        (Kind::Ugv, AutopilotKind::Ugv, Fw::Rover, "ugv", "mesh", "GUIDED", Rtl),
        (Kind::Sub, AutopilotKind::Sub, Fw::Sub, "sub", "acoustic", "POSHOLD", Surface),
        (Kind::Sub, AutopilotKind::Sub, Fw::Sub, "sub", "tether", "POSHOLD", Disarm),
        (Kind::Blimp, AutopilotKind::Blimp, Fw::Blimp, "blimp", "los", "LOITER", Off),
        (Kind::Tracker, AutopilotKind::Tracker, Fw::Tracker, "tracker", "tether", "STOP", Off),
    ];
    for (kind, ap, fw, behavior, link, mode, action) in cases {
        let acoustic = link == "acoustic";
        let configured = crate::decision::preload::configured_params(kind, acoustic, Some(42.0)).expect("configured");
        let mut seeded = Params::seed(fw, &spec(behavior, link), SYSID);
        if let Some(name) = timer_param(behavior) {
            assert!(seeded.preset(name, 42.0));
        }
        for (name, value) in &configured {
            assert_eq!(seeded.get(name).map(f64::from), *value, "{behavior} {link}: {name}");
        }
        let plan = crate::decision::preload::lost_link_plan(ap, Some(mode), true, |n| configured.get(n).copied());
        let timer = match fw {
            _ if action == Off => None,
            Fw::Rover => Some(43.5),
            _ => Some(42.0),
        };
        assert_eq!(plan, Some((action, timer)), "{behavior} {link}");
    }
}
