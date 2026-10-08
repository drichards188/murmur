// Trails of our vehicles while display smoothing is on (drawn.ts), in 2D and 3D. The symbol is
// drawn up to a second behind the vehicle's last report, so a trail that ends at the report
// would run past it. Two deck.gl layers: the body (the trail but its newest few points) and the
// head (those few points cut at the segment the symbol is on, ending at the symbol; recomputed
// on every glide frame). They meet at one shared point; the paths' caps are flat (capRounded
// off), so nothing overlaps there.
// The body's data array is kept while no trail changed (keyed on the trail arrays, which the
// console replaces only when a point is added), so a bridge update that only moved vehicles
// does not tessellate every trail again.
// With smoothing off the trails are the MapLibre line (2D) and trails-3d (map3d.ts), as before.
import { PathLayer } from '@deck.gl/layers'
import { drawnPose, drawnTick } from './drawn'
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

let memo: { vehicles: Vehicle[]; is3d: boolean; head: Vehicle[]; body: Vehicle[]; key: string } | null = null

function trailedOf(vehicles: Vehicle[], is3d: boolean) {
  // animation frames of a glide pass the same vehicles: the same data arrays go back to deck.gl
  if (memo && memo.vehicles === vehicles && memo.is3d === is3d) return memo
  const head = vehicles.filter((v) => v.position && v.trail.length > 1 && (!is3d || v.alt_msl != null))
  const same =
    memo &&
    memo.is3d === is3d &&
    memo.body.length === head.length &&
    memo.body.every((b, i) => b.uid === head[i].uid && b.trail === head[i].trail)
  if (same) memo = { ...memo!, vehicles, head }
  else memo = { vehicles, is3d, head, body: head, key: head.map((v) => v.trail.length).join() }
  return memo!
}

/** `beforeId`: 2D only, the MapLibre layer these draw under (where the MapLibre trails were:
 *  below the taskings, the draft, labels and overlays); undefined draws them on top. */
export function buildTrailLayers(vehicles: Vehicle[], is3d: boolean, beforeId?: string) {
  const { head, body, key } = trailedOf(vehicles, is3d)
  const zOf = is3d ? z : () => 0
  const colorOf = is3d ? (v: Vehicle) => colour(v, 140) : (v: Vehicle): RGBA => (v.kind && AIR_KINDS.includes(v.kind) ? AIR_2D : SEA_2D)
  const width = is3d ? 2 : 1.5
  const point = ([lon, lat, alt]: [number, number, number]): Pt => [lon, lat, zOf(alt)]
  const where = is3d || !beforeId ? {} : { beforeId }
  return [
    new PathLayer<Vehicle>({
      id: 'trail-body',
      data: body,
      getPath: (v) => v.trail.slice(0, 1 - HEAD_POINTS).map(point),
      getColor: colorOf,
      getWidth: width,
      widthUnits: 'pixels',
      updateTriggers: { getPath: key },
      ...where,
    }),
    new PathLayer<Vehicle>({
      id: 'trail-head',
      data: head,
      getPath: (v) => {
        const p = drawnPose(v)
        const tail = v.trail.slice(-HEAD_POINTS).map(point)
        const drawn: Pt = p ? [p.lon, p.lat, zOf(p.alt)] : [v.position![0], v.position![1], zOf(v.alt_msl)]
        return trailHeadPath(tail, [v.position![0], v.position![1], zOf(v.alt_msl)], drawn)
      },
      getColor: colorOf,
      getWidth: width,
      widthUnits: 'pixels',
      updateTriggers: { getPath: `${key}:${drawnTick()}` },
      ...where,
    }),
  ]
}
