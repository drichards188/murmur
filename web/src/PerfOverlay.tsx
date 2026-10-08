import { useEffect, useState } from 'react'
import { perfStats } from './perf'

const f1 = (n: number) => n.toFixed(1)

/** ?perf in the URL: frame rate, DOM size, JS heap and the cost of each bridge update
 *  (see perf.ts), refreshed every second. For checking 1,000+ nodes on a real GPU. */
export function PerfOverlay() {
  const [s, setS] = useState(() => perfStats())
  useEffect(() => {
    const id = window.setInterval(() => setS(perfStats()), 1000)
    return () => window.clearInterval(id)
  }, [])
  return (
    <div className="perf" aria-label="Performance (?perf)">
      <b>{Math.round(s.fps)}</b> fps <span className="muted">p95 frame {Math.round(s.frame.p95)} ms</span>
      <br />
      DOM <b>{s.dom.toLocaleString()}</b> · heap <b>{s.heapMB == null ? 'n/a' : `${Math.round(s.heapMB)} MB`}</b>
      <br />
      update: merge <b>{f1(s.applyDelta.mean)}</b> ms · render <b>{f1(s.render.mean)}</b> ms{' '}
      <span className="muted">(p95 {f1(s.render.p95)}, latency {Math.round(s.latency.mean)})</span>
      <br />
      deck: attrs <b>{f1(s.attrs.mean)}</b> ms <span className="muted">(p95 {f1(s.attrs.p95)}, n {s.attrs.n}; glide frames and 4 Hz updates)</span> · cpu <b>{f1(s.deckCpu.mean)}</b> ms
      <br />
      long tasks <b>{Math.round(s.longTaskMsPerS)}</b> ms/s
    </div>
  )
}
