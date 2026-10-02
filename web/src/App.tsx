import { useCallback, useEffect, useMemo, useState } from 'react'
import { FleetPanel } from './FleetPanel'
import { GlobeInset } from './GlobeInset'
import { MapView, type Draft } from './MapView'
import { PlatformsPanel } from './PlatformsPanel'
import type { Catalog, LinksNow, Overhead, SpaceOverlay } from './space'
import { usePoll } from './space'
import { SpacePanel, type SpaceSelection } from './SpacePanel'
import { TaskPanel } from './TaskPanel'
import type { LngLat } from './types'
import { domainOf, STALE_MS } from './types'
import { useBridge } from './useBridge'

/** ?perf in the URL shows a frame-rate readout (to check 3D on a real GPU). */
function usePerfMeter() {
  const [fps, setFps] = useState<number | null>(null)
  useEffect(() => {
    if (!new URLSearchParams(location.search).has('perf')) return
    let frames = 0
    let start = performance.now()
    let raf = 0
    const tick = (t: number) => {
      frames++
      if (t - start >= 1000) {
        setFps(Math.round((frames * 1000) / (t - start)))
        frames = 0
        start = t
      }
      raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [])
  return fps
}

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
  const fps = usePerfMeter()
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [showPlatforms, setShowPlatforms] = useState(false)
  const [focus, setFocus] = useState<{ uid: number; n: number } | null>(null)
  const [draft, setDraft] = useState<Draft>({ mode: 'orbit', center: null, radius: 250, route: [] })
  const [showSpace, setShowSpace] = useState(false)
  const [showGlobe, setShowGlobe] = useState(false)
  const [mapView, setMapView] = useState<{ center: LngLat; bounds: [number, number, number, number] } | null>(null)
  const [drawing, setDrawing] = useState<LngLat | 'start' | null>(null)
  const [box, setBox] = useState<LngLat[] | null>(null)
  const [spaceSel, setSpaceSel] = useState<SpaceSelection>({ scene: null, norad: null })

  // space data is fetched only while a space view is open
  const spaceOn = showSpace || showGlobe
  const catalog = usePoll<Catalog>(spaceOn ? '/api/space/catalog' : null, 30_000)
  const overhead = usePoll<Overhead>(showSpace ? '/api/space/overhead' : null, 5_000)
  const linksNow = usePoll<LinksNow>(spaceOn ? '/api/space/links/now' : null, 10_000)
  const overlay = useMemo<SpaceOverlay>(
    () => ({
      area: spaceOn ? (catalog.data?.area ?? null) : null,
      overhead: showSpace ? (overhead.data?.overhead ?? []) : [],
      scene: showSpace ? spaceSel.scene : null,
      links: spaceOn ? (linksNow.data?.links ?? []) : [],
      box: showSpace ? box : null,
    }),
    [spaceOn, showSpace, catalog.data, overhead.data, spaceSel, linksNow.data, box],
  )
  useEffect(() => {
    if (!drawing) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setDrawing(null)
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [drawing])
  const onView = useCallback((center: LngLat, bounds: [number, number, number, number]) => setMapView({ center, bounds }), [])

  const vehicles = state?.vehicles ?? []
  const taskings = state?.taskings ?? []
  const lost = vehicles.filter((v) => (v.link.since_last_ms ?? 0) > STALE_MS).length

  const toggle = useCallback((uid: number) => {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(uid)) next.delete(uid)
      else next.add(uid)
      return next
    })
  }, [])

  const onMapClick = useCallback(
    (p: LngLat) => {
      // drawing a collection box: two clicks, opposite corners
      if (drawing === 'start') return setDrawing(p)
      if (drawing) {
        const [a, b] = [drawing, p]
        setBox([
          [Math.min(a[0], b[0]), Math.min(a[1], b[1])],
          [Math.max(a[0], b[0]), Math.min(a[1], b[1])],
          [Math.max(a[0], b[0]), Math.max(a[1], b[1])],
          [Math.min(a[0], b[0]), Math.max(a[1], b[1])],
        ])
        return setDrawing(null)
      }
      setDraft((d) => (d.mode === 'orbit' ? { ...d, center: p } : d.mode === 'patrol' ? { ...d, route: [...d.route, p] } : d))
    },
    [drawing],
  )

  return (
    <div className="app">
      <header className="topbar">
        <span className="brand">MURMUR</span>
        <span className="sub">multi-domain swarm C2</span>
        <span className="spacer" />
        {(['air', 'ground', 'surface', 'subsurface'] as const).map((d) => (
          <span className="stat" key={d}>
            <b>{vehicles.filter((v) => domainOf(v) === d).length}</b> {d}
          </span>
        ))}
        <span className={`stat ${lost ? 'alert' : ''}`}>
          <b>{lost}</b> lost link
        </span>
        <span className={`pill ${status}`}>{status === 'live' ? 'BRIDGE LIVE' : status === 'down' ? 'BRIDGE DOWN' : 'CONNECTING'}</span>
        <button className={`ghost platforms-btn ${showSpace ? 'on' : ''}`} onClick={() => setShowSpace((v) => !v)} title="Space timeline: imaging access, SATCOM, GNSS">
          Space
        </button>
        <button className={`ghost platforms-btn ${showGlobe ? 'on' : ''}`} onClick={() => setShowGlobe((v) => !v)} title="Globe inset: satellites by class">
          Globe
        </button>
        <button className="ghost platforms-btn" onClick={() => setShowPlatforms((v) => !v)}>
          Platforms
        </button>
        {fps != null && <span className="stat" title="frames per second (?perf)"><b>{fps}</b> fps</span>}
        <span className="clock">{clock}</span>
      </header>
      <FleetPanel vehicles={vehicles} selected={selected} onToggle={toggle} onSelect={(ids) => setSelected(new Set(ids))} />
      <main className={`mapwrap ${showSpace ? 'space-open' : ''}`}>
        <MapView
          vehicles={vehicles}
          taskings={taskings}
          selected={selected}
          draft={draft}
          focus={focus}
          onToggle={toggle}
          onMapClick={onMapClick}
          space={overlay}
          onView={onView}
        />
        {showGlobe && (
          <GlobeInset
            center={mapView?.center ?? null}
            area={catalog.data?.area ?? null}
            links={linksNow.data?.links ?? []}
            norad={spaceSel.norad}
            onClose={() => setShowGlobe(false)}
          />
        )}
        {showSpace && (
          <SpacePanel
            catalog={catalog.data}
            catalogError={catalog.error}
            mapBounds={mapView?.bounds ?? null}
            box={drawing && drawing !== 'start' ? null : box}
            drawing={drawing != null}
            onDrawBox={() => setDrawing('start')}
            onClearBox={() => setBox(null)}
            onSelect={setSpaceSel}
            onClose={() => {
              setShowSpace(false)
              setDrawing(null)
              setSpaceSel({ scene: null, norad: null })
            }}
          />
        )}
        {drawing && <div className="banner info">Click two opposite corners of the collection box. Esc cancels.</div>}
        {showPlatforms && (
          <PlatformsPanel
            onClose={() => setShowPlatforms(false)}
            onFocus={(uid) => {
              setSelected(new Set([uid]))
              setFocus((f) => ({ uid, n: (f?.n ?? 0) + 1 }))
              setShowPlatforms(false)
            }}
          />
        )}
        {status !== 'live' && (
          <div className="banner">Bridge not reachable at /ws. Start it with <code>make bridge</code>. Retrying…</div>
        )}
      </main>
      <TaskPanel vehicles={vehicles} selected={selected} draft={draft} setDraft={setDraft} taskings={taskings} />
    </div>
  )
}
