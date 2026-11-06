"""Our SITL vehicles act out a strike's damage (bd murmur-y85j.8; docs/AUTONOMOUS-NODES.md 10).

The truth side's damage model (crates/murmur-contacts `damage`) appends one JSON line per vehicle
of ours a strike hurt to the damage file (`MURMUR_DAMAGE_FILE`, else
`$MURMUR_ROOT/sim/run/damage-<MURMUR_HTTP_PORT, 8765>.jsonl`), and the stand-ins act it out
(crates/murmur-standins `damage`). sim/swarm.py runs this watcher beside the SITL vehicles it
starts, so they act out the same records as far as ArduPilot SITL allows:

- **mobility** (propulsion lost): SITL's own failure injection, `SIM_ENGINE_FAIL` (a mask of
  servo outputs) with `SIM_ENGINE_MUL` 0, on the outputs that drive the kind's propulsion
  (`PROPULSION`). SITL scales those outputs before its physics model reads them
  (libraries/AP_HAL_SITL/SITL_State.cpp, `engine_fail`), so the thrust goes and the vehicle comes
  down or to a stop by forces its IMU feels, while ArduPilot flies on unaware, as an autopilot
  whose motor has died: a plane glides down, a copter or a quadplane falls, a blimp's fins stop
  beating and it drifts, a boat or a UGV coasts to a stop, a sub rises on its own buoyancy. A
  sailboat keeps sailing (its sail draws on the wind and no SIM parameter takes that away) and an
  antenna tracker has no propulsion: both only report.
- **firepower** (weapons out of action): nothing moves differently.
- **destroyed**: silent. The SITL process is stopped (SIGSTOP), so it neither steps, sends nor
  answers, as a destroyed stand-in; its sockets stay open, so the link emulator and the bridge
  meet silence and no error (a killed process would refuse the emulator's datagrams and drop the
  tidewater driver's TCP link), and sim/swarm.py continues it at shutdown so it can exit.

A damaged vehicle that can still talk says so in the stand-in's words, a STATUSTEXT `DAMAGE
mobility: propulsion lost` or `DAMAGE firepower: weapons out of action` (severity CRITICAL), at
once and every `REPORT_EVERY_S` while it lasts. It goes in on the vehicle's SERIAL2 (TCP 5763 +
10 x instance; the tidewater driver has SERIAL1) from the vehicle's own system id as its onboard
computer (component 191), and ArduPilot's MAVLink routing sends it on over every link it has
heard a ground station on, the bridge's among them: the C2 hears it from the vehicle, over the
vehicle's link, as it hears a stand-in's (the bridge reads any component of a vehicle's system,
`vehicle.rs` STATUSTEXT, rules 25). The parameters go in the same way; ArduPilot echoes each
PARAM_VALUE to every link, as it does the tidewater driver's SIM_WIND_* and SIM_TIDE_*, and the
bridge reads none of them.

Records already in the file when the fleet started and not written since are a previous run's
(the contacts process empties the file when it starts): they are skipped. Damage never heals in a
run. With `--keep-eeprom` a damaged vehicle keeps SIM_ENGINE_FAIL in its eeprom into the next run.
"""

from __future__ import annotations

import json
import os
import signal
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# how often a damaged vehicle repeats its report, s (murmur_standins::damage::REPORT_EVERY_S)
REPORT_EVERY_S = 30.0
# the stand-in's words (murmur_standins::damage::State::report); the bridge parses the word after
# "DAMAGE " (murmur-bridge vehicle.rs, DamageReport::parse)
REPORTS = {
    "mobility": "DAMAGE mobility: propulsion lost",
    "firepower": "DAMAGE firepower: weapons out of action",
}
STATES = ("mobility", "firepower", "destroyed")
MAV_SEVERITY_CRITICAL = 2
# MAV_COMP_ID_ONBOARD_COMPUTER: the reports and parameters come from the vehicle's own system as
# its onboard computer (ArduPilot drops a message carrying its own system and component as a loop)
COMPONENT = 191
AUTOPILOT_COMPONENT = 1

# The servo outputs (bit i: SERVO<i+1>) that drive each SITL kind's propulsion (sim/swarm.py's
# VEHICLES keys), from the frames' ArduPilot defaults (Tools/autotest/pysim/vehicleinfo.json) and
# SITL models. SITL_State.cpp maps a failed output to 1000 us (ArduRover: to 1500, neutral) times
# SIM_ENGINE_MUL plus that; at 0 each is zero thrust.
PROPULSION = {
    # SERVO3 throttle (SIM_Plane reads servo 3 for thrust)
    "plane": 0b0000_0100,
    # SERVO3 forward throttle and SERVO5-8 the lift motors (SIM_QuadPlane, motor_offset 4)
    "vtol": 0b1111_0100,
    # SERVO1-4 the motors (quad, x)
    "copter": 0b0000_1111,
    # SERVO1-4 the fins (Motor1-4, blimp.parm): held still they push nothing (SIM_Blimp's thrust
    # comes from fin speed)
    "blimp": 0b0000_1111,
    # SERVO3 throttle and SERVO1 steering (motorboat: SIM_Sailboat with its motor). Our boats steer
    # by their thrust (waterjet or outboard), so with it gone they cannot steer either (SITL's
    # rudder would still turn the boat on what way it has left). It then drifts on the current:
    # tidewater's, or the motorboat defaults' SIM_TIDE_SPEED 0.5 from the north without it
    "boat": 0b0000_0101,
    # SERVO1 and SERVO3 the left and right throttle (rover-skid.parm: functions 73 and 74)
    "ugv": 0b0000_0101,
    # SERVO1-6 the thrusters (vectored): 1000 us is outside the band SIM_Submarine takes as
    # thrust (1000 < pwm < 2000), so none; its slight positive buoyancy brings it up
    "sub": 0b0011_1111,
    # sailboat: none. The sail draws on the wind (SIM_Sailboat); a failed mainsail servo holds
    #   the sail at a fixed angle, which still sails. Reports only (a gap: docs/AUTONOMOUS-NODES.md
    #   2.6).
    # tracker: none. An antenna tracker does not travel. Reports only.
}


def control_port(instance: int) -> int:
    """The SITL instance's SERIAL2 (TCP 5760 + 10 x instance + 3)."""
    return 5763 + 10 * instance


def damage_path(env=None) -> Path:
    """The file the contacts process writes (murmur_standins::damage::path): MURMUR_DAMAGE_FILE,
    else $MURMUR_ROOT/sim/run/damage-<MURMUR_HTTP_PORT, 8765>.jsonl (this checkout without
    MURMUR_ROOT)."""
    env = os.environ if env is None else env
    if (env.get("MURMUR_DAMAGE_FILE") or "").strip():
        return Path(env["MURMUR_DAMAGE_FILE"])
    root = Path(env["MURMUR_ROOT"]) if (env.get("MURMUR_ROOT") or "").strip() else REPO
    port = (env.get("MURMUR_HTTP_PORT") or "").strip() or "8765"
    return root / "sim" / "run" / f"damage-{port}.jsonl"


def read_records(data: bytes, start: int) -> tuple[list[tuple[int, str]], int]:
    """(uid, state) of each whole line of `data` from byte `start` on, and where the next read
    starts (a partial last line waits for it). Lines that are not a record, and states that change
    nothing (`undamaged`), are skipped."""
    out = []
    at = start
    for line in data[start:].splitlines(keepends=True):
        if not line.endswith(b"\n"):
            break
        at += len(line)
        try:
            rec = json.loads(line)
            uid, state = int(rec["uid"]), str(rec["state"])
        except (ValueError, KeyError, TypeError):
            continue
        if state in STATES:
            out.append((uid, state))
    return out, at


@dataclass(frozen=True)
class Plan:
    """What a SITL vehicle does with the damage it has taken: parameters to hold (name, value),
    its reports (repeated every REPORT_EVERY_S) and whether it goes silent."""

    params: tuple[tuple[str, float], ...] = ()
    reports: tuple[str, ...] = ()
    silent: bool = False


def plan(kind: str, states) -> Plan:
    """The plan for a SITL vehicle of `kind` (sim/swarm.py's VEHICLES keys) that has taken
    `states` (in the order taken)."""
    states = list(dict.fromkeys(states))
    if "destroyed" in states:
        return Plan(silent=True)
    params = ()
    if "mobility" in states and kind in PROPULSION:
        # the multiplier first: SIM_ENGINE_FAIL alone would fail the outputs at the old one
        params = (("SIM_ENGINE_MUL", 0.0), ("SIM_ENGINE_FAIL", float(PROPULSION[kind])))
    return Plan(params=params, reports=tuple(REPORTS[s] for s in states if s in REPORTS))


class MavControl:
    """The vehicle's SERIAL2, as its onboard computer (pymavlink, MAVLink 2)."""

    def __init__(self, host: str, instance: int, sysid: int):
        os.environ.setdefault("MAVLINK20", "1")
        from pymavlink import mavutil

        self._mavutil = mavutil
        self.sysid = sysid
        self.values: dict[str, float] = {}
        self.conn = mavutil.mavlink_connection(
            f"tcp:{host}:{control_port(instance)}", source_system=sysid, source_component=COMPONENT,
            retries=0,
        )

    def pump(self) -> None:
        """Drain what the vehicle sends (and forwards) here; keep its parameter echoes."""
        while True:
            m = self.conn.recv_match(blocking=False)
            if m is None:
                return
            if (m.get_type() == "PARAM_VALUE" and m.get_srcSystem() == self.sysid
                    and m.get_srcComponent() == AUTOPILOT_COMPONENT):
                self.values[m.param_id] = float(m.param_value)

    def set_param(self, name: str, value: float) -> None:
        self.conn.mav.param_set_send(self.sysid, AUTOPILOT_COMPONENT, name.encode(), float(value),
                                     self._mavutil.mavlink.MAV_PARAM_TYPE_REAL32)

    def statustext(self, text: str) -> None:
        self.conn.mav.statustext_send(MAV_SEVERITY_CRITICAL, text.encode())

    def close(self) -> None:
        try:
            self.conn.close()
        except OSError:
            pass


@dataclass
class Hurt:
    """One SITL vehicle's damage and how far it has been acted out."""

    vehicle: dict
    states: list = field(default_factory=list)
    reported: tuple = ()  # the states its last report covered
    next_report: float = 0.0
    link: object = None
    confirmed: tuple = ()  # the parameters it has echoed
    silent: bool = False
    failed_at: float = -1e9  # when its control link last failed to open


class Watcher:
    """Follows the damage file and acts out each SITL vehicle's damage.

    `vehicles`: the fleet's SITL vehicles (uid, sysid, instance, kind: sim/swarm.py's VEHICLES
    key, callsign). `freeze(uid)` stops that vehicle's SITL process. `control(vehicle)` opens its
    SERIAL2 (MavControl). `started_ns`: when the fleet started (time.time_ns), against which a
    previous run's file is told apart."""

    RETRY_S = 5.0

    def __init__(self, path, vehicles, freeze, started_ns, host="127.0.0.1", control=None,
                 clock=time.monotonic, log=print):
        self.path = Path(path)
        self.by_uid = {int(v["uid"]): v for v in vehicles}
        self.freeze = freeze
        self.started_ns = started_ns
        self.control = control or (lambda v: MavControl(host, int(v["instance"]), int(v["sysid"])))
        self.clock = clock
        self.log = log
        self.at = None  # where the next read starts (None: the file not looked at yet)
        self.hurt: dict[int, Hurt] = {}

    def poll(self) -> list[tuple[int, str]]:
        """New records for this fleet's SITL vehicles."""
        try:
            st = self.path.stat()
        except OSError:
            if self.at is None:
                self.at = 0
            return []
        if self.at is None:
            # a file not written since the fleet started holds a previous run's records
            self.at = st.st_size if st.st_mtime_ns < self.started_ns else 0
            if self.at:
                self.log(f"damage: {self.path} predates this fleet: its {self.at} bytes are a previous "
                         "run's, skipped")
        if st.st_size < self.at:
            self.at = 0  # emptied (the contacts process starting) or replaced
        if st.st_size == self.at:
            return []
        try:
            data = self.path.read_bytes()
        except OSError:
            return []
        if len(data) < self.at:
            self.at = 0
        records, self.at = read_records(data, self.at)
        new = []
        for uid, state in records:
            v = self.by_uid.get(uid)
            if v is None:
                continue  # a stand-in (its engine acts it out) or not in this fleet
            h = self.hurt.setdefault(uid, Hurt(vehicle=v))
            if state not in h.states:
                h.states.append(state)
                new.append((uid, state))
                self.log(f"damage: {v['callsign']} (uid {uid}, SITL {v['kind']}) {state}")
                if state == "mobility" and v["kind"] not in PROPULSION:
                    self.log(f"damage: {v['callsign']}: SITL cannot take a {v['kind']}'s propulsion; "
                             "it only reports it")
        return new

    def step(self) -> None:
        """Act out every damaged vehicle's plan: go silent, hold the parameters, report."""
        now = self.clock()
        for uid, h in self.hurt.items():
            if h.silent:
                continue
            p = plan(h.vehicle["kind"], h.states)
            if p.silent:
                self._close(h)
                self.freeze(uid)
                h.silent = True
                self.log(f"damage: {h.vehicle['callsign']} destroyed: its SITL process is stopped (silent)")
                continue
            if h.link is None:
                if now - h.failed_at < self.RETRY_S:
                    continue
                try:
                    h.link = self.control(h.vehicle)
                except Exception as e:  # noqa: BLE001 (SITL booting, refused, ...: retry later)
                    h.failed_at = now
                    self.log(f"damage: {h.vehicle['callsign']}: no control link yet ({e}); retrying")
                    continue
            try:
                h.link.pump()
                held = tuple(n for n, val in p.params if abs(h.link.values.get(n, float("nan")) - val) < 0.5)
                for name, value in p.params:
                    if name not in held:
                        h.link.set_param(name, value)
                if held != h.confirmed:
                    h.confirmed = held
                    if p.params and len(held) == len(p.params):
                        self.log(f"damage: {h.vehicle['callsign']}: propulsion lost ("
                                 + ", ".join(f"{n} {v:g}" for n, v in p.params) + ")")
                if tuple(h.states) != h.reported or now >= h.next_report:
                    for text in p.reports:
                        h.link.statustext(text)
                    h.reported = tuple(h.states)
                    h.next_report = now + REPORT_EVERY_S
            except Exception as e:  # noqa: BLE001 (the link broke: open it again)
                self.log(f"damage: {h.vehicle['callsign']}: control link lost ({e}); reopening")
                self._close(h)
                h.failed_at = now

    @staticmethod
    def _close(h: Hurt) -> None:
        if h.link is not None:
            h.link.close()
            h.link = None

    def run(self, stop: threading.Event | None = None, every_s: float = 1.0) -> None:
        stop = stop or threading.Event()
        while not stop.is_set():
            try:
                self.poll()
                self.step()
            except Exception as e:  # noqa: BLE001 (never take the fleet down)
                self.log(f"damage: watcher error: {e}")
            stop.wait(every_s)


def start(vehicles, procs_by_uid, host="127.0.0.1", started_ns=None, path=None, log=print):
    """Follow the damage file for the fleet's SITL `vehicles` in a daemon thread;
    `procs_by_uid`: each one's Popen (started in its own session). Returns the Watcher."""

    def freeze(uid):
        try:
            os.killpg(procs_by_uid[uid].pid, signal.SIGSTOP)
        except (KeyError, ProcessLookupError):
            pass

    w = Watcher(path or damage_path(), vehicles, freeze, started_ns or time.time_ns(), host=host, log=log)
    log(f"damage: following {w.path} for {len(vehicles)} SITL vehicles")
    threading.Thread(target=w.run, name="sitl-damage", daemon=True).start()
    return w
