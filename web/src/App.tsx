import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useBridgeClock, useHiddenAlerts, type Intent } from './decisions'
import { FleetPanel } from './FleetPanel'
import { GlobeInset } from './GlobeInset'
import { InboxButton, InboxPanel, type InboxSelection, type InboxTab } from './InboxPanel'
import { EMPTY_PREVIEW, intentPoints, previewFeatures } from './inboxPreview'
import { MapView, type Draft, type MapLayers } from './MapView'
import { PlatformsPanel } from './PlatformsPanel'
import type { Catalog, LinksNow, Overhead, SpaceOverlay } from './space'
import { usePoll } from './space'
import { SpacePanel, type SpaceSelection } from './SpacePanel'
import { fmtZ } from './decisions'
import { TaskPanel } from './TaskPanel'
import { buildTrackLayers } from './trackLayers'
import { TracksPanel } from './TracksPanel'
import type { Reveal } from './reveal'
import { type Track, trackPredicted } from './tracks'
import { VehicleDetails } from './VehicleDetails'
import type { LngLat, Vehicle } from './types'
import { callsign, DOMAIN_LABEL, domainOf, isStale } from './types'
import { useBridge } from './useBridge'
import { ErrorBoundary } from './ErrorBoundary'
import { EngagePanel } from './EngagePanel'
import { isEngage, previewOf, RUNNING, type EngagePreview, type KeepOutExplanation } from './engage'
import { buildEngageLayers, buildKeepOutLayers, type EngageDrawing } from './engageLayers'
import { IMPACT_STATUS_MS, type ImpactEvent } from './impact'
import { estimated } from './estimate'
import { buildEstimateLayers } from './estimateLayers'
import { circle } from './geo'
import { PERF_ON, perfCommitted, perfPassiveBegin, perfRenderBegin, perfUpdateEnd } from './perf'
import { PerfOverlay } from './PerfOverlay'
import { QueueButton, QueuePanel } from './QueuePanel'
import { buildOrderLayers } from './orderLayers'
import { noteFocusedIntent, noteTrackViewed } from './operatorView'
import { refreshAuth, useAuth } from './auth'
import { EnrollScreen, LoginScreen, OperatorBadge } from './Login'
import { SituationPanel } from './SituationPanel'
import { CoverageControl } from './CoverageControl'
import { gridImage, type CoverageGrid, type LinkRouteDrawing } from './coverage'

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

/** Rules 10: the passkey invitation in the address (`#enroll=<operator>.<code>`), read once and
 *  taken out of the address (it works once anyway). */
function readInvite(): { operator: string; code: string } | null {
  const m = /^#enroll=(.+)\.([A-Za-z0-9_-]+)$/.exec(window.location.hash)
  if (!m) return null
  window.history.replaceState(null, '', window.location.pathname + window.location.search)
  return { operator: decodeURIComponent(m[1]), code: m[2] }
}

export default function App() {
  const { state, status, decisions, tracks, setView } = useBridge()
  // who this console acts for (rules 8): asked once the bridge answers, and again on reconnect
  const auth = useAuth()
  // rules 10: a one-time passkey invitation in the address (`murmur-auth passkey-invite`)
  const [invite, setInvite] = useState(readInvite)
  useEffect(() => {
    void refreshAuth()
  }, [])
  useEffect(() => {
    if (status === 'live') void refreshAuth()
  }, [status])
  perfRenderBegin()
  const clock = useUtcClock()
  // the tracks section in the right column: collapsed to its header, or open
  const [tracksCollapsed, setTracksCollapsed] = useState(false)
  // rows to scroll to and mark in the fleet list and the tracks list (selected outside them)
  const [revealVeh, setRevealVeh] = useState<Reveal | null>(null)
  const [revealTrack, setRevealTrack] = useState<Reveal | null>(null)
  const [selectedTrack, setSelectedTrack] = useState<string | null>(null)
  const [selected, setSelected] = useState<Set<number>>(new Set())
  /** vehicles selected outside the fleet list: scroll the list to the first and mark it */
  const revealVehicles = useCallback((uids: number[]) => {
    if (uids.length) setRevealVeh({ ids: uids.map(String) })
  }, [])
  const [detailsUid, setDetailsUid] = useState<number | null>(null)
  const [following, setFollowing] = useState(true)
  const [showPlatforms, setShowPlatforms] = useState(false)
  const [focus, setFocus] = useState<{ uid: number; n: number } | null>(null)
  const [draft, setDraft] = useState<Draft>({ mode: 'orbit', center: null, radius: 250, route: [] })
  const [showSpace, setShowSpace] = useState(false)
  const [showInbox, setShowInbox] = useState(false)
  // rules 5: orders held for vehicles the link can't reach now
  const [showQueue, setShowQueue] = useState(false)
  const [inboxTab, setInboxTab] = useState<InboxTab>('inbox')
  const [inboxSel, setInboxSel] = useState<InboxSelection>(null)
  // evaluation instrumentation: a track opened is attributed to the selected inbox card only
  useEffect(() => {
    noteFocusedIntent(inboxSel?.kind === 'intent' ? inboxSel.id : null)
  }, [inboxSel])
  // ... and the inbox cards' view events list the tracks opened while they were selected
  // (after the focus effect, so a click that selects a card and a track counts for it)
  useEffect(() => {
    if (selectedTrack) noteTrackViewed(selectedTrack)
  }, [selectedTrack])
  const [fit, setFit] = useState<{ points: LngLat[]; n: number; padRight: number } | null>(null)
  const [showGlobe, setShowGlobe] = useState(false)
  // predicted link coverage (docs/LINK-COVERAGE.md): the map layer, off by default, and the
  // drafted order's predicted link (the task panel's dry run)
  const [showCoverage, setShowCoverage] = useState(false)
  const [coverageGrid, setCoverageGrid] = useState<CoverageGrid | null>(null)
  const coverageImage = useMemo(() => (showCoverage && coverageGrid ? gridImage(coverageGrid) : null), [showCoverage, coverageGrid])
  const [linkRoute, setLinkRoute] = useState<LinkRouteDrawing | null>(null)
  const [mapView, setMapView] = useState<{ center: LngLat; bounds: [number, number, number, number]; zoom: number } | null>(null)
  const [drawing, setDrawing] = useState<LngLat | 'start' | null>(null)
  const [box, setBox] = useState<LngLat[] | null>(null)
  const [spaceSel, setSpaceSel] = useState<SpaceSelection>({ scene: null, norad: null })
  // the engagement nomination form (Phase E), opened from a declared-hostile track or an
  // armed platform, and the preview of the platform it would use
  const [engageForm, setEngageForm] = useState<{ track?: string; uid?: number } | null>(null)
  const [engageDraft, setEngageDraft] = useState<EngagePreview | null>(null)
  // murmur-12e: why the chosen platform's keep-out blocks (its regions on the map)
  const [engageKeepOuts, setEngageKeepOuts] = useState<KeepOutExplanation | null>(null)

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
  const onView = useCallback((center: LngLat, bounds: [number, number, number, number], zoom: number) => setMapView({ center, bounds, zoom }), [])

  const allVehicles = useMemo(() => state?.vehicles ?? [], [state?.vehicles])
  const spent = useMemo(() => new Set(decisions?.spent ?? []), [decisions?.spent])
  const vehicles = useMemo(() => spent.size ? allVehicles.filter((v) => !spent.has(v.uid)) : allVehicles, [allVehicles, spent])
  useEffect(() => {
    if (!spent.size) return
    setSelected((prev) => {
      const next = new Set([...prev].filter((uid) => !spent.has(uid)))
      return next.size === prev.size ? prev : next
    })
    setDetailsUid((uid) => uid != null && spent.has(uid) ? null : uid)
  }, [spent])
  const now = useBridgeClock(state?.t)
  // tracks of contacts that are not ours (Phase C2), drawn through MapView's layers prop;
  // ages advance with the bridge clock (useBridgeClock re-renders twice a second), so the
  // callback's identity changes every tick while there are tracks: rings grow, symbols fade
  const trackTick = tracks?.tracks.length || tracks?.lost?.length ? Math.floor(now() / 500) : 0
  const selectedTrackRef = useRef(selectedTrack)
  selectedTrackRef.current = selectedTrack
  /** a track selected outside the tracks list: open its section, scroll to the row, mark it */
  const revealTrackId = useCallback((id: string) => {
    setSelectedTrack(id)
    setTracksCollapsed(false)
    setRevealTrack({ ids: [id] })
  }, [])
  const onTrackClick = useCallback(
    (id: string) => {
      if (selectedTrackRef.current === id) setSelectedTrack(null)
      else revealTrackId(id)
    },
    [revealTrackId],
  )
  const trackLayers = useCallback<MapLayers>(
    (screen) =>
      buildTrackLayers({
        tracks: tracks?.tracks ?? [],
        lost: tracks?.lost ?? [],
        nowMs: now(),
        selected: selectedTrack,
        staleAfterS: tracks?.stale_after_s ?? 10,
        onClick: onTrackClick,
        screen,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tracks, selectedTrack, trackTick, onTrackClick],
  )
  // engagements in play (and the form's draft) on the map: paths, effect, keep-out, ellipse
  const allIntents = decisions?.intents
  const impactNowMs = now()
  const engageTick = Math.floor(impactNowMs / 1000)
  const impacts = useMemo<ImpactEvent[]>(() => (allIntents ?? []).flatMap((i) => {
    const effect = isEngage(i) ? i.engagement?.effect : null
    const platform = allVehicles.find((v) => v.uid === Number(i.uids[0]))
    return effect ? [{ key: `i${i.id}@${effect.t}`, intentId: i.id, point: effect.point, t: effect.t, radius_m: effect.radius_m,
      platform: platform ? callsign(platform) : undefined, track: i.engagement?.snapshot?.track_id ?? i.engage?.track,
      assessment: i.engagement?.assessment }] : []
  }), [allIntents, allVehicles])
  const seenImpacts = useRef(new Set<string>())
  const focusImpact = useCallback((impact: ImpactEvent) => {
    const ring = circle(impact.point, Math.max(impact.radius_m * 4, 450)).filter((_, k) => k % 8 === 0)
    setFit((f) => ({ points: [impact.point, ...ring], n: (f?.n ?? 0) + 1, padRight: showInbox ? 430 : 70 }))
  }, [showInbox])
  useEffect(() => {
    for (const impact of impacts) {
      if (seenImpacts.current.has(impact.key)) continue
      seenImpacts.current.add(impact.key)
      if (impactNowMs >= impact.t && impactNowMs - impact.t < 4000) focusImpact(impact)
    }
  }, [impacts, impactNowMs, focusImpact])
  const visibleImpact = impacts.filter((impact) => impactNowMs >= impact.t && impactNowMs - impact.t < IMPACT_STATUS_MS).sort((a, b) => b.t - a.t)[0]
  const engageDrawings = useMemo<EngageDrawing[]>(() => {
    const out: EngageDrawing[] = []
    const nowMs = now()
    for (const i of allIntents ?? []) {
      if (!isEngage(i)) continue
      const p = previewOf(i)
      if (!p) continue
      const ended = ['aborted', 'voided', 'done', 'rejected', 'expired', 'failed'].includes(i.state)
      const endT = i.history[i.history.length - 1]?.t ?? 0
      if (ended && nowMs - endT > 120_000) continue
      const phase: EngageDrawing['phase'] =
        i.state === 'pending_approval' ? 'pending' : i.state === 'effected' ? 'effected' : RUNNING.includes(i.state) ? 'running' : 'ended'
      const warn = (i.engagement?.watch && !i.engagement.watch.holds) || (i.engage?.live?.changed ?? []).some((c) => c.worse)
      out.push({ key: `i${i.id}`, label: `ENGAGE i${i.id}`, preview: p, phase, selected: inboxSel?.kind === 'intent' && inboxSel.id === i.id, warn,
        effect: i.engagement?.effect, assessment: i.engagement?.assessment?.result })
    }
    if (engageDraft) out.push({ key: 'draft', label: 'NOMINATION', preview: engageDraft, phase: 'draft', selected: true })
    return out
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allIntents, engageDraft, inboxSel, engageTick])
  // frame the form's preview when it first appears or the platform changes
  const draftKey = engageDraft ? `${engageDraft.start[0].toFixed(3)},${engageDraft.start[1].toFixed(3)}` : null
  useEffect(() => {
    if (!engageDraft) return
    const ring = circle(engageDraft.effect_point, engageDraft.keep_out_m).filter((_, k) => k % 8 === 0)
    setFit((f) => ({ points: [...engageDraft.path, ...ring], n: (f?.n ?? 0) + 1, padRight: 430 }))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draftKey])
  // silent vehicles' estimates (murmur-4y9): the bridge's motion models evaluated every clock
  // tick, so the callback changes identity each tick only while something is silent
  const silent = useMemo(() => state ? estimated(vehicles) : [], [vehicles, state])
  const estimateTick = silent.length ? Math.floor(now() / 500) : 0
  const estimateLayers = useCallback<MapLayers>(
    () => (silent.length ? buildEstimateLayers({ vehicles: silent, nowMs: now(), focus: detailsUid }) : []),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [silent, estimateTick, detailsUid],
  )
  // ordered versus confirmed (rules 5): queued orders' paths dashed, sent ones not yet
  // confirmed labelled
  const queuedIntents = decisions?.inbox.queued
  const orderTick = Math.floor(now() / 1000)
  // the nomination form's keep-out explanation: the platform by its callsign
  const keepOutLayers = useMemo(() => {
    if (!engageKeepOuts) return []
    const v = vehicles.find((x) => String(x.uid) === engageKeepOuts.platform)
    return buildKeepOutLayers(engageKeepOuts, v ? callsign(v) : `vehicle ${engageKeepOuts.platform ?? ''}`)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [engageKeepOuts])
  const mapLayers = useCallback<MapLayers>(
    (screen) => [
      ...estimateLayers(screen),
      ...trackLayers(screen),
      ...keepOutLayers,
      ...(engageDrawings.length ? buildEngageLayers(engageDrawings, now()) : []),
      ...buildOrderLayers({ queued: queuedIntents ?? [], intents: allIntents ?? [], taskings: state?.taskings ?? [], vehicles, nowMs: now() }),
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [estimateLayers, trackLayers, keepOutLayers, engageDrawings, queuedIntents, allIntents, state?.taskings, vehicles, orderTick],
  )
  const focusTrack = useCallback(
    (t: Track) => setFit((f) => ({ points: [trackPredicted(t, now())], n: (f?.n ?? 0) + 1, padRight: 400 })),
    [now],
  )
  const { mem: alertMem, hidden: hiddenAlerts } = useHiddenAlerts(decisions)
  const pending = decisions?.inbox.pending
  const pendingIds = useMemo(() => new Set((pending ?? []).map((i) => i.id)), [pending])
  // The bridge's level of detail follows the view (murmur-and): what the map shows gets full
  // detail, and so do the vehicles the operator is looking at some other way: selected,
  // followed (details open), named by a pending intent or an alert.
  const alerts = decisions?.inbox.alerts
  const pins = useMemo(() => {
    const set = new Set<number>(selected)
    if (detailsUid != null) set.add(detailsUid)
    // and the vehicles of orders waiting in the queue (rules 5): their badge, and the
    // re-check when the link is back, are about them
    for (const i of [...(pending ?? []), ...(queuedIntents ?? [])])
      for (const u of i.uids) if (Number.isFinite(Number(u))) set.add(Number(u))
    for (const a of alerts ?? []) set.add(a.uid)
    return [...set].filter((uid) => !spent.has(uid)).sort((a, b) => a - b)
  }, [selected, detailsUid, pending, queuedIntents, alerts, spent])
  useEffect(() => {
    if (mapView) setView({ bounds: mapView.bounds, zoom: mapView.zoom, pin: pins })
  }, [mapView, pins, setView])
  const selIntent = inboxSel?.kind === 'intent' ? inboxSel.id : null
  const preview = useMemo(
    () => (pending && pending.length ? previewFeatures(pending, vehicles, selIntent) : EMPTY_PREVIEW),
    [pending, vehicles, selIntent],
  )
  const taskings = state?.taskings ?? []
  const lost = vehicles.filter(isStale).length

  /** an inbox item was clicked: highlight its vehicles and frame them with the task's preview */
  const focusInbox = useCallback(
    (sel: InboxSelection, uids: number[], intent: Intent | null) => {
      setInboxSel(sel)
      setSelected(new Set(uids))
      revealVehicles(uids)
      const vs = state?.vehicles ?? []
      const pts: LngLat[] = intent ? intentPoints(intent, vs) : uids.map((u) => vs.find((v) => v.uid === u)?.position).filter((p): p is LngLat => p != null)
      if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: 430 }))
    },
    [state?.vehicles, revealVehicles],
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

  const selectedRef = useRef(selected)
  selectedRef.current = selected
  /** a click on a vehicle in the map or its details: reveal it in the list when this selects it */
  const toggleOutside = useCallback(
    (uid: number) => {
      if (!selectedRef.current.has(uid)) revealVehicles([uid])
      toggle(uid)
    },
    [revealVehicles, toggle],
  )

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

  // rules 10: a passkey invitation is registered first (no sign-in needed)
  if (auth.auth === 'credentials' && invite) return <EnrollScreen invite={invite} onDone={() => setInvite(null)} />
  // a credentials run: nothing but the sign-in until an operator has signed in
  if (auth.auth === 'credentials' && !auth.signed_in) return <LoginScreen />

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
        <QueueButton decisions={decisions} open={showQueue} onClick={() => setShowQueue((v) => !v)} />
        <button className={`ghost platforms-btn ${showSpace ? 'on' : ''}`} onClick={() => setShowSpace((v) => !v)} title="Space timeline: imaging access, SATCOM, GNSS">
          Space
        </button>
        <button className={`ghost platforms-btn ${showGlobe ? 'on' : ''}`} onClick={() => setShowGlobe((v) => !v)} title="Globe inset: satellites by class">
          Globe
        </button>
        <button
          className={`ghost platforms-btn ${showCoverage ? 'on' : ''}`}
          aria-pressed={showCoverage}
          onClick={() => setShowCoverage((v) => !v)}
          title="Predicted link coverage on the map: up, degraded and down for a link class at an altitude (the link emulator's geometry; its random loss and scripted events are not in it)"
        >
          Links
        </button>
        <button className="ghost platforms-btn" onClick={() => setShowPlatforms((v) => !v)}>
          Platforms
        </button>
        <OperatorBadge />
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
        reveal={revealVeh}
      />
      </ErrorBoundary>
      <main className={`mapwrap ${showSpace ? 'space-open' : ''}`}>
        <ErrorBoundary name="Map" className="floating">
        <MapView
          vehicles={allVehicles}
          hiddenUids={spent}
          impacts={impacts}
          impactNowMs={impactNowMs}
          taskings={taskings}
          selected={selected}
          draft={draft}
          focus={focus}
          follow={detailsUid}
          onFollowing={setFollowing}
          preview={preview}
          fit={fit}
          onToggle={toggleOutside}
          onDeselect={deselect}
          onMapClick={onMapClick}
          space={overlay}
          onView={onView}
          layers={mapLayers}
          clusters={state?.clusters ?? null}
          coverage={coverageImage}
          linkRoute={linkRoute}
        />
        </ErrorBoundary>
        {showCoverage && (
          <ErrorBoundary name="Link coverage" className="floating">
            <CoverageControl
              view={mapView}
              vehicle={selected.size === 1 ? (vehicles.find((v) => selected.has(v.uid)) ?? null) : null}
              onGrid={setCoverageGrid}
              onClose={() => setShowCoverage(false)}
            />
          </ErrorBoundary>
        )}
        {/* a situation runner's status (make situation): shown only while one has posted it */}
        <ErrorBoundary name="Situation" className="floating">
          <SituationPanel decisions={decisions} />
        </ErrorBoundary>
        {visibleImpact && (
          <div className="impact-status" role="status">
            <span><strong>{visibleImpact.assessment ? `ASSESSMENT: ${visibleImpact.assessment.result.toUpperCase()}` : 'STRIKE · EFFECT RELEASED'}</strong>
              {' · '}{visibleImpact.platform ?? `ENGAGE i${visibleImpact.intentId}`}{visibleImpact.track ? ` on ${visibleImpact.track}` : ''}{' · '}
              {visibleImpact.assessment?.result === 'ceased' ? 'No target detected where expected'
                : visibleImpact.assessment?.result === 'persists' ? 'Target still detected'
                : visibleImpact.assessment?.result === 'unknown' ? 'Outcome unknown'
                : 'Sensor assessment pending'}</span>
            <button onClick={() => focusImpact(visibleImpact)}>View impact</button>
          </div>
        )}
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
            vehicles={allVehicles}
            now={now}
            tab={inboxTab}
            setTab={setInboxTab}
            selection={inboxSel}
            onFocus={focusInbox}
            onClose={() => setShowInbox(false)}
            mem={alertMem}
            hidden={hiddenAlerts}
            onTrack={(id) => {
              setShowInbox(false)
              revealTrackId(id)
              const t = tracks?.tracks.find((x) => x.id === id)
              if (t) focusTrack(t)
            }}
          />
          </ErrorBoundary>
        )}
        {showQueue && (
          <ErrorBoundary name="Queue" className="floating">
          <QueuePanel
            decisions={decisions}
            vehicles={vehicles}
            now={now}
            onFocus={(uids) => {
              setSelected(new Set(uids))
              revealVehicles(uids)
              const pts = uids.map((u) => vehicles.find((v) => v.uid === u)?.position).filter((p): p is LngLat => p != null)
              if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: 430 }))
            }}
            onClose={() => setShowQueue(false)}
          />
          </ErrorBoundary>
        )}
        {engageForm && (
          <ErrorBoundary name="Engagement" className="floating">
          <EngagePanel
            tracks={tracks}
            vehicles={vehicles}
            now={now}
            initial={engageForm}
            onPreview={setEngageDraft}
            onKeepOuts={setEngageKeepOuts}
            onNominated={(id) => {
              setEngageForm(null)
              setEngageDraft(null)
              setEngageKeepOuts(null)
              setShowInbox(true)
              setInboxTab('inbox')
              setInboxSel({ kind: 'intent', id })
            }}
            onClose={() => {
              setEngageForm(null)
              setEngageDraft(null)
              setEngageKeepOuts(null)
            }}
          />
          </ErrorBoundary>
        )}
        {detailsUid != null && byUidDetails(vehicles, detailsUid) && (
          <ErrorBoundary name="Vehicle details" className="floating">
          <VehicleDetails
            v={byUidDetails(vehicles, detailsUid)!}
            taskings={taskings}
            now={now}
            selected={selected.has(detailsUid)}
            onToggleSelect={() => toggleOutside(detailsUid)}
            onClose={() => setDetailsUid(null)}
            following={following}
            onFollow={() => {
              setFollowing(true)
              setFocus((f) => ({ uid: detailsUid, n: (f?.n ?? 0) + 1 }))
            }}
            onEngage={() => setEngageForm({ uid: detailsUid })}
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
              revealVehicles([uid])
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
      <div className="rail">
        <div className="tk-slot">
          <ErrorBoundary name="Tracks" className="slot">
          <TracksPanel
            tracks={tracks}
            vehicles={allVehicles}
            now={now}
            selected={selectedTrack}
            onSelect={setSelectedTrack}
            onFocus={focusTrack}
            collapsed={tracksCollapsed}
            onCollapse={() => setTracksCollapsed((v) => !v)}
            reveal={revealTrack}
            onEngage={(track) => setEngageForm({ track })}
          />
          </ErrorBoundary>
        </div>
        <ErrorBoundary name="Task panel" className="slot tasking">
        <TaskPanel vehicles={vehicles} historyVehicles={allVehicles} selected={selected} draft={draft} setDraft={setDraft} taskings={taskings} pendingIds={pendingIds} onInbox={openIntent} space={spaceRuns} now={now} onLinkRoute={setLinkRoute} />
        </ErrorBoundary>
      </div>
    </div>
  )
}
