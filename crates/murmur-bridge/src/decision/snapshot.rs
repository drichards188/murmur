//! The world model as the decision layer sees it: a [`WorldSnapshot`] keyed by uid, each
//! fact stamped with when the message carrying it arrived (on the bridge's clock, which the
//! engine reads too), unknown left `None`.

use murmur_decide::{
    AreaSet, AutopilotKind, Catalog, Circle, CircleSource, Fact, GpsFixFrom, LonLat, Time,
    Uid as DUid, VehicleFacts, WorldSnapshot,
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

/// The bridge's clock's seconds (`clock::wall`, the engine's clock) to the engine's integer
/// milliseconds.
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
///
/// `orbit` is the circle an orbit task flies on station (from the task layer); a fixed wing
/// loitering or circling home on its own reports its circle too ([`Vehicle::loiter_circle`]).
pub fn vehicle_facts(
    v: &Vehicle,
    catalog: &Catalog,
    now_mono: f64,
    report_interval_ms: u32,
    orbit: Option<([f64; 2], f64)>,
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
        gps_fix: v.gps_fix_fact().map(|(f, t)| Fact::new(f, ms(t))),
        gps_glitch: v.gps_glitch_fact().map(|(g, t)| Fact::new(g, ms(t))),
        gps_fix_from: (v.gps_fix_hl2 && v.seen.gps.is_some()).then_some(GpsFixFrom::HighLatency2),
        // the circle it flies, stamped with the older of the position and its center
        circling: v.seen.position.and_then(|pos_t| {
            let (center, radius_m, source, center_t) = match orbit {
                Some((c, r)) => (LonLat::new(c[0], c[1]), r, CircleSource::OrbitTask, None),
                None => {
                    let l = v.loiter_circle()?;
                    let source = if l.rtl { CircleSource::Rtl } else { CircleSource::Loiter };
                    (LonLat::new(l.center.1, l.center.0), l.radius_m, source, Some(l.center_t))
                }
            };
            let circle = Circle {
                center,
                radius_m,
                source: Some(source),
                center_t: center_t.map(ms),
            };
            Some(Fact::new(circle, ms(center_t.map_or(pos_t, |c| c.min(pos_t)))))
        }),
        report_interval_ms: Some(report_interval_ms),
        // velocity over ground, observed with the position (an engagement's keep-out carries
        // our vehicles along it)
        velocity_mps: v.vel_en.and_then(|vel| fact(vel, v.seen.position)),
        // altitude above mean sea level, with the position (rules 6: keep-outs in 3D)
        alt_msl_m: v.alt_msl.and_then(|a| fact(a, v.seen.position)),
        // when it booted (rules 5): a queued order approved before a reboot is looked at again
        booted_t: v.booted_wall.map(ms),
        platform,
    }
}

/// Every vehicle the bridge knows, at engine time `t`, and the track picture (Phase C2:
/// contacts that are not ours, as our sensors see them; absent before `tracks::init`).
/// A vehicle the fleet manifest names that has never been heard from is in it too, with its
/// platform and no facts: an engagement's friendly keep-out can't clear a vehicle it knows
/// nothing about (fail closed).
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
        let orbit = world.tasks.orbit_of(&cell);
        let facts = cell.read(|v| vehicle_facts(v, catalog, now, interval, orbit));
        snap.vehicles.insert(duid(cell.uid), facts);
        // rules 12: who it is (a carried restore holds a mission only for the vehicle preloaded)
        snap.identities.insert(duid(cell.uid), super::engage::identity_of(&cell));
    }
    for (uid, ident) in &world.manifest().identities {
        snap.vehicles.entry(duid(*uid)).or_insert_with(|| VehicleFacts {
            platform: ident.str("platform").and_then(|p| catalog.get(p)).cloned(),
            ..VehicleFacts::default()
        });
    }
    snap.areas = Some(areas.clone());
    if let Some(tracks) = tracks {
        snap.tracks = tracks.facts(t.0 as f64 / 1000.0);
    }
    snap
}
