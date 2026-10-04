/// <reference types="geojson" />
import type { Intent } from './decisions'
import { circle } from './geo'
import type { LngLat, Vehicle } from './types'

type FC = GeoJSON.FeatureCollection

export const EMPTY_PREVIEW: FC = { type: 'FeatureCollection', features: [] }

/** Where a pending intent would send its vehicles: the orbit circle or patrol loop, plus a
 *  transit line from each vehicle's current position to the first waypoint (the leg the
 *  geofence and area checks also look at). `sel` marks the one the operator is reading. */
export function previewFeatures(pending: Intent[], vehicles: Vehicle[], selectedId: number | null): FC {
  const byUid = new Map(vehicles.map((v) => [String(v.uid), v]))
  const features: GeoJSON.Feature[] = []
  for (const i of pending) {
    const sel = i.id === selectedId
    const props = { sel, label: `${i.kind.toUpperCase()} i${i.id}` }
    const line = (coordinates: LngLat[], part: string) =>
      features.push({ type: 'Feature', properties: { ...props, part }, geometry: { type: 'LineString', coordinates } })
    const point = (p: LngLat, n: string) =>
      features.push({ type: 'Feature', properties: { ...props, part: 'point', n }, geometry: { type: 'Point', coordinates: p } })
    let first: LngLat | null = null
    const p = i.params
    if (i.kind === 'orbit' && p.center) {
      line(circle(p.center, p.radius_m ?? 200), 'path')
      point(p.center, '')
      first = p.center
    } else if (i.kind === 'patrol' && p.route && p.route.length) {
      line(p.route.length > 2 ? [...p.route, p.route[0]] : p.route, 'path')
      p.route.forEach((q, k) => point(q, String(k + 1)))
      first = p.route[0]
    } else if (i.kind === 'track' && p.target) {
      first = byUid.get(p.target)?.position ?? null
    } else if (i.kind === 'collect' || i.kind === 'cover') {
      // the chosen scene, else the area: no vehicles go anywhere
      const ring = spaceRing(i)
      if (ring) line([...ring, ring[0]], 'path')
      continue
    } else if (i.kind === 'rtl') {
      // the way home is the whole task
      for (const u of i.uids) {
        const v = byUid.get(u)
        if (v?.position && v.home) line([v.position, v.home], 'transit')
      }
      continue
    }
    if (first) {
      for (const u of i.uids) {
        const pos = byUid.get(u)?.position
        if (pos) line([pos, first], 'transit')
      }
    }
  }
  return { type: 'FeatureCollection', features }
}

/** The points an operator should see to judge an intent: its vehicles and its task geometry. */
export function intentPoints(i: Intent, vehicles: Vehicle[]): LngLat[] {
  const byUid = new Map(vehicles.map((v) => [String(v.uid), v]))
  const pts: LngLat[] = []
  for (const u of i.uids) {
    const pos = byUid.get(u)?.position
    if (pos) pts.push(pos)
  }
  const p = i.params
  if (i.kind === 'engage') {
    // the platform's path and the effect point, from the engagement's preview
    const pv = i.engagement?.snapshot?.preview ?? i.engage?.live?.preview ?? i.engagement?.preview
    if (pv) pts.push(...pv.path, ...pv.track_path, ...circle(pv.effect_point, pv.keep_out_m).filter((_, k) => k % 16 === 0))
    return pts
  }
  if (p.center) {
    // include the whole circle, not just its centre
    pts.push(...circle(p.center, p.radius_m ?? 200).filter((_, k) => k % 8 === 0))
  }
  if (p.route) pts.push(...p.route)
  const ring = spaceRing(i)
  if (ring) pts.push(...ring)
  return pts
}

/** A collect's chosen scene, else a collect's or cover's area polygon (a circle's centre). */
function spaceRing(i: Intent): LngLat[] | null {
  if (i.space?.chosen?.scene) return i.space.chosen.scene
  const a = i.params.area as { kind?: string; ring?: LngLat[]; center?: LngLat } | undefined
  if (a?.ring?.length) return a.ring
  if (a?.center) return [a.center]
  return null
}
