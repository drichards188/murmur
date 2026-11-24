import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { type Check, useBridgeClock, useHiddenAlerts, type Intent } from './decisions'
import { FleetPanel } from './FleetPanel'
import { GlobeInset } from './GlobeInset'
import { InboxButton, InboxPanel, type InboxSelection, type InboxTab } from './InboxPanel'
import { EMPTY_PREVIEW, intentPoints, previewFeatures } from './inboxPreview'
import { MapView, type Draft, type MapLayers, type TracerSpec } from './MapView'
import { PlatformsPanel } from './PlatformsPanel'
import type { Catalog, LinksNow, Overhead, SpaceOverlay } from './space'
import { usePoll } from './space'
import { SpacePanel, type SpaceSelection } from './SpacePanel'
import { fmtZ } from './decisions'
import { TaskPanel } from './TaskPanel'
import { buildTrackLayers, trackLabelCandidates, trackTooltip } from './trackLayers'
import { TracksPanel } from './TracksPanel'
import type { Reveal } from './reveal'
import { isTrackStale, type Track, trackPredicted } from './tracks'
import { VehicleDetailsWhenFull } from './VehicleDetails'
import type { LngLat, Vehicle } from './types'
import { callsign, DOMAIN_LABEL, domainOf, isStale, vehicleNamer, type Tasking } from './types'
import { useBridge } from './useBridge'
import { ErrorBoundary } from './ErrorBoundary'
import { EngagePanel } from './EngagePanel'
import { isEngage, previewOf, RUNNING, type EngagePreview, type KeepOutExplanation } from './engage'
import { buildEngageLayers, buildKeepOutLayers, type EngageDrawing } from './engageLayers'
import { IMPACT_STATUS_MS, type ImpactEvent } from './impact'
import { actDrawings, bannerAct, bannerLine, type ActDrawing, type Seen, type Victim } from './hostileAct'
import type { Against } from './ident'
import { span } from './engageSteps'
import { shownAlt } from './drawn'
import './act.css'
import { ESTIMATE_NOTE, weaponBanner, weaponReports, type WeaponTrack } from './weapon'
import { loadoutProfile, secondsToImpact } from './munition'
import { usePlatformLimits } from './taskDefaults'
import { estimated } from './estimate'
import { buildEstimateLayers, estimateLabelCandidates, estimateTooltip, hiddenHypotheses } from './estimateLayers'
import { circle } from './geo'
import { PERF_ON, perfCommitted, perfPassiveBegin, perfRenderBegin, perfUpdateEnd } from './perf'
import { PerfOverlay } from './PerfOverlay'
import { QueueButton, QueuePanel } from './QueuePanel'
import { buildOrderLayers, orderLabelCandidates } from './orderLayers'
import { noteFocusedIntent, noteTrackViewed } from './operatorView'
import { refreshAuth, useAuth } from './auth'
import { EnrollScreen, LoginScreen, OperatorBadge } from './Login'
import { WeatherPanel } from './WeatherPanel'
import { WeatherLegend } from './WeatherLayer'
import { useWeatherGrid } from './useWeatherGrid'
import type { WeatherMode } from './weather'
import { regionsOf } from './weatherField'
import type { Effects, WeatherView } from './weatherMap'
import { saveEffects, savedEffects } from './weatherPrefs'
import { reachBadge, reachFeatures, reachQuery, type DomainChoice, type ReachAnswer, type ReachMode } from './sensorReach'
import { minefieldFeatures, type MinefieldsAnswer } from './minefields'
import { SituationPanel } from './SituationPanel'
import { pendingItems, waitingItems } from './blocking'
import { PendingStrip, WaitingStrip } from './WaitingStrip'
import { Tutorial } from './Tutorial'
import { CoverageControl } from './CoverageControl'
import { gridImage, type CoverageGrid, type LinkRouteDrawing } from './coverage'
import { buildStationLayers, EXCLUDED_PICK_LAYER } from './linkLayers'
import type { ExcludedMark } from './groupOrders'
import { nodeLossView } from './nodeLoss'
import { buildNodeRegionLayers } from './nodeLossLayers'
import { useNodes } from './useNodes'
import { buildSwarmLayers, useSwarms } from './swarms'
import { CROWD, CROWD_SILENT, engagementTracks, focusMap, reviewTracks } from './focus'
import { HiddenLegend, type HiddenRow } from './HiddenLegend'
import type { LabelStats } from './declutter'
import { alarmKey, GROUP_ALARM, groupMarks, groupState, heldAlarms, missingLines, selectedSwarm, shownTaskings, stepAlarms, swarmOf, swarmOutlines, unfolded, type AlarmSeen, type GroupState, type Swarm } from './swarmsModel'
import { GroupStrip } from './GroupStrip'
import { latestOps, vehicleState } from './exceptions'
import type { GroupsOnMap } from './GroupChips'
import type { MapScreen } from './aloft'
import { SwarmsButton, SwarmsPanel } from './SwarmsPanel'
import { useGroups } from './useGroups'
import { groupOfSelection } from './groups'
import { Sitrep } from './Sitrep'
import { TaskingButton, TaskingPanel } from './TaskingPanel'
import { useTasking } from './useTasking'
import { closeDock, fitPad, headsWithWaiting, toggleDock, type DockId } from './dock'
import { awaitingRows, selectionInsert, turnOf } from './officer'
import { Awaiting, TurnCause } from './Awaiting'
import { OfficerCompose, type ComposeInsert } from './OfficerCompose'

function useUtcClock() {
  const [now, setNow] = useState(() => new Date())
  useEffect(() => {
    const id = window.setInterval(() => setNow(new Date()), 1000)
    return () => window.clearInterval(id)
  }, [])
  return now.toISOString().slice(11, 19) + 'Z'
}

const byUidDetails = (vs: Vehicle[], uid: number) => vs.find((v) => v.uid === uid)

/** the map layers whose objects are tracks or lost contacts (their `id`): hovering one draws it in full */
const TRACK_HOVER_LAYERS = new Set(['tracks', 'tracks-ids', 'tracks-aloft', 'lost'])

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

const NO_TASKINGS: Tasking[] = []
const NO_SWARMS: Swarm[] = []
const NO_TRACKS: Track[] = []
const NO_INTENTS: Intent[] = []

/** The top bar's domain counts in a narrow window (dock.css). */
const STAT_SHORT = { air: 'air', ground: 'gnd', surface: 'sea', subsurface: 'sub' } as const

/** No vehicles (the groups panel while closed). */
const NO_UIDS: ReadonlySet<number> = new Set()

/** The console. `onMenu`: the launcher's main menu (`make start`), shown over it; absent when
 *  the stack was started without the launcher. */
/** The sensor reach answer while nothing is asked for (the layer off, nothing selected). */
const NO_REACH: { data: ReachAnswer | null; error: string | null } = { data: null, error: null }

export default function App({ onMenu }: { onMenu?: () => void }) {
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
  // the track under the pointer (drawn in full while it is), and the tracks the person pinned to
  // be drawn in full (focus.ts; a view setting of this console only)
  const [hoverTrack, setHoverTrack] = useState<string | null>(null)
  const [pinnedTracks, setPinnedTracks] = useState<ReadonlySet<string>>(() => new Set())
  // the map legend's Show all (HiddenLegend.tsx, R3): each thing drawn dim, unlabelled or folded,
  // in full while on; and what the label budget drew last
  const [showAll, setShowAll] = useState({ tracks: false, areas: false, estimates: false, labels: false, lists: false })
  const toggleAll = useCallback((k: keyof typeof showAll) => setShowAll((s) => ({ ...s, [k]: !s[k] })), [])
  const [labelStats, setLabelStats] = useState<LabelStats | null>(null)
  // what the lists fold (R6), as each reports it: the legend counts it
  const [fleetFolds, setFleetFolds] = useState({ vehicles: 0, groups: 0 })
  const [trackFolds, setTrackFolds] = useState({ tracks: 0, folds: 0 })
  const [orderFolds, setOrderFolds] = useState({ lines: 0, cards: 0 })
  const togglePin = useCallback((id: string) => {
    setPinnedTracks((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }, [])
  const onMapHover = useCallback((layer: string | null, object: unknown) => {
    const id = layer && TRACK_HOVER_LAYERS.has(layer) ? ((object as { id?: string } | null)?.id ?? null) : null
    setHoverTrack((h) => (h === id ? h : id))
  }, [])
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
  const [showWeather, setShowWeather] = useState(false)
  const [showSpace, setShowSpace] = useState(false)
  // the right dock (dock.ts): one drawer at a time over the map's right edge, the top bar's buttons
  // its tabs: the Inbox; the AI watch officer (TaskingPanel.tsx: requests it carries out, never an
  // approval or a decision); rules 5's Queue of orders held for vehicles the link can't reach now; the situation
  // report (Sitrep.tsx: read-only, the wide one); Swarms (docs/DECISIONS.md, "Swarm units")
  const [dock, setDock] = useState<DockId | null>(null)
  const showInbox = dock === 'inbox'
  const showQueue = dock === 'queue'
  const showSwarms = dock === 'swarms'
  const showSitrep = dock === 'sitrep'
  const showTasking = dock === 'officer'
  const taskingApi = useTasking()
  // the message to the officer being written: kept here so it outlives its drawer (switching to the
  // Inbox to decide and back keeps it)
  const [officerDraft, setOfficerDraft] = useState('')
  // the Swarms drawer's card to bring into view (a chip clicked on the map), and the group whose
  // chip or card is under the pointer
  const [swarmFocus, setSwarmFocus] = useState<{ id: string; n: number } | null>(null)
  const [hoverGroup, setHoverGroup] = useState<string | null>(null)
  // Space held: every group's orders drawn (bd murmur-4nik; else a group's only while it is
  // selected or hovered), except while typing or on a focused control, whose own key it is
  const [allOrders, setAllOrders] = useState(false)
  useEffect(() => {
    const typing = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null
      return !!t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT', 'BUTTON'].includes(t.tagName))
    }
    const down = (e: KeyboardEvent) => {
      if (e.code !== 'Space' || e.ctrlKey || e.metaKey || e.altKey || typing(e)) return
      e.preventDefault()
      if (!e.repeat) setAllOrders(true)
    }
    const up = (e: KeyboardEvent) => e.code === 'Space' && setAllOrders(false)
    const off = () => setAllOrders(false)
    window.addEventListener('keydown', down)
    window.addEventListener('keyup', up)
    window.addEventListener('blur', off)
    return () => {
      window.removeEventListener('keydown', down)
      window.removeEventListener('keyup', up)
      window.removeEventListener('blur', off)
    }
  }, [])
  // a group lit up for a moment after the group strip centred the map on it
  const [flashGroup, setFlashGroup] = useState<string | null>(null)
  useEffect(() => {
    if (!flashGroup) return
    const t = window.setTimeout(() => setFlashGroup(null), 4000)
    return () => window.clearTimeout(t)
  }, [flashGroup])
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
  // the radio sites the coverage is judged from, marked while the Links layer is on
  const stationLayers = useMemo(
    () => (showCoverage ? buildStationLayers(coverageGrid?.sources.ground_stations ?? []) : []),
    [showCoverage, coverageGrid],
  )
  const [linkRoute, setLinkRoute] = useState<LinkRouteDrawing | null>(null)
  const [mapView, setMapView] = useState<{ center: LngLat; bounds: [number, number, number, number]; zoom: number } | null>(null)
  // bd murmur-vr17: every animated effect at once by default (each its own toggle), and a colour
  // field as an extra, off by default; the regions (weatherField.ts) drawn by the map (weatherMap.ts)
  const [weatherMode, setWeatherMode] = useState<WeatherMode>('off')
  const [weatherEffects, setWeatherEffects] = useState<Effects>(savedEffects)
  const weatherOn = weatherMode !== 'off' || Object.values(weatherEffects).some(Boolean)
  const weatherField = useWeatherGrid(mapView?.bounds ?? null, weatherOn)
  const weatherView = useMemo<WeatherView | null>(() => {
    const regions = weatherOn ? regionsOf(weatherField.grid) : null
    return regions ? { regions, effects: weatherEffects, field: weatherMode === 'off' ? null : weatherMode } : null
  }, [weatherField.grid, weatherMode, weatherEffects, weatherOn])
  // sensor reach now (bd murmur-4tq, sensorReach.ts): rings on the map for the selected vehicles
  // (or all, or none) and a badge on each vehicle whose reach is under half its clear day's
  const [reachMode, setReachMode] = useState<ReachMode>('selected')
  const [reachDomain, setReachDomain] = useState<DomainChoice>('auto')
  // only what the rings show, while they show (bd murmur-jvdc: the whole fleet's every 3 s was
  // a fifth of the bridge's time at Saturation's 3,602): the selected vehicles, or those in view
  const reachUrl = useMemo(() => reachQuery(reachMode, selected, mapView?.bounds ?? null), [reachMode, selected, mapView?.bounds])
  const reachPoll = usePoll<ReachAnswer>(reachUrl, 3000)
  // nothing asked for: no answer shown, not the last one asked
  const reach = reachUrl ? reachPoll : NO_REACH
  // rules 27: our minefields not yet inert (minefields.ts), drawn with the reach rings
  const minefields = usePoll<MinefieldsAnswer>('/api/minefields', 5000)
  const [drawing, setDrawing] = useState<LngLat | 'start' | null>(null)
  const [box, setBox] = useState<LngLat[] | null>(null)
  const [spaceSel, setSpaceSel] = useState<SpaceSelection>({ scene: null, norad: null })
  // the engagement nomination form (Phase E), opened from a declared-hostile track or an
  // armed platform, and the preview of the platform it would use
  // the nomination form: opened from a track, a platform, or (murmur-8xe) a pending nomination to replace
  const [engageForm, setEngageForm] = useState<{ track?: string; uid?: number; replaces?: number } | null>(null)
  // a notice for the inbox from the form ("i5 replaced by i6")
  const [inboxNotice, setInboxNotice] = useState<{ n: number; text: string; tone: 'ok' | 'bad' } | null>(null)
  const [engageDraft, setEngageDraft] = useState<EngagePreview | null>(null)
  // murmur-12e: why the chosen platform's keep-out blocks (its regions on the map)
  const [engageKeepOuts, setEngageKeepOuts] = useState<KeepOutExplanation | null>(null)
  // the nomination form's latest checks on its target, for the tutorial's coach card
  const [engageChecks, setEngageChecks] = useState<{ track: string; checks: Check[] } | null>(null)

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
  // Escape clears the selection (the Fleet list's Clear), unless an order is being drawn (that
  // Escape cancels the drawing), a panel that Escape closes is open (the vehicle details, the
  // nomination form, the space timeline, the platforms: that Escape is theirs, and a hundred
  // selected must not go with it) or the key is a text field's or a control's own
  const anySelected = selected.size > 0
  const escapeTaken = drawing != null || detailsUid != null || engageForm != null || showSpace || showPlatforms
  useEffect(() => {
    if (!anySelected || escapeTaken) return
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null
      if (e.key !== 'Escape' || e.defaultPrevented || (t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName)))) return
      setSelected(new Set())
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [anySelected, escapeTaken])
  useEffect(() => {
    if (!drawing) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setDrawing(null)
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [drawing])
  const onView = useCallback((center: LngLat, bounds: [number, number, number, number], zoom: number) => setMapView({ center, bounds, zoom }), [])

  const allVehicles = useMemo(() => state?.vehicles ?? [], [state?.vehicles])
  // rules 30: our vehicles silent, presumed lost, confirmed lost (the review's lists, only in a
  // run under rules 30), and where the keep-out counts each silent one (polled while any is)
  const nodeLoss = useMemo(() => nodeLossView(decisions?.inbox.review), [decisions?.inbox.review])
  const anyStale = useMemo(() => allVehicles.some(isStale), [allVehicles])
  const nodeRows = useNodes(nodeLoss.on && (anyStale || nodeLoss.lost.size > 0 || nodeLoss.review.size > 0))
  const spent = useMemo(() => new Set(decisions?.spent ?? []), [decisions?.spent])
  const vehicles = useMemo(() => spent.size ? allVehicles.filter((v) => !spent.has(v.uid)) : allVehicles, [allVehicles, spent])
  // the swarm summary (GET /api/swarms, about once a second) while the fleet has swarms or the
  // drawer is open; its outlines on the map are rebuilt only when a new summary comes
  const hasSwarms = useMemo(() => allVehicles.some((v) => swarmOf(v) != null), [allVehicles])
  const swarmFeed = useSwarms(hasSwarms || showSwarms)
  // the operator's own groups (groups.ts): shared through the bridge, never an order
  const groupsApi = useGroups()
  // the uids in the fleet now, for the groups panel (only while it can be seen)
  const presentUids = useMemo(() => (showSwarms ? new Set(allVehicles.map((v) => v.uid)) : NO_UIDS), [showSwarms, allVehicles])
  const swarmSel = useMemo(() => selectedSwarm(swarmFeed.doc?.swarms, selected), [swarmFeed.doc, selected])
  // the name an order to exactly a group carries as its `group.swarm` label (rules 34: a label,
  // never a decision input): one of the operator's own groups by its name, else a fleet swarm by its
  // id, as the situations' opening orders label theirs
  const groupLabel = useMemo(() => groupOfSelection(groupsApi.groups, selected)?.name ?? swarmSel, [groupsApi.groups, selected, swarmSel])
  // each group's marks on the map (bd murmur-4nik): code, colour, chip, from each new summary
  const marks = useMemo(() => groupMarks(swarmFeed.doc), [swarmFeed.doc])
  // the groups' outlines round their members heard now (a stale one is left out, never wrapped)
  const swarmShapes = useMemo(() => {
    if (!marks.list.length) return []
    const heard = new Map<number, LngLat>()
    for (const v of allVehicles) if (v.position && !isStale(v) && marks.ofUid.has(v.uid)) heard.set(v.uid, v.position)
    return swarmOutlines(marks, (uid) => heard.get(uid) ?? null, swarmSel, hoverGroup ?? flashGroup)
  }, [marks, allVehicles, swarmSel, hoverGroup, flashGroup])
  // a group order's dry run (bd murmur-4nik): its slots drawn as ghosts in the group's colour when
  // its members are one group's
  const shownRoute = useMemo(() => {
    if (!linkRoute?.slots?.length) return linkRoute
    const ids = new Set(linkRoute.slots.map((sl) => marks.ofUid.get(Number(sl.uid))?.id ?? ''))
    const only = ids.size === 1 ? marks.byId.get([...ids][0]) : undefined
    return only ? { ...linkRoute, slotColor: only.rgb } : linkRoute
  }, [linkRoute, marks])
  // the chips on the map: a click selects the group's members (as the Fleet list's group row does)
  // and brings its card into view in the Swarms drawer; nothing is sent
  const groupsOnMap = useMemo<GroupsOnMap | null>(() => {
    if (!marks.list.length) return null
    return {
      marks,
      selected: swarmSel,
      hovered: hoverGroup ?? flashGroup,
      onHover: setHoverGroup,
      onSelect: (id) => {
        const m = marks.byId.get(id)
        if (!m) return
        setSelected(new Set(m.members))
        setRevealVeh({ ids: m.members.map(String) })
        setDock('swarms')
        setSwarmFocus((f) => ({ id, n: (f?.n ?? 0) + 1 }))
      },
      detail: (id) => {
        const m = marks.byId.get(id)
        if (!m) return []
        const members = new Set(m.members)
        const byUid = new Map(allVehicles.filter((v) => members.has(v.uid)).map((v) => [v.uid, v]))
        const lostNow = m.members.filter((u) => {
          const v = byUid.get(u)
          return v != null && isStale(v)
        })
        return missingLines(m, lostNow, (u) => (byUid.has(u) ? callsign(byUid.get(u)!) : `vehicle ${u}`))
      },
    }
  }, [marks, swarmSel, hoverGroup, flashGroup, allVehicles])
  const reachRings = useMemo(() => {
    const fields = minefieldFeatures(minefields.data)
    if (reachMode === 'off' || (reachMode === 'selected' && selected.size === 0)) {
      return fields.length ? { type: 'FeatureCollection' as const, features: fields } : null
    }
    const positions = new Map<number, LngLat>()
    for (const v of vehicles) if (v.position) positions.set(v.uid, v.position)
    const rings = reachFeatures(reach.data, positions, reachMode === 'selected' ? selected : null, reachDomain)
    if (!fields.length) return rings
    return { type: 'FeatureCollection' as const, features: [...(rings?.features ?? []), ...fields] }
  }, [reach.data, minefields.data, vehicles, selected, reachMode, reachDomain])
  const reachBadges = useMemo(() => {
    const out = new Map<number, { text: string; title: string }>()
    if (!reach.data?.available || reach.data.stale) return out
    for (const v of reach.data.vehicles) {
      const b = reachBadge(v, reachDomain)
      if (b) out.set(v.uid, b)
    }
    return out
  }, [reach.data, reachDomain])
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
  // rules 21: with one vehicle selected and Identify chosen, a click on a contact makes it the
  // target (rules 22: any live one, an identified one for a second look; a stale one says why not)
  const identifyRef = useRef<{ on: boolean; tracks: Track[]; staleAfterS: number }>({ on: false, tracks: [], staleAfterS: 10 })
  const onTrackClick = useCallback(
    (id: string) => {
      const pick = identifyRef.current
      if (pick.on) {
        const t = pick.tracks.find((x) => x.id === id)
        const stale = !t || isTrackStale(t, now(), pick.staleAfterS)
        const note = stale ? `${id} is not live (no fresh detection): pick a contact our sensors see now` : null
        setDraft((d) => (d.mode === 'identify' ? { ...d, track: note ? d.track : id, note } : d))
        return
      }
      if (selectedTrackRef.current === id) setSelectedTrack(null)
      else revealTrackId(id)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [revealTrackId],
  )
  // what is drawn in full on the map (focus.ts, docs/research/saturation-display.md R1): the
  // selected, hovered and pinned tracks, hostile and suspect ones, declared or flagged, a review
  // item's, an engagement's target or blocker; the rest dim
  const reviewIds = useMemo(() => reviewTracks(decisions?.inbox.review, decisions?.inbox.blocking), [decisions?.inbox.review, decisions?.inbox.blocking])
  const engageIds = useMemo(() => engagementTracks(decisions?.intents, engageKeepOuts, engageForm?.track), [decisions?.intents, engageKeepOuts, engageForm?.track])
  const trackFocus = useMemo(
    () => focusMap(tracks?.tracks ?? [], tracks?.lost ?? [], { selected: selectedTrack, hovered: hoverTrack, pinned: pinnedTracks, review: reviewIds, engagement: engageIds }),
    [tracks, selectedTrack, hoverTrack, pinnedTracks, reviewIds, engageIds],
  )
  // the dim rule holds once the picture is crowded; a few contacts are each drawn in full, as before
  const crowded = (tracks?.tracks.length ?? 0) + (tracks?.lost?.length ?? 0) > CROWD
  const trackAll = useMemo(() => ({ tracks: showAll.tracks || !crowded, areas: showAll.areas || !crowded }), [showAll.tracks, showAll.areas, crowded])
  const trackLayers = useCallback<MapLayers>(
    (screen, plan) =>
      buildTrackLayers({
        tracks: tracks?.tracks ?? [],
        lost: tracks?.lost ?? [],
        nowMs: now(),
        selected: selectedTrack,
        staleAfterS: tracks?.stale_after_s ?? 10,
        onClick: onTrackClick,
        // picking an identify target: ring the contacts it can go to (every live one)
        pickable:
          draft.mode === 'identify' && selected.size === 1
            ? new Set((tracks?.tracks ?? []).filter((t) => !isTrackStale(t, now(), tracks?.stale_after_s ?? 10)).map((t) => t.id))
            : null,
        screen,
        focus: trackFocus,
        showAll: trackAll,
        plan,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tracks, selectedTrack, trackTick, onTrackClick, draft.mode, selected, trackFocus, trackAll],
  )
  identifyRef.current = { on: draft.mode === 'identify' && selected.size === 1, tracks: tracks?.tracks ?? [], staleAfterS: tracks?.stale_after_s ?? 10 }
  // rules 21: the identify draft on the map: its orbit's ring on the target where it is now, a
  // line from the vehicle
  const mapDraft = useMemo<Draft>(() => {
    if (draft.mode !== 'identify' || !draft.track || selected.size !== 1) return draft.mode === 'identify' ? { ...draft, center: null } : draft
    const t = tracks?.tracks.find((x) => x.id === draft.track)
    const v = vehicles.find((x) => selected.has(x.uid))
    return { ...draft, center: t ? trackPredicted(t, now()) : null, from: v?.position ?? null }
  }, [draft, tracks, vehicles, selected, now])
  // engagements in play (and the form's draft) on the map: paths, effect, keep-out, ellipse
  const allIntents = decisions?.intents
  const impactNowMs = now()
  const engageTick = Math.floor(impactNowMs / 1000)
  // the platform catalog (/api/platforms): the one-way platforms' tags below, and the profile a
  // munition is drawn by, which the intent record does not carry
  const platformCatalog = usePlatformLimits()
  // Strikes (bd murmur-64z): a munition's impact, the time of flight after its release; a
  // platform that is its own munition strikes at the release.
  const impacts = useMemo<ImpactEvent[]>(() => (allIntents ?? []).flatMap((i) => {
    const effect = isEngage(i) ? i.engagement?.effect : null
    const m = i.engagement?.munition
    const platform = allVehicles.find((v) => v.uid === Number(i.uids[0]))
    const t = m ? m.impact_t : effect?.t
    const profile = m ? loadoutProfile(platformCatalog?.get(platform?.identity?.platform ?? '')?.loadout, i.engagement?.weapon?.name ?? m.name) : undefined
    // rules 35: a salvo's every round has a flight and a strike of its own (the release is the
    // first of them), each drawn and flashed on its own. Keyed `r{n}` from the first flight on,
    // so round 1's key does not change when a later round joins (which would re-frame it afresh)
    const flights = i.engagement?.fire?.salvo?.flights
    if (effect && flights && flights.length >= 1) {
      return flights.map((f, n) => ({ key: `i${i.id}r${n + 1}@${f.impact_t}`, intentId: i.id, point: f.impact_point, t: f.impact_t, radius_m: effect.radius_m, munition: f, kind: i.engagement?.weapon?.kind, profile,
        platform: platform ? callsign(platform) : undefined, track: i.engagement?.snapshot?.track_id ?? i.engage?.track,
        assessment: i.engagement?.assessment }))
    }
    return effect && t != null ? [{ key: `i${i.id}@${t}`, intentId: i.id, point: m ? m.impact_point : effect.point, t, radius_m: effect.radius_m, munition: m, kind: i.engagement?.weapon?.kind, profile,
      platform: platform ? callsign(platform) : undefined, track: i.engagement?.snapshot?.track_id ?? i.engage?.track,
      assessment: i.engagement?.assessment }] : []
  }), [allIntents, allVehicles, platformCatalog])
  const seenImpacts = useRef(new Set<string>())
  /** frame the strike: its effect circle, with room around it */
  const focusImpact = useCallback((impact: ImpactEvent) => {
    const ring = circle(impact.point, Math.max(impact.radius_m * 4, 450)).filter((_, k) => k % 8 === 0)
    setFit((f) => ({ points: [impact.point, ...ring], n: (f?.n ?? 0) + 1, padRight: fitPad(dock) }))
  }, [dock])
  /** frame a munition's flight: its launch point (up where it was released), the impact point
   *  and the effect circle, so the operator watches it fly and strike */
  const focusFlight = useCallback((impact: ImpactEvent) => {
    const m = impact.munition
    if (!m) return focusImpact(impact)
    const ring = circle(impact.point, Math.max(impact.radius_m * 4, 450)).filter((_, k) => k % 8 === 0)
    setFit((f) => ({ points: [m.launch_point, impact.point, ...ring], aloft: [[...m.launch_point, m.launch_alt_m]], n: (f?.n ?? 0) + 1, padRight: fitPad(dock) }))
  }, [dock, focusImpact])
  // the view goes to a munition at its launch (its flight and strike), to any other strike as
  // it lands; one seen too late for either (a console that reconnects) is left alone
  useEffect(() => {
    for (const impact of impacts) {
      if (seenImpacts.current.has(impact.key)) continue
      const from = impact.munition?.launch_t ?? impact.t
      if (impactNowMs < from) continue // not yet: looked at again on the next tick
      seenImpacts.current.add(impact.key)
      if (impactNowMs - impact.t < 4000) (impact.munition ? focusFlight : focusImpact)(impact)
    }
  }, [impacts, impactNowMs, focusImpact, focusFlight])
  // the status line: a munition on its way (the next to land) before any strike's status, which
  // has the line once nothing is in flight
  const flyingImpact = impacts.filter((x) => x.munition && impactNowMs >= x.munition.launch_t && impactNowMs < x.t).sort((a, b) => a.t - b.t)[0]
  const visibleImpact = flyingImpact ? undefined : impacts.filter((impact) => impactNowMs >= impact.t && impactNowMs - impact.t < IMPACT_STATUS_MS).sort((a, b) => b.t - a.t)[0]
  // Hostile acts our sensors observed (rules 9), as fire events on the map (hostileAct.ts): from
  // where our sensors placed each act's author to its victim, where this console had the victim
  // when it first saw the act. Recomputed when the inbox's acts change; the vehicles and tracks
  // are read as they are then.
  const actsSeen = useRef(new Map<string, Seen>())
  const victimsRef = useRef({ vehicles: allVehicles, tracks: tracks?.tracks ?? [] })
  victimsRef.current = { vehicles: allVehicles, tracks: tracks?.tracks ?? [] }
  const inboxActs = decisions?.inbox.acts
  // Opponent weapons (weapon.ts, docs/OPPONENT-WEAPONS.md), from the bridge's weapon picture: the
  // vehicle each heads for (the estimate's end) is kept where this console first had it, as an
  // act's victim is; a struck one where it was struck
  const weapons = useMemo<WeaponTrack[]>(() => tracks?.weapons ?? [], [tracks?.weapons])
  const weaponActsKey = [...weaponReports(weapons)].sort().join()
  const weaponActs = useMemo(() => new Set(weaponActsKey ? weaponActsKey.split(',') : []), [weaponActsKey])
  const weaponVictims = useRef(new Map<string, LngLat>())
  const weaponVictim = useCallback((w: WeaponTrack): LngLat | null => {
    const key = `${w.id}@${w.launch?.t ?? w.first_cue_t ?? w.struck?.t ?? 0}`
    const kept = weaponVictims.current.get(key)
    if (kept) return kept
    const uid = w.aimed_at ?? w.struck?.uid
    const v = uid ? victimsRef.current.vehicles.find((x) => String(x.uid) === uid) : undefined
    if (!v?.position) return null
    weaponVictims.current.set(key, v.position)
    return v.position
  }, [])
  const namer = useRef<{ vehicles: Vehicle[]; name: (uid: string) => string } | null>(null)
  const nameOf = useCallback((uid: string) => {
    const vs = victimsRef.current.vehicles
    if (namer.current?.vehicles !== vs) namer.current = { vehicles: vs, name: vehicleNamer(vs) }
    return namer.current.name(uid)
  }, [])
  const actDraws = useMemo<ActDrawing[]>(() => {
    const { vehicles: vs, tracks: ts } = victimsRef.current
    const victimAt = (a: Against, t: number): Victim | null => {
      if (a.kind === 'vehicle') {
        const v = vs.find((x) => String(x.uid) === a.uid)
        return v?.position ? { at: v.position, alt: shownAlt(v) } : null
      }
      if (a.kind === 'track') {
        const tr = ts.find((x) => x.id === a.track)
        return tr ? { at: trackPredicted(tr, t), alt: 0 } : null
      }
      return null
    }
    return actDrawings(inboxActs ?? [], actsSeen.current, now(), victimAt, vehicleNamer(vs), weaponActs)
  }, [inboxActs, now, weaponActs])
  // Dismiss: the acts drawn so far leave the map (and the banner) before their minute is up
  const [actsDismissedAt, setActsDismissedAt] = useState<number | null>(null)
  // a weapon's banner (INCOMING, then the strike) before an act's: it says more
  const weaponBan = weaponBanner(weapons, impactNowMs, nameOf, weaponVictim)
  const actBanner = weaponBan ? null : bannerAct(actDraws, impactNowMs, actsDismissedAt)
  /** frame an act: its author's fix with room for its error, and its victim */
  const focusAct = useCallback((d: ActDrawing) => {
    const ring = circle(d.author, Math.max(d.sigma_m * 3, 300)).filter((_, k) => k % 8 === 0)
    const points = d.victim ? [d.author, d.victim.at, ...ring] : [d.author, ...ring]
    const aloft: [number, number, number][] | undefined = d.victim && d.victim.alt > 50 ? [[d.victim.at[0], d.victim.at[1], d.victim.alt]] : undefined
    setFit((f) => ({ points, aloft, n: (f?.n ?? 0) + 1, padRight: fitPad(dock) }))
  }, [dock])
  // platforms that are their own munition (catalog expended_by_effect) on their way: tagged on
  // the map, so the operator sees what is on the way (bd murmur-64z)
  // its tag says what it is doing to which track: "ONE-WAY > T-12" on its way, "STRIKING > T-12"
  // from the engine's release (the engagement's `effect`) to its strike
  const oneWayKey = JSON.stringify(
    (allIntents ?? [])
      .filter((i) => isEngage(i) && i.state === 'dispatched')
      .flatMap((i): [number, string][] => {
        const uid = Number(i.uids[0])
        const platform = allVehicles.find((v) => v.uid === uid)?.identity?.platform
        if (platform == null || !platformCatalog?.get(platform)?.expended_by_effect) return []
        const track = i.engagement?.snapshot?.track_id ?? i.engage?.track
        return [[uid, `${i.engagement?.effect ? 'STRIKING' : 'ONE-WAY'}${track ? ` > ${track}` : ''}`]]
      })
      .sort((a, b) => a[0] - b[0]),
  )
  const oneWay = useMemo(() => new Map<number, string>(JSON.parse(oneWayKey) as [number, string][]), [oneWayKey])
  // Gun bursts (rules 26): the last burst the bridge reported for each gun engagement, from its
  // platform to the target track's latest fix; nothing for a burst not reported
  const tracers = useMemo<TracerSpec[]>(
    () =>
      (allIntents ?? []).flatMap((i) => {
        const fire = i.engagement?.fire
        const id = i.engagement?.snapshot?.track_id ?? i.engage?.track
        const tr = id ? tracks?.tracks.find((x) => x.id === id) : undefined
        if (!isEngage(i) || !fire || !(fire.bursts > 0) || !tr) return []
        return [{ fire: { bursts: fire.bursts, rounds: fire.rounds, last_t: fire.last_t }, uid: Number(i.uids[0]), to: tr.position, toAlt: tr.alt_m ?? 0 }]
      }),
    [allIntents, tracks],
  )
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
        effect: i.engagement?.effect, impactT: i.engagement?.munition?.impact_t, assessment: i.engagement?.assessment?.result })
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
    // the form sits in the right rail, beside the map: only the dock covers its right edge
    setFit((f) => ({ points: [...engageDraft.path, ...ring], n: (f?.n ?? 0) + 1, padRight: fitPad(dock) }))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draftKey])
  // silent vehicles' estimates (murmur-4y9): the bridge's motion models evaluated every clock
  // tick, so the callback changes identity each tick only while something is silent; none for a
  // vehicle an operator confirmed lost (rules 30: what it last reported is not where it went)
  const silent = useMemo(() => (state ? estimated(vehicles).filter((v) => !nodeLoss.lost.has(v.uid)) : []), [vehicles, state, nodeLoss])
  const estimateTick = silent.length ? Math.floor(now() / 500) : 0
  // in focus for the estimates: selected, or details open (both hypotheses, filled)
  const vehicleFocus = useMemo(() => (detailsUid == null ? selected : new Set([...selected, detailsUid])), [selected, detailsUid])
  // a few silent vehicles: both estimates of each, filled, as before
  const estimateAll = showAll.estimates || silent.length <= CROWD_SILENT
  const estimateLayers = useCallback<MapLayers>(
    (_screen, plan) => (silent.length ? buildEstimateLayers({ vehicles: silent, nowMs: now(), focus: detailsUid, inFocus: vehicleFocus, showAll: estimateAll, plan }) : []),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [silent, estimateTick, detailsUid, vehicleFocus, estimateAll],
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
  // rules 30: where each silent vehicle can be as the keep-out counts it, under the estimates
  const nodeRegionLayers = useMemo(() => (nodeRows.size ? buildNodeRegionLayers(nodeRows.values(), vehicleFocus) : []), [nodeRows, vehicleFocus])
  const mapLayers = useCallback<MapLayers>(
    (screen, plan) => [
      ...nodeRegionLayers,
      ...estimateLayers(screen, plan),
      ...trackLayers(screen, plan),
      ...keepOutLayers,
      ...(engageDrawings.length ? buildEngageLayers(engageDrawings, now()) : []),
      ...buildOrderLayers({ queued: queuedIntents ?? [], intents: allIntents ?? [], taskings: state?.taskings ?? [], vehicles, nowMs: now() }, plan),
      ...stationLayers,
      // the groups' outlines last, over the tracks' uncertainty (hundreds of contacts' rings would
      // hide them) and under the vehicles; their names are chips over everything (GroupChips.tsx)
      ...buildSwarmLayers(swarmShapes),
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [swarmShapes, nodeRegionLayers, estimateLayers, trackLayers, keepOutLayers, stationLayers, engageDrawings, queuedIntents, allIntents, state?.taskings, vehicles, orderTick],
  )
  // every text layer's labels for the one label budget (declutter.ts, docs/research/
  // saturation-display.md R2), with our vehicles' (MapView's): the same data and ticks as the
  // layers above, so a new plan comes with them
  const mapLabels = useCallback(
    (screen: MapScreen) => [
      ...(silent.length ? estimateLabelCandidates({ vehicles: silent, nowMs: now(), focus: detailsUid, inFocus: vehicleFocus, showAll: estimateAll }, screen) : []),
      ...trackLabelCandidates({ tracks: tracks?.tracks ?? [], lost: tracks?.lost ?? [], nowMs: now(), staleAfterS: tracks?.stale_after_s ?? 10, screen, focus: trackFocus }),
      ...orderLabelCandidates({ queued: queuedIntents ?? [], intents: allIntents ?? [], taskings: state?.taskings ?? [], vehicles, nowMs: now() }, screen),
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [silent, estimateTick, detailsUid, vehicleFocus, estimateAll, tracks, trackTick, trackFocus, queuedIntents, allIntents, state?.taskings, vehicles, orderTick],
  )
  const onLabelStats = useCallback((s: LabelStats) => setLabelStats(s), [])
  const n = (x: number) => x.toLocaleString('en-US')
  // What is hidden, counted (R3): each row only while it hides something or Show all is on
  const hiddenRows = useMemo<HiddenRow[]>(() => {
    const rows: HiddenRow[] = []
    const ts = tracks?.tracks ?? []
    const lostList = tracks?.lost ?? []
    if (crowded && ts.length) {
      const inFocus = ts.filter((t) => trackFocus.has(t.id)).length
      rows.push({
        id: 'tracks', what: 'Tracks',
        count: showAll.tracks ? `all ${n(ts.length)} in full` : `${n(ts.length - inFocus)} of ${n(ts.length)} dimmed`,
        how: 'A track out of focus has its ring outlined, not filled, a short stem instead of its column, and an unknown one a dimmer symbol. In focus: selected, hovered, pinned, hostile or suspect, declared, flagged, a review item\'s, an engagement\'s target or blocker.',
        hidden: ts.length - inFocus > 0, all: showAll.tracks, onToggle: () => toggleAll('tracks'),
      })
    }
    if (crowded && lostList.length) {
      const inFocus = lostList.filter((l) => trackFocus.has(l.id)).length
      rows.push({
        id: 'areas', what: 'Lost areas',
        count: showAll.areas ? `all ${n(lostList.length)} areas in full` : `${n(lostList.length - inFocus)} of ${n(lostList.length)} outlined`,
        how: "A lost contact out of focus has its possible area (where every engagement keep-out still counts it) drawn as a faint outline, not filled. The keep-out checks count every one either way.",
        hidden: lostList.length - inFocus > 0, all: showAll.areas, onToggle: () => toggleAll('areas'),
      })
    }
    if (silent.length > CROWD_SILENT) {
      const hid = hiddenHypotheses(silent, now(), vehicleFocus, showAll.estimates)
      rows.push({
        id: 'estimates', what: 'Estimates',
        count: showAll.estimates ? `all ${silent.length} in full` : `${silent.length} outlined${hid ? `, ${hid} second hidden` : ''}`,
        how: 'A silent vehicle out of focus (not selected, details closed) shows its leading estimate only, its region outlined. Select it for both, filled.',
        hidden: true, all: showAll.estimates, onToggle: () => toggleAll('estimates'),
      })
    }
    const foldedAny = fleetFolds.vehicles + trackFolds.tracks + orderFolds.lines > 0
    if (foldedAny || showAll.lists) {
      const parts = [
        fleetFolds.vehicles ? `${n(fleetFolds.vehicles)} vehicles` : '',
        trackFolds.tracks ? `${n(trackFolds.tracks)} tracks` : '',
        orderFolds.lines ? `${n(orderFolds.lines)} order lines` : '',
      ].filter(Boolean)
      rows.push({
        id: 'lists', what: 'Lists',
        count: showAll.lists ? 'every fold open' : `${parts.join(', ')} folded`,
        how: 'The fleet list shows one row per group with its members counted by state and only its exceptions (lost link, damaged, an alert of its own, selected, details open); the track list what is in focus first and the rest folded by affiliation and kind; the command log one card per order with its exceptions listed. Each fold opens with a click.',
        hidden: foldedAny, all: showAll.lists, onToggle: () => toggleAll('lists'),
      })
    }
    if (labelStats && (labelStats.none > 0 || labelStats.all)) {
      const drawn = labelStats.full + labelStats.short
      rows.push({
        id: 'labels', what: 'Labels',
        count: labelStats.all ? `all ${n(labelStats.inView)} in full` : `${n(drawn)} of ${n(labelStats.inView)} labelled`,
        how: 'Labels never overlap: focus first in full, then group names, then ours with a lost link, then the rest by their short label where it fits. Hover a symbol for its full block.',
        hidden: labelStats.none > 0, all: labelStats.all, onToggle: () => toggleAll('labels'),
      })
    }
    return rows
    // now: the hidden estimates are counted at this tick
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tracks, crowded, trackFocus, showAll, silent, estimateTick, vehicleFocus, labelStats, toggleAll, fleetFolds, trackFolds, orderFolds])
  // the fleet list's exceptions: a damaged vehicle (its inbox alert), one with an alert of its own
  const listAlerts = decisions?.inbox.alerts
  const damagedUids = useMemo(() => new Set((listAlerts ?? []).filter((a) => (a.kind as string) === 'damaged').map((a) => a.uid)), [listAlerts])
  // the group strip (bd murmur-4nik): each group's state, its order's or what most of its members do
  const groupStates = useMemo(() => {
    const out = new Map<string, GroupState>()
    if (!marks.list.length) return out
    const latest = latestOps(state?.taskings ?? [])
    const byUid = new Map(allVehicles.map((v) => [v.uid, v]))
    for (const m of marks.list) {
      const counts: Partial<Record<string, number>> = {}
      for (const u of m.members) {
        const v = byUid.get(u)
        if (!v) continue
        const st = vehicleState(v, latest, damagedUids)
        counts[st] = (counts[st] ?? 0) + 1
      }
      out.set(m.id, groupState(m, counts, swarmFeed.doc?.t ?? 0))
    }
    return out
  }, [marks, allVehicles, state?.taskings, damagedUids, swarmFeed.doc])
  // group alarms (bd murmur-4nik): under strength, suspended, a re-form refused, raised on the map's
  // banner once held ALARM_HOLD_MS (a link's blip never flashes one), the worst first
  const [alarmSeen, setAlarmSeen] = useState<ReadonlyMap<string, AlarmSeen>>(new Map())
  const [alarmDismissed, setAlarmDismissed] = useState<ReadonlySet<string>>(new Set())
  useEffect(() => {
    setAlarmSeen((prev) => stepAlarms(prev, marks.list, swarmFeed.doc?.t ?? 0))
  }, [marks, swarmFeed.doc])
  const groupAlarms = useMemo(() => heldAlarms(alarmSeen, marks, swarmFeed.doc?.t ?? 0, alarmDismissed), [alarmSeen, marks, swarmFeed.doc, alarmDismissed])
  // the strip's click: the map centred on the group's members, the group lit up for a moment
  const showGroup = useCallback(
    (id: string) => {
      const m = marks.byId.get(id)
      if (!m) return
      const members = new Set(m.members)
      const pts = allVehicles.flatMap((v) => (members.has(v.uid) && v.position ? [v.position] : []))
      if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: fitPad(dock) }))
      setFlashGroup(id)
    },
    [marks, allVehicles, dock],
  )
  const alertedUids = useMemo(() => new Set(unfolded(listAlerts ?? []).map((a) => a.uid)), [listAlerts])
  // the full block of a track, lost contact or estimate under the pointer, while its label is not
  const mapTooltip = useCallback(
    (layer: string, object: unknown) =>
      layer === EXCLUDED_PICK_LAYER ? { key: `x:${(object as ExcludedMark).uid}`, text: (object as ExcludedMark).text } : (trackTooltip(layer, object, now()) ?? estimateTooltip(layer, object)),
    [now],
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
    // (a member alert folded into a swarm alert is the swarm card's: not a pin of its own)
    for (const a of unfolded(alerts ?? [])) set.add(a.uid)
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
  // the map's orders: a group's only on demand (selected, hovered, lit up, a member selected, Space)
  const mapTaskings = useMemo(
    () => shownTaskings(state?.taskings ?? [], marks.ofUid, new Set([swarmSel, hoverGroup, flashGroup].filter((x): x is string => x != null)), selected, allOrders),
    [state?.taskings, marks, swarmSel, hoverGroup, flashGroup, selected, allOrders],
  )
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
  // what a person alone is holding up (blocking.ts): the strip, and the links that open its card
  const waiting = useMemo(() => waitingItems(decisions?.inbox.blocking), [decisions?.inbox.blocking])
  // every rule declaration awaiting confirmation and open flag that holds nothing up: the calm
  // pointer under the top bar (R4), which only opens the card
  const pendingReview = useMemo(() => pendingItems(decisions?.inbox.review, decisions?.inbox.blocking), [decisions?.inbox.review, decisions?.inbox.blocking])
  const [reviewFocus, setReviewFocus] = useState<{ id: string; n: number } | null>(null)
  /** the strip's and the Situation panel's link: the item's card in the Inbox, in view (the person
   *  decides there; nothing here decides) */
  const openWaiting = useCallback((id: string) => {
    setDock('inbox')
    setInboxTab('inbox')
    setReviewFocus((f) => ({ id, n: (f?.n ?? 0) + 1 }))
  }, [])
  /** the task panel's link (and a line at the head of another drawer, Awaiting.tsx): the pending
   *  intent in the inbox, or a finished one in Decisions */
  const openIntent = useCallback(
    (id: number) => {
      setDock('inbox')
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
      // an orbit's centre or a patrol's waypoint is for the vehicles selected: none, no draft
      if (selected.size === 0) return
      setDraft((d) => (d.mode === 'orbit' ? { ...d, center: p } : d.mode === 'patrol' || d.mode === 'transit' ? { ...d, route: [...d.route, p] } : d))
    },
    [drawing, selected],
  )

  // last effect of the tree for this state: children's effects (map, list) have run
  useLayoutEffect(() => perfCommitted(), [state])
  useEffect(() => perfUpdateEnd(), [state])

  // rules 10: a passkey invitation is registered first (no sign-in needed)
  if (auth.auth === 'credentials' && invite) return <EnrollScreen invite={invite} onDone={() => setInvite(null)} />
  // a credentials run: nothing but the sign-in until an operator has signed in
  if (auth.auth === 'credentials' && !auth.signed_in) return <LoginScreen />

  // the banners over the map's top: our munition in flight or its strike, an opponent weapon, a
  // hostile act. The drawer the dock shows (430 px over the map's right edge, the Sitrep 760) takes
  // them at its head instead, under its own header: centred on the map they sat over that
  // header and the Review's, its urgent item included (bd murmur-76gv).
  const drawer = dock
  const groupBan = groupAlarms[0]
  const bannersAbove = [flyingImpact?.munition || visibleImpact, weaponBan, actBanner].filter(Boolean).length
  const banners = flyingImpact?.munition || visibleImpact || weaponBan || actBanner || groupBan ? (
    <>
      {flyingImpact?.munition && (
        <div className="impact-status" role="status" data-tutorial="impact-status">
          <span><strong>MUNITION IN FLIGHT</strong>
            {' · '}{flyingImpact.munition.name} from {flyingImpact.platform ?? `ENGAGE i${flyingImpact.intentId}`}{flyingImpact.track ? ` on ${flyingImpact.track}` : ''}{' · '}
            impact in {secondsToImpact(flyingImpact.munition, impactNowMs)} s</span>
          <button onClick={() => focusFlight(flyingImpact)}>View flight</button>
        </div>
      )}
      {visibleImpact && (
        <div className="impact-status" role="status" data-tutorial="impact-status">
          <span><strong>{visibleImpact.assessment ? `ASSESSMENT: ${visibleImpact.assessment.result.toUpperCase()}` : 'STRIKE · IMPACT'}</strong>
            {' · '}{visibleImpact.platform ?? `ENGAGE i${visibleImpact.intentId}`}{visibleImpact.track ? ` on ${visibleImpact.track}` : ''}{' · '}
            {visibleImpact.assessment?.result === 'ceased' ? 'No target detected where expected'
              : visibleImpact.assessment?.result === 'persists' ? 'Target still detected'
              : visibleImpact.assessment?.result === 'unknown' ? 'Outcome unknown'
              : 'Sensor assessment pending'}</span>
          <button onClick={() => focusImpact(visibleImpact)}>View impact</button>
        </div>
      )}
      {weaponBan && (
        <div className={`act-status weapon-status ${weaponBan.tone}${flyingImpact?.munition || visibleImpact ? ' below' : ''}`} role="status" data-weapon={weaponBan.w.id} data-tutorial="weapon-status" title={ESTIMATE_NOTE}>
          <span><strong>{weaponBan.head}</strong>{' · '}{weaponBan.text}</span>
          <button onClick={() => {
            const at = weaponVictim(weaponBan.w)
            const from = weaponBan.w.launch?.point ?? weaponBan.w.fixes[0]?.point ?? at
            if (from) setFit((f) => ({ points: at ? [from, at, ...circle(from, 300).filter((_, k) => k % 8 === 0)] : [from], n: (f?.n ?? 0) + 1, padRight: fitPad(dock) }))
          }}>View</button>
        </div>
      )}
      {actBanner && (
        <div className={`act-status${flyingImpact?.munition || visibleImpact ? ' below' : ''}`} role="status" data-act={actBanner.d.act}>
          <span><strong>{actBanner.d.weapon ? 'HOSTILE ACT OBSERVED' : 'HOSTILE ACT REPORTED'}</strong>
            {' · '}{bannerLine(actBanner.d)}{' · '}act {actBanner.d.act}, {span(impactNowMs - actBanner.d.observedT)} ago
            {actBanner.others ? ` · ${actBanner.others} more in the last minute` : ''}</span>
          <button onClick={() => focusAct(actBanner.d)}>View</button>
          <button className="act-dismiss" onClick={() => setActsDismissedAt(impactNowMs)} title="Take the acts drawn so far off the map (the Inbox's Review keeps them)">Dismiss</button>
        </div>
      )}
      {groupBan && (
        <div className={`act-status group-status ${GROUP_ALARM[groupBan.seen.alarm].tone}${bannersAbove ? ` below${bannersAbove > 1 ? '2' : ''}` : ''}`} role="status" data-group={groupBan.m.id}>
          <span>
            <strong>{GROUP_ALARM[groupBan.seen.alarm].head}</strong>
            {' · '}
            <span className="gcode" style={{ background: groupBan.m.color, color: groupBan.m.ink }}>
              {groupBan.m.code}
            </span>{' '}
            {groupBan.m.id} {groupBan.m.doing}
            {' · '}
            {groupBan.seen.alarm === 'refused' ? `last re-form refused: ${groupBan.m.refused}` : groupBan.m.parts[0]?.text}
            {' · '}seen {span(Math.max(0, (swarmFeed.doc?.t ?? 0) - groupBan.seen.since))}
            {groupAlarms.length > 1 ? ` · ${groupAlarms.length - 1} more group${groupAlarms.length > 2 ? 's' : ''}` : ''}
          </span>
          <button onClick={() => showGroup(groupBan.m.id)}>View</button>
          <button className="act-dismiss" onClick={() => setAlarmDismissed((d) => new Set([...d, alarmKey(groupBan.m.id, groupBan.seen)]))} title="Take this alarm off the banner (the Swarms drawer and the Inbox keep it; a new one comes up again)">
            Dismiss
          </button>
        </div>
      )}
    </>
  ) : null
  // the head of the drawer the dock shows: the map's banners, then (in any drawer but the Inbox,
  // which holds the cards themselves) every order waiting for a person, so no drawer hides one
  const awaitRows = dock && headsWithWaiting(dock) ? awaitingRows(pending ?? [], taskingApi.items, now(), nameOf) : []
  const dockHead = banners || awaitRows.length ? (
    <>
      {banners}
      <Awaiting rows={awaitRows} onOpen={openIntent} />
    </>
  ) : null
  // what the message box can put in at its cursor: the selection (one group by name and size, or a
  // few vehicles by callsign) and the selected track
  const sel = showTasking ? selectionInsert(groupLabel, [...selected], (u) => nameOf(String(u))) : null
  const composeInserts: ComposeInsert[] = [
    ...(sel ? [sel.text != null ? { key: 'sel', label: sel.text.length > 24 ? `${selected.size} selected` : sel.text, text: sel.text } : { key: 'sel', label: `${selected.size} selected`, text: null, why: sel.why }] : []),
    ...(showTasking && selectedTrack ? [{ key: 'track', label: selectedTrack, text: selectedTrack }] : []),
  ]

  return (
    <div className={`app${waiting.length || pendingReview.length ? ' waiting' : ''}`}>
      <PerfMark />
      <header className={`topbar${onMenu ? ' has-menu' : ''}`}>
        {onMenu && (
          <button className="ghost menu-btn" onClick={onMenu} title="The main menu: the scenario running, ending it, and the others to start">
            Menu
          </button>
        )}
        <span className="brand">MURMUR</span>
        <span className="sub">multi-domain swarm C2</span>
        <span className="spacer" />
        {(['air', 'ground', 'surface', 'subsurface'] as const).map((d) => (
          <span className="stat" key={d} title={`${DOMAIN_LABEL[d]} vehicles`}>
            <b>{vehicles.filter((v) => domainOf(v) === d).length}</b> <span className="stat-l">{DOMAIN_LABEL[d].toLowerCase()}</span>
            <span className="stat-s">{STAT_SHORT[d]}</span>
          </span>
        ))}
        <span className={`stat ${lost ? 'alert' : ''}`} title="vehicles with a lost link">
          <b>{lost}</b> <span className="stat-l">lost link</span>
          <span className="stat-s">lost</span>
        </span>
        <span className={`pill ${status}`} title={status === 'connecting' ? 'Connecting to the bridge' : `The bridge is ${status}`}>
          {status === 'connecting' ? 'CONNECTING' : <><span className="pill-w">BRIDGE </span>{status === 'live' ? 'LIVE' : 'DOWN'}</>}
        </span>
        {/* the dock's tabs (dock.ts): one drawer at a time over the map's right edge */}
        <nav className="dock-tabs" aria-label="Drawers over the map's right edge, one at a time">
          <InboxButton decisions={decisions} open={showInbox} hiddenAlerts={hiddenAlerts} onClick={() => setDock((d) => toggleDock(d, 'inbox'))} />
          <TaskingButton api={taskingApi} open={showTasking} onClick={() => setDock((d) => toggleDock(d, 'officer'))} />
          <QueueButton decisions={decisions} open={showQueue} onClick={() => setDock((d) => toggleDock(d, 'queue'))} />
          <button className={`ghost platforms-btn ix-btn ${showSitrep ? 'on' : ''}`} aria-pressed={showSitrep} onClick={() => setDock((d) => toggleDock(d, 'sitrep'))} title="The situation report: the whole picture, top-down, read-only">
            Sitrep
          </button>
          {(hasSwarms || swarmFeed.status === 'ok' || groupsApi.status === 'ok') && (
            <SwarmsButton feed={swarmFeed} mine={hasSwarms || (swarmFeed.doc?.swarms.length ?? 0) > 0 ? null : groupsApi.groups.length} open={showSwarms} onClick={() => setDock((d) => toggleDock(d, 'swarms'))} />
          )}
        </nav>
        <button className={`ghost platforms-btn ${showWeather ? 'on' : ''}`} onClick={() => setShowWeather(v => !v)}>Weather</button>
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
      {(waiting.length > 0 || pendingReview.length > 0) && (
        <div className="wait-rows">
          {waiting.length > 0 && <WaitingStrip items={waiting} now={now} onOpen={openWaiting} />}
          <PendingStrip items={pendingReview} now={now} onOpen={openWaiting} />
        </div>
      )}
      <ErrorBoundary name="Fleet list" className="slot fleet">
      <FleetPanel vehicles={vehicles} selected={selected} onToggle={toggle} reachBadges={reachBadges}
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
        groups={groupsApi}
        reveal={revealVeh}
        swarms={swarmFeed.doc?.swarms}
        taskings={state?.taskings}
        damaged={damagedUids}
        alerted={alertedUids}
        lost={nodeLoss.lost}
        expandAll={showAll.lists}
        onFolded={setFleetFolds}
      />
      </ErrorBoundary>
      <main className={`mapwrap ${showSpace ? 'space-open' : ''}${dock ? ` docked${dock === 'sitrep' ? ' dock-wide' : ''}` : ''}`}>
        <ErrorBoundary name="Map" className="floating">
        <MapView
          vehicles={allVehicles}
          hiddenUids={spent}
          impacts={impacts}
          impactNowMs={impactNowMs}
          bridgeNow={now}
          oneWay={oneWay}
          nodes={nodeLoss}
          tracers={tracers}
          acts={actDraws}
          actsDismissedAt={actsDismissedAt}
          weapons={weapons}
          weaponVictim={weaponVictim}
          nameOf={nameOf}
          taskings={mapTaskings}
          selected={selected}
          draft={mapDraft}
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
          weather={showCoverage ? null : weatherView}
          reach={reachRings}
          linkRoute={shownRoute}
          onHover={onMapHover}
          labels={mapLabels}
          tooltip={mapTooltip}
          allLabels={showAll.labels}
          onLabelStats={onLabelStats}
          groups={groupsOnMap}
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
        <WeatherLegend mode={weatherMode} onMode={m => { setWeatherMode(m); setShowCoverage(false) }}
          effects={weatherEffects} onEffects={fx => { setWeatherEffects(fx); saveEffects(fx); setShowCoverage(false) }}
          grid={weatherField.grid} error={showCoverage ? 'Links layer shown; turn on a weather effect to show the weather' : weatherField.error} onDetails={() => setShowWeather(true)}
          reach={{ mode: reachMode, onMode: setReachMode, domain: reachDomain, onDomain: setReachDomain, answer: reach.data, error: reach.error, selected: selected.size }} />
        {showWeather && <ErrorBoundary name="Weather" className="floating">
          <WeatherPanel center={mapView?.center ?? [-122.4783, 37.8199]} vehicle={selected.size === 1 ? vehicles.find(v => selected.has(v.uid)) ?? null : null} onClose={() => setShowWeather(false)} />
        </ErrorBoundary>}
        {/* a situation runner's status (make situation): shown only while one has posted it */}
        <ErrorBoundary name="Situation" className="floating">
          <SituationPanel decisions={decisions} now={now} onWaiting={openWaiting} />
        </ErrorBoundary>
        {/* the tutorial's coach card (sim/situations/tutorial.toml): only while the situation running is the tutorial */}
        <ErrorBoundary name="Tutorial" className="floating">
          <Tutorial decisions={decisions} tracks={tracks} now={now} onMenu={onMenu} formChecks={engageChecks} nameOf={nameOf} />
        </ErrorBoundary>
        {!drawer && banners}
        <HiddenLegend rows={hiddenRows} drawer={drawer != null} />
        {groupsOnMap && (
          <ErrorBoundary name="Group strip" className="floating">
            <GroupStrip marks={marks.list} states={groupStates} selected={swarmSel} hovered={hoverGroup ?? flashGroup} onHover={setHoverGroup} onShow={showGroup} drawer={drawer != null} />
          </ErrorBoundary>
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
            banners={banners}
            decisions={decisions}
            vehicles={allVehicles}
            now={now}
            tab={inboxTab}
            setTab={setInboxTab}
            selection={inboxSel}
            onFocus={focusInbox}
            onClose={() => setDock((d) => closeDock(d, 'inbox'))}
            mem={alertMem}
            hidden={hiddenAlerts}
            notice={inboxNotice}
            focus={reviewFocus}
            onReplace={(i) => setEngageForm({ replaces: i.id })}
            cause={(i) => {
              const link = turnOf(i, taskingApi.items)
              return link ? <TurnCause link={link} onShow={() => setDock('officer')} /> : null
            }}
            onTrack={(id) => {
              setDock((d) => closeDock(d, 'inbox'))
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
            banners={dockHead}
            decisions={decisions}
            vehicles={vehicles}
            now={now}
            onFocus={(uids) => {
              setSelected(new Set(uids))
              revealVehicles(uids)
              const pts = uids.map((u) => vehicles.find((v) => v.uid === u)?.position).filter((p): p is LngLat => p != null)
              if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: 430 }))
            }}
            onClose={() => setDock((d) => closeDock(d, 'queue'))}
          />
          </ErrorBoundary>
        )}
        {showTasking && (
          <ErrorBoundary name="Watch officer" className="floating">
            <TaskingPanel
              api={taskingApi}
              now={now}
              banners={dockHead}
              onClose={() => setDock((d) => closeDock(d, 'officer'))}
              compose={<OfficerCompose api={taskingApi} draft={officerDraft} setDraft={setOfficerDraft} inserts={composeInserts} />}
              groups={groupsApi.groups}
              intents={allIntents ?? NO_INTENTS}
              // an order the officer proposed for a request: its card, in the Inbox while it waits
              onIntent={openIntent}
              onGroup={(uids) => {
                // a group the officer made for it: its members selected, nothing sent
                setSelected(new Set(uids))
                revealVehicles(uids)
              }}
            />
          </ErrorBoundary>
        )}
        {showSitrep && (
          <ErrorBoundary name="Sitrep" className="floating">
            <Sitrep
              banners={dockHead}
              vehicles={vehicles}
              taskings={state?.taskings ?? NO_TASKINGS}
              swarms={swarmFeed.doc?.swarms ?? NO_SWARMS}
              marks={marks}
              groups={groupsApi.groups}
              tracks={tracks?.tracks ?? NO_TRACKS}
              decisions={decisions}
              damaged={damagedUids}
              now={now}
              onVehicle={(uid) => {
                // its details, as a click on its fleet row opens them (the map flies there); they
                // open over the map's left edge, under this wide drawer, so the report gives way
                setDock((d) => closeDock(d, 'sitrep'))
                setDetailsUid(uid)
                setFollowing(true)
                setFocus((f) => ({ uid, n: (f?.n ?? 0) + 1 }))
              }}
              // the card is in the Inbox, which takes the dock's place from this report
              onWaiting={openWaiting}
              onIntent={openIntent}
              onTrack={revealTrackId}
              onSelect={(uids) => {
                setSelected(new Set(uids))
                revealVehicles(uids)
              }}
              onClose={() => setDock((d) => closeDock(d, 'sitrep'))}
            />
          </ErrorBoundary>
        )}
        {showSwarms && (
          <ErrorBoundary name="Swarms" className="floating">
          <SwarmsPanel
            banners={dockHead}
            feed={swarmFeed}
            selected={swarmSel}
            focus={swarmFocus}
            hovered={hoverGroup}
            onHover={setHoverGroup}
            alerts={decisions?.inbox.swarm_alerts ?? []}
            groups={{
              api: groupsApi,
              selected,
              present: presentUids,
              nameOf: (uid) => nameOf(String(uid)),
              onSelect: (uids) => {
                // a group of yours is a named selection: the order form then commands it
                setSelected(new Set(uids))
                revealVehicles(uids)
                const pts = allVehicles.flatMap((v) => (v.position && uids.includes(v.uid) ? [v.position] : []))
                if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: 430 }))
              },
            }}
            onSelect={(sw) => {
              // every member, so the order form sends one intent to the whole swarm
              setSelected(new Set(sw.members))
              revealVehicles(sw.members)
              const pts = sw.hull && sw.hull.length ? sw.hull : sw.centroid ? [sw.centroid] : []
              if (pts.length) setFit((f) => ({ points: pts, n: (f?.n ?? 0) + 1, padRight: 430 }))
            }}
            onClose={() => setDock((d) => closeDock(d, 'swarms'))}
            heard={(uid) => {
              const v = allVehicles.find((x) => String(x.uid) === uid)
              return v != null && !isStale(v)
            }}
          />
          </ErrorBoundary>
        )}
        {detailsUid != null && byUidDetails(vehicles, detailsUid) && (
          <ErrorBoundary name="Vehicle details" className="floating">
          <VehicleDetailsWhenFull
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
            nodeLoss={nodeLoss}
            nodeRow={nodeRows.get(detailsUid)}
            onReview={openWaiting}
            nameOf={nameOf}
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
            blocking={decisions?.inbox.blocking}
            pinned={pinnedTracks}
            onPin={togglePin}
            focus={trackFocus}
            crowded={crowded}
            expandAll={showAll.lists}
            onFolded={setTrackFolds}
          />
          </ErrorBoundary>
        </div>
        <ErrorBoundary name="Task panel" className="slot tasking">
        <TaskPanel vehicles={vehicles} historyVehicles={allVehicles} groupLabel={groupLabel} selected={selected} draft={draft} setDraft={setDraft} taskings={taskings} pendingIds={pendingIds} onInbox={openIntent} space={spaceRuns} now={now} onLinkRoute={setLinkRoute} intents={allIntents} expandAll={showAll.lists} onFolded={setOrderFolds} />
        </ErrorBoundary>
        {/* the nomination form (Phase E) takes the rail while open: the map keeps its preview and the inbox stays clear */}
        {engageForm && (
          <ErrorBoundary name="Engagement" className="floating">
          <EngagePanel
            // a Replace from a card starts the form afresh on that nomination's plan
            key={engageForm.replaces != null ? `replace-${engageForm.replaces}` : 'nominate'}
            tracks={tracks}
            vehicles={vehicles}
            now={now}
            initial={engageForm}
            intents={allIntents ?? []}
            onPreview={setEngageDraft}
            onKeepOuts={setEngageKeepOuts}
            onChecks={setEngageChecks}
            onNominated={(id, replaced) => {
              if (replaced != null) setInboxNotice({ n: Date.now(), text: `i${replaced} replaced by i${id}`, tone: 'ok' })
              setEngageForm(null)
              setEngageDraft(null)
              setEngageKeepOuts(null)
              setDock('inbox')
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
      </div>
    </div>
  )
}
