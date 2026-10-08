// Trails of our vehicles while display smoothing is on (drawn.ts), in 2D and 3D. The symbol is
// drawn up to a second behind the vehicle's last report, so a trail that ends at the report
// would run past it. Two deck.gl layers: the body (the trail but its newest few points;
// rebuilt only when the trail changes) and the head (those few points cut at the segment the
// symbol is on, ending at the symbol; recomputed on every glide frame).
// With smoothing off the trails are the MapLibre line (2D) and trails-3d (map3d.ts), as before.
import { PathLayer } from '@deck.gl/layers'
import { drawnAlt, drawnPosition, drawnTick } from './drawn'
import { trailHeadPath, type Pt } from './glide'
import { colour, z } from './map3d'
import type { Vehicle } from './types'
import { AIR_KINDS } from './types'

/** trail points kept in the head: the trail gets a point a second, the symbol is at most about
 *  a second behind the report */
export const HEAD_POINTS = 5

type RGBA = [number, number, number, number]
const AIR_2D: RGBA = [127, 184, 255, 115]
const SEA_2D: RGBA = [111, 224, 196, 115]

let memo: { vehicles: Vehicle[]; is3d: boolean; trailed: Vehicle[]; key: string } | null = null

function trailedOf(vehicles: Vehicle[], is3d: boolean) {
  if (memo && memo.vehicles === vehicles && memo.is3d === is3d) return memo
  const trailed = vehicles.filter((v) => v.position && v.trail.length > 1 && (!is3d || v.alt_msl != null))
  memo = { vehicles, is3d, trailed, key: trailed.map((v) => v.trail.length).join() }
  return memo
}

export function buildTrailLayers(vehicles: Vehicle[], is3d: boolean) {
  const { trailed, key } = trailedOf(vehicles, is3d)
  const zOf = is3d ? z : () => 0
  const colorOf = is3d ? (v: Vehicle) => colour(v, 140) : (v: Vehicle): RGBA => (v.kind && AIR_KINDS.includes(v.kind) ? AIR_2D : SEA_2D)
  const width = is3d ? 2 : 1.5
  const point = ([lon, lat, alt]: [number, number, number]): Pt => [lon, lat, zOf(alt)]
  return [
    new PathLayer<Vehicle>({
      id: 'trail-body',
      data: trailed,
      getPath: (v) => v.trail.slice(0, 1 - HEAD_POINTS).map(point),
      getColor: colorOf,
      getWidth: width,
      widthUnits: 'pixels',
      updateTriggers: { getPath: key },
    }),
    new PathLayer<Vehicle>({
      id: 'trail-head',
      data: trailed,
      getPath: (v) => {
        const p = drawnPosition(v)
        const tail = v.trail.slice(-HEAD_POINTS).map(point)
        return trailHeadPath(tail, [v.position![0], v.position![1], zOf(v.alt_msl)], [p[0], p[1], zOf(drawnAlt(v))])
      },
      getColor: colorOf,
      getWidth: width,
      widthUnits: 'pixels',
      updateTriggers: { getPath: `${key}:${drawnTick()}` },
    }),
  ]
}
