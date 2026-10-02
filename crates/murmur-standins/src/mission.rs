//! Mission store, the upload protocol's state, and the AUTO-mode walker.

use std::collections::HashMap;
use std::time::Instant;

use mavlink::dialects::ardupilotmega::{MISSION_ITEM_INT_DATA, MavCmd, MavFrame};

use crate::kinematics::{Out, State, Target, TargetKind};

/// An upload in progress: the vehicle drives it, asking for the first missing item.
#[derive(Debug)]
pub struct Upload {
    pub items: Vec<Option<MISSION_ITEM_INT_DATA>>,
    pub last_request: Instant,
}

impl Upload {
    pub fn next_missing(&self) -> Option<u16> {
        self.items
            .iter()
            .position(Option::is_none)
            .map(|i| i as u16)
    }
}

#[derive(Debug, Default)]
pub struct Mission {
    pub items: Vec<MISSION_ITEM_INT_DATA>,
    pub seq: u16,
    /// DO_JUMP repeats left, by the jump item's seq
    pub jumps: HashMap<u16, i32>,
    pub upload: Option<Upload>,
}

impl Mission {
    pub fn advance(&mut self, out: &mut Vec<Out>) {
        self.seq = self.seq.saturating_add(1);
        out.push(Out::MissionCurrent(self.seq));
    }

    pub fn reached(&mut self, out: &mut Vec<Out>) {
        out.push(Out::ItemReached(self.seq));
        out.push(Out::Text(
            mavlink::dialects::ardupilotmega::MavSeverity::MAV_SEVERITY_INFO,
            format!("Reached waypoint #{}", self.seq),
        ));
        self.advance(out);
    }

    /// Install a completed upload. Same bookkeeping as ArduPilot: start at item 1 (0 is home).
    pub fn install(&mut self, items: Vec<MISSION_ITEM_INT_DATA>) {
        self.seq = if items.len() > 1 { 1 } else { 0 };
        self.items = items;
        self.jumps.clear();
    }

    /// The target for AUTO: walk DO_JUMPs to the current navigation item. `hold_r` is the
    /// behavior's circle for holding (planes) or 0, `loiter_default` its LOITER_UNLIM radius.
    // GLOBAL_RELATIVE_ALT_INT is deprecated for COMMAND_INT, still the mission item frame
    #[allow(deprecated)]
    pub fn auto_target(
        &mut self,
        st: &State,
        cruise: f64,
        hold_r: f64,
        loiter_default: f64,
    ) -> Target {
        let n = self.items.len();
        // standins.py loops forever on a DO_JUMP cycle with no navigation item; cap the walk.
        let mut budget = 4 * n + 16;
        while (self.seq as usize) < n && budget > 0 {
            budget -= 1;
            let it = &self.items[self.seq as usize];
            if self.seq == 0 {
                self.seq = 1;
                continue;
            }
            if it.command == MavCmd::MAV_CMD_DO_JUMP {
                let left = *self.jumps.get(&self.seq).unwrap_or(&(it.param2 as i32));
                if left == 0 {
                    self.seq += 1;
                    continue;
                }
                if left > 0 {
                    self.jumps.insert(self.seq, left - 1);
                }
                self.seq = it.param1 as u16;
                continue;
            }
            let (lat, lon) = (it.x as f64 / 1e7, it.y as f64 / 1e7);
            let alt = if it.frame == MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT {
                st.home.2 + it.z as f64
            } else {
                it.z as f64
            };
            let t = |lat, lon, alt, radius, kind| Target {
                lat,
                lon,
                alt,
                radius,
                speed: cruise,
                kind,
            };
            return match it.command {
                MavCmd::MAV_CMD_NAV_TAKEOFF | MavCmd::MAV_CMD_NAV_VTOL_TAKEOFF => t(
                    st.lat,
                    st.lon,
                    st.home.2 + it.z as f64,
                    0.0,
                    TargetKind::Takeoff,
                ),
                MavCmd::MAV_CMD_NAV_LOITER_UNLIM => {
                    let r = if it.param3 != 0.0 {
                        (it.param3 as f64).abs()
                    } else {
                        loiter_default
                    };
                    t(lat, lon, alt, r, TargetKind::Loiter)
                }
                MavCmd::MAV_CMD_NAV_RETURN_TO_LAUNCH => {
                    t(st.home.0, st.home.1, st.alt, hold_r, TargetKind::Rtl)
                }
                _ => t(lat, lon, alt, 0.0, TargetKind::Waypoint),
            };
        }
        // mission finished: hold
        Target {
            lat: st.hold_point.0,
            lon: st.hold_point.1,
            alt: st.alt,
            radius: hold_r,
            speed: cruise,
            kind: TargetKind::Hold,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(deprecated)]
    fn wp(seq: u16, command: MavCmd, p1: f32, p2: f32) -> MISSION_ITEM_INT_DATA {
        MISSION_ITEM_INT_DATA {
            seq,
            command,
            param1: p1,
            param2: p2,
            x: 378_000_000 + seq as i32 * 10_000,
            y: -1_225_000_000,
            z: 50.0,
            frame: MavFrame::MAV_FRAME_GLOBAL_RELATIVE_ALT_INT,
            ..Default::default()
        }
    }

    fn state() -> State {
        State {
            lat: 37.8,
            lon: -122.5,
            alt: 0.0,
            heading: 0.0,
            speed: 0.0,
            vz: 0.0,
            ve: 0.0,
            vn: 0.0,
            home: (37.8, -122.5, 10.0),
            armed: true,
            mode: "AUTO",
            hold_point: (37.7, -122.4),
            aground: false,
        }
    }

    #[test]
    fn do_jump_repeats_then_falls_through() {
        let mut m = Mission::default();
        let jump = MavCmd::MAV_CMD_DO_JUMP;
        let nav = MavCmd::MAV_CMD_NAV_WAYPOINT;
        m.install(vec![
            wp(0, nav, 0., 0.),
            wp(1, nav, 0., 0.),
            wp(2, jump, 1., 1.),
            wp(3, nav, 0., 0.),
        ]);
        let st = state();
        assert_eq!(m.auto_target(&st, 5.0, 0.0, 200.0).alt, 60.0);
        m.seq = 2;
        m.auto_target(&st, 5.0, 0.0, 200.0);
        assert_eq!(m.seq, 1, "first pass jumps back");
        m.seq = 2;
        m.auto_target(&st, 5.0, 0.0, 200.0);
        assert_eq!(m.seq, 3, "repeat count used up");
    }

    #[test]
    fn jump_cycle_without_navigation_does_not_hang() {
        let mut m = Mission::default();
        let jump = MavCmd::MAV_CMD_DO_JUMP;
        m.install(vec![
            wp(0, MavCmd::MAV_CMD_NAV_WAYPOINT, 0., 0.),
            wp(1, jump, 1., -1.),
        ]);
        let t = m.auto_target(&state(), 5.0, 0.0, 200.0);
        assert_eq!(t.kind, TargetKind::Hold);
    }
}
