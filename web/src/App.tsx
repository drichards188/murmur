import { useCallback, useEffect, useState } from 'react'
import { FleetPanel } from './FleetPanel'
import { MapView, type Draft } from './MapView'
import { TaskPanel } from './TaskPanel'
import type { LngLat } from './types'
import { STALE_MS } from './types'
import { useBridge } from './useBridge'

function useUtcClock() {
  const [now, setNow] = useState(() => new Date())
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 1000)
    return () => window.clearInterval(id)
  }, [])
  return now.toISOString().slice(11, 19) + 'Z'
}

export default function App() {
  const { state, status } = useBridge()
  const clock = useUtcClock()
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [draft, setDraft] = useState<Draft>({ mode: 'orbit', center: null, radius: 250, route: [] })

  const vehicles = state?.vehicles ?? []
  const taskings = state?.taskings ?? []
  const lost = vehicles.filter((v) => (v.link.since_last_ms ?? 0) > STALE_MS).length

  const toggle = useCallback((sysid: number) => {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(sysid)) next.delete(sysid)
      else next.add(sysid)
      return next
    })
  }, [])

  const onMapClick = useCallback((p: LngLat) => {
    setDraft((d) => (d.mode === 'orbit' ? { ...d, center: p } : d.mode === 'patrol' ? { ...d, route: [...d.route, p] } : d))
  }, [])

  return (
    <div className="app">
      <header className="topbar">
        <span className="brand">MURMUR</span>
        <span className="sub">air · surface swarm C2</span>
        <span className="spacer" />
        <span className="stat">
          <b>{vehicles.filter((v) => v.kind === 'plane').length}</b> air
        </span>
        <span className="stat">
          <b>{vehicles.filter((v) => v.kind !== 'plane').length}</b> surface
        </span>
        <span className={`stat ${lost ? 'alert' : ''}`}>
          <b>{lost}</b> lost link
        </span>
        <span className={`pill ${status}`}>{status === 'live' ? 'BRIDGE LIVE' : status === 'down' ? 'BRIDGE DOWN' : 'CONNECTING'}</span>
        <span className="clock">{clock}</span>
      </header>
      <FleetPanel vehicles={vehicles} selected={selected} onToggle={toggle} onSelect={(ids) => setSelected(new Set(ids))} />
      <main className="mapwrap">
        <MapView vehicles={vehicles} taskings={taskings} selected={selected} draft={draft} onToggle={toggle} onMapClick={onMapClick} />
        {status !== 'live' && (
          <div className="banner">Bridge not reachable at /ws. Start it with <code>make bridge</code>. Retrying…</div>
        )}
      </main>
      <TaskPanel vehicles={vehicles} selected={selected} draft={draft} setDraft={setDraft} taskings={taskings} />
    </div>
  )
}
