import { LineLayer, PathLayer, ScatterplotLayer, SolidPolygonLayer } from '@deck.gl/layers'
import type { LiftedBridge } from './bridges'
import { drawnTick, drawnXYZ, smoothing } from './drawn'
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

/** Where a vehicle is drawn in 3D, in rendered metres (the camera aims here to follow it). */
export function drawnHeight(v: Vehicle) {
  return z(v.alt_msl)
}

/** A translucent sea-level plane around the swarm. Land above 0 m shows through it;
 *  the seabed and anything submerged are seen through the water. */
function seaSurface(vehicles: Vehicle[]) {
  const pts = vehicles.flatMap((v) => (v.position ? [v.position] : []))
  if (!pts.length) return []
  const pad = 0.25 // degrees, ~25 km
  const w = Math.min(...pts.map((p) => p[0])) - pad
  const e = Math.max(...pts.map((p) => p[0])) + pad
  const s = Math.min(...pts.map((p) => p[1])) - pad
  const n = Math.max(...pts.map((p) => p[1])) + pad
  const polygon: XYZ[] = [
    [w, s, 0],
    [e, s, 0],
    [e, n, 0],
    [w, n, 0],
  ]
  return [{ polygon }]
}

/** The 3D scene around the vehicles: bridge decks, sea surface, trails, height stems and
 *  sea-level shadows. The vehicles themselves (both views) are in vehicleLayers.ts.
 *  visible=false keeps the layers (and their GPU resources) alive while the 2D view is shown. */
export function build3dLayers(vehicles: Vehicle[], visible = true, bridges: LiftedBridge[] = []) {
  return [...build3dStatic(vehicles, visible, bridges), ...build3dGlide(vehicles, visible)]
}

/** Bridge decks, sea surface and trails: nothing here moves with a glide (display smoothing,
 *  drawn.ts), so MapView builds them when the data changes and reuses them between frames. */
export function build3dStatic(vehicles: Vehicle[], visible = true, bridges: LiftedBridge[] = []) {
  // nothing to compute while 2D is shown: the layers stay (hidden), their data is empty
  const placed = visible ? vehicles.filter((v) => v.position && v.alt_msl != null) : []
  const at = (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  return [
    // bridge decks at estimated height (see bridges.ts); drawn before the sea surface
    new PathLayer<LiftedBridge>({
      id: 'bridge-decks',
      visible,
      data: bridges,
      getPath: (b) => b.path,
      getColor: (b) => (b.major ? [58, 64, 74, 255] : [44, 49, 57, 255]),
      getWidth: (b) => (b.major ? 14 : 8),
      widthUnits: 'meters',
      widthMinPixels: 2,
      capRounded: true,
      jointRounded: true,
    }),
    new SolidPolygonLayer({
      id: 'sea-surface',
      visible,
      data: seaSurface(placed),
      getPolygon: (d: { polygon: XYZ[] }) => d.polygon,
      getFillColor: [40, 110, 160, 70],
      parameters: { depthWriteEnabled: false },
    }),
    new PathLayer<Vehicle>({
      id: 'trails-3d',
      visible,
      // with smoothing on, trailLayers.ts draws the trails (they end at the drawn position)
      data: smoothing() ? [] : placed.filter((v) => v.trail.length > 1),
      getPath: (v) => [...v.trail.map(([lon, lat, alt]): XYZ => [lon, lat, z(alt)]), at(v)],
      getColor: (v) => colour(v, 140),
      getWidth: 2,
      widthUnits: 'pixels',
      updateTriggers: { getPath: placed.map((v) => v.trail.length).join() },
    }),
  ]
}

// the vehicles placed in 3D, kept while the vehicles are the same (animation frames of a glide)
let memo: { vehicles: Vehicle[]; visible: boolean; placed: Vehicle[] } | null = null

/** Stems and sea-level shadows: they follow a vehicle's drawn position (display smoothing,
 *  drawn.ts), so MapView rebuilds just these on an animation frame. */
export function build3dGlide(vehicles: Vehicle[], visible = true) {
  if (!memo || memo.vehicles !== vehicles || memo.visible !== visible) {
    memo = { vehicles, visible, placed: visible ? vehicles.filter((v) => v.position && v.alt_msl != null) : [] }
  }
  const { placed } = memo
  const smooth = smoothing()
  const at = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  const ground = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, 0) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], 0]
  const triggers = smooth ? { getSourcePosition: drawnTick(), getTargetPosition: drawnTick(), getPosition: drawnTick() } : {}
  return [
    // stem from sea level to the vehicle: shows height (or depth) at a glance
    new LineLayer<Vehicle>({
      id: 'stems',
      visible,
      data: placed,
      getSourcePosition: ground,
      getTargetPosition: at,
      getColor: (v) => colour(v, 160),
      getWidth: 1.5,
      widthUnits: 'pixels',
      updateTriggers: triggers,
    }),
    new ScatterplotLayer<Vehicle>({
      id: 'sea-level-shadow',
      visible,
      data: placed,
      getPosition: ground,
      getRadius: 4,
      radiusUnits: 'pixels',
      getFillColor: [0, 0, 0, 110],
      updateTriggers: triggers,
    }),
  ]
}
