import { LayerExtension } from '@deck.gl/core'
import { LineLayer, PathLayer } from '@deck.gl/layers'
import type { LiftedBridge } from './bridges'
import { drawnTick, drawnXYZ, shownAlt } from './drawn'
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

/** A billboard's size in pixels is exact only at the camera's focal distance: deck.gl scales it
 *  by that distance over the point's own, so in a pitched view what is nearer the camera is drawn
 *  larger. With heights drawn twice over, an aircraft high up can come within a few hundred
 *  metres of the camera: REAPER 1 at 7,600 m after a strike's View was drawn a fifth of the map
 *  tall, its label in 40 px letters (bd murmur-iobd.6). This caps a billboard (icon, text, its
 *  background, a pixel-sized ring) at its nominal size: never larger nearer the camera, smaller
 *  far off as before, so the label budget's boxes (declutter.ts) hold for it. For billboard
 *  layers only: it reads the clip position the billboard branch has just set. */
class NoNearGrowth extends LayerExtension {
  static extensionName = 'NoNearGrowth'
  getShaders() {
    return { inject: { 'vs:DECKGL_FILTER_SIZE': 'size *= clamp(gl_Position.w / project.focalDistance, 0.0, 1.0);' } }
  }
}

/** `extensions` for a billboard layer of pixel-sized marks (NoNearGrowth): one shared instance,
 *  so a rebuilt layer keeps its shader. */
export const NO_NEAR_GROWTH = [new NoNearGrowth()]

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
 *  and build3dGlide (height stems). The vehicles themselves are in
 *  vehicleLayers.ts. Every vehicle with a position is placed: one without an altitude now at
 *  the last one it reported, else at the surface (drawn.ts shownAlt).
 *
 *  The water itself is the style's water fill, tinted and draped on the terrain (sea.ts
 *  drapedWater, MapView.tintDrapedWater): the seabed is tinted wherever it lies, with nothing at
 *  0 m to fight it for depth. What is under water (sea.ts underWater: a submarine's trail and
 *  stem, anything more than a metre down) is drawn in its colour seen through that tint (sea.ts
 *  seenThroughWater), the rest in its own: bridge decks, the trails and stems of surface vessels
 *  and aircraft, and the screen-space overlays. A stem runs from sea level to
 *  its vehicle, so it lies wholly on one side of the surface. */

/** The bridge decks at their estimated height (see bridges.ts), over the water: never tinted.
 *  MapView swaps just this layer when new decks are lifted. */
export function bridgeDeckLayer(bridges: LiftedBridge[]) {
  return new PathLayer<LiftedBridge>({
    id: 'bridge-decks',
    data: bridges,
    getPath: (b) => b.path,
    getColor: (b) => (b.major ? [58, 64, 74, 255] : [44, 49, 57, 255]),
    getWidth: (b) => (b.major ? 14 : 8),
    widthUnits: 'meters',
    widthMinPixels: 2,
    capRounded: true,
    jointRounded: true,
  })
}

/** The bridge decks: nothing here moves with a glide (display smoothing, drawn.ts), so MapView
 *  builds them when the data changes and reuses them between frames. (The trails are
 *  trailLayers.ts's: they end at the drawn position.) */
export function build3dStatic(bridges: LiftedBridge[] = []) {
  return { decks: [bridgeDeckLayer(bridges)] }
}

// the vehicles placed, kept while the vehicles are the same (animation frames of a glide)
let memo: { vehicles: Vehicle[]; under: Vehicle[]; over: Vehicle[] } | null = null

/** Stems, from sea level to the vehicle (the stem's foot marks the point under it): they follow a
 *  vehicle's drawn position (display smoothing, drawn.ts), so MapView rebuilds just these on an
 *  animation frame. The stems of what is under
 *  water are drawn seen through it, the other stems in their own colour: a group member's in its
 *  group's (`stemColor`, bd murmur-4nik; `stemKey` changes when those colours do), fainter. */
export function build3dGlide(vehicles: Vehicle[], stemColor?: (v: Vehicle) => RGBA | null, stemKey?: unknown) {
  if (!memo || memo.vehicles !== vehicles) {
    const placed = vehicles.filter((v) => v.position)
    memo = { vehicles, under: placed.filter(underWater), over: placed.filter((v) => !underWater(v)) }
  }
  const { under, over } = memo
  const at = (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
  const ground = (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, 0) as XYZ
  const triggers = { getSourcePosition: drawnTick(), getTargetPosition: drawnTick() }
  const stemOf = (v: Vehicle) => stemColor?.(v) ?? colour(v, 160)
  // stem from sea level to the vehicle: shows height (or depth) at a glance
  const stems = (id: string, data: Vehicle[], seen = false) =>
    new LineLayer<Vehicle>({
      id,
      data,
      getSourcePosition: ground,
      getTargetPosition: at,
      getColor: seen ? (v) => seenThroughWater(stemOf(v)) : stemOf,
      getWidth: 1.5,
      widthUnits: 'pixels',
      updateTriggers: { ...triggers, getColor: stemKey },
    })
  return {
    stemsUnder: stems('stems-under', under, true),
    stems: stems('stems', over),
  }
}
