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
}
const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }

/** The rings as paths on the surface, each over a dark casing. */
export function reachPaths(fc: GeoJSON.FeatureCollection | null, scale: SurfaceScale): { rings: Ring[]; casings: Ring[] } {
  const rings: Ring[] = [], casings: Ring[] = []
  for (const f of fc?.features ?? []) {
    if (f.geometry.type !== 'LineString') continue
    const kind = String(f.properties?.kind ?? '')
    const st = STYLE[kind]
    if (!st) continue
    const [r, g, b] = SENSOR_RGB[String(f.properties?.sensor ?? '')] ?? [210, 210, 210]
    const lifted = lift(f.geometry.coordinates as LngLat[], scale.surfaceZ)
    casings.push({ path: lifted, color: [11, 21, 32, 115], width: 4 })
    const color: RGBA = [r, g, b, Math.round(255 * st.alpha)]
    if (!st.dash) rings.push({ path: lifted, color, width: st.width })
    else for (const p of dashes(lifted, st.dash[0] * st.width * scale.mPerPx, st.dash[1] * st.width * scale.mPerPx)) rings.push({ path: p, color, width: st.width })
  }
  return { rings, casings }
}

export function buildReachLayers(fc: GeoJSON.FeatureCollection | null, scale: SurfaceScale): LayersList {
  const { rings, casings } = reachPaths(fc, scale)
  if (!rings.length) return []
  const common = { getPath: (d: Ring) => d.path, getColor: (d: Ring) => d.color, getWidth: (d: Ring) => d.width, widthUnits: 'pixels' as const, parameters: onTop }
  return [
    new PathLayer<Ring>({ id: 'sensor-reach-casing', data: casings, ...common }),
    new PathLayer<Ring>({ id: 'sensor-reach-rings', data: rings, ...common }),
  ]
}
