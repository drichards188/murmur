//! A picture shaped like Saturation's after ten minutes (bd murmur-jvdc), for the timing aids
//! (`tracks::tests::scale`, `decision::tests::scale`): 500 drone tracks each seen by about 290 of
//! our cameras (a look at detection from each, recognition from a sixth, identification from a
//! thirteenth) and two radars, and 7 ships each seen by about 4,000 sensors, as the live run's
//! `/api/tracks` had them (170,833 sources and 228,247 looks on 507 tracks).

use super::Tracks;
use super::model::Track;
use super::sensing::{AisHeard, Detection, Level, Look, SensorType};
use super::truth::Domain;

/// A detection of `contact` by observer `uid`'s `sensor`, at `t` (wall s).
pub(crate) fn det(contact: &str, uid: u64, sensor: SensorType, domain: Domain, label: &str, level: Option<Level>, t: f64) -> Detection {
    Detection {
        contact_id: contact.into(),
        uid,
        sensor,
        lat: 37.81 + (uid % 7) as f64 * 1e-6,
        lon: -122.47 + (t % 1_000.0) * 1e-6,
        alt_m: (domain == Domain::Air).then_some(120.0),
        sigma_m: 15.0,
        range_m: 2_000.0,
        velocity_mps: None,
        label: Some(label.into()),
        ais: None,
        domain,
        measured_t: Some(t),
        slant_from: None,
        look: level.map(|level| Look { level, confidence: 0.9 }),
        scan_s: None,
    }
}

/// Track `no` seen by `n` cameras (and `radars` radars) at the levels Saturation's had, its
/// detections 10 ms apart from `t0` (wall s); a ship's (`ais`) heard on AIS too.
pub(crate) fn track(no: u64, domain: Domain, n: u64, radars: u64, labels: [&str; 3], ais: bool, t0: f64) -> Track {
    let id = format!("T-{no:04}");
    let contact = format!("c{no}");
    let mut t = t0;
    let first = det(&contact, 1, SensorType::EoIr, domain, labels[0], Some(Level::Detect), t);
    let mut tr = Track::new(id, no, &first, t);
    for uid in 1..=n {
        let mut levels = vec![Level::Detect];
        if uid % 6 == 0 {
            levels.push(Level::Recognise);
        }
        if uid % 13 == 0 {
            levels.push(Level::Identify);
        }
        for (i, level) in levels.into_iter().enumerate() {
            t += 0.01;
            let label = labels[match level {
                Level::Detect => 0,
                Level::Recognise => 1,
                Level::Identify => 2,
            }];
            let mut d = det(&contact, uid, SensorType::EoIr, domain, label, Some(level), t);
            if ais && i == 0 && uid % 50 == 0 {
                d.ais = Some(AisHeard { mmsi: format!("3669{no:05}"), name: "SHIP".into(), ship_type: "cargo".into() });
            }
            tr.update(&d, t);
        }
    }
    for r in 0..radars {
        t += 0.01;
        tr.update(&det(&contact, 10_000 + r, SensorType::Radar, domain, labels[0], Some(Level::Detect), t), t);
    }
    tr
}

/// Saturation's picture, numbered from `first`: 7 ships, then 500 drones, each seen by `scale`
/// times as many of ours as the live run's, every detection within a few seconds after `t0`.
pub(crate) fn picture(first: u64, scale: f64, t0: f64) -> Vec<Track> {
    let (air_n, ship_n) = ((286.0 * scale) as u64, (4_000.0 * scale) as u64);
    let ships = (first..first + 7).map(|no| track(no, Domain::Surface, ship_n, 2, ["large_surface", "large_vessel", "cargo_ship"], true, t0));
    let drones = (first + 7..first + 507).map(|no| track(no, Domain::Air, air_n, 2, ["small_air", "small_uas", "small_drone"], false, t0));
    ships.chain(drones).collect()
}

impl Tracks {
    /// Put `tracks` into the store as they are.
    pub(crate) fn insert_tracks(&self, tracks: Vec<Track>) {
        let mut inner = super::lock(&self.inner);
        for t in tracks {
            inner.tracks.insert(t.id.clone(), t);
        }
    }

    /// A second of the truth handler's work on the store as Saturation's picture makes it (the
    /// timing aids' load beside the decision loop): `per_track` detections of each track numbered
    /// `first..first + 507` folded in under the store's lock at `t` (wall s), each by one of the
    /// observers that already see it, then the feed published.
    pub(crate) fn fold_like(&self, first: u64, per_track: u64, t: f64) {
        {
            let mut inner = super::lock(&self.inner);
            for no in first..first + 507 {
                let Some(tr) = inner.tracks.get_mut(&format!("T-{no:04}")) else { continue };
                let (domain, label) = if no < first + 7 { (Domain::Surface, "large_surface") } else { (Domain::Air, "small_air") };
                for k in 0..per_track {
                    let d = det(&format!("c{no}"), 1 + k % 286, SensorType::EoIr, domain, label, Some(Level::Detect), t);
                    tr.update(&d, t);
                }
            }
        }
        self.publish(t);
    }
}
