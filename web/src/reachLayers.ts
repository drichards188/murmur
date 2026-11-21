// The sensor reach rings (bd murmur-4tq; sensorReach.ts reachFeatures) drawn by deck.gl on the
// surface: sea level over the water, the ground over land, as the taskings and every other surface
// drawing (surfaceLayers.ts). They were MapLibre lines draped on the terrain, which under water is
// the seabed (the elevation tiles include it, exaggerated twice): in a tilted view a ring sat up to
// a few hundred metres from the vehicle's ground point, its stem's foot (bd murmur-vr17).
import { PathLayer } from '@deck.gl/layers'
import type { LayersList } from '@deck.gl/core'
import type { LngLat } from './types'
import { SENSOR_RGB } from './sensorReach.ts'
import { dashes, lift, type SurfaceScale } from './surfaceLayers.ts'
import { RingCache } from './reachCache.ts'

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]
interface Ring {
  path: XYZ[]
  color: RGBA
  width: number
}

/** the rings' widths and opacities, as their MapLibre layers had them */
const STYLE: Record<string, { width: number; alpha: number; dash?: [number, number] }> = {
  clear: { width: 1.2, alpha: 0.5 },
  detect: { width: 2.2, alpha: 0.95 },
  identify: { width: 1.6, alpha: 0.9, dash: [3, 2] },
  // rules 27: our minefields (minefields.ts): the danger area's outline, a ring on each mine
  minefield: { width: 2, alpha: 0.9, dash: [5, 3] },
  mine: { width: 1.5, alpha: 0.95 },
}
/** our minefields' colour, a warning red apart from the sensors' */
const MINE_RGB: [number, number, number] = [235, 87, 87]
const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }

/** What one feature draws: its dashes or line, and the casing under it. */
interface Built {
  rings: Ring[]
  casing: Ring
}

const move = (path: XYZ[], dLon: number, dLat: number): XYZ[] => path.map((p) => [p[0] + dLon, p[1] + dLat, p[2]])
const moveBuilt = (b: Built, dLon: number, dLat: number): Built => ({
  casing: { ...b.casing, path: move(b.casing.path, dLon, dLat) },
  rings: b.rings.map((r) => ({ ...r, path: move(r.path, dLon, dLat) })),
})

function buildFeature(f: GeoJSON.Feature, st: (typeof STYLE)[string], kind: string, scale: SurfaceScale): Built {
  const [r, g, b] = kind === 'minefield' || kind === 'mine' ? MINE_RGB : SENSOR_RGB[String(f.properties?.sensor ?? '')] ?? [210, 210, 210]
  const lifted = lift((f.geometry as GeoJSON.LineString).coordinates as LngLat[], scale.surfaceZ)
  const color: RGBA = [r, g, b, Math.round(255 * st.alpha)]
  const rings: Ring[] = !st.dash ? [{ path: lifted, color, width: st.width }] : dashes(lifted, st.dash[0] * st.width * scale.mPerPx, st.dash[1] * st.width * scale.mPerPx).map((p) => ({ path: p, color, width: st.width }))
  return { casing: { path: lifted, color: [11, 21, 32, 115], width: 4 }, rings }
}

/** The rings as paths on the surface, each over a dark casing. With a `cache` a sensor ring (one
 *  that says its `id`, centre and radius) is lifted once and then moved with its vehicle while the
 *  move is small (reachCache.ts); `sig` is what else it rests on (the terrain, the zoom). */
export function reachPaths(fc: GeoJSON.FeatureCollection | null, scale: SurfaceScale, cache?: RingCache<Built>, sig = ''): { rings: Ring[]; casings: Ring[] } {
  const rings: Ring[] = [], casings: Ring[] = []
  for (const f of fc?.features ?? []) {
    if (f.geometry.type !== 'LineString') continue
    const kind = String(f.properties?.kind ?? '')
    const st = STYLE[kind]
    if (!st) continue
    const { id, at, m } = (f.properties ?? {}) as { id?: string; at?: LngLat; m?: number }
    const make = () => buildFeature(f, st, kind, scale)
    const built = cache && id && at && m ? cache.get(id, at, m, sig, make, moveBuilt) : make()
    casings.push(built.casing)
    rings.push(...built.rings)
  }
  cache?.sweep()
  return { rings, casings }
}

export type ReachCache = RingCache<Built>
export const newReachCache = (): ReachCache => new RingCache<Built>()

export function buildReachLayers(fc: GeoJSON.FeatureCollection | null, scale: SurfaceScale, cache?: ReachCache, sig = ''): LayersList {
  const { rings, casings } = reachPaths(fc, scale, cache, sig)
  if (!rings.length) return []
  const common = { getPath: (d: Ring) => d.path, getColor: (d: Ring) => d.color, getWidth: (d: Ring) => d.width, widthUnits: 'pixels' as const, parameters: onTop }
  return [
    new PathLayer<Ring>({ id: 'sensor-reach-casing', data: casings, ...common }),
    new PathLayer<Ring>({ id: 'sensor-reach-rings', data: rings, ...common }),
  ]
}
