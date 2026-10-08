// Console performance probes (?perf). Cheap enough to leave on: a few performance.now()
// calls per WebSocket update, written into fixed-size ring buffers (no allocation per sample).
//
//   applyDelta  merging one bridge message into the state (useBridge)
//   render      main-thread work for one state change: React render and commit (App's render
//               to its layout effect) plus the passive effects it caused, which is where the
//               map and the list do their work (first PerfMark effect to App's last effect)
//   latency     from the message to the end of those effects, including any wait in the
//               event loop behind other work (frames, other messages)
//   frame       interval between animation frames (fps on a real GPU)
//   attrs       deck.gl's own attribute updates per frame, summed over layers (its "Update Attributes" stat), which
//               is where the cost of display smoothing lands: every glide frame recomputes the
//               positions of every vehicle in the attached layers. Not visible in the JS the
//               glide loop itself runs (advance and setProps), which deck.gl defers to its render
//   deckCpu     deck.gl's CPU time per frame ("CPU Time" stat from luma.gl)
//
// window.murmurPerfStats() returns a snapshot; the ?perf overlay shows the same numbers.

const N = 240

class Ring {
  private buf = new Float64Array(N)
  private n = 0
  private i = 0
  push(v: number) {
    this.buf[this.i] = v
    this.i = (this.i + 1) % N
    if (this.n < N) this.n++
  }
  stats() {
    if (!this.n) return { n: 0, mean: 0, p50: 0, p95: 0, max: 0 }
    const a = Array.from(this.buf.subarray(0, this.n)).sort((x, y) => x - y)
    const at = (q: number) => a[Math.min(a.length - 1, Math.floor(q * a.length))]
    return { n: this.n, mean: a.reduce((s, x) => s + x, 0) / a.length, p50: at(0.5), p95: at(0.95), max: a[a.length - 1] }
  }
  reset() {
    this.n = 0
    this.i = 0
  }
}

const rings = { applyDelta: new Ring(), render: new Ring(), latency: new Ring(), frame: new Ring(), attrs: new Ring(), deckCpu: new Ring() }
export type PerfName = keyof typeof rings

export function perfPush(name: PerfName, ms: number) {
  rings[name].push(ms)
}

// the update in flight: set when a message changes state, closed by App's last effect
let pendingSince: number | null = null
let renderStart: number | null = null
let commitMs = 0
let passiveStart: number | null = null
export function perfUpdateStart() {
  pendingSince ??= performance.now()
}
/** App's render body, for a pending update */
export function perfRenderBegin() {
  if (pendingSince != null && renderStart == null) renderStart = performance.now()
}
/** App's layout effect: render and commit are done */
export function perfCommitted() {
  if (renderStart == null) return
  commitMs = performance.now() - renderStart
  renderStart = null
}
/** the first passive effect of the tree (PerfMark) */
export function perfPassiveBegin() {
  if (pendingSince != null) passiveStart = performance.now()
}
export function perfUpdateEnd() {
  if (pendingSince == null) return
  const now = performance.now()
  rings.latency.push(now - pendingSince)
  if (passiveStart != null) rings.render.push(commitMs + (now - passiveStart))
  pendingSince = null
  passiveStart = null
  commitMs = 0
}

// long tasks (> 50 ms on the main thread), where the browser reports them
let longMs = 0
let longCount = 0
const since = { t: performance.now() }
try {
  new PerformanceObserver((list) => {
    for (const e of list.getEntries()) {
      longMs += e.duration
      longCount++
    }
  }).observe({ type: 'longtask', buffered: false })
} catch {
  // not supported (Firefox, Safari): the overlay shows "n/a"
}

// deck.gl's stats, sampled every animation frame (MapView registers where to find them)
interface StatLike {
  lastTiming: number
  /** accumulated ms since deck.gl last reset its stats (every 60 frames) */
  time: number
}
let deckStats: (() => { get(name: string): StatLike } | null) | null = null
export function perfWatchDeck(get: () => { get(name: string): StatLike } | null) {
  deckStats = get
}
let attrTime = 0
let lastCpu = ''
function sampleDeck() {
  try {
    const st = deckStats?.()
    if (!st) return
    // attribute updates add up over the layers of a frame: the sum since the last sample
    const a = st.get('Update Attributes')
    if (a.time < attrTime) attrTime = 0 // deck.gl reset its stats
    if (a.time > attrTime) rings.attrs.push(a.time - attrTime)
    attrTime = a.time
    const c = st.get('CPU Time')
    const ck = `${c.time}:${c.lastTiming}`
    if (ck !== lastCpu) {
      lastCpu = ck
      if (c.lastTiming > 0) rings.deckCpu.push(c.lastTiming)
    }
  } catch {
    // stats not there in this deck.gl: the overlay shows zeros
  }
}

let rafStarted = false
function startFrames() {
  if (rafStarted) return
  rafStarted = true
  let last = performance.now()
  const tick = (t: number) => {
    rings.frame.push(t - last)
    sampleDeck()
    last = t
    requestAnimationFrame(tick)
  }
  requestAnimationFrame(tick)
}

export function perfStats() {
  startFrames()
  const mem = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory
  const secs = (performance.now() - since.t) / 1000
  const frame = rings.frame.stats()
  return {
    dom: document.getElementsByTagName('*').length,
    heapMB: mem ? mem.usedJSHeapSize / 2 ** 20 : null,
    fps: frame.mean ? 1000 / frame.mean : 0,
    frame,
    applyDelta: rings.applyDelta.stats(),
    render: rings.render.stats(),
    latency: rings.latency.stats(),
    attrs: rings.attrs.stats(),
    deckCpu: rings.deckCpu.stats(),
    longTaskMsPerS: secs > 0 ? longMs / secs : 0,
    longTasks: longCount,
    windowS: secs,
  }
}

export function perfReset() {
  for (const r of Object.values(rings)) r.reset()
  longMs = 0
  longCount = 0
  since.t = performance.now()
}

export const PERF_ON = typeof location !== 'undefined' && new URLSearchParams(location.search).has('perf')

if (typeof window !== 'undefined') {
  const w = window as unknown as { murmurPerfStats: typeof perfStats; murmurPerfReset: typeof perfReset }
  w.murmurPerfStats = perfStats
  w.murmurPerfReset = perfReset
  if (PERF_ON) startFrames()
}
