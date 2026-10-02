//! Per-vehicle parameter store: ArduPilot names and types, PARAM_* protocol semantics.
//!
//! Holds the parameters a C2 actually reads or sets (system ids, waypoint and loiter radii,
//! failsafes, battery), not ArduPilot's full table of ~1,000: `param_count` reports what is
//! here. Values follow ArduPilot's defaults, then Murmur's `sim/params/*.parm` for the kind
//! (so a stand-in and a SITL vehicle of the same kind answer alike), then the platform
//! catalog. Where Murmur chose a value ArduPilot leaves off (the GCS failsafes), the table
//! says so. Behaviors read their parameters from here.

use mavlink::dialects::ardupilotmega::MavParamType;

use crate::behavior::Firmware;
use crate::config::Spec;

const I8: MavParamType = MavParamType::MAV_PARAM_TYPE_INT8;
const I16: MavParamType = MavParamType::MAV_PARAM_TYPE_INT16;
const I32: MavParamType = MavParamType::MAV_PARAM_TYPE_INT32;
const F32: MavParamType = MavParamType::MAV_PARAM_TYPE_REAL32;

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: &'static str,
    pub ty: MavParamType,
    pub value: f32,
    /// ArduPilot refuses writes from MAVLink ("Param write denied")
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetError {
    /// PARAM_ERROR MAV_PARAM_ERROR_DOES_NOT_EXIST
    DoesNotExist,
    /// NaN or infinite: MAV_PARAM_ERROR_VALUE_OUT_OF_RANGE
    OutOfRange,
    /// read-only: MAV_PARAM_ERROR_PERMISSION_DENIED
    ReadOnly,
}

/// Sorted by name, which is also the PARAM_REQUEST_LIST order and the index.
#[derive(Debug, Clone, Default)]
pub struct Params {
    list: Vec<Param>,
}

impl Params {
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn find(&self, name: &str) -> Option<(usize, &Param)> {
        self.list
            .binary_search_by(|p| p.name.cmp(name))
            .ok()
            .map(|i| (i, &self.list[i]))
    }

    pub fn at(&self, index: usize) -> Option<&Param> {
        self.list.get(index)
    }

    pub fn get(&self, name: &str) -> Option<f32> {
        self.find(name).map(|(_, p)| p.value)
    }

    /// The value as f64, or `default` if this vehicle has no such parameter.
    pub fn f(&self, name: &str, default: f64) -> f64 {
        self.get(name).map_or(default, f64::from)
    }

    /// PARAM_SET: ArduPilot's AP_Param::set_float (integers round with a 0.01 nudge, then
    /// saturate to the type's range). Returns the stored value.
    pub fn set(&mut self, name: &str, value: f32) -> Result<(usize, &Param), SetError> {
        let i = self.find(name).ok_or(SetError::DoesNotExist)?.0;
        if !value.is_finite() {
            return Err(SetError::OutOfRange);
        }
        let p = &mut self.list[i];
        if p.read_only {
            return Err(SetError::ReadOnly);
        }
        p.value = cast(p.ty, value);
        Ok((i, &self.list[i]))
    }

    /// Set from inside the autopilot (e.g. MIS_TOTAL after an upload): no read-only check.
    pub fn update(&mut self, name: &str, value: f32) {
        if let Some((i, _)) = self.find(name) {
            let ty = self.list[i].ty;
            self.list[i].value = cast(ty, value);
        }
    }

    fn put(&mut self, name: &'static str, ty: MavParamType, value: f32, read_only: bool) {
        match self.list.binary_search_by(|p| p.name.cmp(name)) {
            Ok(i) => {
                self.list[i].ty = ty;
                self.list[i].value = cast(ty, value);
            }
            Err(i) => self.list.insert(
                i,
                Param {
                    name,
                    ty,
                    value: cast(ty, value),
                    read_only,
                },
            ),
        }
    }

    /// The table for one vehicle: common, then its firmware, then the catalog.
    pub fn seed(fw: Firmware, spec: &Spec, sysid: u8) -> Params {
        let mut p = Params::default();
        let cruise = spec.cruise_mps as f32;
        let max = spec.max_mps.max(spec.cruise_mps) as f32;
        // GCS_MAVLink (MAV_ prefix in this ArduPilot; SYSID_THISMAV / SYSID_MYGCS before 4.7)
        p.put("MAV_SYSID", I16, f32::from(sysid), false);
        p.put("MAV_GCS_SYSID", I16, 255.0, false);
        // AP_Mission
        p.put("MIS_TOTAL", I16, 0.0, true);
        p.put("MIS_RESTART", I8, 0.0, false);
        p.put("MIS_OPTIONS", I16, 0.0, false);
        // AP_BattMonitor: a voltage + current monitor. Capacity is notional (the stand-in
        // drains it over the catalog endurance); failsafes trigger on remaining capacity,
        // low at 20 %, critical at 10 % (Murmur's choice: ArduPilot ships them at 0 = off).
        p.put("BATT_MONITOR", I8, 4.0, false);
        p.put("BATT_CAPACITY", I32, 10_000.0, false);
        p.put("BATT_LOW_VOLT", F32, 0.0, false);
        p.put("BATT_CRT_VOLT", F32, 0.0, false);
        p.put("BATT_LOW_MAH", F32, 2_000.0, false);
        p.put("BATT_CRT_MAH", F32, 1_000.0, false);
        p.put("BATT_LOW_TIMER", I8, 10.0, false);
        p.put("RALLY_TOTAL", I8, 0.0, false);
        p.put("FENCE_ENABLE", I8, 0.0, false);
        match fw {
            Firmware::Plane => {
                let loiter = spec.loiter_radius_m.unwrap_or(150.0).min(32_767.0) as f32;
                p.put("WP_RADIUS", I16, 90.0, false);
                // stand-ins loiter at the catalog's radius (SITL planes: 150, plane.parm)
                p.put("WP_LOITER_RAD", I16, loiter, false);
                // 0 = loiter at WP_LOITER_RAD around home
                p.put("RTL_RADIUS", I16, 0.0, false);
                // -1 = return at the current altitude (ArduPlane's default is 100 m, which
                // would bring a HALE aircraft down from 18 km)
                p.put("ALT_HOLD_RTL", I32, -1.0, false);
                p.put("AIRSPEED_CRUISE", F32, cruise, false);
                p.put("AIRSPEED_MIN", I16, (cruise * 0.6).round(), false);
                p.put("AIRSPEED_MAX", I16, max.round(), false);
                p.put("TRIM_THROTTLE", I8, 45.0, false);
                p.put("TKOFF_ALT", I16, 60.0, false);
                p.put("THR_FAILSAFE", I8, 0.0, false);
                // GCS heartbeat loss: RTL after FS_LONG_TIMEOUT (Murmur's choice; ArduPlane
                // ships FS_GCS_ENABL 0)
                p.put("FS_GCS_ENABL", I8, 1.0, false);
                p.put("FS_LONG_ACTN", I8, 1.0, false);
                p.put("FS_LONG_TIMEOUT", F32, 5.0, false);
                p.put("FS_SHORT_ACTN", I8, 0.0, false);
                p.put("FS_SHORT_TIMEOUT", F32, 1.5, false);
                let vtol = spec.behavior == "vtol";
                p.put("Q_ENABLE", I8, if vtol { 1.0 } else { 0.0 }, false);
                // low: RTL; critical: Land (no landing sequence in a stand-in: RTL)
                p.put("BATT_FS_LOW_ACT", I8, 1.0, false);
                p.put("BATT_FS_CRT_ACT", I8, 2.0, false);
            }
            Firmware::Rover => {
                p.put("WP_RADIUS", F32, 10.0, false);
                p.put("WP_SPEED", F32, cruise, false);
                p.put("CRUISE_SPEED", F32, cruise, false);
                p.put("CRUISE_THROTTLE", F32, 50.0, false);
                p.put("LOIT_RADIUS", F32, 2.0, false);
                // FRAME_CLASS 2 = boat; ArduRover then defaults MIS_DONE_BEHAVE to 1 (loiter)
                let boat = spec.behavior != "ugv";
                p.put("FRAME_CLASS", I8, if boat { 2.0 } else { 1.0 }, false);
                p.put("MIS_DONE_BEHAVE", I8, if boat { 1.0 } else { 0.0 }, false);
                // GCS heartbeat loss: RTL (Murmur's choice; ArduRover ships it off, action Hold)
                p.put("FS_GCS_ENABLE", I8, 1.0, false);
                p.put("FS_GCS_TIMEOUT", F32, 5.0, false);
                p.put("FS_ACTION", I8, 1.0, false);
                p.put("FS_TIMEOUT", F32, 1.5, false);
                p.put("FS_THR_ENABLE", I8, 0.0, false);
                p.put("FS_OPTIONS", I32, 0.0, false);
                p.put("BATT_FS_LOW_ACT", I8, 1.0, false); // RTL
                p.put("BATT_FS_CRT_ACT", I8, 2.0, false); // Hold
            }
            Firmware::Sub => {
                // ArduSub's waypoint controller works in centimetres
                p.put("WPNAV_RADIUS", F32, 1_000.0, false);
                p.put("WPNAV_SPEED", F32, cruise * 100.0, false);
                // An AUV on an acoustic link is out of contact by design: surface after two
                // minutes without the GCS (to regain RF). ArduSub's default (2 = disarm) is
                // for tethered ROVs and stays for anything else.
                let auv = spec.acoustic();
                p.put("FS_GCS_ENABLE", I8, if auv { 4.0 } else { 2.0 }, false);
                p.put("FS_GCS_TIMEOUT", F32, if auv { 120.0 } else { 5.0 }, false);
                p.put("SURFACE_DEPTH", F32, -10.0, false);
                p.put("BATT_FS_LOW_ACT", I8, 3.0, false); // surface
                p.put("BATT_FS_CRT_ACT", I8, 3.0, false);
            }
            Firmware::Copter => {
                p.put("WPNAV_RADIUS", F32, 200.0, false);
                p.put("WPNAV_SPEED", F32, cruise * 100.0, false);
                p.put("RTL_ALT", I16, 1_500.0, false);
                p.put("FS_GCS_ENABLE", I8, 1.0, false); // RTL
                p.put("FS_GCS_TIMEOUT", F32, 5.0, false);
                p.put("AUTO_OPTIONS", I32, 3.0, false); // copter.parm
                p.put("BATT_FS_LOW_ACT", I8, 2.0, false); // RTL
                p.put("BATT_FS_CRT_ACT", I8, 1.0, false); // Land
            }
            Firmware::Blimp => {
                p.put("FS_GCS_ENABLE", I8, 1.0, false);
                p.put("BATT_FS_LOW_ACT", I8, 1.0, false); // Land
                p.put("BATT_FS_CRT_ACT", I8, 1.0, false);
            }
            Firmware::Tracker => {
                p.put("SYSID_TARGET", I16, 0.0, false);
                p.put("BATT_FS_LOW_ACT", I8, 0.0, false);
                p.put("BATT_FS_CRT_ACT", I8, 0.0, false);
            }
        }
        p
    }

    /// Apply a behavior's `extra_params`.
    pub fn extend(&mut self, extra: Vec<(&'static str, MavParamType, f32)>) {
        for (name, ty, value) in extra {
            self.put(name, ty, value, false);
        }
    }
}

/// AP_Param::set_float: floats as given; integers get +-0.01 so 2.9999 doesn't truncate to
/// 2, then truncate and saturate.
fn cast(ty: MavParamType, v: f32) -> f32 {
    let (lo, hi) = match ty {
        MavParamType::MAV_PARAM_TYPE_INT8 => (f64::from(i8::MIN), f64::from(i8::MAX)),
        MavParamType::MAV_PARAM_TYPE_INT16 => (f64::from(i16::MIN), f64::from(i16::MAX)),
        MavParamType::MAV_PARAM_TYPE_INT32 => (f64::from(i32::MIN), f64::from(i32::MAX)),
        _ => return v,
    };
    let nudge = if v < 0.0 { -0.01 } else { 0.01 };
    (f64::from(v) + nudge).trunc().clamp(lo, hi) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(behavior: &str, link: Option<&str>) -> Spec {
        Spec {
            name: "t".into(),
            behavior: behavior.into(),
            cruise_mps: 90.0,
            max_mps: 105.0,
            climb_mps: Some(10.0),
            turn_dps: Some(3.0),
            loiter_radius_m: Some(3500.0),
            endurance_h: Some(35.0),
            link: link.map(Into::into),
        }
    }

    #[test]
    fn seeded_by_firmware_and_catalog() {
        let p = Params::seed(Firmware::Plane, &spec("plane", None), 7);
        assert_eq!(p.get("MAV_SYSID"), Some(7.0));
        assert_eq!(p.get("WP_LOITER_RAD"), Some(3500.0));
        assert_eq!(p.get("Q_ENABLE"), Some(0.0));
        assert_eq!(p.find("WP_LOITER_RAD").map(|(_, p)| p.ty), Some(I16));
        let v = Params::seed(Firmware::Plane, &spec("vtol", None), 7);
        assert_eq!(v.get("Q_ENABLE"), Some(1.0));
        let s = Params::seed(Firmware::Sub, &spec("sub", Some("acoustic")), 7);
        assert_eq!(s.get("FS_GCS_ENABLE"), Some(4.0));
        assert!(s.get("Q_ENABLE").is_none());
        assert!(s.get("SYSID_TARGET").is_none());
    }

    #[test]
    fn list_is_sorted_and_indexed() {
        let p = Params::seed(Firmware::Rover, &spec("boat", None), 1);
        let names: Vec<_> = (0..p.len())
            .filter_map(|i| p.at(i))
            .map(|p| p.name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let (i, _) = p.find("WP_RADIUS").expect("WP_RADIUS");
        assert_eq!(p.at(i).map(|p| p.name), Some("WP_RADIUS"));
    }

    #[test]
    fn set_rounds_like_ap_param() {
        let mut p = Params::seed(Firmware::Plane, &spec("plane", None), 1);
        assert_eq!(
            p.set("WP_LOITER_RAD", 249.999).map(|(_, p)| p.value),
            Ok(250.0)
        );
        assert_eq!(
            p.set("WP_LOITER_RAD", 1e9).map(|(_, p)| p.value),
            Ok(32767.0)
        );
        assert_eq!(p.set("FS_LONG_TIMEOUT", 7.5).map(|(_, p)| p.value), Ok(7.5));
        assert_eq!(p.set("NO_SUCH", 1.0).err(), Some(SetError::DoesNotExist));
        assert_eq!(
            p.set("WP_RADIUS", f32::NAN).err(),
            Some(SetError::OutOfRange)
        );
        assert_eq!(p.set("MIS_TOTAL", 3.0).err(), Some(SetError::ReadOnly));
        p.update("MIS_TOTAL", 3.0);
        assert_eq!(p.get("MIS_TOTAL"), Some(3.0));
    }
}
