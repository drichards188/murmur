#!/usr/bin/env python3
"""Measure a running Murmur console in a real browser: frame rate, frame time, long tasks,
work per bridge update, whether it keeps up with the feed, heap and DOM, beside the bridge's
CPU and memory and the host's load. Starts nothing: point it at a Chrome or Chromium that has
a remote-debugging port and at a console that is already up (docs/load-test.md, "Console with
the camera moving, on a real GPU", for how the published numbers were taken).

    # a browser on the GPU with its own throwaway profile, e.g. Chrome on Windows from WSL:
    chrome.exe --user-data-dir='C:\\Temp\\cs-profile' --remote-debugging-port=9334 \\
      --window-size=1920,1080 --app=about:blank &
    .venv/bin/python scripts/console_stress.py baseline --cdp 9334
    .venv/bin/python scripts/console_stress.py run --cdp 9334 --console http://localhost:5273 \\
      --bridge-port 8865 --label 1000 --views move3d fleet3d zoom3d --warmup 32 --window 32 \\
      --out logs/stress.jsonl
    .venv/bin/python scripts/console_stress.py table logs/stress.jsonl

`baseline` reports the WebGL renderer (UNMASKED_RENDERER_WEBGL: a real GPU names the GPU,
software names SwiftShader or llvmpipe), the display's frame rate on an empty page, and the
frame rate of a trivial full-screen WebGL page. `run` opens the console with `?perf`, sets each
view, waits `--warmup` seconds, then measures for `--window` seconds:

  move3d    what a watch officer does, the same 32 s lap at every fleet size: pan 14 km across
            the Bay at zoom 12.6, zoom out to the whole Bay (past the cluster switch) and turn,
            back in to the Golden Gate, orbit 120 degrees, tilt up and down, back to the city
  swarm3d   following a swarm up close (zoom 14.5, --swarm-center): orbit it, pan across it,
            out to the whole Bay and back in, tilt, orbit back; a 32 s lap
  fleet3d   still: the whole fleet in view, tilted 50 degrees, the camera the console opens
            at (a view holding more than 800 vehicles is drawn as clusters)
  zoom3d    still: San Francisco at zoom 12.6, tilted 50 degrees, full-detail symbols

Each leg of a moving view is one MapLibre ease (its default ease-in-out): a move event every
frame and a moveend at the end of the leg, as a person's drags and the console's own flights
give; a jumpTo every frame would fire a moveend every frame and overstate the cost. Use a
window of one lap (32 s). The console is 3D only (its top view is a pitch of 0 on the same
map, not measured here). With no bridge answering, `run` measures the console with no
vehicles (the map, its terrain and the panels alone).

Frames are every requestAnimationFrame interval in the window (the console's own ring keeps
only the last 240). Per update numbers (merge, render, latency) are the console's `?perf`
probes (web/src/perf.ts). Keeping up: the bridge offers one delta per 250 ms tick and skips
ticks for a client still busy with the last (a watch channel, crates/murmur-bridge/src/api.rs),
so distinct ticks received per second against 4 is the share it handles (while the camera
moves the bridge also answers each view message with a delta of the same tick, so raw deltas
per second run above 4); "lag growth" is how much
later, against the bridge's own clock in each delta, the last deltas of the window arrived
than the first ones (near 0 when it keeps up; the bridge's clock is steered toward the wall
clock at up to 5 %, so +-1 s in 30 s is noise). Heap is measured after a forced GC at the end
of each window (CDP Runtime.getHeapUsage), never during it. Browser CPU is per Chrome process
type (CDP SystemInfo.getProcessInfo). The camera is driven through `window.murmurMap`, which
the console exposes under `?perf`.

Needs the `websockets` package (in Murmur's .venv).
"""

import argparse
import asyncio
import json
import math
import os
import re
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import websockets

SF = [-122.43, 37.78]
ZOOM_CITY = 12.6
PITCH_3D = 50

INIT_JS = r"""
(() => {
  if (window.__cs) return
  const cs = (window.__cs = { armed: false, frames: [], long: [], legNow: null, msgs: [], cam: null, sent: 0 })
  let last = 0
  const tick = (t) => {
    if (cs.armed && last) { cs.frames.push(t - last); if (t - last > 100) cs.long.push([Math.round(t - last), cs.legNow]) }
    last = t
    if (cs.cam) { try { cs.cam(t) } catch (e) { cs.camError = String(e) } }
    requestAnimationFrame(tick)
  }
  requestAnimationFrame(tick)
  const WS = window.WebSocket
  window.WebSocket = class extends WS {
    constructor(...a) {
      super(...a)
      this.addEventListener('message', (ev) => {
        if (!cs.armed || typeof ev.data !== 'string') return
        const head = ev.data.slice(0, 96)
        const ty = /"type":"(\w+)"/.exec(head)
        const t = /"t":(-?[0-9.]+(?:e[-+]?\d+)?)/.exec(head)
        cs.msgs.push([performance.timeOrigin + performance.now(), ty ? ty[1] : '?', t ? +t[1] : null, ev.data.length])
      })
    }
    send(d) { if (cs.armed) cs.sent++; return super.send(d) }
  }
})()
"""

GL_INFO_JS = r"""
(() => { const c = document.createElement('canvas'); const gl = c.getContext('webgl2') || c.getContext('webgl')
  if (!gl) return { error: 'no WebGL' }
  const e = gl.getExtension('WEBGL_debug_renderer_info')
  return { vendor: e ? gl.getParameter(e.UNMASKED_VENDOR_WEBGL) : null, renderer: e ? gl.getParameter(e.UNMASKED_RENDERER_WEBGL) : null,
    version: gl.getParameter(gl.VERSION), dpr: devicePixelRatio, viewport: [innerWidth, innerHeight], screen: [screen.width, screen.height],
    userAgent: navigator.userAgent } })()
"""

# a trivial WebGL page: one full-screen triangle pair, a cheap fragment shader, drawn every frame
GL_PAGE = """<!doctype html><title>gl</title><style>html,body{margin:0;height:100%;overflow:hidden}canvas{width:100vw;height:100vh;display:block}</style>
<canvas id=c></canvas><script>
const c=document.getElementById('c');c.width=innerWidth*devicePixelRatio;c.height=innerHeight*devicePixelRatio;
const gl=c.getContext('webgl2');const s=(t,src)=>{const x=gl.createShader(t);gl.shaderSource(x,src);gl.compileShader(x);return x};
const p=gl.createProgram();gl.attachShader(p,s(gl.VERTEX_SHADER,'#version 300 es\\nin vec2 a;void main(){gl_Position=vec4(a,0,1);}'));
gl.attachShader(p,s(gl.FRAGMENT_SHADER,'#version 300 es\\nprecision highp float;uniform float t;out vec4 o;void main(){vec2 u=gl_FragCoord.xy/400.;o=vec4(.5+.5*sin(u.x+t),.5+.5*sin(u.y+t*1.3),.5,1);}'));
gl.linkProgram(p);gl.useProgram(p);const b=gl.createBuffer();gl.bindBuffer(gl.ARRAY_BUFFER,b);
gl.bufferData(gl.ARRAY_BUFFER,new Float32Array([-1,-1,1,-1,-1,1,-1,1,1,-1,1,1]),gl.STATIC_DRAW);
gl.enableVertexAttribArray(0);gl.vertexAttribPointer(0,2,gl.FLOAT,false,0,0);const ut=gl.getUniformLocation(p,'t');
const f=(t)=>{gl.uniform1f(ut,t/1000);gl.drawArrays(gl.TRIANGLES,0,6);requestAnimationFrame(f)};requestAnimationFrame(f);
</script>"""


class CDP:
    """A minimal Chrome DevTools Protocol client over one WebSocket."""

    def __init__(self, url: str):
        self.url = url
        self.ws = None
        self.n = 0
        self.pending: dict[int, asyncio.Future] = {}
        self.reader = None
        self.events: asyncio.Queue = asyncio.Queue()

    async def __aenter__(self):
        self.ws = await websockets.connect(self.url, max_size=None, ping_interval=None)
        self.reader = asyncio.create_task(self._read())
        return self

    async def __aexit__(self, *exc):
        self.reader.cancel()
        await self.ws.close()

    async def _read(self):
        async for raw in self.ws:
            m = json.loads(raw)
            if "method" in m and "id" not in m:
                self.events.put_nowait(m)
                continue
            f = self.pending.pop(m.get("id"), None)
            if f and not f.done():
                f.set_result(m)

    async def send(self, method: str, timeout: float = 60, **params):
        self.n += 1
        f = asyncio.get_running_loop().create_future()
        self.pending[self.n] = f
        await self.ws.send(json.dumps({"id": self.n, "method": method, "params": params}))
        m = await asyncio.wait_for(f, timeout)
        if "error" in m:
            raise RuntimeError(f"{method}: {m['error']}")
        return m.get("result", {})

    async def js(self, expr: str, timeout: float = 60):
        r = await self.send("Runtime.evaluate", timeout=timeout, expression=expr, returnByValue=True, awaitPromise=True)
        if "exceptionDetails" in r:
            raise RuntimeError(f"page error: {r['exceptionDetails'].get('exception', {}).get('description', r['exceptionDetails'])}")
        return r["result"].get("value")


def endpoints(host: str, port: int):
    """(page websocket, browser websocket): the first ordinary page, made if there is none."""
    base = f"http://{host}:{port}"
    tabs = json.load(urllib.request.urlopen(f"{base}/json/list"))
    pages = [t for t in tabs if t["type"] == "page" and not t["url"].startswith("devtools://")]
    if not pages:
        req = urllib.request.Request(f"{base}/json/new?about:blank", method="PUT")
        pages = [json.load(urllib.request.urlopen(req))]
    browser = json.load(urllib.request.urlopen(f"{base}/json/version"))["webSocketDebuggerUrl"]
    return pages[0]["webSocketDebuggerUrl"], browser


async def navigate(page: CDP, url: str, settle: float = 0):
    await page.send("Page.navigate", url=url)
    for _ in range(600):
        if await page.js("document.readyState") == "complete":
            break
        await asyncio.sleep(0.1)
    await asyncio.sleep(settle)


async def frames_for(page: CDP, seconds: float):
    await page.js("window.__cs.frames = []; window.__cs.armed = true; true")
    await asyncio.sleep(seconds)
    return await page.js("window.__cs.armed = false; window.__cs.frames")


def frame_stats(fr: list[float], seconds: float):
    if not fr:
        return {"fps": 0, "frames": 0}
    s = sorted(fr)
    q = lambda p: s[min(len(s) - 1, int(p * len(s)))]  # noqa: E731
    return {"fps": round(len(fr) / seconds, 1), "frames": len(fr), "mean_ms": round(sum(fr) / len(fr), 2),
            "p50_ms": round(q(0.5), 2), "p95_ms": round(q(0.95), 2), "p99_ms": round(q(0.99), 2),
            "max_ms": round(s[-1], 1), "over_50ms": sum(1 for x in fr if x > 50), "over_100ms": sum(1 for x in fr if x > 100)}


# ---- host side: the bridge's process, Chrome's processes, the load

def pid_on_port(port: int) -> int | None:
    out = subprocess.run(["ss", "-ltnpH", f"sport = :{port}"], capture_output=True, text=True).stdout
    m = re.search(r"pid=(\d+)", out)
    return int(m.group(1)) if m else None


def proc_cpu_s(pid: int) -> float:
    f = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")


def proc_rss_mb(pid: int) -> float:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith("VmRSS:"):
            return int(line.split()[1]) / 1024
    return 0.0


NVIDIA_SMI = next((p for p in ("/usr/lib/wsl/lib/nvidia-smi", "/usr/bin/nvidia-smi") if Path(p).exists()), None)


class GpuSampler:
    """GPU utilization and memory once a second over a window (nvidia-smi, when there is one)."""

    def __init__(self):
        self.util: list[float] = []
        self.mem: list[float] = []
        self.task = None

    async def _loop(self):
        while True:
            try:
                p = await asyncio.create_subprocess_exec(NVIDIA_SMI, "--query-gpu=utilization.gpu,memory.used",
                                                         "--format=csv,noheader,nounits", stdout=asyncio.subprocess.PIPE)
                out, _ = await p.communicate()
                u, m = out.decode().splitlines()[0].split(",")
                self.util.append(float(u))
                self.mem.append(float(m))
            except (OSError, ValueError, IndexError):
                pass
            await asyncio.sleep(1)

    def start(self):
        if NVIDIA_SMI:
            self.task = asyncio.create_task(self._loop())
        return self

    def stop(self):
        if self.task:
            self.task.cancel()
        if not self.util:
            return None
        return {"util_mean_pct": round(sum(self.util) / len(self.util)), "util_max_pct": round(max(self.util)),
                "mem_max_mib": round(max(self.mem)), "samples": len(self.util)}


def loadavg():
    return [float(x) for x in Path("/proc/loadavg").read_text().split()[:3]]


async def chrome_cpu(browser: CDP) -> dict[str, float]:
    """Cumulative CPU seconds per Chrome process type (browser, renderer, GPU, utility)."""
    try:
        info = await browser.send("SystemInfo.getProcessInfo")
    except RuntimeError:
        return {}
    out: dict[str, float] = {}
    for p in info.get("processInfo", []):
        out[p["type"]] = out.get(p["type"], 0.0) + p["cpuTime"]
    return out


# ---- the console's camera

def camera_js(view: str, bounds, swarm=None) -> str:
    if view == "fleet3d":  # the camera the console opens at: the fit to the fleet, tilted 50 degrees
        return (f"(() => {{ const m = window.murmurMap; m.stop(); m.jumpTo({{ bearing: 0, pitch: {PITCH_3D} }});"
                f" m.fitBounds({json.dumps(bounds)}, {{ padding: 120, maxZoom: 13.5, duration: 0, pitch: {PITCH_3D}, bearing: 0 }});"
                " return m.getZoom() })()")
    if view == "zoom3d":
        return (f"(() => {{ const m = window.murmurMap; m.stop(); m.jumpTo({{ center: {json.dumps(SF)}, zoom: {ZOOM_CITY},"
                f" pitch: {PITCH_3D}, bearing: 0 }}); return m.getZoom() }})()")
    if view in PATHS:
        return path_js(PATHS[view](swarm))
    raise ValueError(view)


# The moving views: the same legs at every fleet size, so runs compare. Each leg is one ease
# (MapLibre's default ease-in-out, as its own flights and a person's hand move it): a move event
# every frame and a moveend at the end of the leg. One lap is PATH_S seconds; a window is one lap.
BAY = [-122.42, 37.80]  # the view of the whole Bay that the officer zooms out to (cluster mode)
GOLDEN_GATE = [-122.4783, 37.8199]
OAKLAND = [-122.27, 37.80]
PATH_S = 32


def officer_path(_swarm):
    """What a watch officer does over the Bay: pan across it, zoom out to the whole of it (past
    the cluster switch) and back in, orbit, tilt."""
    return [
        dict(center=SF, zoom=ZOOM_CITY, bearing=0, pitch=50, duration=1),  # start of the lap
        dict(center=OAKLAND, zoom=ZOOM_CITY, bearing=0, pitch=50, duration=6000),  # pan 14 km across the Bay
        dict(center=BAY, zoom=9.2, bearing=0, pitch=45, duration=4000),  # out to the whole Bay: clusters
        dict(center=BAY, zoom=9.4, bearing=30, pitch=50, duration=3000),  # a slow turn there
        dict(center=GOLDEN_GATE, zoom=ZOOM_CITY, bearing=30, pitch=50, duration=4000),  # back in, the Golden Gate
        dict(center=GOLDEN_GATE, zoom=ZOOM_CITY, bearing=150, pitch=55, duration=6000),  # orbit 120 degrees
        dict(center=GOLDEN_GATE, zoom=ZOOM_CITY, bearing=150, pitch=72, duration=2500),  # tilt up
        dict(center=GOLDEN_GATE, zoom=ZOOM_CITY, bearing=150, pitch=40, duration=2500),  # and down
        dict(center=SF, zoom=ZOOM_CITY, bearing=0, pitch=50, duration=3999),  # back to the start
    ]


def swarm_path(swarm):
    """Following a swarm up close: orbit it, pan across it, out to the whole Bay and back, tilt."""
    c = swarm or GOLDEN_GATE
    kx = 1 / (111320 * math.cos(math.radians(c[1])))
    east = [c[0] + 600 * kx, c[1]]
    return [
        dict(center=c, zoom=14.5, bearing=0, pitch=55, duration=1),
        dict(center=c, zoom=14.5, bearing=170, pitch=55, duration=8000),  # orbit half round it
        dict(center=east, zoom=15, bearing=170, pitch=60, duration=3000),  # follow across it
        dict(center=c, zoom=14.5, bearing=170, pitch=55, duration=2000),
        dict(center=BAY, zoom=10, bearing=170, pitch=45, duration=4000),  # out to the Bay
        dict(center=c, zoom=14.5, bearing=170, pitch=55, duration=4000),  # and back in
        dict(center=c, zoom=14.5, bearing=170, pitch=72, duration=2500),  # tilt up
        dict(center=c, zoom=14.5, bearing=170, pitch=42, duration=2500),  # and down
        dict(center=c, zoom=14.5, bearing=0, pitch=55, duration=5999),  # orbit back
    ]


PATHS = {"move3d": officer_path, "swarm3d": swarm_path}


def path_js(legs) -> str:
    """Run the legs in a loop, each started when the last ends."""
    return (f"(() => {{ const m = window.murmurMap; m.stop(); const legs = {json.dumps(legs)}; let i = 0;"
            " const c = window.__cs; c.moving = true;"
            " const go = () => { if (!c.moving) return; c.legNow = i % legs.length; const L = legs[i++ % legs.length];"
            "  if (L.duration <= 1) { m.jumpTo(L); setTimeout(go, 0); return }"
            "  m.once('moveend', () => setTimeout(go, 0)); m.easeTo(L) };"
            " go(); return m.getZoom() })()")


# A/B switches applied to the live map after it loads (--tweak), for diagnosis only: each is
# what a one-line change in the console would do, without rebuilding it.
TWEAKS = {
    "dpr1": "(() => { window.murmurMap.setPixelRatio(1); return window.murmurMap.getPixelRatio() })()",
    "pitch70": "(() => { window.murmurMap.setMaxPitch(70); return window.murmurMap.getMaxPitch() })()",
    "nohillshade": "(() => { const m = window.murmurMap; const ids = m.getStyle().layers.filter(l => l.type === 'hillshade')"
                   ".map(l => l.id); ids.forEach(id => m.setLayoutProperty(id, 'visibility', 'none')); return ids })()",
    "noterrain": "(() => { window.murmurMap.setTerrain(null); return !window.murmurMap.getTerrain() })()",
    "noscale": "(() => { const m = window.murmurMap; const s = m._controls.filter(c => c.constructor.name === 'ScaleControl'"
               " || (c._container && c._container.classList && c._container.classList.contains('maplibregl-ctrl-scale')));"
               " s.forEach(c => m.removeControl(c)); return s.length })()",
    "nosmooth": "(() => { const b = [...document.querySelectorAll('button')].find(b => /smooth/i.test(b.title || ''));"
                " if (b && b.getAttribute('aria-pressed') !== 'false') b.click(); return b ? b.getAttribute('aria-pressed') : 'no button' })()",
}

STOP_CAM = "(() => { const c = window.__cs; c.cam = null; c.moving = false; if (window.murmurMap) window.murmurMap.stop(); return true })()"


BAY_BOUNDS = [[-123.05, 37.45], [-122.0, 38.1]]  # the scale fleets' extent, for a console with no bridge


def fleet_bounds(bridge: str):
    try:
        st = json.load(urllib.request.urlopen(f"{bridge}/api/state", timeout=30))
    except OSError:
        print(f"note: no bridge at {bridge}: measuring the console with no vehicles", file=sys.stderr)
        return BAY_BOUNDS, 0
    pts = [v["position"] for v in st["vehicles"] if v.get("position")]
    if not pts:
        return BAY_BOUNDS, len(st["vehicles"])
    return [[min(p[0] for p in pts), min(p[1] for p in pts)], [max(p[0] for p in pts), max(p[1] for p in pts)]], len(st["vehicles"])


async def measure_window(page: CDP, browser: CDP, bridge_pid: int | None, seconds: float):
    cpu0 = proc_cpu_s(bridge_pid) if bridge_pid else None
    ch0 = await chrome_cpu(browser)
    load0 = loadavg()
    await page.js("window.murmurPerfReset(); window.__cs.msgs = []; window.__cs.frames = []; window.__cs.long = []; window.__cs.sent = 0;"
                  " window.__cs.armed = true; true")
    t0 = time.monotonic()
    gpu = GpuSampler().start()
    await asyncio.sleep(seconds)
    gpu_stats = gpu.stop()
    raw = await page.js("(() => { const c = window.__cs; c.armed = false;"
                        " return { frames: c.frames, long: c.long, msgs: c.msgs, sent: c.sent, perf: window.murmurPerfStats(),"
                        " dom: document.getElementsByTagName('*').length, camError: c.camError || null,"
                        " viewport: [innerWidth, innerHeight], dpr: devicePixelRatio,"
                        " buffer: (() => { const g = window.murmurMap && window.murmurMap.getCanvas(); return g ? [g.width, g.height] : null })() } })()")
    el = time.monotonic() - t0
    cpu1 = proc_cpu_s(bridge_pid) if bridge_pid else None
    ch1 = await chrome_cpu(browser)
    rss = proc_rss_mb(bridge_pid) if bridge_pid else None
    perf = raw["perf"]
    msgs = raw["msgs"]
    deltas = [m for m in msgs if m[1] == "delta" and m[2] is not None]
    lag_growth = None
    if len(deltas) >= 12:
        off = [m[0] - m[2] * 1000 for m in deltas]
        k = max(3, len(off) // 6)
        lag_growth = round(statistics.median(off[-k:]) - statistics.median(off[:k]))
    span = (deltas[-1][2] - deltas[0][2]) if len(deltas) > 1 else 0
    # forced GC, then the heap (after the window, so it touches no frame)
    await page.send("HeapProfiler.enable")
    await page.send("HeapProfiler.collectGarbage")
    heap = await page.send("Runtime.getHeapUsage")
    pct = lambda a, b, k: round(100 * (b.get(k, 0) - a.get(k, 0)) / el, 1) if k in b else None  # noqa: E731
    return {
        "window_s": round(el, 1),
        "frame": frame_stats(raw["frames"], el),
        "long_tasks": perf["longTasks"], "long_task_ms_per_s": round(perf["longTaskMsPerS"], 1),
        "merge_ms": round(perf["applyDelta"]["mean"], 2),
        "render_ms": round(perf["render"]["mean"], 1), "render_p95_ms": round(perf["render"]["p95"], 1),
        "renders": perf["render"]["n"],
        "latency_ms": round(perf["latency"]["mean"]), "latency_p95_ms": round(perf["latency"]["p95"]),
        "deck_attrs_ms": round(perf["attrs"]["mean"], 2), "deck_cpu_ms": round(perf["deckCpu"]["mean"], 2),
        "weather_frame_ms": round(perf["weatherFrame"]["mean"], 2) if perf["weatherFrame"]["n"] else None,
        "deltas_per_s": round(len(deltas) / el, 2), "ticks_per_s": round(len({m[2] for m in deltas}) / el, 2),
        "msgs": {k: sum(1 for m in msgs if m[1] == k) for k in sorted({m[1] for m in msgs})},
        "kb_per_s": round(sum(m[3] for m in msgs) / el / 1024, 1),
        "lag_growth_ms": lag_growth, "views_sent": raw["sent"], "long_frames_by_leg": raw["long"],
        "heap_mb_after_gc": round(heap["usedSize"] / 2**20, 1),
        "heap_mb_live": round(perf["heapMB"], 1) if perf.get("heapMB") else None,
        "dom": raw["dom"],
        "bridge_cpu_pct": round(100 * (cpu1 - cpu0) / el, 1) if cpu0 is not None else None,
        "bridge_rss_mb": round(rss) if rss is not None else None,
        "chrome_cpu_pct": {k: pct(ch0, ch1, k) for k in ch1},
        "load": [load0, loadavg()],
        "cam_error": raw["camError"], "gpu": gpu_stats,
        "viewport": raw["viewport"], "dpr": raw["dpr"], "buffer": raw["buffer"],
    }


async def set_window(page: CDP, fullscreen: bool):
    """Full screen (the display's whole size at its own scale), or back to a normal window."""
    try:
        w = (await page.send("Browser.getWindowForTarget"))["windowId"]
        state = (await page.send("Browser.getWindowBounds", windowId=w))["bounds"].get("windowState")
        want = "fullscreen" if fullscreen else "normal"
        if state != want:
            await page.send("Browser.setWindowBounds", windowId=w, bounds={"windowState": want})
            await asyncio.sleep(2)
    except RuntimeError as e:
        print(f"note: window state not set ({e})", file=sys.stderr)


async def run(a):
    page_url, browser_url = endpoints(a.cdp_host, a.cdp)
    bridge = f"http://127.0.0.1:{a.bridge_port}"
    bridge_pid = pid_on_port(a.bridge_port)
    if not bridge_pid:
        print(f"note: no process found listening on :{a.bridge_port}; bridge CPU and RSS not recorded", file=sys.stderr)
    bounds, n = fleet_bounds(bridge)
    async with CDP(page_url) as page, CDP(browser_url) as browser:
        await page.send("Page.enable")
        await page.send("Runtime.enable")
        await page.send("Page.bringToFront")
        await set_window(page, a.fullscreen)
        await page.send("Page.addScriptToEvaluateOnNewDocument", source=INIT_JS)
        info = await page.js(GL_INFO_JS)
        if a.weather != "keep":
            fx = {k: a.weather == "on" for k in ("wind", "fog", "rain", "cloud")}
            await navigate(page, a.console)  # same origin, so localStorage is the console's
            await page.js(f"localStorage.setItem('murmur.weather.effects', {json.dumps(json.dumps(fx))}); true")
        await navigate(page, a.console.rstrip("/") + "/?perf", settle=2)
        for _ in range(120):
            if await page.js("!!window.murmurMap && typeof window.murmurPerfStats === 'function'"):
                break
            await asyncio.sleep(0.5)
        else:
            sys.exit("the console never exposed window.murmurMap: is it a build with this script's ?perf hook?")
        await asyncio.sleep(a.load_wait)
        for tw in a.tweak:
            print(f"[{a.label}] tweak {tw}: {await page.js(TWEAKS[tw])}", file=sys.stderr)
        out = Path(a.out) if a.out else None
        for view in a.views:
            await page.js(STOP_CAM)
            zoom = await page.js(camera_js(view, bounds, a.swarm_center))
            print(f"[{a.label}] {view}: zoom {zoom:.2f}, warming up {a.warmup:.0f} s", file=sys.stderr)
            await asyncio.sleep(a.warmup)
            if a.profile:
                p = await profile_window(page, browser, a.profile, f"{a.label}-{view}")
                rec = {"label": a.label, "nodes": n, "view": view, "profile": True, "tweak": a.tweak, **p}
                print(json.dumps(rec, indent=1), flush=True)
                if out:
                    with out.open("a") as f:
                        f.write(json.dumps(rec) + "\n")
                await page.js(STOP_CAM)
                continue
            for i in range(a.repeat):  # back to back, a soak's samples: the camera keeps moving between them
                r = await measure_window(page, browser, bridge_pid, a.window)
                rec = {"label": a.label, "nodes": n, "view": view, "weather": a.weather, "zoom": round(zoom, 2),
                       "sample": i if a.repeat > 1 else None, "renderer": info.get("renderer"),
                       "fullscreen": a.fullscreen, "tweak": a.tweak, "at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), **r}
                print(json.dumps(rec), flush=True)
                if out:
                    with out.open("a") as f:
                        f.write(json.dumps(rec) + "\n")
            await page.js(STOP_CAM)


TRACE_CATEGORIES = ["toplevel", "devtools.timeline", "disabled-by-default-devtools.timeline.frame", "gpu", "viz",
                    "disabled-by-default-gpu.service", "v8.execute", "blink"]


async def read_stream(page: CDP, handle: str) -> str:
    parts = []
    while True:
        r = await page.send("IO.read", handle=handle, size=1 << 20)
        parts.append(r["data"])
        if r.get("eof"):
            break
    await page.send("IO.close", handle=handle)
    return "".join(parts)


def cpu_summary(prof, top: int):
    """Self and inclusive ms per function (name@file:line:col, 1-based line) in a CPU profile."""
    nodes = {n["id"]: n for n in prof["nodes"]}
    parent = {c: n["id"] for n in prof["nodes"] for c in n.get("children", [])}
    dt: dict[int, float] = {}
    for sid, d in zip(prof["samples"], prof["timeDeltas"]):
        dt[sid] = dt.get(sid, 0) + d / 1000

    def key(nid):
        cf = nodes[nid]["callFrame"]
        return f"{cf['functionName'] or '(anon)'}@{cf['url'].rsplit('/', 1)[-1]}:{cf['lineNumber'] + 1}:{cf['columnNumber'] + 1}"

    self_ms: dict[str, float] = {}
    inc: dict[str, float] = {}
    for nid, ms in dt.items():
        self_ms[key(nid)] = self_ms.get(key(nid), 0) + ms
        seen, p = set(), nid
        while p is not None:
            k = key(p)
            if k not in seen:
                inc[k] = inc.get(k, 0) + ms
                seen.add(k)
            p = parent.get(p)
    best = lambda d: [[k, round(v)] for k, v in sorted(d.items(), key=lambda kv: -kv[1])[:top]]  # noqa: E731
    return {"total_ms": round(sum(dt.values())), "self": best(self_ms), "inclusive": best(inc)}


def trace_summary(events, seconds: float, frames: int, top: int):
    """Busy ms per frame on the renderer main thread, its compositor, and the GPU process's main
    and viz threads (top-level tasks), and the longest-running event names on each."""
    pname = {e["pid"]: e["args"]["name"] for e in events if e.get("ph") == "M" and e.get("name") == "process_name"}
    tname = {(e["pid"], e["tid"]): e["args"]["name"] for e in events if e.get("ph") == "M" and e.get("name") == "thread_name"}
    want = {("Renderer", "CrRendererMain"): "renderer main", ("Renderer", "Compositor"): "renderer compositor",
            ("GPU Process", "CrGpuMain"): "GPU main", ("GPU Process", "VizCompositorThread"): "GPU viz"}
    out = {}
    for (pid, tid), tn in tname.items():
        label = want.get((pname.get(pid), tn))
        if not label:
            continue
        ev = [e for e in events if e.get("pid") == pid and e.get("tid") == tid and e.get("ph") == "X" and "dur" in e]
        busy = sum(e["dur"] for e in ev if e.get("name") in ("ThreadControllerImpl::RunTask", "RunTask")) / 1000
        names: dict[str, float] = {}
        for e in ev:
            if e.get("name") not in ("ThreadControllerImpl::RunTask", "RunTask"):
                names[e["name"]] = names.get(e["name"], 0) + e["dur"] / 1000
        prev = out.get(label)
        if prev and prev["busy_ms_per_s"] >= busy / seconds:
            continue  # several renderers (other tabs): keep the busiest
        out[label] = {"busy_ms_per_s": round(busy / seconds), "busy_ms_per_frame": round(busy / max(frames, 1), 1),
                      "top_events_ms_per_frame": [[n, round(v / max(frames, 1), 2)]
                                                  for n, v in sorted(names.items(), key=lambda kv: -kv[1])[:top]]}
    return out


async def profile_window(page: CDP, browser: CDP, seconds: float, label: str, top: int = 25):
    """A CPU profile of the page and a trace of the browser over the same seconds."""
    await page.send("Profiler.enable")
    await page.send("Profiler.setSamplingInterval", interval=250)
    await browser.send("Tracing.start", categories=",".join(TRACE_CATEGORIES), transferMode="ReturnAsStream")
    await page.send("Profiler.start")
    await page.js("window.__cs.frames = []; window.__cs.armed = true; true")
    gpu = GpuSampler().start()
    await asyncio.sleep(seconds)
    gpu_stats = gpu.stop()
    fr = await page.js("window.__cs.armed = false; window.__cs.frames")
    prof = (await page.send("Profiler.stop", timeout=180))["profile"]
    await browser.send("Tracing.end")
    while True:
        ev = await asyncio.wait_for(browser.events.get(), 300)
        if ev["method"] == "Tracing.tracingComplete":
            break
    raw = json.loads(await read_stream(browser, ev["params"]["stream"]))
    events = raw["traceEvents"] if isinstance(raw, dict) else raw
    stem = Path("logs") / f"trace-{label}-{time.strftime('%H%M%S')}"
    Path(f"{stem}.cpuprofile").write_text(json.dumps(prof))
    Path(f"{stem}.json").write_text(json.dumps(events))
    cpu = cpu_summary(prof, top)
    n = len(fr)
    per_frame = lambda rows: [[k, round(v / max(n, 1), 2)] for k, v in rows]  # noqa: E731
    return {"frame": frame_stats(fr, seconds), "cpu_total_ms_per_frame": round(cpu["total_ms"] / max(n, 1), 1),
            "js_self_ms_per_frame": per_frame(cpu["self"]), "js_inclusive_ms_per_frame": per_frame(cpu["inclusive"]),
            "gpu": gpu_stats, "threads": trace_summary(events, seconds, n, 12), "files": [f"{stem}.cpuprofile", f"{stem}.json"]}


CARD_JS = r"""
((lines) => { const d = document.createElement('div'); d.id = '__cs_card'
  d.style.cssText = 'position:fixed;inset:0;z-index:2147483647;display:flex;flex-direction:column;justify-content:center;' +
    'align-items:center;gap:14px;background:rgba(8,12,18,.92);color:#e8eef5;font:500 22px/1.4 "IBM Plex Mono",ui-monospace,monospace;text-align:center;padding:40px'
  lines.forEach((t, i) => { const p = document.createElement('div'); p.textContent = t
    if (i === 0) p.style.cssText = 'font-size:40px;font-weight:700;margin-bottom:12px'; d.appendChild(p) })
  document.body.appendChild(d); return true })
"""


def video_legs(kind: str, bounds, swarm):
    """The clip's camera: fleet-wide, a smooth zoom in, a pan, an orbit with a pitch change, then
    one lap of the measured moving path (so the clip shows what the table measured)."""
    if kind == "swarm":
        c = swarm or GOLDEN_GATE
        intro = [dict(center=BAY, zoom=9.6, bearing=0, pitch=50, duration=1),
                 dict(center=BAY, zoom=9.6, bearing=0, pitch=50, duration=4000),  # the whole Bay: a cluster where the swarm is
                 dict(center=c, zoom=14.5, bearing=0, pitch=55, duration=6000)]  # in to the swarm
        return intro, swarm_path(swarm)
    intro = [dict(center=BAY, zoom=9.2, bearing=0, pitch=50, duration=1),
             dict(center=BAY, zoom=9.3, bearing=0, pitch=50, duration=4000),  # fleet-wide, clusters
             dict(center=SF, zoom=ZOOM_CITY, bearing=0, pitch=50, duration=6000),  # a smooth zoom to San Francisco
             dict(center=OAKLAND, zoom=ZOOM_CITY, bearing=0, pitch=50, duration=6000),  # a pan across the Bay
             dict(center=OAKLAND, zoom=ZOOM_CITY, bearing=140, pitch=68, duration=7000),  # an orbit with a pitch change
             dict(center=SF, zoom=ZOOM_CITY, bearing=0, pitch=50, duration=5000)]
    return intro, officer_path(swarm)


async def video(a):
    """Record the console with CDP's screencast (the browser encodes each frame as a JPEG; the
    only capture this driver has for a browser it does not share a display with), frames and
    their times to a directory, for `record_window.py --frames` to encode. Also measures the
    frame rate during the recording, so the overhead is known."""
    page_url, browser_url = endpoints(a.cdp_host, a.cdp)
    bounds, n = fleet_bounds(f"http://127.0.0.1:{a.bridge_port}")
    outdir = Path(a.frames)
    outdir.mkdir(parents=True, exist_ok=True)
    async with CDP(page_url) as page, CDP(browser_url) as browser:
        for m in ("Page.enable", "Runtime.enable", "Page.bringToFront"):
            await page.send(m)
        await set_window(page, a.fullscreen)
        await page.send("Page.addScriptToEvaluateOnNewDocument", source=INIT_JS)
        if a.weather != "keep":
            fx = {k: a.weather == "on" for k in ("wind", "fog", "rain", "cloud")}
            await navigate(page, a.console)
            await page.js(f"localStorage.setItem('murmur.weather.effects', {json.dumps(json.dumps(fx))}); true")
        await navigate(page, a.console.rstrip("/") + "/?perf", settle=2)
        for _ in range(120):
            if await page.js("!!window.murmurMap"):
                break
            await asyncio.sleep(0.5)
        info = await page.js(GL_INFO_JS)
        buf = await page.js("(() => { const g = window.murmurMap.getCanvas(); return [g.width, g.height] })()")
        intro, lap = video_legs(a.kind, bounds, a.swarm_center)
        await page.js(path_js(intro[:1]))
        await page.js(STOP_CAM)
        await asyncio.sleep(a.load_wait)
        card = [a.title or f"Murmur console: {n:,} simulated vehicles", f"WebGL renderer: {info['renderer']}",
                f"viewport {info['viewport'][0]}x{info['viewport'][1]} CSS px at device pixel ratio {info['dpr']}; "
                f"map drawing buffer {buf[0]}x{buf[1]}",
                f"{info['userAgent'].split(') ')[0].split('(')[1]}; display {info['screen'][0]}x{info['screen'][1]}, 60 Hz",
                "the ?perf overlay (bottom) shows frames per second, p95 frame time and update latency",
                time.strftime("%Y-%m-%d")]
        await page.js(f"({CARD_JS})({json.dumps(card)})")
        await page.send("Page.startScreencast", format="jpeg", quality=a.quality, maxWidth=a.width, maxHeight=a.height, everyNthFrame=1)
        frames: list[list] = []
        stop = asyncio.Event()

        async def pump():
            while not stop.is_set():
                try:
                    ev = await asyncio.wait_for(page.events.get(), 1)
                except asyncio.TimeoutError:
                    continue
                if ev["method"] != "Page.screencastFrame":
                    continue
                p = ev["params"]
                await page.send("Page.screencastFrameAck", sessionId=p["sessionId"])
                f = outdir / f"{len(frames):06d}.jpg"
                f.write_bytes(__import__("base64").b64decode(p["data"]))
                frames.append([p["metadata"]["timestamp"], f.name])

        task = asyncio.create_task(pump())
        await asyncio.sleep(a.card_s)
        await page.js("document.getElementById('__cs_card').remove(); true")
        await page.js("window.murmurPerfReset(); window.__cs.frames = []; window.__cs.armed = true; true")
        t0 = time.monotonic()
        legs = intro + lap
        await page.js(path_js(legs).replace("legs[i++ % legs.length]", "legs[Math.min(i++, legs.length - 1)]")
                      .replace("if (!c.moving) return;", "if (!c.moving || i >= legs.length) return;"))
        total = sum(L["duration"] for L in legs) / 1000
        await asyncio.sleep(total + 1)
        el = time.monotonic() - t0
        fr = await page.js("window.__cs.armed = false; window.__cs.frames")
        perf = await page.js("window.murmurPerfStats()")
        await page.send("Page.stopScreencast")
        stop.set()
        await task
        await page.js(STOP_CAM)
    meta = {"frames": frames, "nodes": n, "renderer": info["renderer"], "viewport": info["viewport"], "dpr": info["dpr"],
            "buffer": buf, "fps_during_recording": frame_stats(fr, el),
            "latency_ms": round(perf["latency"]["mean"]), "capture": "CDP Page.startScreencast (JPEG, browser-side)",
            "kind": a.kind, "fullscreen": a.fullscreen}
    (outdir / "frames.json").write_text(json.dumps(meta))
    span = (frames[-1][0] - frames[0][0]) if len(frames) > 1 else 0
    print(json.dumps({k: v for k, v in meta.items() if k != "frames"} | {"captured": len(frames),
                      "capture_fps": round(len(frames) / span, 1) if span else None}, indent=1))


async def baseline(a):
    page_url, _ = endpoints(a.cdp_host, a.cdp)
    async with CDP(page_url) as page:
        await page.send("Page.enable")
        await page.send("Runtime.enable")
        await page.send("Page.bringToFront")
        await page.send("Page.addScriptToEvaluateOnNewDocument", source=INIT_JS)
        await navigate(page, "data:text/html,<title>blank</title>", settle=2)
        info = await page.js(GL_INFO_JS)
        blank = frame_stats(await frames_for(page, a.window), a.window)
        await navigate(page, "data:text/html;base64," + __import__("base64").b64encode(GL_PAGE.encode()).decode(), settle=2)
        gl = frame_stats(await frames_for(page, a.window), a.window)
        rec = {"baseline": True, **info, "blank_page": blank, "webgl_page": gl, "load": loadavg()}
        soft = any(s in (info.get("renderer") or "") for s in ("SwiftShader", "llvmpipe", "softpipe"))
        print(json.dumps(rec, indent=1))
        if soft:
            print("SOFTWARE RENDERER: frame rates measured here are not a GPU's", file=sys.stderr)
        if a.out:
            with open(a.out, "a") as f:
                f.write(json.dumps(rec) + "\n")


def table(a):
    rows = [json.loads(line) for p in a.files for line in Path(p).read_text().splitlines() if line.strip()]
    base = [r for r in rows if r.get("baseline")]
    for b in base:
        print(f"renderer: {b['renderer']}; viewport {b['viewport'][0]}x{b['viewport'][1]} at DPR {b['dpr']}; "
              f"empty page {b['blank_page']['fps']} fps, trivial WebGL {b['webgl_page']['fps']} fps")
    cols = [("nodes", lambda r: f"{r['nodes']:,}"),
            ("view", lambda r: r["view"] + ("" if r["weather"] == "keep" else f" wx {r['weather']}")
             + ("" if r.get("sample") is None else f" #{r['sample'] + 1}")),
            ("viewport @ DPR (buffer)", lambda r: f"{r['viewport'][0]}x{r['viewport'][1]} @ {r['dpr']}"
             + (f" ({r['buffer'][0]}x{r['buffer'][1]})" if r.get("buffer") else "")),
            ("fps", lambda r: r["frame"]["fps"]), ("p95 frame", lambda r: r["frame"].get("p95_ms")),
            (">50ms", lambda r: r["frame"].get("over_50ms")), ("long tasks", lambda r: f"{r['long_tasks']} ({r['long_task_ms_per_s']} ms/s)"),
            ("merge", lambda r: r["merge_ms"]), ("render (p95)", lambda r: f"{r['render_ms']} ({r['render_p95_ms']})"),
            ("latency (p95)", lambda r: f"{r['latency_ms']} ({r['latency_p95_ms']})"),
            ("ticks/s (of 4)", lambda r: r.get("ticks_per_s", r["deltas_per_s"])), ("lag growth", lambda r: r["lag_growth_ms"]),
            ("heap MB", lambda r: r["heap_mb_after_gc"]), ("DOM", lambda r: r["dom"]),
            ("bridge CPU", lambda r: r["bridge_cpu_pct"]), ("bridge RSS", lambda r: r["bridge_rss_mb"]),
            ("GPU proc CPU", lambda r: r["chrome_cpu_pct"].get("GPU")), ("renderer CPU", lambda r: r["chrome_cpu_pct"].get("renderer")),
            ("GPU util mean/max %", lambda r: f"{r['gpu']['util_mean_pct']}/{r['gpu']['util_max_pct']}" if r.get("gpu") else ""),
            (">100ms", lambda r: r["frame"].get("over_100ms")),
            ("load", lambda r: r["load"][1][0])]
    print("| " + " | ".join(c for c, _ in cols) + " |")
    print("|" + "---|" * len(cols))
    for r in rows:
        if r.get("baseline"):
            continue
        print("| " + " | ".join(str(f(r)) for _, f in cols) + " |")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("run", "baseline", "video"):
        p = sub.add_parser(name)
        p.add_argument("--cdp", type=int, default=9333, help="the browser's remote-debugging port")
        p.add_argument("--cdp-host", default="127.0.0.1")
        p.add_argument("--window", type=float, default=30, help="seconds measured per view")
        p.add_argument("--out", help="append each result as a JSON line here")
    for name in ("run", "video"):
        r = sub.choices[name]
        r.add_argument("--console", default="http://localhost:5273")
        r.add_argument("--bridge-port", type=int, default=8865)
        r.add_argument("--swarm-center", type=lambda s: [float(x) for x in s.split(",")], default=None,
                       help="LON,LAT the swarm3d path circles (default the Golden Gate's mid-span)")
        r.add_argument("--fullscreen", action="store_true", help="full screen (else a normal window, as it is)")
        r.add_argument("--load-wait", type=float, default=5, help="seconds after the console loads, before the first view")
        r.add_argument("--weather", choices=["keep", "on", "off"], default="keep",
                       help="set the weather layer's effects (all on or all off) before loading; keep leaves the viewer's")
    v = sub.choices["video"]
    v.add_argument("--frames", default="logs/video-frames", help="directory for the JPEG frames and frames.json")
    v.add_argument("--kind", choices=["bay", "swarm"], default="bay")
    v.add_argument("--title", help="the title card's first line")
    v.add_argument("--card-s", type=float, default=5)
    v.add_argument("--quality", type=int, default=88)
    v.add_argument("--width", type=int, default=1920)
    v.add_argument("--height", type=int, default=1080)
    r = sub.choices["run"]
    r.add_argument("--label", default="")
    r.add_argument("--views", nargs="+", default=["move3d", "fleet3d", "zoom3d"],
                   choices=["fleet3d", "zoom3d", "move3d", "swarm3d"])
    r.add_argument("--warmup", type=float, default=25)
    r.add_argument("--profile", type=float, default=0, metavar="SECONDS",
                   help="instead of measuring, take a CPU profile and a Chrome trace for this long after the warm-up "
                        "(raw files in logs/trace-*)")
    r.add_argument("--tweak", nargs="*", default=[], choices=sorted(TWEAKS),
                   help="A/B switches on the live map, for diagnosis (see TWEAKS)")
    r.add_argument("--repeat", type=int, default=1,
                   help="windows measured back to back per view (a soak: --views move3d --window 60 --repeat 10)")
    t = sub.add_parser("table")
    t.add_argument("files", nargs="+")
    a = ap.parse_args()
    if a.cmd == "table":
        table(a)
    else:
        asyncio.run({"run": run, "baseline": baseline, "video": video}[a.cmd](a))


if __name__ == "__main__":
    main()
