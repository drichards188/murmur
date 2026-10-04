"""Port blocks for isolated stacks, reserved with lock files.

Each stack takes one block: 100 vehicle UDP ports, 100 bridge UDP ports, one HTTP port for
the bridge, one for the link emulator's stats, one for the console (study mode only).
Ranges: vehicle bases 35000-35999, bridge bases 36000-36999 (inside the 35000-38999 the
evaluation was given); bridge HTTP 10600-10609, link emulator stats 10620-10629, console
10640-10649; a batch's own `tidewater serve` on one of 10690-10699 (all inside 10600-10699,
the evaluation's HTTP range). Clear of the default stack (14560+, 15560+, 8765, 8766,
5173, 8770).

A block is handed out only when (1) its lock file can be locked (`flock`, exclusive, held
for the run's life, so two batches, or a batch and a single run, never share a block), (2)
none of its UDP ranges reaches into the host's ephemeral port range (read now from
`/proc/sys/net/ipv4/ip_local_port_range`: a fixed base inside it collides with clients'
random ports), (3) `ss` shows none of its ports in use, and (4) every one of its ports can be
bound right now (UDP and TCP).
"""

from __future__ import annotations

import fcntl
import os
import socket
import subprocess
import threading
from dataclasses import dataclass
from pathlib import Path

VEHICLE_BASE = 35000
BRIDGE_BASE = 36000
BLOCK = 100
MAX_BLOCKS = 10
HTTP_BASE = 10600
EMU_BASE = 10620
WEB_BASE = 10640
TIDEWATER_PORTS = range(10690, 10700)
LOCK_DIR = Path(os.environ.get("MURMUR_EVAL_LOCK_DIR", "/tmp/murmur-eval-locks"))


@dataclass(frozen=True)
class PortBlock:
    index: int

    @property
    def vehicle_base(self) -> int:
        return VEHICLE_BASE + self.index * BLOCK

    @property
    def bridge_base(self) -> int:
        return BRIDGE_BASE + self.index * BLOCK

    @property
    def http(self) -> int:
        return HTTP_BASE + self.index

    @property
    def emu(self) -> int:
        return EMU_BASE + self.index

    @property
    def web(self) -> int:
        return WEB_BASE + self.index

    def udp_ports(self) -> range:
        return range(self.vehicle_base, self.vehicle_base + BLOCK)

    def bridge_udp_ports(self) -> range:
        return range(self.bridge_base, self.bridge_base + BLOCK)

    def tcp_ports(self) -> tuple[int, ...]:
        return (self.http, self.emu, self.web)

    def as_dict(self) -> dict[str, int]:
        return {
            "index": self.index,
            "vehicle_base": self.vehicle_base,
            "bridge_base": self.bridge_base,
            "http": self.http,
            "emu": self.emu,
            "web": self.web,
        }


def parse_ss(text: str) -> set[int]:
    """Local ports in `ss -lun` / `ss -ltn` output (columns: State Recv-Q Send-Q
    Local:Port Peer:Port, the same for both)."""
    out: set[int] = set()
    for line in text.splitlines()[1:]:
        parts = line.split()
        if len(parts) < 4:
            continue
        port = parts[3].rsplit(":", 1)[-1]
        if port.isdigit():
            out.add(int(port))
    return out


def listening_ports() -> tuple[set[int], set[int]]:
    """(udp, tcp) ports something listens on, from `ss`; empty sets if `ss` is missing."""
    found: list[set[int]] = []
    for flag in ("-lun", "-ltn"):
        try:
            text = subprocess.run(["ss", flag], capture_output=True, text=True, timeout=5).stdout
        except (OSError, subprocess.TimeoutExpired):
            text = ""
        found.append(parse_ss(text))
    return found[0], found[1]


def ephemeral_range() -> tuple[int, int] | None:
    """The host's ephemeral port range, read now; None if it can't be read."""
    try:
        lo, hi = Path("/proc/sys/net/ipv4/ip_local_port_range").read_text().split()[:2]
        return int(lo), int(hi)
    except (OSError, ValueError):
        return None


def overlaps_ephemeral(ports: range, eph: tuple[int, int] | None) -> bool:
    if eph is None:
        return False
    lo, hi = eph
    return not (ports.stop - 1 < lo or ports.start > hi)


def _bindable(port: int, kind: int) -> bool:
    """Can a server bind it now? TCP with SO_REUSEADDR, as the bridge and the emulator do,
    so a previous run's connections in TIME_WAIT don't count as taken; UDP without it, on
    every address, so any socket already on the port makes it fail."""
    s = socket.socket(socket.AF_INET, kind)
    try:
        if kind == socket.SOCK_STREAM:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("0.0.0.0", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def why_not_free(b: PortBlock, udp: set[int] | None = None, tcp: set[int] | None = None, eph: tuple[int, int] | None = None) -> str | None:
    """Why the block can't be used now, or None if it can (the lock aside)."""
    eph = ephemeral_range() if eph is None else eph
    for r in (b.udp_ports(), b.bridge_udp_ports()):
        if overlaps_ephemeral(r, eph):
            return f"UDP {r.start}-{r.stop - 1} overlaps the ephemeral range {eph}"
    if udp is None or tcp is None:
        udp, tcp = listening_ports()
    for p in list(b.udp_ports()) + list(b.bridge_udp_ports()):
        if p in udp:
            return f"UDP {p} in use"
    for p in b.tcp_ports():
        if p in tcp:
            return f"TCP {p} in use"
    for p in list(b.udp_ports()) + list(b.bridge_udp_ports()):
        if not _bindable(p, socket.SOCK_DGRAM):
            return f"UDP {p} cannot be bound"
    for p in b.tcp_ports():
        if not _bindable(p, socket.SOCK_STREAM):
            return f"TCP {p} cannot be bound"
    return None


def block_is_free(b: PortBlock, udp: set[int] | None = None, tcp: set[int] | None = None) -> bool:
    return why_not_free(b, udp, tcp) is None


class Lease:
    """An exclusive `flock` on a lock file, held until `release` (or the process ends: the
    kernel drops it then, so a crashed runner never leaves a block reserved)."""

    def __init__(self, name: str):
        LOCK_DIR.mkdir(parents=True, exist_ok=True)
        self.path = LOCK_DIR / f"{name}.lock"
        self.fd: int | None = os.open(self.path, os.O_CREAT | os.O_RDWR, 0o644)

    def try_lock(self) -> bool:
        assert self.fd is not None
        try:
            fcntl.flock(self.fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            os.close(self.fd)
            self.fd = None
            return False
        os.ftruncate(self.fd, 0)
        os.write(self.fd, f"{os.getpid()}\n".encode())
        return True

    def release(self) -> None:
        if self.fd is not None:
            try:
                fcntl.flock(self.fd, fcntl.LOCK_UN)
            finally:
                os.close(self.fd)
                self.fd = None


@dataclass
class BlockLease:
    block: PortBlock
    lease: Lease | None

    def release(self) -> None:
        if self.lease is not None:
            self.lease.release()


class Allocator:
    """Hands out free blocks, each under its lock file, never the same one twice until
    released (thread-safe: the batch allocates from several worker threads)."""

    def __init__(self, max_blocks: int = MAX_BLOCKS):
        self.max_blocks = max_blocks
        self.held: dict[int, BlockLease] = {}
        self._lock = threading.Lock()
        self.last_refusals: dict[int, str] = {}

    def allocate(self, want: int | None = None) -> BlockLease:
        with self._lock:
            udp, tcp = listening_ports()
            eph = ephemeral_range()
            candidates = [want] if want is not None else list(range(self.max_blocks))
            self.last_refusals = {}
            for i in candidates:
                if i in self.held:
                    self.last_refusals[i] = "held by this process"
                    continue
                lease = Lease(f"block-{i}")
                if not lease.try_lock():
                    self.last_refusals[i] = "reserved by another runner"
                    continue
                b = PortBlock(i)
                why = why_not_free(b, udp, tcp, eph)
                if why is not None:
                    lease.release()
                    self.last_refusals[i] = why
                    continue
                bl = BlockLease(b, lease)
                self.held[i] = bl
                return bl
        raise RuntimeError(f"no free port block: {self.last_refusals}; stop other stacks or lower --parallel")

    def release(self, bl: BlockLease) -> None:
        with self._lock:
            self.held.pop(bl.block.index, None)
            bl.release()

    def free_count(self) -> int:
        udp, tcp = listening_ports()
        eph = ephemeral_range()
        n = 0
        for i in range(self.max_blocks):
            if i in self.held:
                continue
            lease = Lease(f"block-{i}")
            if lease.try_lock():
                if why_not_free(PortBlock(i), udp, tcp, eph) is None:
                    n += 1
                lease.release()
        return n


def lease_tidewater_port() -> tuple[int, Lease] | None:
    """A port for a batch's own `tidewater serve`, under its lock file, free now."""
    for port in TIDEWATER_PORTS:
        lease = Lease(f"tidewater-{port}")
        if not lease.try_lock():
            continue
        if _bindable(port, socket.SOCK_STREAM):
            return port, lease
        lease.release()
    return None
