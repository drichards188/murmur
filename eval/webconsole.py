"""The console's web server under the launcher (eval/launcher.py): one server on MURMUR_WEB_PORT,
either the production build (`vite build`, then `vite preview`: the default; React's development
build costs about 20 times the render time at a few thousand vehicles) or the development server
(hot reload, for people changing the console). Both proxy /api, /ws and /launcher the same way
(web/vite.config.ts: `preview.proxy` is `server.proxy`).

The built console is never stale: a build stamp (a hash of the path, size and modification time
of every file `vite build` reads: web/src, public, index.html, the package and tsconfig files,
vite.config.ts) is written beside the build (web/dist/.murmur-stamp) once it succeeds, and the
console is built whenever the sources' stamp differs from it (or there is no build).

The choice is kept in logs/launcher/console-mode.json (else MURMUR_WEB_MODE=build|dev, else
build). A switch is refused while a scenario is under way (the page it would restart is the one
showing it), and otherwise builds first with the old server still serving (the menu shows the
build and its time), then swaps servers: the page reloads itself when it sees the new `generation`.
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Callable

from . import REPO
from .situation import tcp_answers

MODES = ("build", "dev")
WEB = REPO / "web"
DIST = WEB / "dist"
STAMP = DIST / ".murmur-stamp"
SOURCES = ("src", "public", "index.html", "package.json", "package-lock.json", "vite.config.ts",
           "tsconfig.json", "tsconfig.app.json", "tsconfig.node.json")
BUILD_TIMEOUT_S = 600


def source_stamp(root: Path = WEB) -> str:
    """A hash of what `vite build` reads: each file's path, size and modification time."""
    h = hashlib.sha256()
    files: list[Path] = []
    for name in SOURCES:
        p = root / name
        if p.is_dir():
            files += [f for f in p.rglob("*") if f.is_file()]
        elif p.is_file():
            files.append(p)
    for f in sorted(files):
        st = f.stat()
        h.update(f"{f.relative_to(root)}\0{st.st_size}\0{st.st_mtime_ns}\n".encode())
    return h.hexdigest()


def read_stamp(path: Path = STAMP) -> str | None:
    try:
        return path.read_text().strip() or None
    except OSError:
        return None


def built_is_current(root: Path = WEB) -> bool:
    return (root / "dist" / "index.html").is_file() and read_stamp(root / "dist" / ".murmur-stamp") == source_stamp(root)


def default_mode(pref_file: Path) -> str:
    try:
        m = json.loads(pref_file.read_text()).get("mode")
        if m in MODES:
            return m
    except (OSError, ValueError, AttributeError):
        pass
    env = os.environ.get("MURMUR_WEB_MODE", "")
    return env if env in MODES else "build"


class WebConsole:
    def __init__(self, port: int, api_port: int, log_dir: Path, busy: Callable[[], bool], note: Callable[[str], None]):
        self.port = port
        self.api_port = api_port
        self.log_dir = log_dir
        self.pref = log_dir / "console-mode.json"
        self.busy = busy  # whether a scenario is under way
        self.note = note
        self.lock = threading.RLock()
        self.mode = default_mode(self.pref)  # the preference
        self.serving: str | None = None  # the mode the running server is
        self.proc: subprocess.Popen | None = None
        self.generation = 0
        self.working: str | None = None  # "building" or "restarting" while a switch runs
        self.build: dict[str, Any] = {"state": "none", "seconds": None, "at": None, "error": None, "started": None}
        self.last_error: str | None = None

    # ---- the state the menu shows
    def public(self) -> dict[str, Any]:
        with self.lock:
            stale = None
            if self.serving == "build" or self.mode == "build":
                try:
                    stale = not built_is_current()
                except OSError:
                    stale = True
            return {"mode": self.mode, "serving": self.serving, "working": self.working, "generation": self.generation,
                    "build": dict(self.build), "stale": stale, "error": self.last_error}

    # ---- processes
    def _env(self) -> dict[str, str]:
        from .launcher import MAKE_VARS  # the launcher's own list of make variables to drop

        return {
            **{k: v for k, v in os.environ.items() if k not in MAKE_VARS},
            "MURMUR_BRIDGE_URL": f"http://127.0.0.1:{self.api_port + 1}",
            "MURMUR_LAUNCHER_URL": f"http://127.0.0.1:{self.api_port}",
        }

    def _vite(self) -> Path:
        vite = WEB / "node_modules" / ".bin" / "vite"
        if not vite.exists():
            raise RuntimeError("no web/node_modules/.bin/vite: run `make setup` (npm install in web/)")
        return vite

    def _stop(self) -> None:
        p = self.proc
        self.proc = None
        self.serving = None
        if p is None or p.poll() is not None:
            return
        p.terminate()
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait(timeout=5)

    def _serve(self, mode: str) -> None:
        args = [str(self._vite())] + (["preview"] if mode == "build" else []) + ["--host", "127.0.0.1", "--port", str(self.port), "--strictPort"]
        with (self.log_dir / "web.log").open("a") as f:
            f.write(f"--- serving the {'production build' if mode == 'build' else 'development server'} ---\n")
            f.flush()
            proc = subprocess.Popen(args, cwd=str(WEB), env=self._env(), stdout=f, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + 60
        while not tcp_answers(self.port):
            if proc.poll() is not None or time.monotonic() > deadline:
                if proc.poll() is None:
                    proc.kill()
                raise RuntimeError(f"the console did not start on :{self.port}; see logs/launcher/web.log")
            time.sleep(0.2)
        self.proc = proc
        self.serving = mode
        self.generation += 1

    def _build(self) -> None:
        """vite build when the sources' stamp is not the build's; records how long it took."""
        stamp = source_stamp()
        if (DIST / "index.html").is_file() and read_stamp() == stamp:
            return
        with self.lock:
            self.working = "building"
            self.build = {**self.build, "state": "building", "started": time.time(), "error": None}
        self.note("building the console (vite build)")
        t0 = time.monotonic()
        with (self.log_dir / "web-build.log").open("w") as f:
            try:
                r = subprocess.run([str(self._vite()), "build", "--logLevel", "warn"], cwd=str(WEB), env=self._env(), stdout=f,
                                   stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, timeout=BUILD_TIMEOUT_S)
                code = r.returncode
            except subprocess.TimeoutExpired:
                code = -1
        secs = round(time.monotonic() - t0, 1)
        if code != 0:
            err = f"vite build failed (exit {code}); see logs/launcher/web-build.log"
            with self.lock:
                self.build = {**self.build, "state": "failed", "seconds": secs, "error": err}
            raise RuntimeError(err)
        STAMP.write_text(stamp + "\n")
        with self.lock:
            self.build = {"state": "done", "seconds": secs, "at": time.time(), "error": None, "started": None}
        self.note(f"the console is built ({secs} s)")

    # ---- switching
    def apply(self, mode: str, first: bool = False) -> None:
        """Make the server `mode`: build first (old server still up), then swap. Raises on failure."""
        try:
            if mode == "build":
                self._build()
            self.working = "restarting"
            if not first:
                self._stop()
            self._serve(mode)
            self.last_error = None
        except Exception as e:  # noqa: BLE001 (said in the menu)
            self.last_error = str(e)
            self.note(f"console: {e}")
            if self.serving is None and not first:
                try:  # never leave the page without a server: back to a development one
                    self._serve("dev")
                except Exception:  # noqa: BLE001
                    pass
            raise
        finally:
            with self.lock:
                self.working = None

    def start(self) -> None:
        self.apply(self.mode, first=True)

    def request(self, mode: Any) -> tuple[int, dict[str, Any]]:
        if mode not in MODES:
            return 400, {"detail": f"mode is one of {', '.join(MODES)}"}
        with self.lock:
            if self.working:
                return 409, {"detail": f"the console is {self.working}", "console": self.public()}
            if self.busy():
                return 409, {"detail": "a scenario is starting or running: end it before changing how the console is served", "console": self.public()}
            if mode == self.serving and not (mode == "build" and not built_is_current()):
                self.mode = mode
                self._save()
                return 200, {"console": self.public()}
            self.mode = mode
            self._save()
            self.working = "building" if mode == "build" else "restarting"
        threading.Thread(target=self._switch, args=(mode,), name="console-switch", daemon=True).start()
        return 202, {"console": self.public()}

    def _switch(self, mode: str) -> None:
        try:
            self.apply(mode)
        except Exception:  # noqa: BLE001 (kept in last_error)
            pass

    def _save(self) -> None:
        try:
            self.pref.write_text(json.dumps({"mode": self.mode}) + "\n")
        except OSError:
            pass

    def alive(self) -> bool:
        return self.working is not None or self.proc is None or self.proc.poll() is None

    def close(self) -> None:
        with self.lock:
            self._stop()
