import { LineLayer, PathLayer, ScatterplotLayer } from '@deck.gl/layers'
import type { LiftedBridge } from './bridges'
import { drawnTick, drawnXYZ, shownAlt, smoothing } from './drawn'
import { seenThroughWater, underWater } from './sea'
import type { Vehicle } from './types'
import { domainOf } from './types'

// Heights are exaggerated in 3D so 80 m altitudes and 5 m depths read at bay scale.
// Terrain uses the same factor, so vehicles stay in the right place relative to hills and seabed.
export const HEIGHT_EXAGGERATION = 2

// Terrarium-encoded elevation tiles (AWS open data, Mapzen). Includes bathymetry:
// the Golden Gate channel reads -96 m, matching NOAA's DEM.
export const DEM_TILES = 'https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png'

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]
const AIR: RGBA = [127, 184, 255, 255]
const SEA: RGBA = [111, 224, 196, 255]
const GROUND: RGBA = [214, 196, 140, 255]

export function colour(v: Vehicle, alpha = 255): RGBA {
  const c = domainOf(v) === 'air' ? AIR : domainOf(v) === 'ground' ? GROUND : SEA
  return [c[0], c[1], c[2], alpha]
}

export function z(metresMsl: number | null) {
  return (metresMsl ?? 0) * HEIGHT_EXAGGERATION
}

/** Where a vehicle is drawn, in rendered metres (the camera aims here to follow it): its
 *  altitude, or the last one it reported while it has none (drawn.ts shownAlt). */
export function drawnHeight(v: Vehicle) {
  return z(shownAlt(v))
}

/** The scene around the vehicles is built in two parts: build3dStatic (trails, bridge decks)
 *  and build3dGlide (height stems and sea-level shadows). The vehicles themselves are in
 *  vehicleLayers.ts. Every vehicle with a position is placed: one without an altitude now at
 *  the last one it reported, else at the surface (drawn.ts shownAlt).
 *
 *  The water itself is the style's water fill, tinted and draped on the terrain (sea.ts
 *  drapedWater, MapView.tintDrapedWater): the seabed is tinted wherever it lies, with nothing at
 *  0 m to fight it for depth. What is under water (sea.ts underWater: a submarine's trail and
 *  stem, anything more than a metre down) is drawn in its colour seen through that tint (sea.ts
 *  seenThroughWater), the rest in its own: bridge decks, the trails and stems of surface vessels
 *  and aircraft, sea-level shadows and the screen-space overlays. A stem runs from sea level to
 *  its vehicle, so it lies wholly on one side of the surface. */

/** Trails and bridge decks: nothing here moves with a glide (display smoothing, drawn.ts), so
 *  MapView builds them when the data changes and reuses them between frames. `under`: the
 *  trails of what is under water, seen through it; `over` the rest. */
export function build3dStatic(vehicles: Vehicle[], bridges: LiftedBridge[] = []) {
  const placed = vehicles.filter((v) => v.position)
  const at = (v: Vehicle): XYZ => [v.position![0], v.position![1], drawnHeight(v)]
  // with smoothing on, trailLayers.ts draws the trails (they end at the drawn position)
  const trailed = smoothing() ? [] : placed.filter((v) => v.trail.length > 1)
  const trails = (id: string, data: Vehicle[], seen = false) =>
    new PathLayer<Vehicle>({
      id,
      data,
      getPath: (v) => [...v.trail.map(([lon, lat, alt]): XYZ => [lon, lat, z(alt)]), at(v)],
      getColor: seen ? (v) => seenThroughWater(colour(v, 140)) : (v) => colour(v, 140),
      getWidth: 2,
      widthUnits: 'pixels',
      updateTriggers: { getPath: data.map((v) => v.trail.length).join() },
    })
  return {
    under: [trails('trails-3d-under', trailed.filter(underWater), true)],
    decks: [
      // bridge decks at estimated height (see bridges.ts), over the water: never tinted
      new PathLayer<LiftedBridge>({
        id: 'bridge-decks',
        data: bridges,
        getPath: (b) => b.path,
        getColor: (b) => (b.major ? [58, 64, 74, 255] : [44, 49, 57, 255]),
        getWidth: (b) => (b.major ? 14 : 8),
        widthUnits: 'meters',
        widthMinPixels: 2,
        capRounded: true,
        jointRounded: true,
      }),
    ],
    over: [trails('trails-3d', trailed.filter((v) => !underWater(v)))],
  }
}

// the vehicles placed, kept while the vehicles are the same (animation frames of a glide)
let memo: { vehicles: Vehicle[]; placed: Vehicle[]; under: Vehicle[]; over: Vehicle[] } | null = null

/** Stems and sea-level shadows: they follow a vehicle's drawn position (display smoothing,
 *  drawn.ts), so MapView rebuilds just these on an animation frame. The stems of what is under
 *  water are drawn seen through it, the other stems in their own colour. */
export function build3dGlide(vehicles: Vehicle[]) {
  if (!memo || memo.vehicles !== vehicles) {
    const placed = vehicles.filter((v) => v.position)
    memo = { vehicles, placed, under: placed.filter(underWater), over: placed.filter((v) => !underWater(v)) }
  }
  const { placed, under, over } = memo
  const smooth = smoothing()
  const at = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], drawnHeight(v)]
  const ground = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, 0) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], 0]
  const triggers = smooth ? { getSourcePosition: drawnTick(), getTargetPosition: drawnTick(), getPosition: drawnTick() } : {}
  // stem from sea level to the vehicle: shows height (or depth) at a glance
  const stems = (id: string, data: Vehicle[], seen = false) =>
    new LineLayer<Vehicle>({
      id,
      data,
      getSourcePosition: ground,
      getTargetPosition: at,
      getColor: seen ? (v) => seenThroughWater(colour(v, 160)) : (v) => colour(v, 160),
      getWidth: 1.5,
      widthUnits: 'pixels',
      updateTriggers: triggers,
    })
  return {
    stemsUnder: stems('stems-under', under, true),
    stems: stems('stems', over),
    shadows: new ScatterplotLayer<Vehicle>({
      id: 'sea-level-shadow',
      data: placed,
      getPosition: ground,
      getRadius: 4,
      radiusUnits: 'pixels',
      getFillColor: [0, 0, 0, 110],
      updateTriggers: triggers,
    }),
  }
}
