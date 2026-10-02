//! Surface vessel (ArduRover boat): stops in HOLD, drifts with the current, runs aground.

use mavlink::dialects::ardupilotmega::MavType;

use super::marine::{self, Hull};
use super::{Behavior, Tick, modes};
use crate::kinematics::{State, climb, guide, integrate, turn_and_accelerate};

pub struct Boat;

impl Behavior for Boat {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_SURFACE_BOAT
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::ROVER
    }

    fn hold_mode(&self) -> &'static str {
        "HOLD"
    }

    fn wants_conditions(&self) -> bool {
        true
    }

    fn arming_refusal(&self, st: &State) -> Option<&'static str> {
        super::rover::arming_refusal(st.mode)
    }

    fn step(&mut self, t: &mut Tick) {
        let tgt = marine::target(t);
        let reach = t.params.f("WP_RADIUS", 10.0);
        let want = guide(t.st, tgt.as_ref(), reach, true, t.mission, t.out);
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), 0.5);
        climb(t.st, &want, t.dt, t.spec.climb_mps());
        let drift = marine::water(
            t,
            Hull::Surface {
                draft_m: marine::draft_m(t.spec.max_mps),
            },
        );
        let (e, n) = marine::settle(t, drift);
        integrate(t.st, t.dt, e, n);
    }
}
