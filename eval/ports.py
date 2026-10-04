"""Port blocks for isolated stacks.

Each stack takes one block: 100 vehicle UDP ports, 100 bridge UDP ports, one HTTP port for
the bridge, one for the link emulator's stats, one for the console (study mode only).
Ranges: vehicle bases 35000-35999, bridge bases 36000-36999 (inside the 35000-38999 the
evaluation was given, and below this machine's ephemeral range, 37795+ here, so a fixed
base never collides with a client's random port); bridge HTTP 10600-10609, link emulator
stats 10620-10629, console 10640-10649, a shared `tidewater serve` 10699 (all inside
10600-10699, the evaluation's HTTP range). Clear of the default stack (14560+, 15560+,
8765, 8766, 5173, 8770).

A block is used only if none of its ports is already bound (`ss`), and the batch never
hands the same block to two runs at once.
"""

from __future__ import annotations

import socket
import subprocess
import threading
from dataclasses import dataclass

VEHICLE_BASE = 35000
BRIDGE_BASE = 36000
BLOCK = 100
MAX_BLOCKS = 10
HTTP_BASE = 10600
EMU_BASE = 10620
WEB_BASE = 10640
TIDEWATER_PORT = 10699


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


def _bindable(port: int, kind: int) -> bool:
    """Can a server bind it? With SO_REUSEADDR, as the bridge and the emulator do, so a
    previous run's connections lingering in TIME_WAIT don't count as taken."""
    s = socket.socket(socket.AF_INET, kind)
    try:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("127.0.0.1", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def block_is_free(b: PortBlock, udp: set[int] | None = None, tcp: set[int] | None = None) -> bool:
    if udp is None or tcp is None:
        udp, tcp = listening_ports()
    if any(p in udp for p in b.udp_ports()) or any(p in udp for p in b.bridge_udp_ports()):
        return False
    if any(p in tcp for p in b.tcp_ports()):
        return False
    # ss can miss sockets in other namespaces: bind the TCP ports once to be sure
    return all(_bindable(p, socket.SOCK_STREAM) for p in b.tcp_ports())


class Allocator:
    """Hands out free blocks, never the same one twice until released (thread-safe: the
    batch allocates from several worker threads)."""

    def __init__(self, max_blocks: int = MAX_BLOCKS):
        self.max_blocks = max_blocks
        self.taken: set[int] = set()
        self._lock = threading.Lock()

    def allocate(self, want: int | None = None) -> PortBlock:
        with self._lock:
            udp, tcp = listening_ports()
            candidates = [want] if want is not None else list(range(self.max_blocks))
            for i in candidates:
                if i in self.taken:
                    continue
                b = PortBlock(i)
                if block_is_free(b, udp, tcp):
                    self.taken.add(i)
                    return b
        raise RuntimeError(
            f"no free port block (tried {candidates}); stop other stacks or lower --parallel"
        )

    def release(self, b: PortBlock) -> None:
        with self._lock:
            self.taken.discard(b.index)

    def free_count(self) -> int:
        udp, tcp = listening_ports()
        return sum(1 for i in range(self.max_blocks) if i not in self.taken and block_is_free(PortBlock(i), udp, tcp))
