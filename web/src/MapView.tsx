/// <reference types="geojson" />
import * as maplibregl from 'maplibre-gl'
import 'maplibre-gl/dist/maplibre-gl.css'
import { Layer, type LayersList, type PickingInfo } from '@deck.gl/core'
import { MapboxOverlay } from '@deck.gl/mapbox'
import { useEffect, useRef, useState } from 'react'
import { circle, distanceM } from './geo'
import { BRIDGE_FILTER, liftBridges, type LiftedBridge } from './bridges'
import { build3dGlide, build3dStatic, drawnHeight, refreshSea, DEM_TILES, HEIGHT_EXAGGERATION } from './map3d'
import { declutter } from './declutter'
import { advanceDrawn, drawnAlt, drawnPosition, drawnPositionOf, drawnTick, glidingCount, isGliding, observeVehicles, resetDrawn, setSmoothing, snapAllDrawn } from './drawn'
import { GLIDE_LAYER_IDS } from './orderLayers'
import { buildTrailLayers } from './trailLayers'
import { perfSeaCheck, perfWatchDeck } from './perf'
import { reducedMotion } from './reveal'
import { drapedWater } from './sea'
import { vehiclesGeneration } from './useBridge'
import type { SpaceOverlay } from './space'
import { EMPTY_OVERLAY } from './space'
import { linkStubs, spaceFeatures, spaceLinkLayers } from './spaceLayers'
import type { Cluster, Clusters, LngLat, Tasking, Vehicle } from './types'
import { callsign } from './types'
import { buildVehicleLayers, VEHICLE_PICK_LAYERS } from './vehicleLayers'
import { buildClusterLayers, CLUSTER_PICK_LAYERS } from './clusterLayers'
import { type Bounds, type MapViewport, visibleVehicles } from './viewport'

export interface Draft {
  mode: 'orbit' | 'patrol' | null
  center: LngLat | null
  radius: number
  route: LngLat[]
}

interface Props {
  vehicles: Vehicle[]
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
  /** inbox: fit the view to these points (n changes on every request) */
  fit?: { points: LngLat[]; n: number; padRight: number } | null
  /** space overlays: area, passes in progress, highlighted scene, SATCOM link lines */
  space?: SpaceOverlay
  /** the view while it moves (4 times a second) and when it stops: centre, bounds (w, s, e,
   *  n) and zoom; what the bridge's level of detail follows (murmur-and) */
  onView?: (center: LngLat, bounds: Bounds, zoom: number) => void
  /** counts per tile from the bridge while the view holds more vehicles than it sends in
   *  full: drawn as cluster markers instead of the lite vehicles' symbols (clusterLayers.ts) */
  clusters?: Clusters | null
  /** Extension point for more deck.gl layers (tracks, contacts, ...): called with the current
   *  view whenever MapView rebuilds its layers, and again whenever this function changes, so
   *  pass a memoised callback whose identity changes when its data does. Its layers draw above
   *  the map and the 3D scene and below the vehicles; they survive a WebGL context loss (the
   *  overlay is rebuilt from this callback). Hide 3D-only layers with `visible: view === '3d'`.
   *  Pickable layers get deck.gl's own onClick/onHover. */
  layers?: MapLayers
}

export type ViewMode = '2d' | '3d'
export type MapLayers = (view: ViewMode) => LayersList

/** pending-approval previews from the inbox: dashed violet, the one being read in bold */
function addPreviewLayers(m: maplibregl.Map) {
  m.addSource('inbox-preview', { type: 'geojson', data: empty })
  const w = ['case', ['get', 'sel'], 3, 1.5] as maplibregl.ExpressionSpecification
  m.addLayer({
    id: 'inbox-transit',
    type: 'line',
    source: 'inbox-preview',
    filter: ['all', ['==', ['geometry-type'], 'LineString'], ['==', ['get', 'part'], 'transit']],
    paint: { 'line-color': '#c49bff', 'line-width': w, 'line-dasharray': [0.5, 2], 'line-opacity': 0.8 },
  })
  m.addLayer({
    id: 'inbox-path',
    type: 'line',
    source: 'inbox-preview',
    filter: ['all', ['==', ['geometry-type'], 'LineString'], ['==', ['get', 'part'], 'path']],
    paint: { 'line-color': '#c49bff', 'line-width': w, 'line-dasharray': [3, 2], 'line-opacity': 0.95 },
  })
  m.addLayer({
    id: 'inbox-points',
    type: 'circle',
    source: 'inbox-preview',
    filter: ['==', ['geometry-type'], 'Point'],
    paint: { 'circle-radius': ['case', ['get', 'sel'], 7, 4], 'circle-color': '#c49bff', 'circle-stroke-color': '#0b0f14', 'circle-stroke-width': 1.5 },
  })
}

const STYLE = 'https://tiles.openfreemap.org/styles/dark'
const START: LngLat = [-117.2, 32.703]

type FC = GeoJSON.FeatureCollection
const empty: FC = { type: 'FeatureCollection', features: [] }

function taskingFeatures(taskings: Tasking[]): FC {
  const features: GeoJSON.Feature[] = []
  for (const t of taskings) {
    const live = t.ops.filter((o) => o.state !== 'superseded' && o.state !== 'failed' && o.state !== 'requeued')
    if (live.length === 0) continue
    // ordered vs confirmed: dashed until a vehicle's own telemetry shows it executing (an
    // ACK says it accepted the order, not that it flies it)
    const confirmed = live.some((o) => ['active', 'done'].includes(o.state))
    const props = { kind: t.task, confirmed, label: `${t.task.toUpperCase()} #${t.id}` }
    if (t.task === 'orbit' && t.params.center) {
      features.push({
        type: 'Feature',
        properties: props,
        geometry: { type: 'LineString', coordinates: circle(t.params.center, t.params.radius ?? 200) },
      })
      features.push({ type: 'Feature', properties: props, geometry: { type: 'Point', coordinates: t.params.center } })
    } else if (t.task === 'patrol' && t.params.route) {
      const r = t.params.route
      features.push({
        type: 'Feature',
        properties: props,
        geometry: { type: 'LineString', coordinates: [...r, r[0]] },
      })
      r.forEach((p) => features.push({ type: 'Feature', properties: props, geometry: { type: 'Point', coordinates: p } }))
    }
  }
  return { type: 'FeatureCollection', features }
}

function draftFeatures(d: Draft): FC {
  const features: GeoJSON.Feature[] = []
  if (d.mode === 'orbit' && d.center) {
    features.push({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: circle(d.center, d.radius) } })
    features.push({ type: 'Feature', properties: { n: '' }, geometry: { type: 'Point', coordinates: d.center } })
  }
  if (d.mode === 'patrol' && d.route.length) {
    const coords = d.route.length > 2 ? [...d.route, d.route[0]] : d.route
    if (coords.length > 1) features.push({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: coords } })
    d.route.forEach((p, i) =>
      features.push({ type: 'Feature', properties: { n: String(i + 1) }, geometry: { type: 'Point', coordinates: p } }),
    )
  }
  return { type: 'FeatureCollection', features }
}

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

function trailFeatures(vehicles: Vehicle[]): FC {
  return {
    type: 'FeatureCollection',
    features: vehicles
      .filter((v) => v.trail.length > 1)
      .map((v) => ({
        type: 'Feature' as const,
        properties: { kind: v.kind },
        geometry: {
          type: 'LineString' as const,
          coordinates: [...v.trail.map(([lon, lat]) => [lon, lat]), ...(v.position ? [v.position] : [])],
        },
      })),
  }
}

const MAX_PITCH = 85
// middle-drag orbit sensitivity, degrees per pixel
const ORBIT_YAW = 0.35
const ORBIT_TILT = 0.25
const VIEW_KEY = 'murmur.view'
const SMOOTH_KEY = 'murmur.smooth'
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

function savedView(): ViewMode {
  try {
    return localStorage.getItem(VIEW_KEY) === '3d' ? '3d' : '2d'
  } catch {
    return '2d'
  }
}

/** 2D: flat map, vehicles drawn flat by deck.gl. 3D: terrain + seabed, tilted camera, vehicles drawn by
 *  deck.gl at their true altitude/depth. Tasking and drafting work the same in both. */
// original filters of the style's road layers, so 2D can restore them
const roadFilters = new WeakMap<maplibregl.Map, Map<string, maplibregl.FilterSpecification | undefined>>()

/** In 3D, draped bridges would dive to the seabed: drop them from the style's road layers
 *  (they're redrawn at deck height by deck.gl). 2D restores the style's own filters. */
function setDrapedBridges(m: maplibregl.Map, show: boolean) {
  let saved = roadFilters.get(m)
  if (!saved) {
    saved = new Map()
    for (const l of m.getStyle().layers) {
      if ('source-layer' in l && l['source-layer'] === 'transportation') saved.set(l.id, l.filter)
    }
    roadFilters.set(m, saved)
  }
  for (const [id, filter] of saved) {
    // the style's road filters are expression filters, so they combine with ours under 'all'
    const combined = filter ? (['all', filter, BRIDGE_FILTER] as unknown as maplibregl.FilterSpecification) : BRIDGE_FILTER
    m.setFilter(id, show ? filter : combined)
  }
}

// the style's own water fill (colour, sort key) per water layer, so 2D can restore it
const waterFills = new WeakMap<maplibregl.Map, { id: string; color: unknown; sortKey: unknown }[]>()
let warnedWater = false

/** In 3D the sea surface (map3d.ts) tints only the sea, at 0 m; every other water polygon
 *  (lakes, reservoirs, ponds, rivers) is tinted by the style's own water fill, draped on the
 *  terrain, in the colour the surface gives the sea (sea.ts drapedWater), with the sea's
 *  polygons drawn last so a lake inside the coastline is tinted once. 2D restores the style's
 *  own fill. Changing either relays out the water's tiles, as the road filters do. */
function setDrapedWater(m: maplibregl.Map, is3d: boolean) {
  let saved = waterFills.get(m)
  if (!saved) {
    saved = m
      .getStyle()
      .layers.filter((l) => l.type === 'fill' && 'source-layer' in l && l['source-layer'] === 'water')
      .map((l) => ({ id: l.id, color: m.getPaintProperty(l.id, 'fill-color'), sortKey: m.getLayoutProperty(l.id, 'fill-sort-key') }))
    waterFills.set(m, saved)
  }
  for (const { id, color, sortKey } of saved) {
    const draped = is3d ? drapedWater(color) : null
    if (is3d && !draped && !warnedWater) {
      warnedWater = true
      console.warn(`[murmur] the style's water colour (${id}) is not a plain colour: lakes and rivers are left untinted in 3D (sea.ts drapedWater)`)
    }
    m.setPaintProperty(id, 'fill-color', (draped?.color ?? color) as maplibregl.ExpressionSpecification)
    m.setLayoutProperty(id, 'fill-sort-key', (draped?.sortKey ?? sortKey) as maplibregl.ExpressionSpecification)
  }
}

/** `animate`: tilt the camera into the view (a toggle); false keeps it (restoring a lost context) */
function applyView(m: maplibregl.Map, view: ViewMode, animate = true) {
  setDrapedBridges(m, view === '2d')
  setDrapedWater(m, view === '3d')
  if (view === '3d') {
    m.setTerrain({ source: 'dem', exaggeration: HEIGHT_EXAGGERATION })
    m.setLayoutProperty('hillshade', 'visibility', 'visible')
    m.setLayoutProperty('trails', 'visibility', 'none')
    if (animate) m.easeTo({ pitch: 62, bearing: m.getBearing() || -25, duration: 900 })
  } else {
    m.setTerrain(null)
    m.setLayoutProperty('hillshade', 'visibility', 'none')
    m.setLayoutProperty('trails', 'visibility', 'visible')
    if (animate) m.easeTo({ pitch: 0, bearing: 0, duration: 700 })
  }
}

const SPACE_SOURCES = ['space-area', 'space-for', 'space-scenes', 'space-sel', 'space-links', 'space-box', 'space-tasked'] as const

/** Space overlays, drawn subtly under the vehicles: the earthbound picture stays primary. */
function addSpaceLayers(m: maplibregl.Map) {
  for (const id of SPACE_SOURCES) m.addSource(id, { type: 'geojson', data: empty })
  const before = m.getLayer('trails') ? 'trails' : undefined
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
  m.addLayer({ id: 'space-sel-fill', type: 'fill', source: 'space-sel', paint: { 'fill-color': '#ffb347', 'fill-opacity': 0.12 } }, before)
  m.addLayer({ id: 'space-sel', type: 'line', source: 'space-sel', paint: { 'line-color': '#ffb347', 'line-width': 2 } }, before)
  m.addLayer({ id: 'space-box', type: 'line', source: 'space-box', paint: { 'line-color': '#f4f1de', 'line-width': 1.5, 'line-dasharray': [2, 2] } })
  m.addLayer({ id: 'space-links', type: 'line', source: 'space-links', filter: ['==', ['geometry-type'], 'LineString'], paint: { 'line-color': '#c49bff', 'line-width': 1, 'line-opacity': 0.7, 'line-dasharray': [2, 2] } })
  m.addLayer({
    id: 'space-labels',
    type: 'symbol',
    source: 'space-scenes',
    filter: ['==', ['geometry-type'], 'Point'],
    layout: { 'text-field': ['get', 'label'], 'text-size': 10, 'text-font': ['Noto Sans Regular'], 'text-anchor': 'top-left', 'text-offset': [0.3, 0.2] },
    paint: { 'text-color': ['get', 'color'], 'text-opacity': 0.8, 'text-halo-color': '#0b0f14', 'text-halo-width': 1 },
  })
  m.addLayer({
    id: 'space-sel-label',
    type: 'symbol',
    source: 'space-sel',
    filter: ['==', ['geometry-type'], 'Point'],
    layout: { 'text-field': ['get', 'label'], 'text-size': 11, 'text-font': ['Noto Sans Bold'], 'text-anchor': 'bottom-left', 'text-allow-overlap': true },
    paint: { 'text-color': '#ffb347', 'text-halo-color': '#0b0f14', 'text-halo-width': 1.2 },
  })
  m.addLayer({
    id: 'space-link-labels',
    type: 'symbol',
    source: 'space-links',
    filter: ['==', ['geometry-type'], 'Point'],
    layout: { 'text-field': ['get', 'label'], 'text-size': 10, 'text-font': ['Noto Sans Regular'], 'text-anchor': 'left', 'text-offset': [0.4, 0] },
    paint: { 'text-color': '#c49bff', 'text-opacity': 0.85, 'text-halo-color': '#0b0f14', 'text-halo-width': 1 },
  })
  // tasked satellite collections still running: their scene outline
  m.addLayer({ id: 'space-tasked', type: 'line', source: 'space-tasked', filter: ['==', ['geometry-type'], 'LineString'], paint: { 'line-color': '#c49bff', 'line-width': 2, 'line-dasharray': [4, 2] } })
  m.addLayer({
    id: 'space-tasked-label',
    type: 'symbol',
    source: 'space-tasked',
    filter: ['==', ['geometry-type'], 'Point'],
    layout: { 'text-field': ['get', 'label'], 'text-size': 11, 'text-font': ['Noto Sans Bold'], 'text-anchor': 'bottom-left', 'text-allow-overlap': true },
    paint: { 'text-color': '#c49bff', 'text-halo-color': '#0b0f14', 'text-halo-width': 1.2 },
  })
}

function setSpaceData(m: maplibregl.Map, space: SpaceOverlay, view: ViewMode) {
  const f = spaceFeatures(space)
  for (const id of SPACE_SOURCES) (m.getSource(id) as maplibregl.GeoJSONSource | undefined)?.setData(id === 'space-links' ? (view === '3d' ? empty : linkStubs(space.links)) : f[id])
}

/** width of the vehicle details card (.vdetails) plus its margin */
const FOLLOW_PAD_LEFT = 350

export function MapView({ vehicles, taskings, selected, draft, focus, follow = null, onFollowing, preview = empty, fit, onToggle, onDeselect, onMapClick, space = EMPTY_OVERLAY, onView, layers, clusters = null }: Props) {
  const [view, setView] = useState<ViewMode>(savedView)
  const viewRef = useRef(view)
  // display smoothing of our vehicles' motion (glide.ts, drawn.ts): the choice is remembered
  const [smooth, setSmooth] = useState(savedSmooth)
  const smoothRef = useRef(smooth)
  const frameId = useRef(0)
  // the layers that do not move with a glide, kept between animation frames
  const staticLayers = useRef<{ three: ReturnType<typeof build3dStatic>; space: LayersList; ext: LayersList; clusters: LayersList } | null>(null)
  const lastGlideFrame = useRef(0)
  const lastGeneration = useRef<number | undefined>(undefined)
  const previewGliding = useRef(false)
  const overlay = useRef<MapboxOverlay | null>(null)
  const bridges = useRef<LiftedBridge[]>([])
  const latest = useRef({ vehicles, selected, bare: new Set<number>(), clusters })
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

  /** everything deck.gl draws: the 3D scene (hidden in 2D), SATCOM links, extension layers
   *  (the `layers` prop), then the vehicles on top. `frame`: an animation frame of a glide, which
   *  rebuilds only what is attached to a vehicle and reuses the rest as it was. Around the sea
   *  surface the order is what decides the tint (map3d.ts): what is under water before it,
   *  everything else after it. */
  const deckLayers = (is3d: boolean, frame = false): LayersList => {
    const { vehicles: vs, selected: sel, bare, clusters: cl } = latest.current
    if (!frame || !staticLayers.current) {
      staticLayers.current = {
        three: build3dStatic(vs, is3d, bridges.current),
        space: spaceLinkLayers(spaceRef.current.links, is3d),
        ext: layersRef.current?.(is3d ? '3d' : '2d') ?? [],
        clusters: buildClusterLayers(cl),
      }
    }
    const { three, space, ext, clusters: clusterLayers } = staticLayers.current
    const tick = drawnTick()
    // 2D trails draw where the MapLibre trails layer sat (under the taskings), not on top of everything
    const trailsBefore = !is3d && ready.current && map.current?.getLayer('taskings-intent') ? 'taskings-intent' : undefined
    const glide = build3dGlide(vs, is3d)
    const trails = smoothRef.current ? buildTrailLayers(vs, is3d, trailsBefore) : { under: [], over: [] }
    const layers: LayersList = [
      // seen through the water: the trails and stems of what is under it
      ...three.under,
      ...trails.under,
      glide.stemsUnder,
      // the sea surface, then what is above or on it (never tinted): bridge decks, the other
      // trails and stems, sea-level shadows
      ...three.surface,
      ...three.over,
      ...trails.over,
      glide.stems,
      glide.shadows,
      ...space,
      // the extension layers that sit at a vehicle follow its glide: re-trigger just those
      ...(frame ? ext.map((l) => (l instanceof Layer && GLIDE_LAYER_IDS.includes(l.id) ? l.clone({ updateTriggers: { ...l.props.updateTriggers, getPosition: tick } } as never) : l)) : ext),
      ...clusterLayers,
      ...buildVehicleLayers({ vehicles: vs, selected: sel, bare, is3d }),
    ]
    if (import.meta.env.DEV && is3d && !frame && !seaOrderWarned.current.before) {
      const before = layers.find((l) => l instanceof Layer && l.props.visible && (l.props as { beforeId?: string }).beforeId)
      if (before) {
        seaOrderWarned.current.before = true
        console.warn(`[murmur] 3D layer ${(before as Layer).id} has a beforeId: it draws in another group, out of the sea's order (map3d.ts)`)
      }
    }
    return layers
  }
  /** Dev only: the sea's stencil passes and depth order assume nothing of MapLibre's draws after
   *  it in 3D: every 3D deck.gl layer in the group with no beforeId (checked here as the layers
   *  are built), and that group the style's last layer (checked after every frame, in the map's
   *  render handler). Each warns once. */
  const seaOrderWarned = useRef({ before: false })
  const pushLayers = () => overlay.current?.setProps({ layers: deckLayers(viewRef.current === '3d') })

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
  const glideFrame = (t: number) => {
    frameId.current = 0
    if (!smoothRef.current || lostRef.current || !overlay.current) return
    const every = latest.current.vehicles.length > GLIDE_MANY ? 1000 / 30 : 1000 / 60
    if (!following.current && t - lastGlideFrame.current < every - 2) {
      // too soon: wait for the next display frame (the glide may have ended meanwhile; the next real frame then draws its end)
      frameId.current = requestAnimationFrame(glideFrame)
      return
    }
    lastGlideFrame.current = t
    const t0 = performance.now()
    const n = advanceDrawn(t)
    overlay.current.setProps({ layers: deckLayers(viewRef.current === '3d', true) })
    if (import.meta.env.DEV) {
      const w = window as unknown as { murmurPerf: Record<string, number[]> }
      w.murmurPerf ??= {}
      const a = (w.murmurPerf.glideFrame ??= [])
      a.push(performance.now() - t0)
      if (a.length > 600) a.splice(0, a.length - 600)
    }
    // the preview is a MapLibre source: rewritten only while one of its vehicles glides (and once more as it ends)
    const m = map.current
    if (m && ready.current) {
      const uids = previewRef.current.features.flatMap((f) => (f.properties?.uid == null ? [] : [f.properties.uid as number]))
      const now = uids.some(isGliding)
      if (now || previewGliding.current) (m.getSource('inbox-preview') as maplibregl.GeoJSONSource | undefined)?.setData(previewData())
      previewGliding.current = now
    }
    if (n > 0) frameId.current = requestAnimationFrame(glideFrame)
  }
  const wakeGlide = () => {
    if (!frameId.current && smoothRef.current && glidingCount() > 0 && document.visibilityState === 'visible') frameId.current = requestAnimationFrame(glideFrame)
  }

  useEffect(() => {
    const cam = camera.current
    const m = new maplibregl.Map({
      container: container.current!,
      style: STYLE,
      center: cam?.center ?? START,
      zoom: cam?.zoom ?? 12.6,
      bearing: cam?.bearing ?? 0,
      pitch: cam?.pitch ?? 0,
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
        tip.textContent = `${callsign(v)}${v.identity?.short ? ` · ${v.identity.short}` : ''}`
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

    /** the vehicle under a screen point, in either view */
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
      clickRef.current([e.lngLat.lng, e.lngLat.lat])
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
    // Middle-drag orbits the camera like a 3D modelling tool: left/right spins (bearing),
    // up/down tilts (pitch). In 2D only the spin applies, so the map stays flat.
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
      const pitch =
        viewRef.current === '3d' ? Math.max(0, Math.min(MAX_PITCH, orbit.pitch - (e.clientY - orbit.y) * ORBIT_TILT)) : 0
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
      // features some layer uses, and in 3D the road layers drop bridges (see setDrapedBridges)
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
          layout: { visibility: 'none' },
          paint: { 'hillshade-exaggeration': 0.35, 'hillshade-shadow-color': '#000000' },
        },
        firstSymbol,
      )
      m.addSource('trails', { type: 'geojson', data: empty })
      m.addSource('beams', { type: 'geojson', data: empty })
      m.addLayer({
        id: 'beams',
        type: 'line',
        source: 'beams',
        paint: { 'line-color': '#f4f1de', 'line-width': 1.5, 'line-opacity': 0.55, 'line-dasharray': [4, 3] },
      })
      m.addSource('taskings', { type: 'geojson', data: empty })
      m.addSource('draft', { type: 'geojson', data: empty })
      m.addLayer({
        id: 'trails',
        type: 'line',
        source: 'trails',
        paint: {
          'line-color': ['match', ['get', 'kind'], ['plane', 'vtol', 'copter', 'blimp'], '#7fb8ff', '#6fe0c4'],
          'line-opacity': 0.45,
          'line-width': 1.5,
        },
      })
      const color = ['match', ['get', 'kind'], 'orbit', '#ffb347', 'patrol', '#c49bff', '#d7dde4'] as maplibregl.ExpressionSpecification
      m.addLayer({
        id: 'taskings-intent',
        type: 'line',
        source: 'taskings',
        filter: ['all', ['==', ['geometry-type'], 'LineString'], ['!', ['get', 'confirmed']]],
        paint: { 'line-color': color, 'line-width': 2, 'line-dasharray': [2, 2], 'line-opacity': 0.9 },
      })
      m.addLayer({
        id: 'taskings-confirmed',
        type: 'line',
        source: 'taskings',
        filter: ['all', ['==', ['geometry-type'], 'LineString'], ['get', 'confirmed']],
        paint: { 'line-color': color, 'line-width': 2, 'line-opacity': 0.75 },
      })
      m.addLayer({
        id: 'taskings-points',
        type: 'circle',
        source: 'taskings',
        filter: ['==', ['geometry-type'], 'Point'],
        paint: { 'circle-radius': 4, 'circle-color': color, 'circle-stroke-color': '#0b0f14', 'circle-stroke-width': 1.5 },
      })
      m.addLayer({
        id: 'draft-line',
        type: 'line',
        source: 'draft',
        filter: ['==', ['geometry-type'], 'LineString'],
        paint: { 'line-color': '#f4f1de', 'line-width': 2, 'line-dasharray': [1, 1.5] },
      })
      m.addLayer({
        id: 'draft-points',
        type: 'circle',
        source: 'draft',
        filter: ['==', ['geometry-type'], 'Point'],
        paint: { 'circle-radius': 9, 'circle-color': '#f4f1de', 'circle-stroke-color': '#0b0f14', 'circle-stroke-width': 2 },
      })
      m.addLayer({
        id: 'draft-labels',
        type: 'symbol',
        source: 'draft',
        filter: ['==', ['geometry-type'], 'Point'],
        layout: { 'text-field': ['get', 'n'], 'text-size': 11, 'text-font': ['Noto Sans Bold'], 'text-allow-overlap': true },
        paint: { 'text-color': '#0b0f14' },
      })
      addPreviewLayers(m)
      ;(m.getSource('inbox-preview') as maplibregl.GeoJSONSource).setData(previewRef.current)
      addSpaceLayers(m)
      ready.current = true
      if (viewRef.current === '3d') applyView(m, '3d')
      setSpaceData(m, spaceRef.current, viewRef.current)
      pushLayers()
      reportView()
      setStyleGen((n) => n + 1)
    })
    // re-estimate bridge decks when the 3D camera stops or new map/terrain tiles arrive.
    // (Not 'idle': live vehicle updates repaint the map several times a second, so it never is.)
    let bridgeTimer: number | undefined
    const refreshBridges = () => {
      window.clearTimeout(bridgeTimer)
      bridgeTimer = window.setTimeout(() => {
        if (!ready.current || viewRef.current !== '3d' || !m.getTerrain()) return
        const t0 = performance.now()
        bridges.current = liftBridges(m)
        if (import.meta.env.DEV) {
          const w = window as unknown as { murmurPerf: Record<string, number[]> }
          w.murmurPerf ??= {}
          ;(w.murmurPerf.liftBridges ??= []).push(performance.now() - t0)
        }
        if (import.meta.env.DEV) (window as unknown as { murmurBridges: LiftedBridge[] }).murmurBridges = bridges.current
        pushLayers()
      }, 400)
    }
    // The 3D sea surface follows the basemap's water tiles (map3d.ts refreshSea): checked after a
    // frame, at most 4 times a second, and rebuilt only when the set of water tiles changed (a
    // check reads which tiles hold how many water features, not their geometry). After a frame,
    // not on moves or tile events: tiles MapLibre takes back from its cache become renderable on
    // a later frame with no event of their own, and a check 250 ms after a jump can run before
    // that frame.
    let seaTimer: number | undefined
    const refreshSeaSoon = () => {
      if (seaTimer) return
      seaTimer = window.setTimeout(() => {
        seaTimer = undefined
        if (!ready.current || lostRef.current || viewRef.current !== '3d') return
        const t0 = performance.now()
        const changed = refreshSea(m)
        perfSeaCheck(performance.now() - t0, changed)
        if (changed) pushLayers()
      }, 250)
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
    m.on('render', refreshSeaSoon)
    if (import.meta.env.DEV) {
      // after every frame in 3D: was a MapLibre layer drawn after the deck.gl group, so after the
      // sea? (deck.gl moves its group back to the end whenever its layers are set, so this would
      // be a layer added since.) Warns once, then stops looking.
      const checkSeaLast = () => {
        if (viewRef.current !== '3d' || !ready.current) return
        const order = m.getLayersOrder()
        const last = order[order.length - 1]
        if (order.includes('deck-layer-group-last') && last !== 'deck-layer-group-last') {
          m.off('render', checkSeaLast)
          console.warn(`[murmur] a frame drew MapLibre's ${last} after the deck.gl group, so after the sea (map3d.ts)`)
        }
      }
      m.on('render', checkSeaLast)
    }
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
      overlay.current = null
      surface.removeEventListener('mousedown', onDown)
      surface.removeEventListener('auxclick', noAux)
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
      window.clearTimeout(bridgeTimer)
      window.clearTimeout(viewTimer)
      window.clearTimeout(seaTimer)
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
    if (ready.current) (map.current?.getSource('inbox-preview') as maplibregl.GeoJSONSource | undefined)?.setData(previewData())
    // previewData reads refs only
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
    const gen = vehiclesGeneration(vehicles)
    const newGeneration = gen !== lastGeneration.current
    lastGeneration.current = gen
    if (newGeneration) {
      if (import.meta.env.DEV) console.debug('[murmur] new data generation', gen, ': vehicles drawn as reported')
    }
    if (smoothRef.current) {
      // report changes start glides; a new generation or a hidden tab: draw as reported
      const now = performance.now()
      observeVehicles(vehicles, now)
      if (newGeneration || document.visibilityState === 'hidden') snapAllDrawn(now)
    }
    const clustered = !!clusters?.cells.length
    const drawn = visibleVehicles(vehicles, viewport.current, clustered)
    // murmur-02g: where symbols bunch up (planes on a runway), only one keeps its labels;
    // the rest show the bare symbol and their callsign on hover
    const bare = declutter(
      drawn.flatMap((v) => (v.position ? [{ uid: v.uid, ...m.project(v.position) }] : [])),
      selected,
      m.getZoom(),
    )
    latest.current = { vehicles: drawn, selected, bare, clusters: clustered ? clusters : null }
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

    if (ready.current) {
      // the 2D trails layer is hidden in 3D: skip rebuilding it there
      // (with smoothing on, trailLayers.ts draws them, ending at the drawn position)
      if (view === '2d') (m.getSource('trails') as maplibregl.GeoJSONSource).setData(smooth ? empty : trailFeatures(drawn))
      ;(m.getSource('beams') as maplibregl.GeoJSONSource).setData(beamFeatures(drawn))
    }

    const pts = vehicles.flatMap((v) => (v.position ? [v.position] : []))
    if (pts.length && !lostRef.current) {
      const c: LngLat = [pts.reduce((a, p) => a + p[0], 0) / pts.length, pts.reduce((a, p) => a + p[1], 0) / pts.length]
      if (!fitAt.current || distanceM(fitAt.current, c) > 50_000) {
        const b = new maplibregl.LngLatBounds(pts[0], pts[0])
        pts.forEach((p) => b.extend(p))
        m.fitBounds(b, { padding: 120, maxZoom: 13.5, duration: fitAt.current ? 1500 : 0 })
        fitAt.current = c
      }
    }
    // pushLayers reads refs; moved re-runs this after a move, styleGen after a (re)load
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [vehicles, selected, view, moved, styleGen, clusters, smooth])

  useEffect(() => {
    spaceRef.current = space
    const m = map.current
    if (!m || !ready.current) return
    setSpaceData(m, space, view)
    pushLayers()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [space, view, styleGen])

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
  // Aim at the vehicle itself, not the ground under it: in 3D aircraft are drawn at altitude x
  // the exaggeration, so the camera centre gets that elevation. Only jumpTo honours a centre
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
        const elev = viewRef.current === '3d' ? (smooth ? drawnAlt(v) * HEIGHT_EXAGGERATION : drawnHeight(v)) : 0
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

  useEffect(() => {
    viewRef.current = view
    try {
      localStorage.setItem(VIEW_KEY, view)
    } catch {
      // private mode / blocked storage: the toggle still works for this session
    }
    if (map.current && ready.current) {
      applyView(map.current, view)
      map.current.fire('moveend')
    }
  }, [view])

  useEffect(() => {
    if (ready.current) (map.current?.getSource('taskings') as maplibregl.GeoJSONSource | undefined)?.setData(taskingFeatures(taskings))
    // styleGen: push again after a (re)load
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [taskings, styleGen])

  useEffect(() => {
    previewRef.current = preview
    if (ready.current) (map.current?.getSource('inbox-preview') as maplibregl.GeoJSONSource | undefined)?.setData(previewData())
    // previewData reads refs only
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [preview, styleGen])

  useEffect(() => {
    const m = map.current
    if (!fit || !m || fit.points.length === 0 || lostRef.current) return
    const bounds = new maplibregl.LngLatBounds(fit.points[0], fit.points[0])
    for (const p of fit.points) bounds.extend(p)
    m.fitBounds(bounds, { padding: { top: 70, bottom: 70, left: 70, right: fit.padRight + 20 }, maxZoom: 13, duration: 1000 })
  }, [fit])

  useEffect(() => {
    if (ready.current) (map.current?.getSource('draft') as maplibregl.GeoJSONSource | undefined)?.setData(draftFeatures(draft))
    cursor.current = draft.mode ? 'crosshair' : ''
    if (map.current) map.current.getCanvas().style.cursor = cursor.current
  }, [draft, styleGen])

  return (
    <>
      <div ref={container} className="map" />
      <div ref={tooltip} className="vtip" hidden />
      {glLost && (
        <div className="banner gl-banner" role="status">
          Map graphics paused: the browser lost the WebGL context. Waiting for it to come back…{' '}
          <button className="ghost mini" onClick={() => setMapGen((n) => n + 1)}>
            Reload map
          </button>
        </div>
      )}
      <div className="viewtoggle" role="group" aria-label="Map view">
        <button className={view === '2d' ? 'on' : ''} onClick={() => setView('2d')}>
          2D
        </button>
        <button className={view === '3d' ? 'on' : ''} onClick={() => setView('3d')}>
          3D
        </button>
        <button
          className={`smoothbtn${smooth ? ' on' : ''}`}
          aria-pressed={smooth}
          title="Smooth the motion of vehicles on the map between position reports. Numbers always show the reports; a vehicle never moves past its last report."
          onClick={() => setSmooth((s) => !s)}
        >
          smooth
        </button>
        {view === '3d' && <span className="viewnote">heights ×{HEIGHT_EXAGGERATION} · middle-drag: spin + tilt</span>}
      </div>
    </>
  )
}
