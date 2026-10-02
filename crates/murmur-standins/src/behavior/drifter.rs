//! Unpowered drifting buoy. Speaks ArduRover (it has to report something) but has no
//! propulsion: it refuses missions and every mode but HOLD and MANUAL, and goes where the
//! water takes it.

use mavlink::dialects::ardupilotmega::MavType;

use super::marine::{self, Hull};
use super::{Behavior, Tick, modes};
use crate::kinematics::{climb, guide, integrate, turn_and_accelerate};

pub struct Drifter;

impl Behavior for Drifter {
    fn mav_type(&self) -> MavType {
        MavType::MAV_TYPE_SURFACE_BOAT
    }

    fn modes(&self) -> &'static [(u32, &'static str)] {
        modes::ROVER
    }

    fn hold_mode(&self) -> &'static str {
        "HOLD"
    }

    fn accepts_mode(&self, name: &str) -> bool {
        matches!(name, "HOLD" | "MANUAL")
    }

    fn accepts_missions(&self) -> bool {
        false
    }

    fn marine(&self) -> bool {
        true
    }

    fn step(&mut self, t: &mut Tick) {
        let want = guide(t.st, None, 10.0, true, t.mission, t.out);
        turn_and_accelerate(t.st, &want, t.dt, t.spec.turn_dps(), 0.5);
        climb(t.st, &want, t.dt, t.spec.climb_mps());
        let (mut e, mut n) = marine::water(
            t,
            Hull::Surface {
                draft_m: marine::draft_m(t.spec.max_mps),
            },
        );
        // wave and windage jitter on top of the current
        e += t.rng.sample(0.0, 0.02);
        n += t.rng.sample(0.0, 0.02);
        let (e, n) = marine::settle(t, (e, n));
        integrate(t.st, t.dt, e, n);
    }
}
