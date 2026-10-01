/// <reference types="geojson" />
import * as maplibregl from 'maplibre-gl'
import 'maplibre-gl/dist/maplibre-gl.css'
import { useEffect, useRef } from 'react'
import { circle, distanceM } from './geo'
import { symbolSvg } from './symbols'
import type { LngLat, Tasking, Vehicle } from './types'
import { STALE_MS } from './types'

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
  onToggle: (sysid: number) => void
  onMapClick: (p: LngLat) => void
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

function trailFeatures(vehicles: Vehicle[]): FC {
  return {
    type: 'FeatureCollection',
    features: vehicles
      .filter((v) => v.trail.length > 1)
      .map((v) => ({
        type: 'Feature' as const,
        properties: { kind: v.kind },
        geometry: { type: 'LineString' as const, coordinates: v.position ? [...v.trail, v.position] : v.trail },
      })),
  }
}

export function MapView({ vehicles, taskings, selected, draft, onToggle, onMapClick }: Props) {
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
  }, [onMapClick, onToggle])

  useEffect(() => {
    const live = markers.current
    const m = new maplibregl.Map({
      container: container.current!,
      style: STYLE,
      center: START,
      zoom: 12.6,
      attributionControl: { compact: true },
    })
    m.addControl(new maplibregl.NavigationControl({ showCompass: true }), 'bottom-right')
    m.addControl(new maplibregl.ScaleControl({ unit: 'metric' }), 'bottom-left')
    m.on('click', (e: maplibregl.MapMouseEvent) => clickRef.current([e.lngLat.lng, e.lngLat.lat]))
    m.on('load', () => {
      m.addSource('trails', { type: 'geojson', data: empty })
      m.addSource('taskings', { type: 'geojson', data: empty })
      m.addSource('draft', { type: 'geojson', data: empty })
      m.addLayer({
        id: 'trails',
        type: 'line',
        source: 'trails',
        paint: {
          'line-color': ['match', ['get', 'kind'], 'plane', '#7fb8ff', '#6fe0c4'],
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
      ready.current = true
    })
    map.current = m
    return () => {
      ready.current = false
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
    for (const v of vehicles) {
      if (!v.position) continue
      seen.add(v.sysid)
      const stale = (v.link.since_last_ms ?? 0) > STALE_MS
      const sym = symbolSvg(v, { stale })
      const isSel = selected.has(v.sysid)
      const staleS = stale ? Math.floor((v.link.since_last_ms ?? 0) / 1000) : 0
      const key = `${sym.svg.length}|${sym.anchor.x}|${sym.anchor.y}|${isSel}|${staleS}|${v.heading}`
      let entry = markers.current.get(v.sysid)
      if (!entry) {
        const el = document.createElement('div')
        el.className = 'vmarker'
        el.addEventListener('click', (e) => {
          e.stopPropagation()
          toggleRef.current(v.sysid)
        })
        const marker = new maplibregl.Marker({ element: el, anchor: 'top-left' }).setLngLat(v.position).addTo(m)
        entry = { marker, el, key: '' }
        markers.current.set(v.sysid, entry)
      }
      if (entry.key !== key) {
        entry.el.innerHTML =
          sym.svg +
          (isSel ? `<span class="vmarker-ring" style="left:${sym.anchor.x}px;top:${sym.anchor.y}px"></span>` : '') +
          (stale ? `<span class="vmarker-lost" style="left:${sym.anchor.x}px;top:${sym.anchor.y + 22}px">LOST ${staleS}s</span>` : '')
        entry.el.classList.toggle('stale', stale)
        entry.marker.setOffset([-sym.anchor.x, -sym.anchor.y])
        entry.key = key
      }
      entry.marker.setLngLat(v.position)
    }
    for (const [sysid, entry] of markers.current) {
      if (!seen.has(sysid)) {
        entry.marker.remove()
        markers.current.delete(sysid)
      }
    }
    if (ready.current) (m.getSource('trails') as maplibregl.GeoJSONSource).setData(trailFeatures(vehicles))

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
  }, [vehicles, selected])

  useEffect(() => {
    if (ready.current) (map.current?.getSource('taskings') as maplibregl.GeoJSONSource | undefined)?.setData(taskingFeatures(taskings))
  }, [taskings])

  useEffect(() => {
    if (ready.current) (map.current?.getSource('draft') as maplibregl.GeoJSONSource | undefined)?.setData(draftFeatures(draft))
    if (map.current) map.current.getCanvas().style.cursor = draft.mode ? 'crosshair' : ''
  }, [draft])

  return <div ref={container} className="map" />
}
