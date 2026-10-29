/// <reference types="geojson" />
import * as maplibregl from 'maplibre-gl'
import 'maplibre-gl/dist/maplibre-gl.css'
import './impact.css'
import { Layer, WebMercatorViewport, type LayersList, type PickingInfo, type Viewport } from '@deck.gl/core'
import { MapboxOverlay } from '@deck.gl/mapbox'
import { useEffect, useMemo, useRef, useState } from 'react'
import { distanceM } from './geo'
import { IMPACT_ANIMATION_MS, impactElement, type ImpactEvent } from './impact'
import { mapScreen, NO_SCREEN, type MapScreen } from './aloft'
import { BRIDGE_FILTER, liftBridges, type LiftedBridge } from './bridges'
import { build3dGlide, build3dStatic, drawnHeight, DEM_TILES, HEIGHT_EXAGGERATION, z } from './map3d'
import { declutter } from './declutter'
import { advanceDrawn, drawnAlt, drawnPosition, drawnPositionOf, drawnTick, glidingCount, isGliding, noteAltitudes, observeVehicles, resetDrawn, setSmoothing, snapAllDrawn } from './drawn'
import { LAUNCH_FLASH_MS, munitionShown } from './munition'
import { buildMunitionLayers } from './munitionLayers'
import { buildActLayers } from './actLayers'
import { type ActDrawing, animating, visibleActs } from './hostileAct'
import { GLIDE_LAYER_IDS } from './orderLayers'
import { buildTrailLayers } from './trailLayers'
import { perfWatchDeck } from './perf'
import { reducedMotion } from './reveal'
import { drapedWater } from './sea'
import { vehiclesGeneration } from './useBridge'
import type { SpaceOverlay } from './space'
import { EMPTY_OVERLAY } from './space'
import { spaceFeatures, spaceLinkLayers } from './spaceLayers'
import { buildPreviewLayers, buildSurfaceLayers, type SurfaceScale } from './surfaceLayers'
import type { Cluster, Clusters, LngLat, Tasking, Vehicle } from './types'
import { callsign } from './types'
import { buildVehicleLayers, heightLabel, VEHICLE_PICK_LAYERS } from './vehicleLayers'
import { buildClusterLayers, CLUSTER_PICK_LAYERS } from './clusterLayers'
import { cellAt, cellDetail, type CoverageImage, type LinkRouteDrawing } from './coverage'
import { buildLinkRouteLayers } from './linkLayers'
import { type Bounds, type MapViewport, visibleVehicles } from './viewport'

export interface Draft {
  /** `identify` (rules 21): one vehicle sent to identify the track `track`, picked on the map */
  mode: 'orbit' | 'patrol' | 'identify' | null
  center: LngLat | null
  radius: number
  route: LngLat[]
  /** identify: the target track's id, and why a click picked none (an identified track) */
  track?: string | null
  note?: string | null
  /** identify, for the map only: the vehicle the line to the target starts at */
  from?: LngLat | null
}

interface Props {
  vehicles: Vehicle[]
  hiddenUids?: ReadonlySet<number>
  impacts?: ImpactEvent[]
  impactNowMs?: number
  /** the console's clock (the bridge's, as `impactNowMs` is): where a munition in flight is drawn
   *  on each animation frame (munitionLayers.ts) and when its strike starts */
  bridgeNow?: () => number
  /** platforms that are their own munition, on their way: tagged (vehicleLayers.ts) */
  oneWay?: ReadonlySet<number>
  /** hostile acts our sensors observed (hostileAct.ts), drawn as fire events on the clock of
   *  `bridgeNow`; those started at or before `actsDismissedAt` are no longer drawn */
  acts?: ActDrawing[]
  actsDismissedAt?: number | null
  taskings: Tasking[]
  selected: Set<number>
  draft: Draft
  onToggle: (uid: number) => void
  /** right click on a vehicle (uid) or on empty map (null): deselect */
  onDeselect?: (uid: number | null) => void
  onMapClick: (p: LngLat) => void
  /** fly to this vehicle (n changes on every request, so repeat clicks work) */
  focus?: { uid: number; n: number } | null
  /** keep the camera on this vehicle (its details are open) until the operator pans away */
  follow?: number | null
  /** following stopped (the operator panned) or resumed */
  onFollowing?: (following: boolean) => void
  /** inbox: where pending intents would send their vehicles (see inboxPreview.ts) */
  preview?: FC
  /** inbox: fit the view to these points (n changes on every request); `aloft`, points at their
   *  height (metres above mean sea level) the view must hold too, such as a munition's launch
   *  point thousands of metres up: the camera then backs off until they are in it */
  fit?: { points: LngLat[]; n: number; padRight: number; aloft?: [number, number, number][] } | null
  /** space overlays: area, passes in progress, highlighted scene, SATCOM link lines */
  space?: SpaceOverlay
  /** the view while it moves (4 times a second) and when it stops: centre, bounds (w, s, e,
   *  n) and zoom; what the bridge's level of detail follows (murmur-and) */
  onView?: (center: LngLat, bounds: Bounds, zoom: number) => void
  /** counts per tile from the bridge while the view holds more vehicles than it sends in
   *  full: drawn as cluster markers instead of the lite vehicles' symbols (clusterLayers.ts) */
  clusters?: Clusters | null
  /** Extension point for more deck.gl layers (tracks, contacts, ...): called whenever MapView
   *  rebuilds its layers, and again whenever this function changes, so pass a memoised
   *  callback whose identity changes when its data does. Its layers draw above the map and the
   *  3D scene and below the vehicles; they survive a WebGL context loss (the overlay is rebuilt
   *  from this callback). Pickable layers get deck.gl's own onClick/onHover. `screen` says what
   *  the camera cannot show at its height (aloft.ts), as of the rebuild. */
  layers?: MapLayers
  /** predicted link coverage (coverage.ts): one image draped on the terrain under the labels,
   *  its cell under the pointer said in a tooltip */
  coverage?: CoverageImage | null
  /** the drafted order's legs coloured by predicted link, on the surface like the draft */
  linkRoute?: LinkRouteDrawing | null
}

export type MapLayers = (screen: MapScreen) => LayersList

/** a transparent pixel: the coverage image source's placeholder while the layer is off */
const BLANK_PNG = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=='
const COVERAGE = 'link-coverage'

const STYLE = 'https://tiles.openfreemap.org/styles/dark'
const START: LngLat = [-117.2, 32.703]

type FC = GeoJSON.FeatureCollection
const empty: FC = { type: 'FeatureCollection', features: [] }
const noImpacts: ImpactEvent[] = []
const noActs: ActDrawing[] = []

// antenna trackers: a beam along the yaw the tracker reports (where it is actually pointing)
function beamFeatures(vehicles: Vehicle[]): FC {
  const features: GeoJSON.Feature[] = []
  for (const v of vehicles) {
    if (v.kind !== 'tracker' || !v.position || v.yaw == null) continue
    const [lon, lat] = v.position
    const len = 4000
    const a = (v.yaw * Math.PI) / 180
    const end: LngLat = [lon + (len * Math.sin(a)) / (111_320 * Math.cos((lat * Math.PI) / 180)), lat + (len * Math.cos(a)) / 111_320]
    features.push({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: [v.position, end] } })
  }
  return { type: 'FeatureCollection', features }
}

const MAX_PITCH = 85
/** The camera an operator starts with: tilted enough that heights and depths read (the stems,
 *  the columns), not so far that the far side of the bay flattens into the horizon; north up,
 *  so the map reads as a chart. Top view (the button, or T) looks straight down from there. */
const INITIAL_PITCH = 50
const INITIAL_BEARING = 0
/** a pitch at or below this is the top view (the button offers to tilt back) */
const TOP_PITCH = 1
// middle-drag orbit sensitivity, degrees per pixel
const ORBIT_YAW = 0.35
const ORBIT_TILT = 0.25
const SMOOTH_KEY = 'murmur.smooth'
// the 2D or 3D choice of the console before 3D was its only map (10-12): no longer read
try {
  localStorage.removeItem('murmur.view')
} catch {
  // storage unavailable: nothing to remove
}
/** Above this many vehicles drawn the glide animation runs at 30 Hz instead of 60. The cost of a
 *  glide frame scales with the vehicles drawn, not those gliding: every vehicle's position is
 *  recomputed in each attached layer whenever anything glides. */
const GLIDE_MANY = 300

// prefers-reduced-motion, kept current by a listener (not asked of the browser every frame)
let reducedNow = reducedMotion()
try {
  window.matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', (e) => {
    reducedNow = e.matches
  })
} catch {
  // no matchMedia: motion stays as it was read
}

/** display smoothing (drawn.ts) is on unless the operator turned it off */
function savedSmooth(): boolean {
  try {
    return localStorage.getItem(SMOOTH_KEY) !== 'off'
  } catch {
    return true
  }
}

/** The map is 3D (terrain and seabed, a tilted camera, vehicles and tracks at their height or
 *  depth): the operator's only map since 10-12 (DECISIONS.md). Draped bridges would dive to
 *  the seabed, so the style's road layers drop them (they're redrawn at deck height by deck.gl,
 *  bridges.ts); bridge-probe keeps their features indexed for that. */
function hideDrapedBridges(m: maplibregl.Map) {
  for (const l of m.getStyle().layers) {
    if (l.id === 'bridge-probe' || !('source-layer' in l) || l['source-layer'] !== 'transportation') continue
    // the style's road filters are expression filters, so they combine with ours under 'all'
    m.setFilter(l.id, l.filter ? (['all', l.filter, BRIDGE_FILTER] as unknown as maplibregl.FilterSpecification) : BRIDGE_FILTER)
  }
}

let warnedWater = false

/** The water is tinted by the style's own water fill, draped on the terrain (sea.ts
 *  drapedWater): the sea, lakes, ponds and rivers alike, at whatever height the terrain has
 *  them. Changing it relays out the water's tiles, as the road filters do. */
function tintDrapedWater(m: maplibregl.Map) {
  const fills = m.getStyle().layers.filter((l) => l.type === 'fill' && 'source-layer' in l && l['source-layer'] === 'water')
  for (const { id } of fills) {
    const draped = drapedWater(m.getPaintProperty(id, 'fill-color'))
    if (draped) m.setPaintProperty(id, 'fill-color', draped.color as maplibregl.ExpressionSpecification)
    else if (!warnedWater) {
      warnedWater = true
      console.warn(`[murmur] the style's water colour (${id}) is not a plain colour: the water is left untinted (sea.ts drapedWater)`)
    }
  }
}

/** Terrain and seabed, the bridges and the water, on a freshly loaded style. */
function apply3d(m: maplibregl.Map) {
  hideDrapedBridges(m)
  tintDrapedWater(m)
  m.setTerrain({ source: 'dem', exaggeration: HEIGHT_EXAGGERATION })
}

const SPACE_SOURCES = ['space-area', 'space-for', 'space-scenes'] as const

/** Space overlays, drawn subtly under the vehicles, draped on the terrain as context: the
 *  earthbound picture stays primary. The selected scene, the drawn box and the tasked
 *  collections lie on the surface instead (surfaceLayers.ts). */
function addSpaceLayers(m: maplibregl.Map) {
  for (const id of SPACE_SOURCES) m.addSource(id, { type: 'geojson', data: empty })
  // under the tracker beams
  const before = m.getLayer('beams') ? 'beams' : undefined
  m.addLayer({ id: 'space-for', type: 'fill', source: 'space-for', paint: { 'fill-color': ['get', 'color'], 'fill-opacity': 0.035 } }, before)
  m.addLayer(
    { id: 'space-for-edge', type: 'line', source: 'space-for', paint: { 'line-color': ['get', 'color'], 'line-opacity': 0.35, 'line-width': 1, 'line-dasharray': [3, 3] } },
    before,
  )
  m.addLayer(
    { id: 'space-scenes', type: 'line', source: 'space-scenes', paint: { 'line-color': ['get', 'color'], 'line-opacity': 0.55, 'line-width': 1.2 } },
    before,
  )
  m.addLayer(
    { id: 'space-area', type: 'line', source: 'space-area', paint: { 'line-color': '#f4f1de', 'line-opacity': 0.35, 'line-width': 1, 'line-dasharray': [6, 4] } },
    before,
  )
  m.addLayer({
    id: 'space-labels',
    type: 'symbol',
    source: 'space-scenes',
    filter: ['==', ['geometry-type'], 'Point'],
    layout: { 'text-field': ['get', 'label'], 'text-size': 10, 'text-font': ['Noto Sans Regular'], 'text-anchor': 'top-left', 'text-offset': [0.3, 0.2] },
    paint: { 'text-color': ['get', 'color'], 'text-opacity': 0.8, 'text-halo-color': '#0b0f14', 'text-halo-width': 1 },
  })
}

/** the space overlays' sources (the SATCOM links are deck.gl rays, spaceLinkLayers) */
function setSpaceData(m: maplibregl.Map, space: SpaceOverlay) {
  const f = spaceFeatures(space)
  for (const id of SPACE_SOURCES) (m.getSource(id) as maplibregl.GeoJSONSource | undefined)?.setData(f[id])
}

/** width of the vehicle details card (.vdetails) plus its margin */
const FOLLOW_PAD_LEFT = 350

/** Where a drawing on the surface goes (surfaceLayers.ts): the surface's height at a point,
 *  rendered metres (the water's 0 over the sea, the terrain's over land, as the terrain is drawn
 *  at this zoom), and metres per pixel at the view's centre. */
function surfaceScale(m: maplibregl.Map): SurfaceScale {
  const mPerPx = (40_075_016.686 * Math.cos((m.getCenter().lat * Math.PI) / 180)) / (512 * 2 ** m.getZoom())
  const terrain = m.terrain
  if (!terrain) return { surfaceZ: () => 0, mPerPx }
  // the elevation tiles' zoom the terrain is drawn from here (its source stops at 15)
  const zoom = Math.min(15, Math.max(0, Math.floor(m.getZoom())))
  return { surfaceZ: ([lng, lat]) => Math.max(0, terrain.getElevationForLngLatZoom(new maplibregl.LngLat(lng, lat), zoom)), mPerPx }
}

/** Where a click on the map lands: on the ground over land, on the water's surface over the sea.
 *  MapLibre gives the terrain under the pointer, which under water is the seabed (the elevation
 *  tiles include it), up to a few hundred metres past the boat or the spot the pointer is on in
 *  a tilted view; the water's surface is at sea level, where the boats and every drawing on the
 *  water are. */
function surfacePoint(m: maplibregl.Map, e: maplibregl.MapMouseEvent): LngLat {
  const ground = e.lngLat
  if ((m.queryTerrainElevation(ground) ?? 0) >= 0) return [ground.lng, ground.lat]
  // The ray from the camera through the pointer crosses sea level before it meets the seabed.
  // MapLibre's flat unprojection (MercatorTransform, the v5 the console is pinned to) works in
  // a frame whose 0 is the camera centre's elevation (rendered metres, the exaggeration
  // applied), so sea level is at minus that elevation there.
  const tr = m.transform as unknown as { elevation: number; screenPointToMercatorCoordinateAtZ?: (p: maplibregl.Point, z: number) => maplibregl.MercatorCoordinate }
  const water = tr.screenPointToMercatorCoordinateAtZ?.(e.point, -tr.elevation)?.toLngLat()
  return water ? [water.lng, water.lat] : [ground.lng, ground.lat]
}

export function MapView({ vehicles: allVehicles, hiddenUids, impacts = noImpacts, impactNowMs = 0, bridgeNow, oneWay, acts = noActs, actsDismissedAt = null, taskings, selected, draft, focus, follow = null, onFollowing, preview = empty, fit, onToggle, onDeselect, onMapClick, space = EMPTY_OVERLAY, onView, layers, clusters = null, coverage = null, linkRoute = null }: Props) {
  const vehicles = useMemo(() => hiddenUids?.size ? allVehicles.filter((v) => !hiddenUids.has(v.uid)) : allVehicles, [allVehicles, hiddenUids])
  // the top view (pitch 0, looking straight down) and the pitch to tilt back to from it
  const [top, setTop] = useState(false)
  const tiltBack = useRef(INITIAL_PITCH)
  // display smoothing of our vehicles' motion (glide.ts, drawn.ts): the choice is remembered
  const [smooth, setSmooth] = useState(savedSmooth)
  const smoothRef = useRef(smooth)
  const frameId = useRef(0)
  // the layers that do not move with a glide, kept between animation frames
  const staticLayers = useRef<{ three: ReturnType<typeof build3dStatic>; surface: LayersList; preview: LayersList; link: LayersList; space: LayersList; ext: LayersList; clusters: LayersList } | null>(null)
  const lastGlideFrame = useRef(0)
  const lastGeneration = useRef<number | undefined>(undefined)
  const previewGliding = useRef(false)
  const overlay = useRef<MapboxOverlay | null>(null)
  const bridges = useRef<LiftedBridge[]>([])
  const latest = useRef({ vehicles, selected, bare: new Set<number>(), aloft: new Map<number, number>(), clusters, oneWay })
  // the surface drawings' inputs (surfaceLayers.ts), and what they were last built from
  const taskingsRef = useRef(taskings)
  const draftRef = useRef(draft)
  const surfaceMemo = useRef<{ key: unknown[]; layers: LayersList } | null>(null)
  // the drafted order's predicted link (linkLayers.ts), and what it was last built from
  const linkRouteRef = useRef(linkRoute)
  const linkMemo = useRef<{ key: unknown[]; layers: LayersList } | null>(null)
  // the coverage grid under the pointer (its tooltip)
  const coverageRef = useRef(coverage)
  const covTip = useRef<HTMLDivElement>(null)
  // bumped when new terrain arrives (the surface drawings follow its heights)
  const terrainGen = useRef(0)
  // the camera's bounds and zoom, as last reported (what the drawn set is cut to)
  const viewport = useRef<MapViewport | null>(null)
  const spaceRef = useRef(space)
  const previewRef = useRef(preview)
  const layersRef = useRef(layers)
  const viewCb = useRef(onView)
  // bumped when the camera stops, so label declutter re-runs at the new scale
  const [moved, setMoved] = useState(0)
  // WebGL context lost: the map and deck.gl are paused until the browser restores it
  const [glLost, setGlLost] = useState(false)
  // the same for map callbacks: no camera moves while lost (with terrain on, MapLibre draws
  // a depth pass to unproject, and its painter is gone)
  const lostRef = useRef(false)
  // bumped to throw the map away and build a new one (context restored, or the Reload button)
  const [mapGen, setMapGen] = useState(0)
  // the camera to come back to after a rebuild
  const camera = useRef<{ center: maplibregl.LngLat; zoom: number; bearing: number; pitch: number } | null>(null)
  // bumped when the style is (re)loaded, first load or back from a context loss, so every
  // source and layer is pushed again
  const [styleGen, setStyleGen] = useState(0)
  const container = useRef<HTMLDivElement>(null)
  const tooltip = useRef<HTMLDivElement>(null)
  const map = useRef<maplibregl.Map | null>(null)
  const impactMarkers = useRef(new Map<string, maplibregl.Marker>())
  // the strikes and the clock, for the animation frames of a munition's flight; the latest clock
  // reading the markers were reconciled at (never back: see syncImpacts)
  const impactsRef = useRef(impacts)
  const clockRef = useRef(bridgeNow)
  const syncedAt = useRef(0)
  // munitions were drawn in the last layers built
  const munitionsDrawn = useRef(false)
  // the observed acts, the last dismissal, and whether any was drawn in the last layers built
  const actsRef = useRef(acts)
  const actsDismissed = useRef(actsDismissedAt)
  const actsDrawn = useRef(false)
  const ready = useRef(false)
  // centroid we last zoomed to; refit when the swarm shows up somewhere else (new --location)
  const fitAt = useRef<LngLat | null>(null)
  const cursor = useRef('')
  const clickRef = useRef(onMapClick)
  const toggleRef = useRef(onToggle)
  const deselectRef = useRef(onDeselect)
  useEffect(() => {
    clickRef.current = onMapClick
    toggleRef.current = onToggle
    deselectRef.current = onDeselect
    viewCb.current = onView
  }, [onMapClick, onToggle, onDeselect, onView])

  /** what this camera cannot show at its height (aloft.ts): from deck.gl's viewport, what it
   *  draws with */
  const screenNow = (): MapScreen => {
    const m = map.current
    const deck = (overlay.current as unknown as { _deck?: { isInitialized?: boolean; getViewports(): Viewport[] } } | null)?._deck
    if (!m || !deck?.isInitialized || !ready.current) return NO_SCREEN
    let vp: Viewport | undefined
    try {
      vp = deck.getViewports()[0]
    } catch {
      return NO_SCREEN // no view yet
    }
    if (!vp) return NO_SCREEN
    const c = m.getCanvas()
    return mapScreen(vp, c.clientWidth, c.clientHeight)
  }

  /** the taskings, the draft and the space segment's drawings on the surface: rebuilt when one of
   *  them changes, the terrain under them does, or the zoom (their dashes are in pixels) */
  const surfaceLayers = (m: maplibregl.Map | null): LayersList => {
    if (!m) return []
    const sp = spaceRef.current
    const key = [taskingsRef.current, draftRef.current, sp.box, sp.scene, sp.tasked, terrainGen.current, Math.round(m.getZoom() * 4)]
    const memo = surfaceMemo.current
    if (memo && memo.key.every((k, i) => k === key[i])) return memo.layers
    const layers = buildSurfaceLayers({ taskings: taskingsRef.current, draft: draftRef.current, box: sp.box, scene: sp.scene, tasked: sp.tasked ?? [] }, surfaceScale(m))
    surfaceMemo.current = { key, layers }
    return layers
  }

  /** the drafted order's predicted link: rebuilt when it changes, the terrain under it does, or
   *  the zoom (the return leg's dashes are in pixels) */
  const linkLayers = (m: maplibregl.Map | null): LayersList => {
    const d = linkRouteRef.current
    if (!m || !d) return []
    const key = [d, terrainGen.current, Math.round(m.getZoom() * 4)]
    const memo = linkMemo.current
    if (memo && memo.key.every((k, i) => k === key[i])) return memo.layers
    const layers = buildLinkRouteLayers(d, surfaceScale(m))
    linkMemo.current = { key, layers }
    return layers
  }

  /** everything deck.gl draws: the 3D scene, the drawings on the surface, SATCOM links,
   *  extension layers (the `layers` prop), then the vehicles on top. `frame`: an animation frame
   *  of a glide, which rebuilds only what is attached to a vehicle and reuses the rest as it was
   *  (the inbox preview too while one of its vehicles glides: `previewToo`). */
  const deckLayers = (frame = false, previewToo = false): LayersList => {
    const { vehicles: vs, selected: sel, bare, aloft, clusters: cl, oneWay: ow } = latest.current
    const m = ready.current ? map.current : null
    if (!frame || !staticLayers.current) {
      staticLayers.current = {
        three: build3dStatic(vs, bridges.current),
        surface: surfaceLayers(m),
        preview: m ? buildPreviewLayers(previewData(), surfaceScale(m)) : [],
        link: linkLayers(m),
        space: spaceLinkLayers(spaceRef.current.links),
        ext: layersRef.current?.(screenNow()) ?? [],
        clusters: buildClusterLayers(cl),
      }
    } else if (previewToo && m) staticLayers.current.preview = buildPreviewLayers(previewData(), surfaceScale(m))
    const { three, surface, preview: previewLayers, link, space, ext, clusters: clusterLayers } = staticLayers.current
    const tick = drawnTick()
    const glide = build3dGlide(vs)
    const trails = smoothRef.current ? buildTrailLayers(vs) : { under: [], over: [] }
    const layers: LayersList = [
      // seen through the water (map3d.ts): the trails and stems of what is under it
      ...three.under,
      ...trails.under,
      glide.stemsUnder,
      // on the water's surface (or the ground): taskings, the draft, the inbox preview, the
      // draft's predicted link
      ...surface,
      ...previewLayers,
      ...link,
      // what is above or on the water: bridge decks, the other trails and stems, sea-level shadows
      ...three.decks,
      ...three.over,
      ...trails.over,
      glide.stems,
      glide.shadows,
      ...space,
      // the extension layers that sit at a vehicle follow its glide: re-trigger just those
      ...(frame ? ext.map((l) => (l instanceof Layer && GLIDE_LAYER_IDS.includes(l.id) ? l.clone({ updateTriggers: { ...l.props.updateTriggers, getPosition: tick } } as never) : l)) : ext),
      ...clusterLayers,
      ...buildVehicleLayers({ vehicles: vs, selected: sel, bare, aloft, oneWay: ow }),
      // hostile acts our sensors observed: the fire event, then what lingers of it
      ...actLayers(),
      // munitions in flight over everything: what is on the way
      ...munitionLayers(),
    ]
    return layers
  }

  /** the munitions in flight, or flashing at their launch, at `t` (the bridge's clock) */
  const munitionsAt = (t: number) => impactsRef.current.flatMap((i) => (i.munition && munitionShown(i.munition, t) ? [i.munition] : []))
  /** Animation frames are wanted for a munition: one is in flight or flashing at its launch, or
   *  is to be launched within a second. Never with reduced motion (the console's tick steps it). */
  const munitionsFlying = () => {
    const t = clockRef.current?.()
    if (t == null || reducedNow) return false
    return impactsRef.current.some(({ munition: m }) => m && t >= m.launch_t - 1000 && (t < m.impact_t || t < m.launch_t + LAUNCH_FLASH_MS))
  }
  const munitionLayers = (): LayersList => {
    const t = clockRef.current?.()
    const live = t == null ? [] : munitionsAt(t)
    munitionsDrawn.current = live.length > 0
    return live.length ? buildMunitionLayers(live, t!, screenNow(), reducedNow) : []
  }
  /** the observed acts drawn at `t` (hostileAct.ts visibleActs) */
  const actsAt = (t: number) => visibleActs(actsRef.current, t, actsDismissed.current)
  /** Animation frames are wanted for an act: its streak, a flash or its label's fade is under
   *  way. Never with reduced motion (the console's tick steps it). */
  const actsAnimating = () => {
    const t = clockRef.current?.()
    if (t == null || reducedNow) return false
    return actsAt(t).some((d) => animating(t - d.start))
  }
  const actLayers = (): LayersList => {
    const t = clockRef.current?.()
    const live = t == null ? [] : actsAt(t)
    actsDrawn.current = live.length > 0
    return live.length ? buildActLayers(live, t!, reducedNow) : []
  }
  /** a munition or an act wants animation frames */
  const animated = () => munitionsFlying() || actsAnimating()
  const pushLayers = () => overlay.current?.setProps({ layers: deckLayers() })

  /** the inbox preview's transit lines start at a vehicle: that end follows the drawn position */
  const previewData = (): FC => {
    const base = previewRef.current
    if (!smoothRef.current) return base
    return {
      ...base,
      features: base.features.map((f) => {
        const uid = f.properties?.uid
        if (uid == null || f.geometry.type !== 'LineString') return f
        const at = drawnPositionOf(uid, null)
        return at ? { ...f, geometry: { ...f.geometry, coordinates: [at, ...f.geometry.coordinates.slice(1)] } } : f
      }),
    }
  }

  // Animation frames while some vehicle is gliding, none otherwise (the loop ends when the last
  // glide has; the next changed report starts it again). Capped at 60 Hz so that 120 and 144 Hz
  // displays do not double the work, and at 30 Hz while more than GLIDE_MANY vehicles are drawn:
  // each frame recomputes every drawn vehicle's position in each attached layer (deck.gl cannot
  // update only the rows that moved) and repaints the whole map, so the cost grows with the
  // fleet, and 30 Hz is still smooth for glides of 150 ms to a second. Not throttled while the
  // camera follows a vehicle: the camera aims at the pose of every display frame, and deck.gl
  // must draw that same pose or the followed symbol shakes against the view.
  //
  // The same frames draw a munition in flight (munitionLayers.ts) and an observed act's fire event
  // (actLayers.ts), smoothing on or off: then they run while one flies or fires, and step only the
  // glides there are (none: no vehicle is re-positioned).
  const glideFrame = (t: number) => {
    frameId.current = 0
    const flying = animated()
    if ((!smoothRef.current && !flying) || lostRef.current || !overlay.current) return
    const every = latest.current.vehicles.length > GLIDE_MANY ? 1000 / 30 : 1000 / 60
    if (!following.current && t - lastGlideFrame.current < every - 2) {
      // too soon: wait for the next display frame (the glide may have ended meanwhile; the next real frame then draws its end)
      frameId.current = requestAnimationFrame(glideFrame)
      return
    }
    lastGlideFrame.current = t
    const t0 = performance.now()
    const n = glidingCount() > 0 ? advanceDrawn(t) : 0
    // the preview's transit lines start at a vehicle: rebuilt while one of them glides (and once more as it ends)
    const uids = previewRef.current.features.flatMap((f) => (f.properties?.uid == null ? [] : [f.properties.uid as number]))
    const previewNow = uids.some(isGliding)
    overlay.current.setProps({ layers: deckLayers(true, previewNow || previewGliding.current) })
    previewGliding.current = previewNow
    if (import.meta.env.DEV) {
      const w = window as unknown as { murmurPerf: Record<string, number[]> }
      w.murmurPerf ??= {}
      const a = (w.murmurPerf.glideFrame ??= [])
      a.push(performance.now() - t0)
      if (a.length > 600) a.splice(0, a.length - 600)
    }
    // a munition that has just landed: its strike starts this frame, not at the next tick
    if (flying && clockRef.current) syncImpacts(clockRef.current())
    if (n > 0 || animated()) frameId.current = requestAnimationFrame(glideFrame)
  }
  const wakeGlide = () => {
    if (frameId.current || document.visibilityState !== 'visible') return
    if ((smoothRef.current && glidingCount() > 0) || animated()) frameId.current = requestAnimationFrame(glideFrame)
  }

  useEffect(() => {
    const markers = impactMarkers.current
    const cam = camera.current
    const m = new maplibregl.Map({
      container: container.current!,
      style: STYLE,
      center: cam?.center ?? START,
      zoom: cam?.zoom ?? 12.6,
      bearing: cam?.bearing ?? INITIAL_BEARING,
      pitch: cam?.pitch ?? INITIAL_PITCH,
      attributionControl: { compact: true },
      maxPitch: MAX_PITCH,
    })
    if (import.meta.env.DEV) (window as unknown as { murmurMap: maplibregl.Map }).murmurMap = m

    // Bare symbols (decluttered) show their callsign on hover; any vehicle shows a pointer.
    const onHover = (info: PickingInfo) => {
      const v = info.layer && VEHICLE_PICK_LAYERS.includes(info.layer.id) ? (info.object as Vehicle | undefined) : undefined
      m.getCanvas().style.cursor = v ? 'pointer' : cursor.current
      const tip = tooltip.current
      if (!tip) return
      if (v && latest.current.bare.has(v.uid)) {
        const h = heightLabel(v)
        tip.textContent = `${callsign(v)}${v.identity?.short ? ` · ${v.identity.short}` : ''}${h ? ` · ${h}` : ''}`
        tip.style.transform = `translate(${info.x}px, ${info.y - 14}px) translate(-50%, -100%)`
        tip.hidden = false
      } else tip.hidden = true
    }
    const attachDeck = () => {
      const deck = new MapboxOverlay({ interleaved: true, layers: [], onHover })
      m.addControl(deck)
      overlay.current = deck
      // ?perf: deck.gl's own attribute-update time, which is where a glide's cost lands
      perfWatchDeck(() => (deck as unknown as { _deck?: { stats: never } })._deck?.stats ?? null)
    }
    attachDeck()
    // deck.gl takes the camera from the map on each move, and draws with the camera of the frame;
    // but MapLibre raises or lowers the camera with the terrain under the view's centre as tiles
    // arrive, without a move, and deck.gl's picking (a click on a vehicle or a track, the hover)
    // then reads a camera off by that much: tens of pixels over the deep channel, so a click on a
    // vehicle missed it. A move whenever that elevation changes gives deck.gl the camera again.
    let deckElevation = m.transform.elevation
    m.on('render', () => {
      if (m.transform.elevation === deckElevation) return
      deckElevation = m.transform.elevation
      m.fire('move')
    })
    m.addControl(new maplibregl.NavigationControl({ showCompass: true }), 'bottom-right')
    m.addControl(new maplibregl.ScaleControl({ unit: 'metric' }), 'bottom-left')

    // murmur-s8w: a lost WebGL context took the page down (deck.gl asked the map for its
    // projection, but MapLibre drops its style on context loss). Detach deck.gl while the context
    // is gone, and on restore build a fresh overlay and its layers on the restored style.
    m.on('webglcontextlost', () => {
      ready.current = false
      lostRef.current = true
      m.stop()
      camera.current = { center: m.getCenter(), zoom: m.getZoom(), bearing: m.getBearing(), pitch: m.getPitch() }
      // no gestures either (MapLibre owns the container's classes, so not through React)
      m.getContainer().classList.add('gl-lost')
      const deck = overlay.current
      overlay.current = null
      if (deck) {
        try {
          m.removeControl(deck)
        } catch (err) {
          console.warn('[murmur] removing the deck.gl overlay after context loss', err)
          ;(m as unknown as { __deck?: unknown }).__deck = null
        }
      }
      if (tooltip.current) tooltip.current.hidden = true
      setGlLost(true)
    })
    // MapLibre can restore its own style in place, but not reliably with terrain on (its
    // render-to-texture framebuffers belong to the dead context), and luma.gl caches its device
    // on the context object. A fresh map on a fresh canvas is the dependable way back: same
    // camera, same sources (pushed again on load), a new deck.gl overlay and layers.
    m.on('webglcontextrestored', () => setMapGen((n) => n + 1))

    /** the vehicle under a screen point */
    const pickVehicle = (p: maplibregl.Point) => {
      const deck = overlay.current
      if (!deck || !ready.current) return null
      try {
        const hit = deck.pickObject({ x: p.x, y: p.y, radius: 6, layerIds: VEHICLE_PICK_LAYERS })
        return (hit?.object as Vehicle | undefined) ?? null
      } catch {
        return null // deck.gl not initialised yet
      }
    }
    /** the cluster marker under a screen point */
    const pickCluster = (p: maplibregl.Point) => {
      const deck = overlay.current
      if (!deck || !ready.current || !latest.current.clusters) return null
      try {
        const hit = deck.pickObject({ x: p.x, y: p.y, radius: 4, layerIds: CLUSTER_PICK_LAYERS })
        return (hit?.object as Cluster | undefined) ?? null
      } catch {
        return null
      }
    }
    m.on('click', (e: maplibregl.MapMouseEvent) => {
      // vehicles are deck.gl objects: a click on one selects it instead of drafting
      const v = pickVehicle(e.point)
      if (v) return toggleRef.current(v.uid)
      // a cluster marker: zoom in on it (two levels: its tile is then four markers or symbols)
      const c = pickCluster(e.point)
      if (c) return m.flyTo({ center: c.c, zoom: m.getZoom() + 2, duration: 700 })
      clickRef.current(surfacePoint(m, e))
    })
    // Right click deselects: a vehicle under the pointer, else everything. A right drag rotates
    // the map instead, so only a press that barely moved counts as a click.
    let rightDown: { x: number; y: number } | null = null
    m.on('mousedown', (e: maplibregl.MapMouseEvent) => {
      if (e.originalEvent.button === 2) rightDown = { x: e.point.x, y: e.point.y }
    })
    m.on('contextmenu', (e: maplibregl.MapMouseEvent) => {
      e.preventDefault()
      const d = rightDown ? Math.hypot(e.point.x - rightDown.x, e.point.y - rightDown.y) : 0
      rightDown = null
      if (d > 5) return
      const v = pickVehicle(e.point)
      deselectRef.current?.(v ? v.uid : null)
    })
    // Predicted link coverage under the pointer: the cell it is over (the image is draped on the
    // terrain, so the terrain point under the pointer is the cell drawn there), unless a
    // vehicle's own tooltip is showing.
    const hideCovTip = () => {
      if (covTip.current) covTip.current.hidden = true
    }
    m.on('mousemove', (e: maplibregl.MapMouseEvent) => {
      const tip = covTip.current
      const img = coverageRef.current
      if (!tip) return
      const c = img && (tooltip.current?.hidden ?? true) ? cellAt(img.grid, e.lngLat.lng, e.lngLat.lat) : null
      if (!c || !img) return hideCovTip()
      const g = img.grid
      const alt = g.alt_ref === 'surface' ? `${g.alt_m} m above surface` : g.alt_m < 0 ? `${-g.alt_m} m deep` : `${g.alt_m} m MSL`
      tip.replaceChildren()
      const head = document.createElement('b')
      head.className = c.state
      head.textContent = `predicted ${c.state === 'unknown' ? 'not yet' : c.state}`
      const words = document.createTextNode(` · ${cellDetail(c)}`)
      const foot = document.createElement('small')
      foot.textContent = `${g.callsign ? `${g.callsign}: ` : ''}${g.profile} at ${alt}`
      tip.append(head, words, foot)
      tip.style.transform = `translate(${e.point.x + 14}px, ${e.point.y + 14}px)`
      tip.hidden = false
    })
    m.on('mouseout', hideCovTip)
    m.on('movestart', hideCovTip)
    // Middle-drag orbits the camera like a 3D modelling tool: left/right spins (bearing),
    // up/down tilts (pitch).
    const surface = m.getCanvasContainer()
    let orbit: { x: number; y: number; bearing: number; pitch: number } | null = null
    const onDown = (e: MouseEvent) => {
      if (e.button !== 1) return
      e.preventDefault() // no browser autoscroll
      m.stop()
      orbit = { x: e.clientX, y: e.clientY, bearing: m.getBearing(), pitch: m.getPitch() }
      surface.style.cursor = 'grabbing'
    }
    const onMove = (e: MouseEvent) => {
      if (!orbit || lostRef.current) return
      const bearing = orbit.bearing + (e.clientX - orbit.x) * ORBIT_YAW
      const pitch = Math.max(0, Math.min(MAX_PITCH, orbit.pitch - (e.clientY - orbit.y) * ORBIT_TILT))
      m.jumpTo({ bearing, pitch })
    }
    const onUp = (e: MouseEvent) => {
      if (e.button !== 1 || !orbit) return
      orbit = null
      surface.style.cursor = ''
    }
    const noAux = (e: MouseEvent) => e.button === 1 && e.preventDefault()
    surface.addEventListener('mousedown', onDown)
    surface.addEventListener('auxclick', noAux)
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)

    m.on('load', () => {
      m.addSource('dem', {
        type: 'raster-dem',
        tiles: [DEM_TILES],
        encoding: 'terrarium',
        tileSize: 256,
        maxzoom: 15,
        attribution: 'Terrain: Mapzen/AWS Terrain Tiles',
      })
      // MapLibre renders better with separate sources for 3D terrain and hillshade
      m.addSource('dem-shade', { type: 'raster-dem', tiles: [DEM_TILES], encoding: 'terrarium', tileSize: 256, maxzoom: 15 })
      // invisible layer that keeps bridge features indexed: MapLibre only lets you query
      // features some layer uses, and the road layers drop bridges (see hideDrapedBridges)
      m.addLayer({
        id: 'bridge-probe',
        type: 'line',
        source: 'openmaptiles',
        'source-layer': 'transportation',
        filter: ['==', ['get', 'brunnel'], 'bridge'],
        paint: { 'line-opacity': 0 },
      })
      const firstSymbol = m.getStyle().layers.find((l) => l.type === 'symbol')?.id
      m.addLayer(
        {
          id: 'hillshade',
          type: 'hillshade',
          source: 'dem-shade',
          paint: { 'hillshade-exaggeration': 0.35, 'hillshade-shadow-color': '#000000' },
        },
        firstSymbol,
      )
      // predicted link coverage: one image (coverage.ts gridImage), draped on the terrain over
      // the hillshade and under the labels; hidden until there is a grid
      m.addSource(COVERAGE, { type: 'image', url: BLANK_PNG, coordinates: [[-0.001, 0.001], [0.001, 0.001], [0.001, -0.001], [-0.001, -0.001]] })
      m.addLayer(
        {
          id: COVERAGE,
          type: 'raster',
          source: COVERAGE,
          layout: { visibility: 'none' },
          paint: { 'raster-opacity': 1, 'raster-resampling': 'nearest', 'raster-fade-duration': 0 },
        },
        firstSymbol,
      )
      m.addSource('beams', { type: 'geojson', data: empty })
      m.addLayer({
        id: 'beams',
        type: 'line',
        source: 'beams',
        paint: { 'line-color': '#f4f1de', 'line-width': 1.5, 'line-opacity': 0.55, 'line-dasharray': [4, 3] },
      })
      addSpaceLayers(m)
      ready.current = true
      apply3d(m)
      setSpaceData(m, spaceRef.current)
      pushLayers()
      reportView()
      setStyleGen((n) => n + 1)
    })
    // re-estimate bridge decks when the camera stops or new map/terrain tiles arrive.
    // (Not 'idle': live vehicle updates repaint the map several times a second, so it never is.)
    let bridgeTimer: number | undefined
    const refreshBridges = () => {
      window.clearTimeout(bridgeTimer)
      bridgeTimer = window.setTimeout(() => {
        if (!ready.current || !m.getTerrain()) return
        const t0 = performance.now()
        bridges.current = liftBridges(m)
        // and the surface drawings follow the terrain that came
        terrainGen.current++
        if (import.meta.env.DEV) {
          const w = window as unknown as { murmurPerf: Record<string, number[]> }
          w.murmurPerf ??= {}
          ;(w.murmurPerf.liftBridges ??= []).push(performance.now() - t0)
        }
        if (import.meta.env.DEV) (window as unknown as { murmurBridges: LiftedBridge[] }).murmurBridges = bridges.current
        pushLayers()
      }, 400)
    }
    const reportView = () => {
      if (!ready.current) return
      const c = m.getCenter()
      const b = m.getBounds()
      const bounds: Bounds = [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()]
      viewport.current = { bounds, zoom: m.getZoom() }
      viewCb.current?.([c.lng, c.lat], bounds, m.getZoom())
    }
    // while the camera moves, the view goes out 4 times a second (the bridge's tick), so
    // the level of detail follows a pan; and once more when it stops
    let viewTimer: number | undefined
    const reportViewSoon = () => {
      if (viewTimer) return
      viewTimer = window.setTimeout(() => {
        viewTimer = undefined
        reportView()
        // the follow camera's moves also redo the declutter, at this pace
        if (followMoved) {
          followMoved = false
          setMoved((n) => n + 1)
        }
      }, 250)
    }
    let followMoved = false
    m.on('move', reportViewSoon)
    m.on('moveend', refreshBridges)
    m.on('moveend', (e: { follow?: boolean }) => {
      if (e.follow) {
        // the follow camera ends a move every frame: report and redo the declutter at the
        // timer's pace (4 Hz), not per frame, or App renders and every layer is rebuilt each frame
        followMoved = true
        reportViewSoon()
        return
      }
      window.clearTimeout(viewTimer)
      viewTimer = undefined
      reportView()
      // the drawn set is cut to the view, and label declutter runs in screen space
      setMoved((n) => n + 1)
    })
    m.on('sourcedata', (e) => {
      if (e.isSourceLoaded && (e.sourceId === 'openmaptiles' || e.sourceId === 'dem')) refreshBridges()
    })
    map.current = m
    return () => {
      ready.current = false
      for (const marker of markers.values()) marker.remove()
      markers.clear()
      overlay.current = null
      surface.removeEventListener('mousedown', onDown)
      surface.removeEventListener('auxclick', noAux)
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
      window.clearTimeout(bridgeTimer)
      window.clearTimeout(viewTimer)
      if (map.current === m) map.current = null
      if (following.current) {
        following.current = null
        followingCb.current?.(false)
      }
      try {
        m.remove()
      } catch (err) {
        console.warn('[murmur] removing a map after context loss', err)
      }
      // a rebuilt map comes up without the paused state
      lostRef.current = false
      setGlLost(false)
    }
    // mapGen: rebuild after a WebGL context loss (see webglcontextrestored)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mapGen])

  // CSS runs the short flash and shockwave; MapLibre keeps its anchor on the effect point
  // while the operator pans, tilts or zooms. Reconcile by strike id so bridge ticks do not
  // restart the animation, and do not replay old effects when a console reconnects. Called at
  // each tick and on the animation frames of a munition's flight, whose clock readings can come a
  // little after a tick's: the latest reading within a second counts, so a strike a frame started
  // is not taken down again by a tick read just before it (and a clock set back, a new run, is
  // followed).
  const syncImpacts = (nowMs: number) => {
    const m = map.current
    if (!m || !ready.current) return
    const now = nowMs < syncedAt.current && syncedAt.current - nowMs < 1000 ? syncedAt.current : nowMs
    syncedAt.current = now
    const live = new Set<string>()
    for (const impact of impactsRef.current) {
      const age = now - impact.t
      if (age < 0 || age >= IMPACT_ANIMATION_MS) continue
      live.add(impact.key)
      if (impactMarkers.current.has(impact.key)) continue
      const marker = new maplibregl.Marker({ element: impactElement(impact, age), anchor: 'center' })
        .setLngLat(impact.point)
        .addTo(m)
      impactMarkers.current.set(impact.key, marker)
    }
    for (const [key, marker] of impactMarkers.current) {
      if (live.has(key)) continue
      marker.remove()
      impactMarkers.current.delete(key)
    }
  }
  useEffect(() => {
    impactsRef.current = impacts
    actsRef.current = acts
    actsDismissed.current = actsDismissedAt
    clockRef.current = bridgeNow
    syncImpacts(impactNowMs)
    // a munition in flight or an act firing: animation frames; none (reduced motion), a step at
    // each tick instead, and one more to take off one that has landed (or was left drawn while
    // the tab was hidden); an act's lingering line and marker fade at the tick
    if (animated()) wakeGlide()
    else if (overlay.current && (munitionsDrawn.current || actsDrawn.current || actsAt(impactNowMs).length || (reducedNow && munitionsAt(impactNowMs).length))) overlay.current.setProps({ layers: deckLayers(true) })
    // syncImpacts, wakeGlide and deckLayers read refs only
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [impacts, acts, actsDismissedAt, impactNowMs, bridgeNow, styleGen, mapGen])

  // the smooth toggle: remembered, and every vehicle is drawn as reported until its next report
  useEffect(() => {
    smoothRef.current = smooth
    // also on mount: a remounted map (the error boundary's Retry) must not glide from its predecessor's poses
    resetDrawn()
    setSmoothing(smooth)
    try {
      localStorage.setItem(SMOOTH_KEY, smooth ? 'on' : 'off')
    } catch {
      // storage unavailable: the toggle still works for this session
    }
    if (!smooth && frameId.current) {
      cancelAnimationFrame(frameId.current)
      frameId.current = 0
    }
    pushLayers()
    // pushLayers reads refs only
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [smooth])

  // a hidden tab draws vehicles as reported (no animation frames run there anyway)
  useEffect(() => {
    const onVisibility = () => {
      if (document.visibilityState !== 'hidden') return
      snapAllDrawn(performance.now())
      if (frameId.current) cancelAnimationFrame(frameId.current)
      frameId.current = 0
    }
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      document.removeEventListener('visibilitychange', onVisibility)
      if (frameId.current) cancelAnimationFrame(frameId.current)
      frameId.current = 0
    }
  }, [])

  // Vehicles: deck.gl layers rebuilt here (a new state from the bridge, a selection, the view, a
  // move that changes what is in it or the declutter); while a vehicle glides, only the layers
  // attached to a vehicle are rebuilt per animation frame (glideFrame), reusing these arrays. Only the
  // vehicles in (or near) the view are drawn, the set the bridge sends in full (viewport.ts);
  // in cluster mode the lite ones are the markers' numbers, not symbols.
  useEffect(() => {
    const m = map.current
    if (!m) return
    // a new data generation (the first state after a connect, so after a reconnect too): the
    // vehicles are drawn as reported
    const gen = vehiclesGeneration(allVehicles)
    const newGeneration = gen !== lastGeneration.current
    lastGeneration.current = gen
    if (newGeneration) {
      if (import.meta.env.DEV) console.debug('[murmur] new data generation', gen, ': vehicles drawn as reported')
    }
    // a vehicle that stops reporting an altitude is drawn at the last one it reported
    noteAltitudes(vehicles)
    if (smoothRef.current) {
      // report changes start glides; a new generation or a hidden tab: draw as reported
      const now = performance.now()
      observeVehicles(vehicles, now)
      if (newGeneration || document.visibilityState === 'hidden') snapAllDrawn(now)
    }
    const clustered = !!clusters?.cells.length
    const drawn = visibleVehicles(vehicles, viewport.current, clustered)
    // where each is seen: at its height, or at its foot when that is above the view (aloft.ts)
    const screen = screenNow()
    const surface = ready.current ? surfaceScale(m).surfaceZ : () => 0
    const aloft = new Map<number, number>()
    const seen = drawn.flatMap((v) => {
      if (!v.position) return []
      const [lon, lat] = v.position
      const z = drawnHeight(v)
      if (screen.aloft(lon, lat, z)) aloft.set(v.uid, surface(v.position))
      const p = screen.at(lon, lat, aloft.has(v.uid) ? 0 : z)
      return [p ? { uid: v.uid, x: p[0], y: p[1] } : { uid: v.uid, ...m.project(v.position) }]
    })
    // murmur-02g: where symbols bunch up (planes on a runway), only one keeps its labels;
    // the rest show the bare symbol and their callsign on hover
    const bare = declutter(seen, selected, m.getZoom())
    latest.current = { vehicles: drawn, selected, bare, aloft, clusters: clustered ? clusters : null, oneWay }
    const t1 = performance.now()
    pushLayers()
    wakeGlide()
    if (import.meta.env.DEV) {
      const w = window as unknown as { murmurPerf: Record<string, number[]> }
      w.murmurPerf ??= {}
      const a = (w.murmurPerf.vehicleLayers ??= [])
      ;(window as unknown as { murmurDrawn: number }).murmurDrawn = drawn.length
      a.push(performance.now() - t1)
      if (a.length > 200) a.splice(0, a.length - 200)
    }

    if (ready.current) (m.getSource('beams') as maplibregl.GeoJSONSource).setData(beamFeatures(drawn))

    const pts = vehicles.flatMap((v) => (v.position ? [v.position] : []))
    if (pts.length && !lostRef.current) {
      const c: LngLat = [pts.reduce((a, p) => a + p[0], 0) / pts.length, pts.reduce((a, p) => a + p[1], 0) / pts.length]
      if (!fitAt.current || distanceM(fitAt.current, c) > 50_000) {
        const b = new maplibregl.LngLatBounds(pts[0], pts[0])
        pts.forEach((p) => b.extend(p))
        m.fitBounds(b, { padding: 120, maxZoom: 13.5, duration: fitAt.current ? 1500 : 0, bearing: m.getBearing() })
        fitAt.current = c
      }
    }
    // pushLayers reads refs; moved re-runs this after a move, styleGen after a (re)load
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [vehicles, selected, moved, styleGen, clusters, smooth, oneWay])

  useEffect(() => {
    spaceRef.current = space
    const m = map.current
    if (!m || !ready.current) return
    setSpaceData(m, space)
    pushLayers()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [space, styleGen])

  // predicted link coverage: the image swapped in place (one texture; nothing per frame), or the
  // layer hidden
  useEffect(() => {
    coverageRef.current = coverage
    const m = map.current
    if (!coverage && covTip.current) covTip.current.hidden = true
    if (!m || !ready.current || !m.getLayer(COVERAGE)) return
    const src = m.getSource(COVERAGE) as maplibregl.ImageSource | undefined
    if (coverage && src) {
      src.updateImage({ url: coverage.url, coordinates: coverage.coordinates })
      m.setLayoutProperty(COVERAGE, 'visibility', 'visible')
    } else m.setLayoutProperty(COVERAGE, 'visibility', 'none')
  }, [coverage, styleGen])

  // the drafted order's predicted link
  useEffect(() => {
    if (linkRouteRef.current === linkRoute) return
    linkRouteRef.current = linkRoute
    pushLayers()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [linkRoute])

  // extension layers changed (see the `layers` prop)
  useEffect(() => {
    if (layersRef.current === layers) return
    layersRef.current = layers
    pushLayers()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [layers])

  const vehiclesRef = useRef(vehicles)
  useEffect(() => {
    vehiclesRef.current = vehicles
  }, [vehicles])
  // Aim at the vehicle itself, not the ground under it: aircraft are drawn at altitude x the
  // exaggeration, so the camera centre gets that elevation. Only jumpTo honours a centre
  // elevation (easeTo/flyTo drop it), so following is a per-frame glide built on jumpTo, which
  // also smooths the 4 Hz position updates.
  const following = useRef<{ uid: number; zoom: number | null } | null>(null)
  const followingCb = useRef(onFollowing)
  useEffect(() => {
    followingCb.current = onFollowing
  }, [onFollowing])

  const startFollow = (uid: number, zoom: number | null) => {
    const m = map.current
    if (!m) return
    following.current = { uid, zoom }
    // the details card covers the map's left edge: centre the vehicle in what's left visible
    m.setPadding({ top: 0, bottom: 0, right: 0, left: FOLLOW_PAD_LEFT })
    let last = performance.now()
    const step = (now: number) => {
      const f = following.current
      if (!f || f.uid !== uid || map.current !== m) return
      // real elapsed time, so the glide keeps pace at any frame rate (capped after a stalled tab)
      const dt = Math.min(1, (now - last) / 1000)
      last = now
      if (m.dragPan.isActive()) {
        // the operator grabbed the map: stop following, let the terrain hold the centre again
        following.current = null
        m.setCenterClampedToGround(true)
        m.setPadding({ top: 0, bottom: 0, right: 0, left: 0 })
        followingCb.current?.(false)
        return
      }
      const v = vehiclesRef.current.find((x) => x.uid === uid)
      if (v?.position && !lostRef.current && !m.isZooming() && !m.isRotating()) {
        // smoothing on: aim at where the symbol is drawn (so the symbol stays put in the view
        // instead of stepping ahead of the glide and drifting back), with a tighter glide
        const smooth = smoothRef.current
        if (smooth) advanceDrawn(now)
        const at = smooth ? drawnPosition(v) : v.position
        // tight only while the vehicle is gliding (the target moves smoothly); a snap, a resting
        // vehicle and reduced-motion users get the slow approach as before
        const k = 1 - Math.exp(-dt / (smooth && isGliding(uid) && !reducedNow ? 0.08 : 0.25))
        const c = m.getCenter()
        const elev = smooth ? drawnAlt(v) * HEIGHT_EXAGGERATION : drawnHeight(v)
        m.setCenterClampedToGround(false)
        const z = m.getZoom()
        const zoom = f.zoom == null ? z : z + (f.zoom - z) * k
        if (f.zoom != null && Math.abs(f.zoom - zoom) < 0.01) f.zoom = null
        // `follow` marks the moveend this fires every frame (see the moveend handler)
        m.jumpTo(
          {
            center: [c.lng + (at[0] - c.lng) * k, c.lat + (at[1] - c.lat) * k],
            elevation: m.getCenterElevation() + (elev - m.getCenterElevation()) * k,
            zoom,
          },
          { follow: true },
        )
      }
      requestAnimationFrame(step)
    }
    requestAnimationFrame(step)
  }

  useEffect(() => {
    const m = map.current
    if (!focus || !m || lostRef.current) return
    const v = vehiclesRef.current.find((x) => x.uid === focus.uid)
    if (!v?.position) return
    if (follow === focus.uid) {
      followingCb.current?.(true)
      startFollow(focus.uid, Math.max(m.getZoom(), 13))
    } else {
      m.setCenterClampedToGround(true)
      m.flyTo({ center: v.position, zoom: Math.max(m.getZoom(), 13), duration: 1200 })
    }
    // startFollow reads refs only; a new focus request is the trigger
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus])

  useEffect(() => {
    if (follow == null && following.current) {
      following.current = null
      map.current?.setCenterClampedToGround(true)
      map.current?.setPadding({ top: 0, bottom: 0, right: 0, left: 0 })
    }
  }, [follow])

  // Top view: straight down (pitch 0), for plan judgments (distances, keep-outs), and back to
  // the pitch it left. Only the camera turns: vehicles and tracks stay at their height. The
  // button follows the pitch however it got there (a drag, the compass, the keyboard).
  useEffect(() => {
    const m = map.current
    if (!m) return
    const onPitch = () => setTop(m.getPitch() <= TOP_PITCH)
    onPitch()
    m.on('pitchend', onPitch)
    return () => {
      m.off('pitchend', onPitch)
    }
  }, [mapGen])
  const toggleTop = () => {
    const m = map.current
    if (!m || lostRef.current) return
    if (m.getPitch() > TOP_PITCH) {
      tiltBack.current = m.getPitch()
      m.easeTo({ pitch: 0, duration: reducedNow ? 0 : 600 })
    } else m.easeTo({ pitch: tiltBack.current > TOP_PITCH ? tiltBack.current : INITIAL_PITCH, duration: reducedNow ? 0 : 600 })
  }
  const toggleTopRef = useRef(toggleTop)
  toggleTopRef.current = toggleTop
  // T toggles it, except while typing (a form's field) or with a modifier (the browser's own keys)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 't' && e.key !== 'T') return
      if (e.ctrlKey || e.metaKey || e.altKey || e.repeat) return
      const t = e.target as HTMLElement | null
      if (t && (t.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(t.tagName))) return
      toggleTopRef.current()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  // the drawings on the surface (surfaceLayers.ts): the taskings, the inbox preview, the draft
  useEffect(() => {
    taskingsRef.current = taskings
    previewRef.current = preview
    draftRef.current = draft
    pushLayers()
    // pushLayers reads refs only; styleGen: push again after a (re)load
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [taskings, preview, draft, styleGen])

  useEffect(() => {
    const m = map.current
    if (!fit || !m || fit.points.length === 0 || lostRef.current) return
    const bounds = new maplibregl.LngLatBounds(fit.points[0], fit.points[0])
    for (const p of fit.points) bounds.extend(p)
    const padding = { top: 70, bottom: 70, left: 70, right: fit.padRight + 20 }
    const options = { padding, maxZoom: 13, duration: 1000, bearing: m.getBearing() }
    const cam = fit.aloft?.length ? m.cameraForBounds(bounds, options) : undefined
    if (!cam?.center || cam.zoom == null) {
      m.fitBounds(bounds, options)
      return
    }
    // MapLibre fits on the ground; a point thousands of metres up (drawn twice as high, map3d.ts)
    // is above that view at the operator's pitch. Seen through deck.gl's camera (what draws it),
    // with room at the top for the status line over the map (and below, for the strike): centre
    // the points' extent in the padded view and back off a quarter zoom level at a time until it
    // holds them all, then come back in while it still does.
    const c = m.getCanvas()
    const [width, height] = [c.clientWidth, c.clientHeight]
    const pad = { ...padding, top: 130, bottom: 110 }
    const mid = [(pad.left + width - pad.right) / 2, (pad.top + height - pad.bottom) / 2]
    const bearing = cam.bearing ?? m.getBearing()
    const pitch = m.getPitch()
    const start = maplibregl.LngLat.convert(cam.center)
    const home: LngLat = [start.lng, start.lat]
    const pts: [number, number, number][] = [...fit.points.map(([lon, lat]): [number, number, number] => [lon, lat, 0]), ...fit.aloft!.map(([lon, lat, alt]): [number, number, number] => [lon, lat, z(alt)])]
    /** where the points are on the screen at this centre and zoom; null when one is behind the camera */
    const extent = (center: LngLat, zoom: number) => {
      const vp = new WebMercatorViewport({ width, height, longitude: center[0], latitude: center[1], zoom, pitch, bearing })
      const screen = mapScreen(vp, width, height)
      const xs: number[] = []
      const ys: number[] = []
      for (const [lon, lat, h] of pts) {
        const p = screen.at(lon, lat, h)
        if (!p) return null
        xs.push(p[0])
        ys.push(p[1])
      }
      return { vp, x0: Math.min(...xs), x1: Math.max(...xs), y0: Math.min(...ys), y1: Math.max(...ys) }
    }
    type Extent = ReturnType<typeof extent>
    const holds = (e: Extent) => e != null && e.x0 >= pad.left && e.x1 <= width - pad.right && e.y0 >= pad.top && e.y1 <= height - pad.bottom
    /** The centre that moves the middle of the points' extent towards the middle of the padded
     *  view: by at most a quarter of the view each way (a point far off the screen would send the
     *  centre past the horizon); the centre as it was when even that lands past the horizon or
     *  tens of kilometres off. */
    const centred = (center: LngLat, zoom: number): LngLat => {
      const e = extent(center, zoom)
      if (!e) return center
      const dx = Math.max(-width / 4, Math.min(width / 4, (e.x0 + e.x1) / 2 - mid[0]))
      const dy = Math.max(-height / 4, Math.min(height / 4, (e.y0 + e.y1) / 2 - mid[1]))
      const to = e.vp.unproject([width / 2 + dx, height / 2 + dy], { targetZ: 0 })
      return Number.isFinite(to[0]) && Number.isFinite(to[1]) && distanceM(home, [to[0], to[1]]) < 50_000 ? [to[0], to[1]] : center
    }
    let center = home
    let zoom = cam.zoom
    for (let k = 0; k < 32; k++) {
      // centred twice (the perspective moves the points as the centre moves)
      center = centred(centred(center, zoom), zoom)
      if (holds(extent(center, zoom))) break
      zoom -= 0.25
    }
    // a quarter level closer, centred again, while that holds them too
    for (let k = 0; k < 12 && zoom < 13; k++) {
      const closer = centred(centred(center, zoom + 0.25), zoom + 0.25)
      if (!holds(extent(closer, zoom + 0.25))) break
      center = closer
      zoom += 0.25
    }
    m.flyTo({ center, zoom, bearing, duration: reducedNow ? 0 : 1000 })
  }, [fit])

  useEffect(() => {
    cursor.current = draft.mode ? 'crosshair' : ''
    if (map.current) map.current.getCanvas().style.cursor = cursor.current
  }, [draft, styleGen])

  return (
    <>
      <div ref={container} className="map" />
      <div ref={tooltip} className="vtip" hidden />
      <div ref={covTip} className="covtip" hidden />
      {glLost && (
        <div className="banner gl-banner" role="status">
          Map graphics paused: the browser lost the WebGL context. Waiting for it to come back…{' '}
          <button className="ghost mini" onClick={() => setMapGen((n) => n + 1)}>
            Reload map
          </button>
        </div>
      )}
      <div className="viewtoggle" role="group" aria-label="Map view">
        <button
          className={`topbtn${top ? ' on' : ''}`}
          aria-pressed={top}
          title={
            top
              ? 'Tilt the camera back (T). Heights and depths read from the side.'
              : 'Top view (T): look straight down, for plan judgments such as the distance to a keep-out. Only the camera turns: vehicles and tracks stay at their height.'
          }
          onClick={toggleTop}
        >
          top view
        </button>
        <button
          className={`smoothbtn${smooth ? ' on' : ''}`}
          aria-pressed={smooth}
          title="Smooth the motion of vehicles on the map between position reports. Numbers always show the reports; a vehicle never moves past its last report."
          onClick={() => setSmooth((s) => !s)}
        >
          smooth
        </button>
        <span className="viewnote">heights ×{HEIGHT_EXAGGERATION} · middle-drag: spin + tilt · T: top view</span>
      </div>
    </>
  )
}
