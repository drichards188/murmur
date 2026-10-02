//! What a stand-in sends, and when.
//!
//! Rates are ArduPilot's per-message intervals (GCS_MAVLINK::set_message_interval). With
//! Murmur's `sim/params/common.parm` every stream group is 0, so out of the box an ArduPilot
//! vehicle sends only its 1 Hz HEARTBEAT plus events (STATUSTEXT, COMMAND_ACK, the mission
//! protocol); the GCS asks for the rest with SET_MESSAGE_INTERVAL (the bridge's telemetry
//! plan). Stand-ins do the same. Messages are produced on physics ticks (10 Hz), so a faster
//! request is accepted and sent at 10 Hz.
//!
//! Link-aware: an acoustic-linked vehicle under water can't stream. It sends a compact status
//! every `ACOUSTIC_STATUS_S`: HEARTBEAT plus HIGH_LATENCY2 (MAVLink's message for
//! low-bandwidth links: position, depth, heading, speed, battery, waypoint, failures), 79
//! bytes on the wire, ~32 bit/s. Basis: AUV acoustic modems carry tens to a few thousand bit/s
//! (WHOI Micromodem: 80 bit/s FSK, up to ~5 kbit/s PSK; JANUS: 80 bit/s), and survey AUVs
//! report status acoustically every few tens of seconds. Events still go out as they happen;
//! what they cost on a real modem is the link emulator's job (Phase D). At the surface it
//! streams normally again.

use mavlink::dialects::ardupilotmega::*;
use mavlink::types::CharArray;
use mavlink::{MessageData, types};

use crate::behavior::Firmware;
use crate::geo::{bearing_deg, distance_m, wrap180};
use crate::messages;
use crate::vehicle::Vehicle;

/// Seconds between acoustic status reports while submerged (estimate, see above).
pub const ACOUSTIC_STATUS_S: f64 = 20.0;
/// Below this depth there is no RF (GPS, radio): same threshold as the sub's GPS.
pub const SUBMERGED_M: f64 = 1.0;

/// Messages this stand-in can produce (ArduPilot knows many more; asking for those is denied,
/// with the text a SITL build sends).
pub const KNOWN: &[u32] = &[
    HEARTBEAT_DATA::ID,
    SYS_STATUS_DATA::ID,
    SYSTEM_TIME_DATA::ID,
    GPS_RAW_INT_DATA::ID,
    ATTITUDE_DATA::ID,
    GLOBAL_POSITION_INT_DATA::ID,
    MISSION_CURRENT_DATA::ID,
    NAV_CONTROLLER_OUTPUT_DATA::ID,
    VFR_HUD_DATA::ID,
    BATTERY_STATUS_DATA::ID,
    AUTOPILOT_VERSION_DATA::ID,
    EKF_STATUS_REPORT_DATA::ID,
    HIGH_LATENCY2_DATA::ID,
    HOME_POSITION_DATA::ID,
];

pub fn known(id: u32) -> bool {
    KNOWN.contains(&id)
}

/// The interval with no SET_MESSAGE_INTERVAL: stream groups at 0 leave only the heartbeat.
pub fn default_interval_ms(id: u32) -> u32 {
    if id == HEARTBEAT_DATA::ID { 1000 } else { 0 }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    id: u32,
    interval_ms: u32,
    next_ms: u64,
}

/// Per-message intervals for this link, and messages asked for once.
#[derive(Debug, Clone)]
pub struct Streams {
    entries: Vec<Entry>,
    pub once: Vec<u32>,
}

impl Default for Streams {
    fn default() -> Self {
        Streams {
            entries: vec![Entry {
                id: HEARTBEAT_DATA::ID,
                interval_ms: 1000,
                next_ms: 0,
            }],
            once: Vec::new(),
        }
    }
}

impl Streams {
    /// 0 = stop.
    pub fn set(&mut self, id: u32, interval_ms: u32, now_ms: u64) {
        self.entries.retain(|e| e.id != id);
        if interval_ms > 0 {
            self.entries.push(Entry {
                id,
                interval_ms,
                next_ms: now_ms,
            });
        }
    }

    /// Current interval, ms (0 = not streaming).
    pub fn interval_ms(&self, id: u32) -> u32 {
        self.entries
            .iter()
            .find(|e| e.id == id)
            .map_or(0, |e| e.interval_ms)
    }

    /// Messages per second this schedule sends (for tests and the stats line).
    pub fn rate_hz(&self) -> f64 {
        self.entries
            .iter()
            .map(|e| 1000.0 / f64::from(e.interval_ms.max(100)))
            .sum()
    }
}

/// Once per physics tick: whatever is due.
pub fn send(v: &mut Vehicle) {
    let now_ms = v.time_boot_ms();
    for id in std::mem::take(&mut v.streams.once) {
        if let Some(m) = build(v, id) {
            v.link.send(&m);
        }
    }
    if v.cfg.spec.acoustic() && v.st.depth() > SUBMERGED_M {
        let now = v.now();
        if now >= v.acoustic_next {
            v.acoustic_next = now + ACOUSTIC_STATUS_S;
            for m in [heartbeat(v), high_latency2(v)] {
                v.link.send(&m);
            }
        }
        return;
    }
    v.acoustic_next = 0.0;
    for i in 0..v.streams.entries.len() {
        let e = v.streams.entries[i];
        if now_ms < e.next_ms {
            continue;
        }
        if let Some(m) = build(v, e.id) {
            v.link.send(&m);
        }
        let mut next = e.next_ms + u64::from(e.interval_ms);
        if next <= now_ms {
            next = now_ms + u64::from(e.interval_ms); // fell behind: don't burst
        }
        v.streams.entries[i].next_ms = next;
    }
}

pub fn build(v: &Vehicle, id: u32) -> Option<MavMessage> {
    Some(match id {
        HEARTBEAT_DATA::ID => heartbeat(v),
        SYS_STATUS_DATA::ID => sys_status(v),
        SYSTEM_TIME_DATA::ID => MavMessage::SYSTEM_TIME(SYSTEM_TIME_DATA {
            time_unix_usec: messages::unix_usec(),
            time_boot_ms: v.time_boot_ms() as u32,
        }),
        GPS_RAW_INT_DATA::ID => gps_raw_int(v),
        ATTITUDE_DATA::ID => attitude(v),
        GLOBAL_POSITION_INT_DATA::ID => global_position_int(v),
        MISSION_CURRENT_DATA::ID => mission_current(v),
        NAV_CONTROLLER_OUTPUT_DATA::ID => nav_controller_output(v)?,
        VFR_HUD_DATA::ID => vfr_hud(v),
        BATTERY_STATUS_DATA::ID => battery_status(v),
        AUTOPILOT_VERSION_DATA::ID => autopilot_version(v),
        EKF_STATUS_REPORT_DATA::ID => ekf_status_report(v),
        HIGH_LATENCY2_DATA::ID => high_latency2(v),
        HOME_POSITION_DATA::ID => home_position(v),
        _ => return None,
    })
}

pub fn heartbeat(v: &Vehicle) -> MavMessage {
    let mut base = MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED;
    if v.st.armed {
        base |= MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED;
    }
    let system_status = if v.crashed {
        MavState::MAV_STATE_EMERGENCY
    } else {
        v.behavior.system_status(&v.st, v.failsafe_active())
    };
    MavMessage::HEARTBEAT(HEARTBEAT_DATA {
        custom_mode: v.behavior.mode_number(v.st.mode).unwrap_or(0),
        mavtype: v.behavior.mav_type(),
        autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
        base_mode: base,
        system_status,
        mavlink_version: 3,
    })
}

fn attitude(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    // a coordinated turn banks atan(v * omega / g); climbing pitches up
    let (roll, pitch) = if v.airborne() {
        let roll = (st.speed * v.yaw_rate / 9.81).atan();
        let pitch = st.vz.atan2(st.speed.max(1.0));
        (roll, pitch)
    } else {
        (0.0, 0.0)
    };
    MavMessage::ATTITUDE(ATTITUDE_DATA {
        time_boot_ms: v.time_boot_ms() as u32,
        roll: roll as f32,
        pitch: pitch as f32,
        yaw: wrap180(st.heading).to_radians() as f32,
        rollspeed: 0.0,
        pitchspeed: 0.0,
        yawspeed: v.yaw_rate as f32,
    })
}

/// Throttle %, from the speed demand: ArduRover's feed-forward (CRUISE_THROTTLE at
/// CRUISE_SPEED, linear), ArduPlane's TRIM_THROTTLE at cruise plus a climb share.
pub fn throttle_pct(v: &Vehicle) -> u16 {
    let st = &v.st;
    if !st.armed || v.flat() {
        return 0;
    }
    let cruise = v.cfg.spec.cruise_mps;
    if cruise <= 0.0 {
        return 0;
    }
    let p = &v.params;
    let pct = match v.firmware {
        Firmware::Plane => {
            let trim = p.f("TRIM_THROTTLE", 45.0);
            let climb = if v.cfg.spec.climb_mps() > 0.0 {
                (st.vz / v.cfg.spec.climb_mps()).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            trim * st.demand / cruise + climb * (100.0 - trim)
        }
        Firmware::Rover => {
            p.f("CRUISE_THROTTLE", 50.0) * st.demand / p.f("CRUISE_SPEED", cruise).max(0.1)
        }
        _ => 50.0 * st.demand / cruise,
    };
    pct.clamp(0.0, 100.0).round() as u16
}

fn vfr_hud(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    MavMessage::VFR_HUD(VFR_HUD_DATA {
        airspeed: st.speed as f32,
        groundspeed: st.ve.hypot(st.vn) as f32,
        heading: st.heading as i16,
        throttle: throttle_pct(v),
        alt: st.alt as f32,
        climb: st.vz as f32,
    })
}

fn global_position_int(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    MavMessage::GLOBAL_POSITION_INT(GLOBAL_POSITION_INT_DATA {
        time_boot_ms: v.time_boot_ms() as u32,
        lat: (st.lat * 1e7) as i32,
        lon: (st.lon * 1e7) as i32,
        alt: (st.alt * 1000.0) as i32,
        relative_alt: ((st.alt - st.home.2) * 1000.0) as i32,
        vx: (st.vn * 100.0) as i16,
        vy: (st.ve * 100.0) as i16,
        vz: (-st.vz * 100.0) as i16,
        hdg: (st.heading * 100.0) as u16,
    })
}

fn mission_current(v: &Vehicle) -> MavMessage {
    let m = &v.mission;
    let in_auto = v.st.mode == "AUTO";
    MavMessage::MISSION_CURRENT(MISSION_CURRENT_DATA {
        seq: m.seq,
        // excluding home, as ArduPilot reports it
        total: m.items.len().saturating_sub(1) as u16,
        mission_state: m.state(in_auto),
        mission_mode: u8::from(in_auto),
        mission_id: 0,
        fence_id: 0,
        rally_points_id: 0,
    })
}

/// Where the autopilot is navigating to: the mission item in AUTO, home in RTL.
fn nav_target(v: &Vehicle) -> Option<(f64, f64)> {
    match v.st.mode {
        "AUTO" => v.mission.current_location(),
        "RTL" | "SMART_RTL" => Some((v.st.home.0, v.st.home.1)),
        _ => None,
    }
}

fn nav_controller_output(v: &Vehicle) -> Option<MavMessage> {
    let (lat, lon) = nav_target(v)?;
    let st = &v.st;
    let bearing = bearing_deg(st.lat, st.lon, lat, lon);
    Some(MavMessage::NAV_CONTROLLER_OUTPUT(
        NAV_CONTROLLER_OUTPUT_DATA {
            nav_roll: 0.0,
            nav_pitch: 0.0,
            alt_error: 0.0,
            aspd_error: 0.0,
            xtrack_error: 0.0,
            nav_bearing: wrap180(bearing).round() as i16,
            target_bearing: wrap180(bearing).round() as i16,
            wp_dist: distance_m(st.lat, st.lon, lat, lon).min(65_535.0) as u16,
        },
    ))
}

fn sensors(v: &Vehicle) -> (MavSysStatusSensor, MavSysStatusSensor) {
    use MavSysStatusSensor as S;
    let present = S::MAV_SYS_STATUS_SENSOR_3D_GYRO
        | S::MAV_SYS_STATUS_SENSOR_3D_ACCEL
        | S::MAV_SYS_STATUS_SENSOR_3D_MAG
        | S::MAV_SYS_STATUS_SENSOR_ABSOLUTE_PRESSURE
        | S::MAV_SYS_STATUS_SENSOR_GPS
        | S::MAV_SYS_STATUS_AHRS
        | S::MAV_SYS_STATUS_SENSOR_BATTERY
        | S::MAV_SYS_STATUS_PREARM_CHECK;
    let mut healthy = present;
    if !v.nav.gps {
        healthy.remove(S::MAV_SYS_STATUS_SENSOR_GPS);
    }
    if v.battery.level > crate::battery::Level::Ok {
        healthy.remove(S::MAV_SYS_STATUS_SENSOR_BATTERY);
    }
    (present, healthy)
}

fn sys_status(v: &Vehicle) -> MavMessage {
    let cap = v.capacity_mah();
    let (present, healthy) = sensors(v);
    MavMessage::SYS_STATUS(SYS_STATUS_DATA {
        onboard_control_sensors_present: present,
        onboard_control_sensors_enabled: present,
        onboard_control_sensors_health: healthy,
        load: 0,
        voltage_battery: (v.battery.voltage(cap) * 1000.0) as u16,
        current_battery: (v.battery.current_a * 100.0).min(32_767.0) as i16,
        drop_rate_comm: 0,
        errors_comm: 0,
        errors_count1: 0,
        errors_count2: 0,
        errors_count3: 0,
        errors_count4: 0,
        battery_remaining: v.battery.remaining_pct(cap),
        onboard_control_sensors_present_extended: MavSysStatusSensorExtended::empty(),
        onboard_control_sensors_enabled_extended: MavSysStatusSensorExtended::empty(),
        onboard_control_sensors_health_extended: MavSysStatusSensorExtended::empty(),
    })
}

fn battery_status(v: &Vehicle) -> MavMessage {
    let cap = v.capacity_mah();
    let mut voltages = [u16::MAX; 10];
    voltages[0] = (v.battery.voltage(cap) * 1000.0) as u16;
    let charge_state = match v.battery.level {
        crate::battery::Level::Ok => MavBatteryChargeState::MAV_BATTERY_CHARGE_STATE_OK,
        crate::battery::Level::Low => MavBatteryChargeState::MAV_BATTERY_CHARGE_STATE_LOW,
        crate::battery::Level::Critical => MavBatteryChargeState::MAV_BATTERY_CHARGE_STATE_CRITICAL,
    };
    MavMessage::BATTERY_STATUS(BATTERY_STATUS_DATA {
        current_consumed: v.battery.consumed_mah as i32,
        energy_consumed: -1,
        temperature: i16::MAX,
        voltages,
        current_battery: (v.battery.current_a * 100.0).min(32_767.0) as i16,
        id: 0,
        battery_function: MavBatteryFunction::MAV_BATTERY_FUNCTION_UNKNOWN,
        mavtype: MavBatteryType::MAV_BATTERY_TYPE_LION,
        battery_remaining: v.battery.remaining_pct(cap),
        time_remaining: 0,
        charge_state,
        voltages_ext: [0; 4],
        mode: MavBatteryMode::MAV_BATTERY_MODE_UNKNOWN,
        fault_bitmask: MavBatteryFault::empty(),
    })
}

fn gps_raw_int(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    let (fix_type, satellites_visible) = if v.nav.gps {
        v.behavior.gps(st)
    } else if v.behavior.gps(st).0 as u8 >= GpsFixType::GPS_FIX_TYPE_3D_FIX as u8 {
        // surfaced, still acquiring
        (GpsFixType::GPS_FIX_TYPE_NO_FIX, 4)
    } else {
        v.behavior.gps(st)
    };
    let fix = fix_type as u8 >= GpsFixType::GPS_FIX_TYPE_3D_FIX as u8;
    // without a fix the GPS driver keeps its last position
    let (lat, lon, alt) = v.nav.last_fix;
    MavMessage::GPS_RAW_INT(GPS_RAW_INT_DATA {
        time_usec: messages::unix_usec(),
        fix_type,
        lat: (lat * 1e7) as i32,
        lon: (lon * 1e7) as i32,
        alt: (alt * 1000.0) as i32,
        eph: if fix { 100 } else { u16::MAX },
        epv: if fix { 100 } else { u16::MAX },
        vel: if fix {
            (st.ve.hypot(st.vn) * 100.0) as u16
        } else {
            0
        },
        cog: if fix { (st.heading * 100.0) as u16 } else { 0 },
        satellites_visible,
        ..Default::default()
    })
}

/// EKF3's flags (NavEKF3_core::updateFilterStatus): with GPS, absolute position; dead
/// reckoning on inertial + DVL, relative position only.
pub fn ekf_flags(v: &Vehicle) -> EkfStatusFlags {
    let mut f = EkfStatusFlags::EKF_ATTITUDE
        | EkfStatusFlags::EKF_VELOCITY_HORIZ
        | EkfStatusFlags::EKF_VELOCITY_VERT
        | EkfStatusFlags::EKF_POS_HORIZ_REL
        | EkfStatusFlags::EKF_POS_VERT_ABS
        | EkfStatusFlags::EKF_PRED_POS_HORIZ_REL;
    if v.nav.gps {
        f |= EkfStatusFlags::EKF_POS_HORIZ_ABS | EkfStatusFlags::EKF_PRED_POS_HORIZ_ABS;
    }
    f
}

fn ekf_status_report(v: &Vehicle) -> MavMessage {
    MavMessage::EKF_STATUS_REPORT(EKF_STATUS_REPORT_DATA {
        flags: ekf_flags(v),
        velocity_variance: 0.1,
        // no position measurement to test against while dead reckoning
        pos_horiz_variance: if v.nav.gps { 0.1 } else { 0.0 },
        pos_vert_variance: 0.1,
        compass_variance: 0.1,
        terrain_alt_variance: 0.1,
        ..Default::default()
    })
}

fn home_position(v: &Vehicle) -> MavMessage {
    let (lat, lon, alt) = v.st.home;
    MavMessage::HOME_POSITION(HOME_POSITION_DATA {
        latitude: (lat * 1e7) as i32,
        longitude: (lon * 1e7) as i32,
        altitude: (alt * 1000.0) as i32,
        q: [1.0, 0.0, 0.0, 0.0],
        time_usec: messages::unix_usec(),
        ..Default::default()
    })
}

// PARAM_FLOAT is deprecated; ArduPilot still sets it for older GCSs
#[allow(deprecated)]
fn autopilot_version(v: &Vehicle) -> MavMessage {
    use MavProtocolCapability as C;
    MavMessage::AUTOPILOT_VERSION(AUTOPILOT_VERSION_DATA {
        capabilities: C::MAV_PROTOCOL_CAPABILITY_MISSION_FLOAT
            | C::MAV_PROTOCOL_CAPABILITY_PARAM_FLOAT
            | C::MAV_PROTOCOL_CAPABILITY_MISSION_INT
            | C::MAV_PROTOCOL_CAPABILITY_COMMAND_INT
            | C::MAV_PROTOCOL_CAPABILITY_PARAM_ENCODE_C_CAST
            | C::MAV_PROTOCOL_CAPABILITY_MISSION_FENCE
            | C::MAV_PROTOCOL_CAPABILITY_MISSION_RALLY
            | C::MAV_PROTOCOL_CAPABILITY_MAVLINK2,
        uid: u64::from(v.cfg.uid()),
        // 4.8.0, FIRMWARE_VERSION_TYPE_DEV
        flight_sw_version: 0x0408_0000,
        middleware_sw_version: 0,
        os_sw_version: 0,
        board_version: 0,
        vendor_id: 0,
        product_id: 0,
        flight_custom_version: [0; 8],
        middleware_custom_version: [0; 8],
        os_custom_version: [0; 8],
        uid2: [0; 18],
    })
}

/// HIGH_LATENCY2 (ArduPilot's GCS_MAVLINK::send_high_latency2 field by field).
pub fn high_latency2(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    let mut failure = HlFailureFlag::empty();
    if !v.nav.gps {
        failure |= HlFailureFlag::HL_FAILURE_FLAG_GPS;
    }
    if v.battery.level > crate::battery::Level::Ok {
        failure |= HlFailureFlag::HL_FAILURE_FLAG_BATTERY;
    }
    let target = nav_target(v);
    let (target_heading, target_distance) = target.map_or((0, 0), |(lat, lon)| {
        let b = bearing_deg(st.lat, st.lon, lat, lon);
        let d = distance_m(st.lat, st.lon, lat, lon) / 10.0;
        ((b / 2.0) as u8, d.min(65_535.0) as u16)
    });
    let gs = st.ve.hypot(st.vn);
    MavMessage::HIGH_LATENCY2(HIGH_LATENCY2_DATA {
        timestamp: v.time_boot_ms() as u32,
        latitude: (st.lat * 1e7) as i32,
        longitude: (st.lon * 1e7) as i32,
        custom_mode: v.behavior.mode_number(st.mode).unwrap_or(0) as u16,
        altitude: st.alt.clamp(-32_768.0, 32_767.0) as i16,
        target_altitude: st.alt.clamp(-32_768.0, 32_767.0) as i16,
        target_distance,
        wp_num: v.mission.seq,
        failure_flags: failure,
        mavtype: v.behavior.mav_type(),
        autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
        heading: (st.heading.rem_euclid(360.0) / 2.0) as u8,
        target_heading,
        throttle: throttle_pct(v) as u8,
        airspeed: (st.speed * 5.0).clamp(0.0, 255.0) as u8,
        airspeed_sp: (st.demand * 5.0).clamp(0.0, 255.0) as u8,
        groundspeed: (gs * 5.0).clamp(0.0, 255.0) as u8,
        windspeed: 0,
        wind_heading: 0,
        eph: u8::MAX,
        epv: u8::MAX,
        temperature_air: i8::MAX,
        climb_rate: (st.vz * 10.0).clamp(-128.0, 127.0) as i8,
        battery: v.battery.remaining_pct(v.capacity_mah()),
        custom0: 0,
        custom1: 0,
        custom2: 0,
    })
}

/// MESSAGE_INTERVAL reply to GET_MESSAGE_INTERVAL / REQUEST_MESSAGE(MESSAGE_INTERVAL).
pub fn message_interval(id: u32, interval_us: i32) -> MavMessage {
    MavMessage::MESSAGE_INTERVAL(MESSAGE_INTERVAL_DATA {
        interval_us,
        message_id: id.min(u32::from(u16::MAX)) as u16,
    })
}

/// PARAM_VALUE as ArduPilot sends it.
pub fn param_value(id: &str, value: f32, ty: MavParamType, count: usize, index: u16) -> MavMessage {
    MavMessage::PARAM_VALUE(PARAM_VALUE_DATA {
        param_value: value,
        param_count: count.min(usize::from(u16::MAX)) as u16,
        param_index: index,
        param_id: CharArray::<16>::from(id),
        param_type: ty,
    })
}

pub fn param_error(
    id: types::CharArray<16>,
    index: i16,
    error: MavParamError,
    to: (u8, u8),
) -> MavMessage {
    MavMessage::PARAM_ERROR(PARAM_ERROR_DATA {
        param_index: index,
        target_system: to.0,
        target_component: to.1,
        param_id: id,
        error,
    })
}
