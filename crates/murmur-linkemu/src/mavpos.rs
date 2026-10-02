//! Reads vehicle positions out of the MAVLink passing through, without altering it.

use crate::geo::GeoPoint;
use mavlink::MavHeader;
use mavlink::ReadVersion;
use mavlink::dialects::ardupilotmega::GLOBAL_POSITION_INT_DATA;
use mavlink::dialects::ardupilotmega::MavMessage;
use mavlink::peek_reader::PeekReader;

const GLOBAL_POSITION_INT: u32 = 33;

/// Returns the last GLOBAL_POSITION_INT in a datagram, if any. A datagram can carry several
/// concatenated frames (v1 or v2); frames are walked by their length fields and only
/// GLOBAL_POSITION_INT frames are handed to the mavlink crate (which checks the CRC).
pub fn scan_position(buf: &[u8]) -> Option<GeoPoint> {
    let mut found = None;
    let mut i = 0;
    while i < buf.len() {
        let (total, msgid) = match buf[i] {
            0xFD if i + 10 <= buf.len() => {
                let len = buf[i + 1] as usize;
                let signed = buf[i + 2] & 1 != 0;
                let id = u32::from_le_bytes([buf[i + 7], buf[i + 8], buf[i + 9], 0]);
                (12 + len + if signed { 13 } else { 0 }, id)
            }
            0xFE if i + 6 <= buf.len() => (8 + buf[i + 1] as usize, u32::from(buf[i + 5])),
            _ => {
                i += 1;
                continue;
            }
        };
        if i + total > buf.len() {
            break;
        }
        if msgid == GLOBAL_POSITION_INT {
            let mut frame = &buf[i..i + total];
            let mut reader = PeekReader::new(&mut frame);
            if let Ok((_, MavMessage::GLOBAL_POSITION_INT(d))) =
                mavlink::read_versioned_msg::<MavMessage, _>(&mut reader, ReadVersion::Any)
            {
                found = Some(GeoPoint {
                    lat: f64::from(d.lat) * 1e-7,
                    lon: f64::from(d.lon) * 1e-7,
                    alt_m: f64::from(d.alt) * 1e-3,
                });
            }
        }
        i += total;
    }
    found
}

/// A v2 GLOBAL_POSITION_INT frame, for tests and the load generator.
pub fn position_frame(seq: u8, lat: f64, lon: f64, alt_m: f64) -> Vec<u8> {
    let msg = MavMessage::GLOBAL_POSITION_INT(GLOBAL_POSITION_INT_DATA {
        time_boot_ms: 1000,
        lat: (lat * 1e7) as i32,
        lon: (lon * 1e7) as i32,
        alt: (alt_m * 1e3) as i32,
        relative_alt: (alt_m * 1e3) as i32,
        vx: 0,
        vy: 0,
        vz: 0,
        hdg: 0,
    });
    let mut out = Vec::new();
    let header = MavHeader {
        sequence: seq,
        system_id: 1,
        component_id: 1,
    };
    mavlink::write_versioned_msg(&mut out, mavlink::MavlinkVersion::V2, header, &msg)
        .expect("write");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_position_and_ignores_other_bytes() {
        let f = position_frame(3, 37.8068, -122.446, -12.5);
        let p = scan_position(&f).expect("position");
        assert!((p.lat - 37.8068).abs() < 1e-6);
        assert!((p.lon + 122.446).abs() < 1e-6);
        assert!((p.alt_m + 12.5).abs() < 1e-3);
        assert!(scan_position(&[0u8; 20]).is_none());
        assert!(scan_position(&f[..f.len() - 3]).is_none());
    }

    #[test]
    fn finds_position_in_concatenated_frames() {
        let mut f = position_frame(1, 10.0, 20.0, 0.0);
        f.extend(position_frame(2, 11.0, 21.0, 0.0));
        let p = scan_position(&f).expect("position");
        assert!((p.lat - 11.0).abs() < 1e-6);
    }
}
