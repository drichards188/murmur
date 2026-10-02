/// <reference types="geojson" />
// Theater-map drawing for the space segment: the scenario area, imaging passes in progress
// (field of regard, faint, and the scene they would take), a highlighted scene, a drawn box,
// and link lines from SATCOM vehicles toward their best satellite.
import { LineLayer, TextLayer } from '@deck.gl/layers'
import { HEIGHT_EXAGGERATION } from './map3d'
import type { LinkNow, SpaceOverlay } from './space'
import { classColor } from './space'
import type { LngLat } from './types'

type FC = GeoJSON.FeatureCollection
const fc = (features: GeoJSON.Feature[]): FC => ({ type: 'FeatureCollection', features })
const closed = (ring: LngLat[]) => [...ring, ring[0]]
const poly = (ring: LngLat[], properties: Record<string, unknown> = {}): GeoJSON.Feature => ({
  type: 'Feature',
  properties,
  geometry: { type: 'Polygon', coordinates: [closed(ring)] },
})

export function spaceFeatures(s: SpaceOverlay): Record<string, FC> {
  const scenes: GeoJSON.Feature[] = []
  const fields: GeoJSON.Feature[] = []
  for (const o of s.overhead) {
    const color = classColor(o.class_id)
    // field of regard: hundreds of km across for LEO imagers, so only a faint wash
    fields.push(poly(o.field_of_regard, { color }))
    if (o.scene) {
      const dark = o.daylight_ok === false
      scenes.push({ type: 'Feature', properties: { color }, geometry: { type: 'LineString', coordinates: closed(o.scene) } })
      scenes.push({
        type: 'Feature',
        properties: { color, label: `${o.label ?? o.satellite}${dark ? ' (dark)' : ''} now` },
        geometry: { type: 'Point', coordinates: o.scene[0] },
      })
    }
  }
  const sel: GeoJSON.Feature[] = s.scene
    ? [poly(s.scene.ring), { type: 'Feature', properties: { label: s.scene.label }, geometry: { type: 'Point', coordinates: topLeft(s.scene.ring) } }]
    : []
  return {
    'space-area': fc(s.area ? [{ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: closed(s.area.ring) } }] : []),
    'space-for': fc(fields),
    'space-scenes': fc(scenes),
    'space-sel': fc(sel),
    'space-box': fc(s.box ? [{ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: closed(s.box) } }] : []),
    'space-tasked': fc(
      (s.tasked ?? []).flatMap((t): GeoJSON.Feature[] => [
        { type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: closed(t.ring) } },
        { type: 'Feature', properties: { label: t.label }, geometry: { type: 'Point', coordinates: topLeft(t.ring) } },
      ]),
    ),
  }
}

function topLeft(ring: LngLat[]): LngLat {
  return ring.reduce((best, p) => (p[1] - p[0] * 0.2 > best[1] - best[0] * 0.2 ? p : best), ring[0])
}

/** Point `m` metres from `from` along `bearing` (flat-earth, fine at these scales). */
function offset(from: LngLat, bearingDeg: number, m: number): LngLat {
  const a = (bearingDeg * Math.PI) / 180
  const lat = from[1]
  return [from[0] + (m * Math.sin(a)) / (111_320 * Math.cos((lat * Math.PI) / 180)), from[1] + (m * Math.cos(a)) / 111_320]
}

/** Horizontal reach of a link stub: long when the satellite is low (it is far away along
 *  that bearing), short when it is nearly overhead. */
function stubLength(elDeg: number) {
  return 900 + 4500 * Math.cos((elDeg * Math.PI) / 180)
}

function linkLabel(l: LinkNow) {
  const b = l.best!
  const lat = b.latency_ms != null ? ` ${Math.round(b.latency_ms)} ms` : ''
  return `${b.satellite} ${Math.round(b.elevation_deg)}°${lat}`
}

/** 2D: a dashed stub from each SATCOM vehicle toward its best satellite's azimuth. */
export function linkStubs(links: LinkNow[]): FC {
  const features: GeoJSON.Feature[] = []
  for (const l of links) {
    if (!l.best) continue
    const p: LngLat = [l.position[0], l.position[1]]
    const end = offset(p, l.best.azimuth_deg, stubLength(l.best.elevation_deg))
    features.push({ type: 'Feature', properties: {}, geometry: { type: 'LineString', coordinates: [p, end] } })
    features.push({ type: 'Feature', properties: { label: linkLabel(l) }, geometry: { type: 'Point', coordinates: end } })
  }
  return fc(features)
}

type XYZ = [number, number, number]
interface Ray {
  from: XYZ
  to: XYZ
  label: string
  color: [number, number, number, number]
}

/** 3D: the same links as rays along the true look direction (azimuth and elevation). */
export function spaceLinkLayers(links: LinkNow[], visible: boolean) {
  const rays: Ray[] = links
    .filter((l) => l.best)
    .map((l) => {
      const b = l.best!
      const el = (b.elevation_deg * Math.PI) / 180
      const len = 6000
      const p: LngLat = [l.position[0], l.position[1]]
      const end = offset(p, b.azimuth_deg, len * Math.cos(el))
      const alt = Math.max(0, l.position[2])
      const hex = classColor(l.class_id)
      const rgb = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)) as [number, number, number]
      return {
        from: [p[0], p[1], alt * HEIGHT_EXAGGERATION],
        to: [end[0], end[1], (alt + len * Math.sin(el)) * HEIGHT_EXAGGERATION],
        label: linkLabel(l),
        color: [rgb[0], rgb[1], rgb[2], 170],
      }
    })
  return [
    new LineLayer<Ray>({
      id: 'satcom-rays',
      visible,
      data: rays,
      getSourcePosition: (r) => r.from,
      getTargetPosition: (r) => r.to,
      getColor: (r) => r.color,
      getWidth: 1.5,
      widthUnits: 'pixels',
    }),
    new TextLayer<Ray>({
      id: 'satcom-ray-labels',
      visible,
      data: rays,
      getPosition: (r) => r.to,
      getText: (r) => r.label,
      getColor: [196, 155, 255, 220],
      getSize: 11,
      getTextAnchor: 'start',
      getPixelOffset: [6, 0],
      fontFamily: 'IBM Plex Mono, monospace',
      outlineWidth: 2,
      outlineColor: [11, 15, 20, 255],
      fontSettings: { sdf: true },
      characterSet: 'auto',
    }),
  ]
}
