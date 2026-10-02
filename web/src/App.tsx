import { useCallback, useEffect, useLayoutEffect, useMemo, useState } from 'react'
import { useBridgeClock, useHiddenAlerts, type Intent } from './decisions'
import { FleetPanel } from './FleetPanel'
import { GlobeInset } from './GlobeInset'
import { InboxButton, InboxPanel, type InboxSelection, type InboxTab } from './InboxPanel'
import { EMPTY_PREVIEW, intentPoints, previewFeatures } from './inboxPreview'
import { MapView, type Draft } from './MapView'
import { PlatformsPanel } from './PlatformsPanel'
import type { Catalog, LinksNow, Overhead, SpaceOverlay } from './space'
import { usePoll } from './space'
import { SpacePanel, type SpaceSelection } from './SpacePanel'
import { fmtZ } from './decisions'
import { TaskPanel } from './TaskPanel'
import { VehicleDetails } from './VehicleDetails'
import type { LngLat, Vehicle } from './types'
import { DOMAIN_LABEL, domainOf, isStale } from './types'
import { useBridge } from './useBridge'
import { ErrorBoundary } from './ErrorBoundary'
import { PERF_ON, perfCommitted, perfPassiveBegin, perfRenderBegin, perfUpdateEnd } from './perf'
import { PerfOverlay } from './PerfOverlay'

function useUtcClock() {
  const [now, setNow] = useState(() => new Date())
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 1000)
    return () => window.clearInterval(id)
  }, [])
  return now.toISOString().slice(11, 19) + 'Z'
}

const byUidDetails = (vs: Vehicle[], uid: number) => vs.find((v) => v.uid === uid)

/** Renders nothing. Its effect is the first passive effect of the tree on every commit (it
 *  is App's first child and has no children), so it marks where the effects' work begins. */
function PerfMark() {
  useEffect(() => perfPassiveBegin())
  return null
}

export default function App() {
  const { state, status, decisions } = useBridge()
  perfRenderBegin()
  const clock = useUtcClock()
  const [selected, setSelected] = useState<Set<number>>(new Set())
  const [detailsUid, setDetailsUid] = useState<number | null>(null)
  const [following, setFollowing] = useState(true)
  const [showPlatforms, setShowPlatforms] = useState(false)
  const [focus, setFocus] = useState<{ uid: number; n: number } | null>(null)
  const [draft, setDraft] = useState<Draft>({ mode: 'orbit', center: null, radius: 250, route: [] })
  const [showSpace, setShowSpace] = useState(false)
  const [showInbox, setShowInbox] = useState(false)
  const [inboxTab, setInboxTab] = useState<InboxTab>('inbox')
  const [inboxSel, setInboxSel] = useState<InboxSelection>(null)
  const [fit, setFit] = useState<{ points: LngLat[]; n: number; padRight: number } | null>(null)
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
  // tasked collections still running: outlined on the map whether or not the space panel is open
  const spaceRuns = decisions?.space
  const tasked = useMemo(
    () =>
      (spaceRuns?.runs ?? [])
        .filter((r) => r.kind === 'collect' && r.collect && !r.outcome)
        .map((r) => ({
          ring: r.collect!.scene ?? r.collect!.area,
          label: `COLLECT i${r.intent_id} ${r.collect!.satellite} ${fmtZ(r.collect!.collect_time)}${r.collect!.scene ? '' : ' (area)'}`,
        })),
    [spaceRuns],
  )
  const overlay = useMemo<SpaceOverlay>(
    () => ({
      area: spaceOn ? (catalog.data?.area ?? null) : null,
      overhead: showSpace ? (overhead.data?.overhead ?? []) : [],
      scene: showSpace ? spaceSel.scene : null,
      links: spaceOn ? (linksNow.data?.links ?? []) : [],
      box: showSpace ? box : null,
      tasked,
    }),
    [spaceOn, showSpace, catalog.data, overhead.data, spaceSel, linksNow.data, box, tasked],
  )
  useEffect(() => {
    if (!drawing) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setDrawing(null)
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [drawing])
  const onView = useCallback((center: LngLat, bounds: [number, number, number, number]) => setMapView({ center, bounds }), [])

  const vehicles = state?.vehicles ?? []
  const now = useBridgeClock(state?.t)
  const { mem: alertMem, hidden: hiddenAlerts } = useHiddenAlerts(decisions)
  const pending = decisions?.inbox.pending
  const pendingIds = useMemo(() => new Set((pending ?? []).map((i) => i.id)), [pending])
  const selIntent = inboxSel?.kind === 'intent' ? inboxSel.id : null
  const preview = useMemo(
    () => (pending && pending.length ? previewFeatures(pending, state?.vehicles ?? [], selIntent) : EMPTY_PREVIEW),
    [pending, state?.vehicles, selIntent],
  )
  const taskings = state?.taskings ?? []
  const lost = vehicles.filter(isStale).length

  /** an inbox item was clicked: highlight its vehicles and frame them with the task's preview */
  const focusInbox = useCallback(
    (sel: InboxSelection, uids: number[], intent: Intent | null) => {
      setInboxSel(sel)
      setSelected(new Set(uids))
      const vs = state?.vehicles ?? []
      const pts: LngLat[] = intent ? intentPoints(intent, vs) : uids.map((u) => vs.find((v) => v.uid === u)?.position).filter((p): p is LngLat => p != null)
      if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: 430 }))
    },
    [state?.vehicles],
  )
  /** the task panel's link: the pending intent in the inbox, or a finished one in Decisions */
  const openIntent = useCallback(
    (id: number) => {
      setShowInbox(true)
      const i = (pending ?? []).find((x) => x.id === id)
      if (i) {
        setInboxTab('inbox')
        focusInbox({ kind: 'intent', id }, i.uids.map(Number).filter(Number.isFinite), i)
      } else setInboxTab('decisions')
    },
    [pending, focusInbox],
  )

  const toggle = useCallback((uid: number) => {
    setSelected((prev) => {
      const next = new Set(prev)
      if (next.has(uid)) next.delete(uid)
      else next.add(uid)
      return next
    })
  }, [])

  // right click: deselect one vehicle, or everything (uid null)
  const deselect = useCallback((uid: number | null) => {
    setSelected((prev) => {
      if (uid == null) return prev.size ? new Set() : prev
      if (!prev.has(uid)) return prev
      const next = new Set(prev)
      next.delete(uid)
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

  // last effect of the tree for this state: children's effects (map, list) have run
  useLayoutEffect(() => perfCommitted(), [state])
  useEffect(() => perfUpdateEnd(), [state])

  return (
    <div className="app">
      <PerfMark />
      <header className="topbar">
        <span className="brand">MURMUR</span>
        <span className="sub">multi-domain swarm C2</span>
        <span className="spacer" />
        {(['air', 'ground', 'surface', 'subsurface'] as const).map((d) => (
          <span className="stat" key={d}>
            <b>{vehicles.filter((v) => domainOf(v) === d).length}</b> {DOMAIN_LABEL[d].toLowerCase()}
          </span>
        ))}
        <span className={`stat ${lost ? 'alert' : ''}`}>
          <b>{lost}</b> lost link
        </span>
        <span className={`pill ${status}`}>{status === 'live' ? 'BRIDGE LIVE' : status === 'down' ? 'BRIDGE DOWN' : 'CONNECTING'}</span>
        <InboxButton decisions={decisions} open={showInbox} hiddenAlerts={hiddenAlerts} onClick={() => setShowInbox((v) => !v)} />
        <button className={`ghost platforms-btn ${showSpace ? 'on' : ''}`} onClick={() => setShowSpace((v) => !v)} title="Space timeline: imaging access, SATCOM, GNSS">
          Space
        </button>
        <button className={`ghost platforms-btn ${showGlobe ? 'on' : ''}`} onClick={() => setShowGlobe((v) => !v)} title="Globe inset: satellites by class">
          Globe
        </button>
        <button className="ghost platforms-btn" onClick={() => setShowPlatforms((v) => !v)}>
          Platforms
        </button>
        <span className="clock">{clock}</span>
      </header>
      <ErrorBoundary name="Fleet list" className="slot fleet">
      <FleetPanel vehicles={vehicles} selected={selected} onToggle={toggle}
        onDeselect={deselect}
        detailsUid={detailsUid}
        onDetails={(uid) => {
          // a second click on the same row closes it; opening flies the map there
          setDetailsUid((d) => (d === uid ? null : uid))
          if (detailsUid !== uid) {
            setFollowing(true)
            setFocus((f) => ({ uid, n: (f?.n ?? 0) + 1 }))
          }
        }}
        onSelect={(ids) => setSelected(new Set(ids))}
      />
      </ErrorBoundary>
      <main className={`mapwrap ${showSpace ? 'space-open' : ''}`}>
        <ErrorBoundary name="Map" className="floating">
        <MapView
          vehicles={vehicles}
          taskings={taskings}
          selected={selected}
          draft={draft}
          focus={focus}
          follow={detailsUid}
          onFollowing={setFollowing}
          preview={preview}
          fit={fit}
          onToggle={toggle}
          onDeselect={deselect}
          onMapClick={onMapClick}
          space={overlay}
          onView={onView}
        />
        </ErrorBoundary>
        {showGlobe && (
          <ErrorBoundary name="Globe" className="floating">
          <GlobeInset
            center={mapView?.center ?? null}
            area={catalog.data?.area ?? null}
            links={linksNow.data?.links ?? []}
            norad={spaceSel.norad}
            onClose={() => setShowGlobe(false)}
          />
          </ErrorBoundary>
        )}
        {showSpace && (
          <ErrorBoundary name="Space panel" className="floating">
          <SpacePanel
            catalog={catalog.data}
            catalogError={catalog.error}
            mapBounds={mapView?.bounds ?? null}
            box={drawing && drawing !== 'start' ? null : box}
            drawing={drawing != null}
            onDrawBox={() => setDrawing('start')}
            onClearBox={() => setBox(null)}
            onSelect={setSpaceSel}
            space={spaceRuns}
            bridgeNow={now}
            onIntent={openIntent}
            onClose={() => {
              setShowSpace(false)
              setDrawing(null)
              setSpaceSel({ scene: null, norad: null })
            }}
          />
          </ErrorBoundary>
        )}
        {showInbox && (
          <ErrorBoundary name="Inbox" className="floating">
          <InboxPanel
            decisions={decisions}
            vehicles={vehicles}
            now={now}
            tab={inboxTab}
            setTab={setInboxTab}
            selection={inboxSel}
            onFocus={focusInbox}
            onClose={() => setShowInbox(false)}
            mem={alertMem}
            hidden={hiddenAlerts}
          />
          </ErrorBoundary>
        )}
        {detailsUid != null && byUidDetails(vehicles, detailsUid) && (
          <ErrorBoundary name="Vehicle details" className="floating">
          <VehicleDetails
            v={byUidDetails(vehicles, detailsUid)!}
            taskings={taskings}
            selected={selected.has(detailsUid)}
            onToggleSelect={() => toggle(detailsUid)}
            onClose={() => setDetailsUid(null)}
            following={following}
            onFollow={() => {
              setFollowing(true)
              setFocus((f) => ({ uid: detailsUid, n: (f?.n ?? 0) + 1 }))
            }}
          />
          </ErrorBoundary>
        )}
        {drawing && <div className="banner info">Click two opposite corners of the collection box. Esc cancels.</div>}
        {showPlatforms && (
          <ErrorBoundary name="Platforms" className="floating">
          <PlatformsPanel
            onClose={() => setShowPlatforms(false)}
            onFocus={(uid) => {
              setSelected(new Set([uid]))
              setFocus((f) => ({ uid, n: (f?.n ?? 0) + 1 }))
              setShowPlatforms(false)
            }}
          />
          </ErrorBoundary>
        )}
        {status !== 'live' && (
          <div className="banner">Bridge not reachable at /ws. Start it with <code>make bridge</code>. Retrying…</div>
        )}
        {PERF_ON && <PerfOverlay />}
      </main>
      <ErrorBoundary name="Task panel" className="slot tasking">
      <TaskPanel vehicles={vehicles} selected={selected} draft={draft} setDraft={setDraft} taskings={taskings} pendingIds={pendingIds} onInbox={openIntent} space={spaceRuns} now={now} />
      </ErrorBoundary>
    </div>
  )
}
