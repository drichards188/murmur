//! The world model as the decision layer sees it: a [`WorldSnapshot`] keyed by uid, each
//! fact stamped with when the message carrying it arrived, unknown left `None`.

use murmur_decide::{
    AreaSet, AutopilotKind, Catalog, Fact, LonLat, Time, Uid as DUid, VehicleFacts, WorldSnapshot,
};

use crate::clock;
use crate::mav::Kind;
use crate::vehicle::{Uid, Vehicle};
use crate::world::World;

/// The decision layer's name for a vehicle: its fleet uid in decimal.
pub fn duid(uid: Uid) -> DUid {
    DUid::new(uid.to_string())
}

pub fn bridge_uid(uid: &DUid) -> Option<Uid> {
    uid.as_str().parse().ok()
}

/// Wall-clock seconds to the engine's integer milliseconds.
pub fn ms(wall_s: f64) -> Time {
    Time((wall_s * 1000.0).round() as i64)
}

fn autopilot(kind: Kind) -> Option<AutopilotKind> {
    Some(match kind {
        Kind::Plane => AutopilotKind::Plane,
        Kind::Vtol => AutopilotKind::Vtol,
        Kind::Copter => AutopilotKind::Copter,
        Kind::Blimp => AutopilotKind::Blimp,
        Kind::Boat => AutopilotKind::Boat,
        Kind::Ugv => AutopilotKind::Ugv,
        Kind::Sub => AutopilotKind::Sub,
        Kind::Tracker => AutopilotKind::Tracker,
        Kind::Unknown => return None,
    })
}

/// One vehicle's facts. The autopilot kind comes from its heartbeat; a drifting buoy
/// reports itself as a boat (its stand-in imitates ArduRover), so when the catalog says the
/// platform is a drifter and the heartbeat says boat, the fact is "drifter" (as Q_ENABLE
/// turns a plane heartbeat into a VTOL).
pub fn vehicle_facts(
    v: &Vehicle,
    catalog: &Catalog,
    now_mono: f64,
    report_interval_ms: u32,
) -> VehicleFacts {
    let platform = v
        .identity
        .as_ref()
        .and_then(|i| i.str("platform"))
        .and_then(|p| catalog.get(p))
        .cloned();
    let refine = |k: AutopilotKind| match (k, platform.as_ref().and_then(|p| p.autopilot)) {
        (AutopilotKind::Boat, Some(AutopilotKind::Drifter)) => AutopilotKind::Drifter,
        _ => k,
    };
    fn fact<T>(value: T, t: Option<f64>) -> Option<Fact<T>> {
        t.map(|t| Fact::new(value, ms(t)))
    }
    VehicleFacts {
        autopilot: v
            .kind
            .and_then(autopilot)
            .and_then(|k| fact(refine(k), v.seen.heartbeat)),
        nav_ready: v
            .nav_ready_fact(now_mono)
            .map(|(ready, t)| Fact::new(ready, ms(t))),
        position: v
            .pos
            .and_then(|(lat, lon)| fact(LonLat::new(lon, lat), v.seen.position)),
        home: v
            .home
            .and_then(|(lat, lon)| fact(LonLat::new(lon, lat), v.seen.home)),
        battery_pct: v
            .battery_pct
            .and_then(|b| fact(f64::from(b), v.seen.battery)),
        nav_source: v
            .nav_source_fact(now_mono)
            .map(|(source, t)| Fact::new(source, ms(t))),
        report_interval_ms: Some(report_interval_ms),
        // course and ground speed, observed with the position (an engagement's keep-out
        // carries our vehicles along it)
        velocity_mps: v.heading.zip(v.groundspeed).and_then(|(h, gs)| {
            let r = h.to_radians();
            fact([gs * r.sin(), gs * r.cos()], v.seen.position)
        }),
        platform,
    }
}

/// Every vehicle the bridge knows, at engine time `t`, and the track picture (Phase C2:
/// contacts that are not ours, as our sensors see them; absent before `tracks::init`).
pub fn build(
    world: &World,
    catalog: &Catalog,
    areas: &AreaSet,
    t: Time,
    tracks: Option<&crate::tracks::Tracks>,
) -> WorldSnapshot {
    let now = clock::mono();
    let mut snap = WorldSnapshot::new(t);
    for cell in world.vehicles() {
        let interval = cell.link_snapshot(now).expected_interval_ms;
        let facts = cell.read(|v| vehicle_facts(v, catalog, now, interval));
        snap.vehicles.insert(duid(cell.uid), facts);
    }
    snap.areas = Some(areas.clone());
    if let Some(tracks) = tracks {
        snap.tracks = tracks.facts(t.0 as f64 / 1000.0);
    }
    snap
}
