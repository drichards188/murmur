//! The link-aware telemetry plan: which messages the bridge asks each vehicle for, and how
//! often (MAV_CMD_SET_MESSAGE_INTERVAL), chosen by what carries the vehicle's link now.
//!
//! ArduPilot (and the stand-ins) send only HEARTBEAT and events until the GCS asks
//! (`sim/params/common.parm` sets every stream group to 0), so the plan is the whole of a
//! vehicle's streamed telemetry. One plan for every link (about 8.5 msg/s) is fine on a radio
//! or Starlink, wasteful on GEO SATCOM and hopeless on a 300 bps acoustic modem (murmur-f6q).
//!
//! The choice, per vehicle, once a second (`choose`):
//!
//! 1. The link profile carrying the vehicle now: the link emulator's `active_profile` when it
//!    runs (it knows `[platform_profile]` overrides and which radio a surfaced AUV uses), else
//!    the manifest's `link` class and `space_class` (and depth, for an acoustic AUV).
//! 2. That profile's tier from `LINKS` below (the one table: sources and guesses marked).
//! 3. Down a tier while the plan would not fit `BANDWIDTH_SHARE` of the link's bandwidth
//!    (a profile not in the table, or a narrower link than the table assumes).
//! 4. Down a tier while the emulator reports uplink queue drops (held for `QUEUE_HOLD_S`): the
//!    measured version of rule 3.
//!
//! Rules 3 and 4 stop at `minimal`: a vehicle that streams nothing would leave the operator
//! without its position. Only the table says `silent`, for a vehicle that reports on its own.
//!
//! Tiers (message rates in `messages`):
//!
//! - `full`: position 2 Hz (air) or 1 Hz, attitude, speed and waypoint 1 Hz, battery, GPS and
//!   EKF 0.5 Hz. About 8 msg/s, 300 B/s with the heartbeat.
//! - `reduced`: position 1 Hz (air) or 0.5 Hz, speed and waypoint 0.5 Hz, the rest 0.2 Hz.
//! - `minimal`: HIGH_LATENCY2 every 5 s, MAVLink's message for exactly this (ArduPilot sends it
//!   for its Iridium RockBLOCK support: position, altitude, heading, speeds, battery, mode,
//!   waypoint and failure flags in 54 bytes). The 1 Hz HEARTBEAT stays, so the bridge's
//!   lost-link check (`link::STALE_INTERVALS` of a 1 s interval) is unchanged.
//! - `silent`: nothing requested. An AUV under water reports HEARTBEAT + HIGH_LATENCY2 on its
//!   own every 20 s (`link::HIGH_LATENCY_INTERVAL_MS`) and ignores stream requests; the GCS
//!   heartbeat drops to every `gcs_heartbeat_s` too (5 s: the AUV's own GCS failsafe is
//!   120 s, and the emulator's loss bursts are counted in packets, so at one heartbeat per
//!   20 s a single burst silenced the GCS for two minutes and surfaced the AUV).
//!
//! `MURMUR_TELEMETRY=uniform` turns all of this off: the full plan on every link, as before
//! (for A/B runs against the link emulator).

use mavlink::MessageData;
use serde::Serialize;

use crate::clock::round;
use crate::link::EmuLink;
use crate::mav::{Kind, apm};

/// Share of a link's bandwidth the streamed telemetry may use: the rest is headroom for
/// commands, mission uploads, retransmissions and bursts of events. GUESS.
pub const BANDWIDTH_SHARE: f64 = 0.25;
/// Uplink queue drops at or above this share of the recent packets (with at least
/// `QUEUE_MIN_PKTS` of them) demote the plan one tier.
pub const QUEUE_DROP_SHARE: f64 = 0.02;
pub const QUEUE_MIN_PKTS: u64 = 10;
/// A queue demotion lasts this long after the last drops were seen, so the plan does not
/// flap between a tier that overflows and one that does not.
pub const QUEUE_HOLD_S: f64 = 120.0;
/// Deeper than this (m below the surface) an AUV is on its acoustic modem: the stand-ins'
/// `SUBMERGED_M` and the emulator's `submerged_depth_m`.
pub const SUBMERGED_M: f64 = 1.0;
/// MAVLink 2 framing around a payload: 10 bytes of header, 2 of checksum (no signing).
const MAV2_OVERHEAD: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Silent,
    Minimal,
    Reduced,
    Full,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Full => "full",
            Tier::Reduced => "reduced",
            Tier::Minimal => "minimal",
            Tier::Silent => "silent",
        }
    }

    fn down(self) -> Tier {
        match self {
            Tier::Full => Tier::Reduced,
            Tier::Reduced => Tier::Minimal,
            Tier::Minimal | Tier::Silent => Tier::Silent,
        }
    }

    /// Seconds between the reports that carry position and speed under this tier: what the
    /// task layer's NOT MOVING check and its confirmations must allow for.
    pub fn report_s(self) -> f64 {
        match self {
            Tier::Full => 1.0,
            Tier::Reduced => 2.0,
            Tier::Minimal => 5.0,
            Tier::Silent => f64::from(crate::link::HIGH_LATENCY_INTERVAL_MS) / 1000.0,
        }
    }
}

/// One link profile (the names of `sim/links.toml`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinkRow {
    pub profile: &'static str,
    /// For the console: "telemetry: reduced for GEO SATCOM".
    pub label: &'static str,
    pub tier: Tier,
    /// Per direction, as `sim/links.toml` (a fallback; the emulator's figure wins).
    pub bandwidth_bps: u32,
    /// Typical round trip, s: the timeout basis before any TIMESYNC answer or emulator figure.
    pub rtt_s: f64,
    /// Seconds between GCS heartbeats.
    pub gcs_heartbeat_s: u64,
}

/// The table. Bandwidths and latencies are `sim/links.toml`'s (which marks its own guesses);
/// the tier is the policy:
/// - LOS, mesh, tether, LEO (Starlink-class): full. Hundreds of kbps to Mbps; the full plan
///   is about 2.4 kbps.
/// - GEO SATCOM: reduced. The capacity would carry the full plan; GEO terminals on MALE/HALE
///   aircraft are leased and shared, and a 1 Hz position already beats the ~0.6 s round trip
///   plus the operator's reaction time. GUESS (a policy, not a measurement).
/// - Iridium-class (the buoy; an AUV's mast radio at the surface): minimal. 22 kbps here
///   (Certus 100-class), but Iridium is billed per byte and SBD carries a few hundred bytes a
///   message; ArduPilot's own answer for Iridium is HIGH_LATENCY2. The rate (every 5 s) is a
///   GUESS: often enough to watch a task, little enough to leave the link to commands.
/// - Acoustic: silent. 300 bps (`links.toml`, GUESS in the 80-5400 bps modem band): the AUV's
///   own 20 s HIGH_LATENCY2 is all it can carry. RTT GUESS: sound over ~2 km both ways plus
///   modem processing and a 30-byte ping's airtime. GCS heartbeat every 5 s: 21 bytes, about
///   11% of the modem's downlink.
#[rustfmt::skip]
pub const LINKS: &[LinkRow] = &[
    LinkRow { profile: "los", label: "LOS radio", tier: Tier::Full, bandwidth_bps: 500_000, rtt_s: 0.03, gcs_heartbeat_s: 1 },
    LinkRow { profile: "mesh", label: "mesh radio", tier: Tier::Full, bandwidth_bps: 300_000, rtt_s: 0.05, gcs_heartbeat_s: 1 },
    LinkRow { profile: "tether", label: "tether", tier: Tier::Full, bandwidth_bps: 10_000_000, rtt_s: 0.002, gcs_heartbeat_s: 1 },
    LinkRow { profile: "satcom_leo", label: "LEO SATCOM", tier: Tier::Full, bandwidth_bps: 2_000_000, rtt_s: 0.05, gcs_heartbeat_s: 1 },
    LinkRow { profile: "satcom", label: "GEO SATCOM", tier: Tier::Reduced, bandwidth_bps: 256_000, rtt_s: 0.6, gcs_heartbeat_s: 1 },
    LinkRow { profile: "satcom_ku_geo", label: "GEO SATCOM", tier: Tier::Reduced, bandwidth_bps: 256_000, rtt_s: 0.6, gcs_heartbeat_s: 1 },
    LinkRow { profile: "satcom_iridium", label: "Iridium", tier: Tier::Minimal, bandwidth_bps: 22_000, rtt_s: 1.4, gcs_heartbeat_s: 1 },
    LinkRow { profile: "acoustic_surface", label: "Iridium (surfaced)", tier: Tier::Minimal, bandwidth_bps: 22_000, rtt_s: 1.4, gcs_heartbeat_s: 1 },
    LinkRow { profile: "acoustic", label: "acoustic modem", tier: Tier::Silent, bandwidth_bps: 300, rtt_s: 6.0, gcs_heartbeat_s: 5 },
];

pub fn row(profile: &str) -> Option<&'static LinkRow> {
    LINKS.iter().find(|r| r.profile == profile)
}

/// The profile a vehicle's link uses without the emulator to ask: `sim/links.toml`'s rules
/// (SATCOM by `space_class`, an acoustic AUV on its surface radio when not submerged).
pub fn profile_from_class(
    class: Option<&str>,
    space_class: Option<&str>,
    submerged: bool,
) -> Option<&'static str> {
    Some(match (class?, space_class) {
        ("satcom", Some("starlink")) => "satcom_leo",
        ("satcom", Some("iridium")) => "satcom_iridium",
        ("satcom", Some("ku-geo")) => "satcom_ku_geo",
        ("satcom", _) => "satcom",
        ("acoustic", _) if submerged => "acoustic",
        ("acoustic", _) => "acoustic_surface",
        (c, _) => row(c)?.profile,
    })
}

/// Every message the bridge ever streams, so a plan change can stop what it no longer wants.
pub const STREAMS: &[u32] = &[
    apm::GLOBAL_POSITION_INT_DATA::ID,
    apm::ATTITUDE_DATA::ID,
    apm::VFR_HUD_DATA::ID,
    apm::MISSION_CURRENT_DATA::ID,
    apm::SYS_STATUS_DATA::ID,
    apm::GPS_RAW_INT_DATA::ID,
    apm::EKF_STATUS_REPORT_DATA::ID,
    apm::NAV_CONTROLLER_OUTPUT_DATA::ID,
    apm::HIGH_LATENCY2_DATA::ID,
];

/// (message id, Hz) for a tier. Position fastest for aircraft; everything else slow.
/// Event messages (HEARTBEAT, STATUSTEXT, COMMAND_ACK, mission protocol) aren't streams.
pub fn messages(tier: Tier, kind: Option<Kind>, air: bool) -> Vec<(u32, f64)> {
    let (pos, fast, slow) = match tier {
        Tier::Full => (if air { 2.0 } else { 1.0 }, 1.0, 0.5),
        Tier::Reduced => (if air { 1.0 } else { 0.5 }, 0.5, 0.2),
        Tier::Minimal => return vec![(apm::HIGH_LATENCY2_DATA::ID, 0.2)],
        Tier::Silent => return Vec::new(),
    };
    let attitude = if tier == Tier::Full { fast } else { slow };
    let mut plan = vec![
        (apm::GLOBAL_POSITION_INT_DATA::ID, pos),
        (apm::ATTITUDE_DATA::ID, attitude),
        (apm::VFR_HUD_DATA::ID, fast),
        (apm::MISSION_CURRENT_DATA::ID, fast),
        (apm::SYS_STATUS_DATA::ID, slow),
        (apm::GPS_RAW_INT_DATA::ID, slow),
        (apm::EKF_STATUS_REPORT_DATA::ID, slow),
    ];
    if kind == Some(Kind::Tracker) {
        plan.push((apm::NAV_CONTROLLER_OUTPUT_DATA::ID, fast));
    }
    plan
}

/// Bytes on the wire for one message (MAVLink 2, payload untruncated: an upper bound).
fn wire_bytes(id: u32) -> usize {
    use apm::*;
    MAV2_OVERHEAD
        + match id {
            HEARTBEAT_DATA::ID => HEARTBEAT_DATA::ENCODED_LEN,
            GLOBAL_POSITION_INT_DATA::ID => GLOBAL_POSITION_INT_DATA::ENCODED_LEN,
            ATTITUDE_DATA::ID => ATTITUDE_DATA::ENCODED_LEN,
            VFR_HUD_DATA::ID => VFR_HUD_DATA::ENCODED_LEN,
            MISSION_CURRENT_DATA::ID => MISSION_CURRENT_DATA::ENCODED_LEN,
            SYS_STATUS_DATA::ID => SYS_STATUS_DATA::ENCODED_LEN,
            GPS_RAW_INT_DATA::ID => GPS_RAW_INT_DATA::ENCODED_LEN,
            EKF_STATUS_REPORT_DATA::ID => EKF_STATUS_REPORT_DATA::ENCODED_LEN,
            NAV_CONTROLLER_OUTPUT_DATA::ID => NAV_CONTROLLER_OUTPUT_DATA::ENCODED_LEN,
            HIGH_LATENCY2_DATA::ID => HIGH_LATENCY2_DATA::ENCODED_LEN,
            _ => 64,
        }
}

/// (messages, bytes) per second the vehicle streams under `plan`, its 1 Hz HEARTBEAT included
/// (a silent AUV's own 20 s report is not the plan's and is left out).
pub fn load(plan: &[(u32, f64)]) -> (f64, f64) {
    let hb = (1.0, wire_bytes(apm::HEARTBEAT_DATA::ID) as f64);
    plan.iter().fold(hb, |(n, b), &(id, hz)| {
        (n + hz, b + hz * wire_bytes(id) as f64)
    })
}

/// What the plan selection looks at.
#[derive(Debug, Clone, Default)]
pub struct PlanInput<'a> {
    pub kind: Option<Kind>,
    pub air: bool,
    /// The manifest's `link` class and `space_class`.
    pub link_class: Option<&'a str>,
    pub space_class: Option<&'a str>,
    /// Below the stand-ins' 1 m (an AUV on its acoustic modem).
    pub submerged: bool,
    pub emu: Option<&'a EmuLink>,
    /// A queue demotion is in force (`QUEUE_HOLD_S` after the last drops).
    pub queue_hold: bool,
    /// `MURMUR_TELEMETRY=uniform`: the full plan whatever the link.
    pub uniform: bool,
}

/// `MURMUR_TELEMETRY=uniform` (read once).
pub fn uniform_from_env() -> bool {
    static UNIFORM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *UNIFORM.get_or_init(|| std::env::var("MURMUR_TELEMETRY").is_ok_and(|v| v.trim() == "uniform"))
}

/// The chosen plan, as `/api/state` shows it per vehicle (`link.telemetry`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlanChoice {
    pub tier: Tier,
    /// The link profile it was chosen for (`sim/links.toml` names), when known.
    pub profile: Option<String>,
    /// "GEO SATCOM", "Iridium", ...: for "telemetry: reduced for GEO SATCOM".
    pub label: Option<&'static str>,
    /// Why this tier: "link" (the table), "bandwidth" (stepped down to fit) or "queue"
    /// (stepped down for measured queue drops).
    pub reason: &'static str,
    /// What the vehicle streams under it, HEARTBEAT included.
    pub msgs_per_s: f64,
    pub bytes_per_s: f64,
    /// Seconds between position/speed reports (`Tier::report_s`).
    pub report_s: f64,
    #[serde(skip)]
    pub gcs_heartbeat_s: u64,
    /// The profile's typical round trip, s (a timeout basis before anything is measured).
    #[serde(skip)]
    pub nominal_rtt_s: Option<f64>,
}

impl PlanChoice {
    pub fn messages(&self, kind: Option<Kind>, air: bool) -> Vec<(u32, f64)> {
        messages(self.tier, kind, air)
    }
}

/// Recent uplink queue drops on the emulator high enough to step the plan down.
pub fn queue_pressure(emu: &EmuLink) -> bool {
    emu.up_pkts >= QUEUE_MIN_PKTS
        && emu.up_queue_drops as f64 >= QUEUE_DROP_SHARE * emu.up_pkts as f64
}

pub fn choose(input: &PlanInput) -> PlanChoice {
    // The emulator knows overrides and gates, but it learns depth only from the positions the
    // vehicle sends: before the first one an AUV reads as on its modem, and if that made the
    // plan silent the vehicle would never send one. So the acoustic gate is the bridge's own
    // depth.
    let emu_profile = input
        .emu
        .filter(|e| e.profile.as_deref() != Some("acoustic"))
        .and_then(|e| e.active_profile.as_deref());
    let profile: Option<String> = emu_profile.map(str::to_string).or_else(|| {
        profile_from_class(input.link_class, input.space_class, input.submerged).map(str::to_string)
    });
    let known = profile.as_deref().and_then(row);
    let bandwidth = input
        .emu
        .filter(|e| e.active_profile == e.profile)
        .and_then(|e| e.bandwidth_bps)
        .filter(|b| *b > 0)
        .or(known.map(|r| r.bandwidth_bps));
    let mut tier = known.map_or(Tier::Full, |r| r.tier);
    let mut reason = "link";
    if input.uniform {
        let (msgs, bytes) = load(&messages(Tier::Full, input.kind, input.air));
        return PlanChoice {
            tier: Tier::Full,
            profile,
            label: known.map(|r| r.label),
            reason: "uniform",
            msgs_per_s: round(msgs, 1),
            bytes_per_s: round(bytes, 0),
            report_s: Tier::Full.report_s(),
            gcs_heartbeat_s: 1,
            nominal_rtt_s: known.map(|r| r.rtt_s),
        };
    }
    if let Some(bw) = bandwidth {
        let budget = BANDWIDTH_SHARE * f64::from(bw) / 8.0;
        while tier > Tier::Minimal && load(&messages(tier, input.kind, input.air)).1 > budget {
            tier = tier.down();
            reason = "bandwidth";
        }
    }
    if input.queue_hold && tier > Tier::Minimal {
        tier = tier.down();
        reason = "queue";
    }
    let (msgs, bytes) = load(&messages(tier, input.kind, input.air));
    PlanChoice {
        tier,
        profile,
        label: known.map(|r| r.label),
        reason,
        msgs_per_s: round(msgs, 1),
        bytes_per_s: round(bytes, 0),
        report_s: tier.report_s(),
        gcs_heartbeat_s: known.map_or(1, |r| r.gcs_heartbeat_s),
        nominal_rtt_s: known.map(|r| r.rtt_s),
    }
}

/// The SET_MESSAGE_INTERVAL requests that take a vehicle from `sent` (None: unknown, e.g. a
/// bridge restart) to `want`: (message id, interval in microseconds; -1 stops it).
pub fn interval_requests(want: &[(u32, f64)], sent: Option<&[(u32, f64)]>) -> Vec<(u32, f32)> {
    let mut out: Vec<(u32, f32)> = want
        .iter()
        .map(|&(id, hz)| (id, (1e6 / hz) as f32))
        .collect();
    let stale: Vec<u32> = match sent {
        Some(s) => s.iter().map(|&(id, _)| id).collect(),
        None => STREAMS.to_vec(),
    };
    for id in stale {
        if !want.iter().any(|&(w, _)| w == id) {
            out.push((id, -1.0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(class: &'a str, space: Option<&'a str>) -> PlanInput<'a> {
        PlanInput {
            kind: Some(Kind::Plane),
            air: true,
            link_class: Some(class),
            space_class: space,
            ..PlanInput::default()
        }
    }

    #[test]
    fn full_plan_matches_the_old_one() {
        let plan = messages(Tier::Full, Some(Kind::Plane), true);
        assert_eq!(plan[0], (33, 2.0));
        assert_eq!(plan.len(), 7);
        assert_eq!(
            messages(Tier::Full, Some(Kind::Tracker), false).last(),
            Some(&(62, 1.0))
        );
        let (n, b) = load(&plan);
        assert_eq!(n, 7.5);
        assert!((250.0..300.0).contains(&b), "{b}");
    }

    #[test]
    fn tier_by_link_class() {
        let t = |class, space| choose(&input(class, space)).tier;
        assert_eq!(t("los", None), Tier::Full);
        assert_eq!(t("mesh", None), Tier::Full);
        assert_eq!(t("tether", None), Tier::Full);
        assert_eq!(t("satcom", Some("starlink")), Tier::Full);
        assert_eq!(t("satcom", Some("wgs")), Tier::Reduced);
        assert_eq!(t("satcom", Some("ku-geo")), Tier::Reduced);
        assert_eq!(t("satcom", None), Tier::Reduced);
        assert_eq!(t("satcom", Some("iridium")), Tier::Minimal);
        assert_eq!(t("acoustic", None), Tier::Minimal); // at the surface: its mast radio
        let under = PlanInput {
            submerged: true,
            ..input("acoustic", None)
        };
        let c = choose(&under);
        assert_eq!((c.tier, c.gcs_heartbeat_s), (Tier::Silent, 5));
        assert_eq!(c.report_s, 20.0);
        let geo = choose(&input("satcom", Some("wgs")));
        assert_eq!(
            (geo.label, geo.reason, geo.profile.as_deref()),
            (Some("GEO SATCOM"), "link", Some("satcom"))
        );
        assert_eq!(geo.nominal_rtt_s, Some(0.6));
        // unknown class: full, nothing known about it
        let c = choose(&input("laser", None));
        assert_eq!((c.tier, c.profile, c.label), (Tier::Full, None, None));
    }

    #[test]
    fn minimal_is_high_latency2_and_fits_iridium_easily() {
        let c = choose(&input("satcom", Some("iridium")));
        assert_eq!(
            messages(c.tier, None, true),
            [(apm::HIGH_LATENCY2_DATA::ID, 0.2)]
        );
        assert_eq!(c.msgs_per_s, 1.2);
        assert!(c.bytes_per_s < 40.0, "{}", c.bytes_per_s); // vs 2750 B/s of capacity
        assert_eq!(c.label, Some("Iridium"));
    }

    #[test]
    fn the_emulators_profile_wins_and_bandwidth_steps_down() {
        // the emulator says the surfaced AUV is on its surface radio
        let emu = EmuLink {
            profile: Some("acoustic".into()),
            active_profile: Some("acoustic_surface".into()),
            bandwidth_bps: Some(300), // the main profile's, not the active one's
            ..EmuLink::default()
        };
        let c = choose(&PlanInput {
            emu: Some(&emu),
            ..input("acoustic", None)
        });
        assert_eq!((c.tier, c.reason), (Tier::Minimal, "link"));
        // for the acoustic gate the bridge's own depth decides: before the AUV's first
        // position the emulator reads it as on its modem, and a silent plan would never
        // let it send one
        let blind = EmuLink {
            active_profile: Some("acoustic".into()),
            ..emu.clone()
        };
        let c = choose(&PlanInput {
            emu: Some(&blind),
            ..input("acoustic", None)
        });
        assert_eq!(c.tier, Tier::Minimal);
        let c = choose(&PlanInput {
            emu: Some(&emu),
            submerged: true,
            ..input("acoustic", None)
        });
        assert_eq!(c.tier, Tier::Silent);
        // an overridden profile the table doesn't know, 4800 bps: full won't fit a quarter
        let radio = EmuLink {
            profile: Some("sik".into()),
            active_profile: Some("sik".into()),
            bandwidth_bps: Some(4800),
            ..EmuLink::default()
        };
        let c = choose(&PlanInput {
            emu: Some(&radio),
            ..input("los", None)
        });
        assert_eq!(
            (c.tier, c.reason, c.label),
            (Tier::Reduced, "bandwidth", None)
        );
        assert!(c.bytes_per_s <= 150.0);
        // 1200 bps: down to HIGH_LATENCY2
        let slow = EmuLink {
            bandwidth_bps: Some(1200),
            ..radio
        };
        let c = choose(&PlanInput {
            emu: Some(&slow),
            ..input("los", None)
        });
        assert_eq!(c.tier, Tier::Minimal);
        // 300 bps, too narrow even for that: minimal is the floor (silent would leave the
        // operator without a position), and queue drops don't go below it either
        let modem = EmuLink {
            bandwidth_bps: Some(300),
            ..slow
        };
        let c = choose(&PlanInput {
            emu: Some(&modem),
            queue_hold: true,
            ..input("los", None)
        });
        assert_eq!((c.tier, c.reason), (Tier::Minimal, "bandwidth"));
    }

    #[test]
    fn queue_drops_demote_one_tier() {
        let mut emu = EmuLink {
            up_pkts: 100,
            up_queue_drops: 1,
            ..EmuLink::default()
        };
        assert!(!queue_pressure(&emu));
        emu.up_queue_drops = 5;
        assert!(queue_pressure(&emu));
        emu.up_pkts = 5; // too few packets to judge
        assert!(!queue_pressure(&emu));
        let c = choose(&PlanInput {
            queue_hold: true,
            ..input("los", None)
        });
        assert_eq!((c.tier, c.reason), (Tier::Reduced, "queue"));
    }

    #[test]
    fn uniform_is_the_old_plan_everywhere() {
        let c = choose(&PlanInput {
            submerged: true,
            uniform: true,
            ..input("acoustic", None)
        });
        assert_eq!(
            (c.tier, c.reason, c.gcs_heartbeat_s),
            (Tier::Full, "uniform", 1)
        );
        assert_eq!(c.nominal_rtt_s, Some(6.0)); // the timing still knows the link
    }

    #[test]
    fn plan_changes_stop_what_is_no_longer_wanted() {
        let full = messages(Tier::Full, None, false);
        let min = messages(Tier::Minimal, None, false);
        let r = interval_requests(&min, Some(&full));
        assert_eq!(r[0], (apm::HIGH_LATENCY2_DATA::ID, 5e6));
        assert_eq!(r.len(), 1 + full.len());
        assert!(r[1..].iter().all(|&(_, us)| us == -1.0));
        // unknown previous state (bridge restart): everything else is stopped
        let r = interval_requests(&full, None);
        assert_eq!(r.len(), STREAMS.len());
        assert_eq!(r.iter().filter(|x| x.1 < 0.0).count(), 2); // NAV_CONTROLLER_OUTPUT, HL2
        // same plan again: only the sets
        assert_eq!(interval_requests(&full, Some(&full)).len(), full.len());
    }
}
