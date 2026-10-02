/// <reference types="geojson" />
import * as maplibregl from 'maplibre-gl'
import { useEffect, useRef, useState } from 'react'
import type { AreaInfo, LinkNow, Positions, Track } from './space'
import { CLASS_COLOR, getJson, usePoll } from './space'
import type { LngLat } from './types'

interface Props {
  /** theater map centre; the globe follows it until the operator grabs it */
  center: LngLat | null
  area: AreaInfo | null
  links: LinkNow[]
  /** satellite whose ground track to draw */
  norad: number | null
  onClose: () => void
}

const STYLE = 'https://tiles.openfreemap.org/styles/dark'
type FC = GeoJSON.FeatureCollection
const empty: FC = { type: 'FeatureCollection', features: [] }

/** Split a track where it crosses the antimeridian so lines don't wrap the globe. */
function splitTrack(track: LngLat[]): LngLat[][] {
  const parts: LngLat[][] = [[]]
  for (let i = 0; i < track.length; i++) {
    if (i > 0 && Math.abs(track[i][0] - track[i - 1][0]) > 180) parts.push([])
    parts[parts.length - 1].push(track[i])
  }
  return parts.filter((p) => p.length > 1)
}

/** A small MapLibre globe linked to the theater map: satellites by class (subsampled by the
 *  bridge), the selected satellite's ground track, the scenario area and SATCOM links.
 *  Mounted only while shown, so it costs the main map nothing when hidden. */
export function GlobeInset({ center, area, links, norad, onClose }: Props) {
  const container = useRef<HTMLDivElement>(null)
  const map = useRef<maplibregl.Map | null>(null)
  const [ready, setReady] = useState(false)
  const positions = usePoll<Positions>('/api/space/positions?limit=250', 5000)
  const [track, setTrack] = useState<Track | null>(null)
  // follow the theater map until the operator spins the globe themselves
  const [follow, setFollow] = useState(true)
  const [big, setBig] = useState(false)

  useEffect(() => {
    const m = new maplibregl.Map({
      container: container.current!,
      style: STYLE,
      center: center ?? [-122.5, 37.8],
      zoom: 0.9,
      attributionControl: false,
      // drag spins the globe; right-drag or ctrl-drag tilts and turns it; scroll zooms
      maxPitch: 85,
      minZoom: 0,
      maxZoom: 6,
    })
    // only a gesture by the operator (it carries an originalEvent) stops the follow, not our jumpTo
    const grab = (e: { originalEvent?: unknown }) => {
      if (e.originalEvent) setFollow(false)
    }
    m.on('dragstart', grab)
    m.on('rotatestart', grab)
    m.on('pitchstart', grab)
    m.on('zoomstart', grab)
    if (import.meta.env.DEV) (window as unknown as { murmurGlobe: maplibregl.Map }).murmurGlobe = m
    m.on('style.load', () => {
      m.setProjection({ type: 'globe' })
      // the base style is busy at this scale: keep land, water and boundaries only
      for (const l of m.getStyle().layers) {
        if (l.type === 'symbol' || (l.id.includes('road') && l.type === 'line')) m.setLayoutProperty(l.id, 'visibility', 'none')
      }
      m.addSource('area', { type: 'geojson', data: empty })
      m.addSource('track', { type: 'geojson', data: empty })
      m.addSource('sats', { type: 'geojson', data: empty })
      m.addSource('glinks', { type: 'geojson', data: empty })
      m.addLayer({ id: 'track', type: 'line', source: 'track', paint: { 'line-color': '#ffb347', 'line-width': 1.5, 'line-opacity': 0.8 } })
      m.addLayer({ id: 'glinks', type: 'line', source: 'glinks', paint: { 'line-color': '#c49bff', 'line-width': 1, 'line-opacity': 0.7 } })
      m.addLayer({
        id: 'sats',
        type: 'circle',
        source: 'sats',
        paint: {
          'circle-radius': ['case', ['get', 'sel'], 4, ['get', 'small'], 1.2, 2],
          'circle-color': ['get', 'color'],
          'circle-opacity': ['case', ['get', 'small'], 0.55, 0.95],
          'circle-stroke-width': ['case', ['get', 'sel'], 1.5, 0],
          'circle-stroke-color': '#ffffff',
        },
      })
      m.addLayer({ id: 'area', type: 'line', source: 'area', paint: { 'line-color': '#f4f1de', 'line-width': 1.5 } })
      setReady(true)
    })
    map.current = m
    return () => {
      map.current = null
      m.remove()
    }
    // the centre follows props below; creating the map once is intended
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    if (follow && center && map.current) map.current.jumpTo({ center })
  }, [center, follow])

  useEffect(() => {
    map.current?.resize()
  }, [big])

  const recenter = () => {
    setFollow(true)
    map.current?.easeTo({ center: center ?? undefined, bearing: 0, pitch: 0, zoom: 0.9, duration: 600 })
  }

  useEffect(() => {
    if (norad == null) return
    let stop = false
    getJson<Track>(`/api/space/track?norad=${norad}&step=30`)
      .then((t) => !stop && setTrack(t))
      .catch(() => !stop && setTrack(null))
    return () => {
      stop = true
    }
  }, [norad])

  useEffect(() => {
    const m = map.current
    if (!ready || !m) return
    const features: GeoJSON.Feature[] = []
    for (const c of positions.data?.classes ?? []) {
      const small = c.class_id === 'starlink'
      for (const [lon, lat, , id] of c.sats)
        features.push({
          type: 'Feature',
          properties: { color: CLASS_COLOR[c.class_id] ?? '#aab6c3', small, sel: id === norad },
          geometry: { type: 'Point', coordinates: [lon, lat] },
        })
    }
    ;(m.getSource('sats') as maplibregl.GeoJSONSource).setData({ type: 'FeatureCollection', features })
  }, [positions.data, norad, ready])

  useEffect(() => {
    const m = map.current
    if (!ready || !m) return
    const t = norad != null && track?.norad_id === norad ? track.track : []
    ;(m.getSource('track') as maplibregl.GeoJSONSource).setData({
      type: 'FeatureCollection',
      features: splitTrack(t).map((coordinates) => ({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates } })),
    })
  }, [track, norad, ready])

  useEffect(() => {
    const m = map.current
    if (!ready || !m) return
    ;(m.getSource('area') as maplibregl.GeoJSONSource).setData(
      area
        ? { type: 'Feature', properties: {}, geometry: { type: 'Polygon', coordinates: [[...area.ring, area.ring[0]]] } }
        : empty,
    )
    ;(m.getSource('glinks') as maplibregl.GeoJSONSource).setData({
      type: 'FeatureCollection',
      features: links
        .filter((l) => l.best?.position)
        .map((l) => ({
          type: 'Feature',
          properties: {},
          geometry: { type: 'LineString', coordinates: [[l.position[0], l.position[1]], [l.best!.position![0], l.best!.position![1]]] },
        })),
    })
  }, [area, links, ready])

  const counts = positions.data?.classes.map((c) => `${c.class_id} ${c.shown}${c.shown < c.total ? `/${c.total}` : ''}`).join(' · ')
  return (
    <div className={`globe${big ? ' big' : ''}`} aria-label="Globe inset">
      <div ref={container} className="globe-map" />
      <div className="globe-head">
        <span>GLOBE</span>
        {positions.data?.time_shifted && (
          <span className="shifted" title={positions.data.time_note}>
            +{positions.data.time_map.shift_days} d
          </span>
        )}
        <span className="globe-tools">
          {!follow && (
            <button className="ghost mini" onClick={recenter} title="Follow the theater map again (resets tilt and rotation)">
              follow
            </button>
          )}
          <button className="ghost mini" onClick={() => setBig((b) => !b)} title={big ? 'Shrink' : 'Enlarge'} aria-label={big ? 'Shrink globe' : 'Enlarge globe'}>
            {big ? '–' : '+'}
          </button>
        </span>
        <button className="ghost mini" onClick={onClose} aria-label="Hide globe">
          ✕
        </button>
      </div>
      <div className="globe-foot" title={counts}>
        {positions.error ? positions.error : counts ? 'satellites by class, subsampled' : 'loading…'}
        {track && norad === track.norad_id ? ` · track: ${track.satellite}` : ''}
        {' · drag to spin, right-drag to tilt'}
      </div>
    </div>
  )
}
