// Trails of our vehicles while display smoothing is on (drawn.ts). The symbol is drawn up to a
// second behind the vehicle's last report, so a trail that ends at the report would run past
// it. Two deck.gl layers, each twice (what is under water, drawn seen through the water's tint,
// and the rest): the body (the trail but its newest few points) and the head (those few points
// cut at the segment the symbol is on, ending at the symbol; recomputed on every glide frame).
// They meet at one shared point; the paths' caps are flat (capRounded off), so nothing overlaps
// there.
// The body's data array is kept while no trail changed (keyed on the trail arrays, which the
// console replaces only when a point is added), so a bridge update that only moved vehicles
// does not tessellate every trail again.
// With smoothing off the trails are trails-3d and trails-3d-under (map3d.ts), split the same way.
// A vehicle without an altitude now keeps its trail: it ends where the symbol is drawn (drawn.ts
// shownAlt), as every trail does.
import { PathLayer } from '@deck.gl/layers'
import { drawnPose, drawnTick, shownAlt } from './drawn'
import { trailHeadPath, type Pt } from './glide'
import { colour, z } from './map3d'
import { belowOf, seenThroughWater, splitUnder, type UnderSplit } from './sea'
import type { Vehicle } from './types'

/** trail points kept in the head: the trail gets a point a second, the symbol is at most about
 *  a second behind the report */
export const HEAD_POINTS = 5

let memo: { vehicles: Vehicle[]; head: Vehicle[]; body: Vehicle[]; key: string } | null = null

function trailedOf(vehicles: Vehicle[]) {
  // animation frames of a glide pass the same vehicles: the same data arrays go back to deck.gl
  if (memo && memo.vehicles === vehicles) return memo
  const head = vehicles.filter((v) => v.position && v.trail.length > 1)
  const same = memo && memo.body.length === head.length && memo.body.every((b, i) => b.uid === head[i].uid && b.trail === head[i].trail)
  if (same) memo = { ...memo!, vehicles, head }
  else memo = { vehicles, head, body: head, key: head.map((v) => v.trail.length).join() }
  return memo!
}

// the body and the head split into what is under water and the rest, each kept while its list
// and the split are the same (sea.ts splitUnder): a bridge update that only moved vehicles keeps
// the body's arrays, and a glide frame keeps the head's (deck.gl redoes a layer whose data array
// changes)
let bodySplit: UnderSplit<Vehicle> | null = null
let headSplit: UnderSplit<Vehicle> | null = null

/** Trails as deck.gl layers: `under` the trails seen through the water (sea.ts underWater, drawn
 *  in sea.ts seenThroughWater's colour), `over` the rest. */
export function buildTrailLayers(vehicles: Vehicle[]) {
  const { head, body, key } = trailedOf(vehicles)
  const colorOf = (v: Vehicle) => colour(v, 140)
  const point = ([lon, lat, alt]: [number, number, number]): Pt => [lon, lat, z(alt)]
  // judged on the latest reports (head); the body's vehicles can be older objects
  const { below, sig } = belowOf(head)
  bodySplit = splitUnder(bodySplit, body, below, sig)
  headSplit = splitUnder(headSplit, head, below, sig)
  const seen = (v: Vehicle) => seenThroughWater(colorOf(v))
  const layers = (suffix: string, bodyData: Vehicle[], headData: Vehicle[], getColor = colorOf) => [
    new PathLayer<Vehicle>({
      id: `trail-body${suffix}`,
      data: bodyData,
      getPath: (v) => v.trail.slice(0, 1 - HEAD_POINTS).map(point),
      getColor,
      getWidth: 2,
      widthUnits: 'pixels',
      updateTriggers: { getPath: key },
    }),
    new PathLayer<Vehicle>({
      id: `trail-head${suffix}`,
      data: headData,
      getPath: (v) => {
        const p = drawnPose(v)
        const tail = v.trail.slice(-HEAD_POINTS).map(point)
        const reported: Pt = [v.position![0], v.position![1], z(shownAlt(v))]
        const drawn: Pt = p ? [p.lon, p.lat, z(p.alt)] : reported
        return trailHeadPath(tail, reported, drawn)
      },
      getColor,
      getWidth: 2,
      widthUnits: 'pixels',
      updateTriggers: { getPath: `${key}:${drawnTick()}` },
    }),
  ]
  return { under: layers('-under', bodySplit.under, headSplit.under, seen), over: layers('', bodySplit.over, headSplit.over) }
}
