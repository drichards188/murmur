//! Which ArduPilot failsafe status texts mean the failsafe took control from the mission,
//! so an engagement on that platform stops at once.
//!
//! Only texts that say control changed count: a switch to another mode than AUTO, or a
//! disarm, or a landing. A failsafe that keeps flying the mission ("Failsafe - Continuing
//! Auto Mode", "GCS Failsafe - Continuing Auto Mode", "RC Short Failsafe On" with no
//! action, "Throttle failsafe on|off"), any "... Cleared", and a system status change are
//! not. Texts that don't say what was done (Copter's and Rover's bare "GCS Failsafe", Sub's
//! "RC Failsafe") are left to the observed mode: a dispatched platform that leaves AUTO is
//! stopped anyway (`log_vehicle_events`), and that also covers a failsafe whose own message
//! was lost on the link that failed.
//!
//! The texts, per firmware (ArduPilot source; the stand-ins send the same):
//! - Plane (events.cpp): "<GCS|RC Long> Failsafe On: switched to <mode>",
//!   "RC Short Failsafe: switched to <mode>"; no action: "RC Short Failsafe On".
//! - Copter (events.cpp `announce_failsafe`, ekf_check.cpp): "<type> Failsafe - Disarming",
//!   "<type> Failsafe - Continuing Landing", "EKF Failsafe: changed to <mode> Mode"; no
//!   action: "<type> Failsafe - Continuing Auto|Auto Mode|Guided Mode|Pilot Control".
//! - Blimp (events.cpp): "<type> Failsafe - Disarming", "<type> Failsafe - Continuing
//!   Landing".
//! - Rover (failsafe.cpp): "<type> Failsafe" then, with no action, "Failsafe - Continuing
//!   Auto Mode"; the action shows as the mode change.

/// The mode a text says the vehicle switched to, if it says so.
fn switched_to(lower: &str) -> Option<&str> {
    for marker in ["failsafe on: switched to ", "failsafe: switched to ", "failsafe: changed to "] {
        if let Some(i) = lower.find(marker) {
            let rest = &lower[i + marker.len()..];
            let mode = rest.trim().trim_end_matches(" mode").trim();
            return Some(mode);
        }
    }
    None
}

/// Whether a failsafe status text says the failsafe took control from the mission.
pub(crate) fn failsafe_took_over(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if !lower.contains("failsafe") || lower.contains("cleared") {
        return false;
    }
    if let Some(mode) = switched_to(&lower) {
        return mode != "auto";
    }
    lower.contains("failsafe - disarming") || lower.contains("failsafe - continuing landing")
}

#[cfg(test)]
mod tests {
    use super::failsafe_took_over;

    #[test]
    fn texts_that_take_control() {
        for t in [
            "[severity 4] GCS Failsafe On: switched to RTL",
            "RC Long Failsafe On: switched to RTL",
            "RC Long Failsafe On: switched to AUTOLAND",
            "RC Short Failsafe: switched to CIRCLE",
            "[severity 2] EKF Failsafe: changed to LAND Mode",
            "EKF Failsafe: changed to ALT_HOLD Mode",
            "Radio Failsafe - Disarming",
            "Battery Failsafe - Disarming",
            "GCS Failsafe - Disarming",
            "Battery Failsafe - Continuing Landing",
            "Radio + Battery Failsafe - Continuing Landing",
            "GCS + Battery Failsafe - Continuing Landing",
        ] {
            assert!(failsafe_took_over(t), "{t}");
        }
    }

    #[test]
    fn texts_that_keep_the_mission_or_do_not_say() {
        for t in [
            "Failsafe - Continuing Auto Mode",
            "GCS Failsafe - Continuing Auto Mode",
            "Radio Failsafe - Continuing Auto",
            "Radio Failsafe - Continuing Guided Mode",
            "GCS Failsafe - Continuing Pilot Control",
            "RC Short Failsafe On",
            "Throttle failsafe on",
            "Throttle failsafe off",
            "GCS Failsafe On: switched to AUTO",
            "[severity 4] GCS Failsafe Cleared",
            "RC Short Failsafe Cleared",
            "EKF Failsafe Cleared",
            "Radio Failsafe Cleared",
            "system status MAV_STATE_CRITICAL",
            "system status MAV_STATE_ACTIVE",
            // no action said: left to the observed mode
            "GCS Failsafe",
            "Battery Failsafe",
            "RC Failsafe",
            "EKF failsafe",
        ] {
            assert!(!failsafe_took_over(t), "{t}");
        }
    }
}
