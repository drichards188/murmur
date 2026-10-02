//! The telemetry schedule, per physics tick (10 Hz): HEARTBEAT, ATTITUDE, VFR_HUD and
//! MISSION_CURRENT at 1 Hz, GLOBAL_POSITION_INT at 2 Hz, SYS_STATUS, GPS_RAW_INT and
//! EKF_STATUS_REPORT at 0.5 Hz. Seven messages a second, as standins.py sends.

use mavlink::dialects::ardupilotmega::*;

use crate::geo::wrap180;
use crate::messages;
use crate::vehicle::{PHYSICS_HZ, Vehicle};

const HZ: u64 = PHYSICS_HZ as u64;

/// How long an upload waits for a lost item before asking again.
const UPLOAD_RETRY_S: f64 = 1.5;

pub fn send(v: &mut Vehicle) {
    let tick = v.tick;
    if tick.is_multiple_of(HZ) {
        for m in [
            heartbeat(v),
            attitude(v),
            vfr_hud(v),
            messages::mission_current(v.mission.seq),
        ] {
            v.link.send(&m);
        }
        let lost = v
            .mission
            .upload
            .as_ref()
            .filter(|up| up.last_request.elapsed().as_secs_f64() > UPLOAD_RETRY_S);
        if let Some(seq) = lost.and_then(|up| up.next_missing()) {
            request_item(v, seq); // lost item: ask again
        }
    }
    if tick.is_multiple_of(HZ / 2) {
        let m = global_position_int(v);
        v.link.send(&m);
    }
    if tick.is_multiple_of(HZ * 2) {
        for m in [sys_status(v), gps_raw_int(v), ekf_status_report()] {
            v.link.send(&m);
        }
    }
}

/// Ask the GCS for mission item `seq` (MISSION_REQUEST_INT) and note when.
pub fn request_item(v: &mut Vehicle, seq: u16) {
    if let Some(up) = &mut v.mission.upload {
        up.last_request = std::time::Instant::now();
    }
    v.link.send(&messages::mission_request_int(seq));
}

fn heartbeat(v: &Vehicle) -> MavMessage {
    let mut base = MavModeFlag::MAV_MODE_FLAG_CUSTOM_MODE_ENABLED;
    if v.st.armed {
        base |= MavModeFlag::MAV_MODE_FLAG_SAFETY_ARMED;
    }
    MavMessage::HEARTBEAT(HEARTBEAT_DATA {
        custom_mode: v.behavior.mode_number(v.st.mode).unwrap_or(0),
        mavtype: v.behavior.mav_type(),
        autopilot: MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
        base_mode: base,
        system_status: MavState::MAV_STATE_ACTIVE,
        mavlink_version: 3,
    })
}

fn attitude(v: &Vehicle) -> MavMessage {
    MavMessage::ATTITUDE(ATTITUDE_DATA {
        yaw: wrap180(v.st.heading).to_radians() as f32,
        ..Default::default()
    })
}

fn vfr_hud(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    MavMessage::VFR_HUD(VFR_HUD_DATA {
        airspeed: st.speed as f32,
        groundspeed: st.ve.hypot(st.vn) as f32,
        heading: st.heading as i16,
        throttle: if st.speed != 0.0 { 50 } else { 0 },
        alt: st.alt as f32,
        climb: st.vz as f32,
    })
}

fn global_position_int(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    MavMessage::GLOBAL_POSITION_INT(GLOBAL_POSITION_INT_DATA {
        time_boot_ms: v.started.elapsed().as_millis() as u32,
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

fn sys_status(v: &Vehicle) -> MavMessage {
    // battery from endurance: a linear drain since power-on
    let hours = v.started.elapsed().as_secs_f64() / 3600.0;
    let batt = (100.0 - 100.0 * hours / v.cfg.spec.endurance_h())
        .trunc()
        .max(0.0) as i8;
    // spelled out: the generated bitflag defaults are a flag, not empty
    MavMessage::SYS_STATUS(SYS_STATUS_DATA {
        onboard_control_sensors_present: MavSysStatusSensor::empty(),
        onboard_control_sensors_enabled: MavSysStatusSensor::empty(),
        onboard_control_sensors_health: MavSysStatusSensor::empty(),
        load: 0,
        voltage_battery: 0,
        current_battery: -1,
        drop_rate_comm: 0,
        errors_comm: 0,
        errors_count1: 0,
        errors_count2: 0,
        errors_count3: 0,
        errors_count4: 0,
        battery_remaining: batt,
        onboard_control_sensors_present_extended: MavSysStatusSensorExtended::empty(),
        onboard_control_sensors_enabled_extended: MavSysStatusSensorExtended::empty(),
        onboard_control_sensors_health_extended: MavSysStatusSensorExtended::empty(),
    })
}

fn gps_raw_int(v: &Vehicle) -> MavMessage {
    let st = &v.st;
    let (fix_type, satellites_visible) = v.behavior.gps(st);
    MavMessage::GPS_RAW_INT(GPS_RAW_INT_DATA {
        fix_type,
        lat: (st.lat * 1e7) as i32,
        lon: (st.lon * 1e7) as i32,
        alt: (st.alt * 1000.0) as i32,
        eph: 100,
        epv: 100,
        vel: (st.speed * 100.0) as u16,
        cog: (st.heading * 100.0) as u16,
        satellites_visible,
        ..Default::default()
    })
}

fn ekf_status_report() -> MavMessage {
    MavMessage::EKF_STATUS_REPORT(EKF_STATUS_REPORT_DATA {
        flags: EkfStatusFlags::EKF_ATTITUDE
            | EkfStatusFlags::EKF_VELOCITY_HORIZ
            | EkfStatusFlags::EKF_VELOCITY_VERT
            | EkfStatusFlags::EKF_POS_HORIZ_REL
            | EkfStatusFlags::EKF_POS_HORIZ_ABS
            | EkfStatusFlags::EKF_POS_VERT_ABS,
        velocity_variance: 0.1,
        pos_horiz_variance: 0.1,
        pos_vert_variance: 0.1,
        compass_variance: 0.1,
        terrain_alt_variance: 0.1,
        ..Default::default()
    })
}
