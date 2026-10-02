/// <reference types="geojson" />
import * as maplibregl from 'maplibre-gl'
import 'maplibre-gl/dist/maplibre-gl.css'
import { MapboxOverlay } from '@deck.gl/mapbox'
import { useEffect, useRef, useState } from 'react'
import { circle, distanceM } from './geo'
import { BRIDGE_FILTER, liftBridges, type LiftedBridge } from './bridges'
import { build3dLayers, DEM_TILES, HEIGHT_EXAGGERATION } from './map3d'
import { declutter } from './declutter'
import type { SpaceOverlay } from './space'
import { EMPTY_OVERLAY } from './space'
import { linkStubs, spaceFeatures, spaceLinkLayers } from './spaceLayers'
import { symbolSvg } from './symbols'
import type { LngLat, Tasking, Vehicle } from './types'
import { callsign, STALE_MS } from './types'

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
  onMapClick: (p: LngLat) => void
  /** fly to this vehicle (n changes on every request, so repeat clicks work) */
  focus?: { uid: number; n: number } | null
  /** inbox: where pending intents would send their vehicles (see inboxPreview.ts) */
  preview?: FC
  /** inbox: fit the view to these points (n changes on every request) */
  fit?: { points: LngLat[]; n: number; padRight: number } | null
  /** space overlays: area, passes in progress, highlighted scene, SATCOM link lines */
  space?: SpaceOverlay
  /** the view after every move: centre and bounds (w, s, e, n) */
  onView?: (center: LngLat, bounds: [number, number, number, number]) => void
}

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
    const live = t.ops.filter((o) => o.state !== 'superseded' && o.state !== 'failed')
    if (live.length === 0) continue
    // intent stays dashed until at least one vehicle has confirmed it
    const confirmed = live.some((o) => ['acked', 'active', 'done'].includes(o.state))
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

type ViewMode = '2d' | '3d'
const MAX_PITCH = 85
// middle-drag orbit sensitivity, degrees per pixel
const ORBIT_YAW = 0.35
const ORBIT_TILT = 0.25
const VIEW_KEY = 'murmur.view'

function savedView(): ViewMode {
  try {
    return localStorage.getItem(VIEW_KEY) === '3d' ? '3d' : '2d'
  } catch {
    return '2d'
  }
}

/** 2D: flat map, HTML symbol markers. 3D: terrain + seabed, tilted camera, vehicles drawn by
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

function applyView(m: maplibregl.Map, view: ViewMode) {
  setDrapedBridges(m, view === '2d')
  if (view === '3d') {
    m.setTerrain({ source: 'dem', exaggeration: HEIGHT_EXAGGERATION })
    m.setLayoutProperty('hillshade', 'visibility', 'visible')
    m.setLayoutProperty('trails', 'visibility', 'none')
    m.easeTo({ pitch: 62, bearing: m.getBearing() || -25, duration: 900 })
  } else {
    m.setTerrain(null)
    m.setLayoutProperty('hillshade', 'visibility', 'none')
    m.setLayoutProperty('trails', 'visibility', 'visible')
    m.easeTo({ pitch: 0, bearing: 0, duration: 700 })
  }
}

const SPACE_SOURCES = ['space-area', 'space-for', 'space-scenes', 'space-sel', 'space-links', 'space-box'] as const

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
}

function setSpaceData(m: maplibregl.Map, space: SpaceOverlay, view: ViewMode) {
  const f = spaceFeatures(space)
  for (const id of SPACE_SOURCES) (m.getSource(id) as maplibregl.GeoJSONSource | undefined)?.setData(id === 'space-links' ? (view === '3d' ? empty : linkStubs(space.links)) : f[id])
}

export function MapView({ vehicles, taskings, selected, draft, focus, preview = empty, fit, onToggle, onMapClick, space = EMPTY_OVERLAY, onView }: Props) {
  const [view, setView] = useState<ViewMode>(savedView)
  const viewRef = useRef(view)
  const overlay = useRef<MapboxOverlay | null>(null)
  const bridges = useRef<LiftedBridge[]>([])
  const latest = useRef({ vehicles, selected, bare: new Set<number>() })
  const spaceRef = useRef(space)
  const previewRef = useRef(preview)
  const viewCb = useRef(onView)
  // bumped when the camera stops, so label declutter re-runs at the new scale
  const [moved, setMoved] = useState(0)
  const container = useRef<HTMLDivElement>(null)
  const map = useRef<maplibregl.Map | null>(null)
  const ready = useRef(false)
  // centroid we last zoomed to; refit when the swarm shows up somewhere else (new --location)
  const fitAt = useRef<LngLat | null>(null)
  const markers = useRef(new Map<number, { marker: maplibregl.Marker; el: HTMLDivElement; key: string }>())
  const clickRef = useRef(onMapClick)
  const toggleRef = useRef(onToggle)
  useEffect(() => {
    clickRef.current = onMapClick
    toggleRef.current = onToggle
    viewCb.current = onView
  }, [onMapClick, onToggle, onView])

  /** vehicles, bridges and SATCOM link lines for deck.gl (3D only; hidden layers in 2D) */
  const deckLayers = (is3d: boolean) => {
    const { vehicles: vs, selected: sel, bare } = latest.current
    return [...build3dLayers(vs, sel, is3d, bridges.current, bare), ...spaceLinkLayers(spaceRef.current.links, is3d)]
  }

  useEffect(() => {
    const live = markers.current
    const m = new maplibregl.Map({
      container: container.current!,
      style: STYLE,
      center: START,
      zoom: 12.6,
      attributionControl: { compact: true },
      maxPitch: MAX_PITCH,
    })
    if (import.meta.env.DEV) (window as unknown as { murmurMap: maplibregl.Map }).murmurMap = m
    const deck = new MapboxOverlay({ interleaved: true, layers: [] })
    m.addControl(deck)
    overlay.current = deck
    m.addControl(new maplibregl.NavigationControl({ showCompass: true }), 'bottom-right')
    m.addControl(new maplibregl.ScaleControl({ unit: 'metric' }), 'bottom-left')
    m.on('click', (e: maplibregl.MapMouseEvent) => {
      // in 3D, vehicles are deck.gl objects: a click on one selects it instead of drafting
      if (viewRef.current === '3d') {
        const hit = deck.pickObject({ x: e.point.x, y: e.point.y, radius: 6, layerIds: ['vehicles-3d'] })
        if (hit?.object) {
          toggleRef.current((hit.object as Vehicle).uid)
          return
        }
      }
      clickRef.current([e.lngLat.lng, e.lngLat.lat])
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
      if (!orbit) return
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
      reportView()
    })
    // re-estimate bridge decks when the 3D camera stops or new map/terrain tiles arrive.
    // (Not 'idle': live vehicle updates repaint the map several times a second, so it never is.)
    let bridgeTimer: number | undefined
    const refreshBridges = () => {
      window.clearTimeout(bridgeTimer)
      bridgeTimer = window.setTimeout(() => {
        if (viewRef.current !== '3d' || !m.getTerrain()) return
        const t0 = performance.now()
        bridges.current = liftBridges(m)
        if (import.meta.env.DEV) {
          const w = window as unknown as { murmurPerf: Record<string, number[]> }
          w.murmurPerf ??= {}
          ;(w.murmurPerf.liftBridges ??= []).push(performance.now() - t0)
        }
        if (import.meta.env.DEV) (window as unknown as { murmurBridges: LiftedBridge[] }).murmurBridges = bridges.current
        deck.setProps({ layers: deckLayers(true) })
      }, 400)
    }
    const reportView = () => {
      const c = m.getCenter()
      const b = m.getBounds()
      viewCb.current?.([c.lng, c.lat], [b.getWest(), b.getSouth(), b.getEast(), b.getNorth()])
    }
    m.on('moveend', refreshBridges)
    m.on('moveend', reportView)
    m.on('zoomend', () => setMoved((n) => n + 1))
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
      live.forEach(({ marker }) => marker.remove())
      live.clear()
      m.remove()
    }
  }, [])

  // vehicle markers
  useEffect(() => {
    const m = map.current
    if (!m) return
    const seen = new Set<number>()
    // murmur-02g: where symbols bunch up (planes on a runway), only one keeps its labels;
    // the rest show the bare symbol and their callsign on hover
    const bare = declutter(
      vehicles.flatMap((v) => (v.position ? [{ uid: v.uid, ...m.project(v.position) }] : [])),
      selected,
      m.getZoom(),
    )
    for (const v of vehicles) {
      if (!v.position) continue
      seen.add(v.uid)
      const stale = (v.link.since_last_ms ?? 0) > STALE_MS
      const labels = !bare.has(v.uid)
      const sym = symbolSvg(v, { stale, labels })
      const isSel = selected.has(v.uid)
      const staleS = stale ? Math.floor((v.link.since_last_ms ?? 0) / 1000) : 0
      const key = `${sym.svg.length}|${sym.anchor.x}|${sym.anchor.y}|${isSel}|${staleS}|${v.heading}|${labels}`
      let entry = markers.current.get(v.uid)
      if (!entry) {
        const el = document.createElement('div')
        el.className = 'vmarker'
        el.addEventListener('click', (e) => {
          e.stopPropagation()
          toggleRef.current(v.uid)
        })
        const marker = new maplibregl.Marker({ element: el, anchor: 'top-left' }).setLngLat(v.position).addTo(m)
        entry = { marker, el, key: '' }
        markers.current.set(v.uid, entry)
      }
      if (entry.key !== key) {
        entry.el.innerHTML =
          sym.svg +
          (isSel ? `<span class="vmarker-ring" style="left:${sym.anchor.x}px;top:${sym.anchor.y}px"></span>` : '') +
          (stale ? `<span class="vmarker-lost" style="left:${sym.anchor.x}px;top:${sym.anchor.y + 22}px">LOST ${staleS}s</span>` : '')
        entry.el.classList.toggle('stale', stale)
        entry.el.classList.toggle('bare', !labels)
        entry.el.dataset.callsign = labels ? '' : `${callsign(v)}${v.identity?.short ? ` · ${v.identity.short}` : ''}`
        entry.marker.setOffset([-sym.anchor.x, -sym.anchor.y])
        entry.key = key
      }
      entry.marker.setLngLat(v.position)
    }
    for (const [uid, entry] of markers.current) {
      if (!seen.has(uid)) {
        entry.marker.remove()
        markers.current.delete(uid)
      }
    }
    for (const { el } of markers.current.values()) el.style.display = view === '3d' ? 'none' : ''
    latest.current = { vehicles, selected, bare }
    const t1 = performance.now()
    overlay.current?.setProps({ layers: deckLayers(view === '3d') })
    if (import.meta.env.DEV) {
      const w = window as unknown as { murmurPerf: Record<string, number[]> }
      w.murmurPerf ??= {}
      ;(w.murmurPerf.build3d ??= []).push(performance.now() - t1)
    }

    if (ready.current) {
      ;(m.getSource('trails') as maplibregl.GeoJSONSource).setData(trailFeatures(vehicles))
      ;(m.getSource('beams') as maplibregl.GeoJSONSource).setData(beamFeatures(vehicles))
    }

    const pts = vehicles.flatMap((v) => (v.position ? [v.position] : []))
    if (pts.length) {
      const c: LngLat = [pts.reduce((a, p) => a + p[0], 0) / pts.length, pts.reduce((a, p) => a + p[1], 0) / pts.length]
      if (!fitAt.current || distanceM(fitAt.current, c) > 50_000) {
        const b = new maplibregl.LngLatBounds(pts[0], pts[0])
        pts.forEach((p) => b.extend(p))
        m.fitBounds(b, { padding: 120, maxZoom: 13.5, duration: fitAt.current ? 1500 : 0 })
        fitAt.current = c
      }
    }
    // deckLayers reads refs; moved re-runs the declutter after a zoom
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [vehicles, selected, view, moved])

  useEffect(() => {
    spaceRef.current = space
    const m = map.current
    if (!m || !ready.current) return
    setSpaceData(m, space, view)
    overlay.current?.setProps({ layers: deckLayers(view === '3d') })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [space, view])

  const vehiclesRef = useRef(vehicles)
  useEffect(() => {
    vehiclesRef.current = vehicles
  }, [vehicles])
  useEffect(() => {
    if (!focus || !map.current) return
    const v = vehiclesRef.current.find((x) => x.uid === focus.uid)
    if (v?.position) map.current.flyTo({ center: v.position, zoom: Math.max(map.current.getZoom(), 13), duration: 1200 })
  }, [focus])

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
  }, [taskings])

  useEffect(() => {
    previewRef.current = preview
    if (ready.current) (map.current?.getSource('inbox-preview') as maplibregl.GeoJSONSource | undefined)?.setData(preview)
  }, [preview])

  useEffect(() => {
    const m = map.current
    if (!fit || !m || fit.points.length === 0) return
    const bounds = new maplibregl.LngLatBounds(fit.points[0], fit.points[0])
    for (const p of fit.points) bounds.extend(p)
    m.fitBounds(bounds, { padding: { top: 70, bottom: 70, left: 70, right: fit.padRight + 20 }, maxZoom: 13, duration: 1000 })
  }, [fit])

  useEffect(() => {
    if (ready.current) (map.current?.getSource('draft') as maplibregl.GeoJSONSource | undefined)?.setData(draftFeatures(draft))
    if (map.current) map.current.getCanvas().style.cursor = draft.mode ? 'crosshair' : ''
  }, [draft])

  return (
    <>
      <div ref={container} className="map" />
      <div className="viewtoggle" role="group" aria-label="Map view">
        <button className={view === '2d' ? 'on' : ''} onClick={() => setView('2d')}>
          2D
        </button>
        <button className={view === '3d' ? 'on' : ''} onClick={() => setView('3d')}>
          3D
        </button>
        {view === '3d' && <span className="viewnote">heights ×{HEIGHT_EXAGGERATION} · middle-drag: spin + tilt</span>}
      </div>
    </>
  )
}
