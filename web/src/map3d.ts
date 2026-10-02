import { IconLayer, LineLayer, PathLayer, ScatterplotLayer, SolidPolygonLayer } from '@deck.gl/layers'
import type { LiftedBridge } from './bridges'
import { symbolSvg } from './symbols'
import type { Vehicle } from './types'
import { domainOf, isStale } from './types'

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

function colour(v: Vehicle, alpha = 255): RGBA {
  const c = domainOf(v) === 'air' ? AIR : domainOf(v) === 'ground' ? GROUND : SEA
  return [c[0], c[1], c[2], alpha]
}

function z(metresMsl: number | null) {
  return (metresMsl ?? 0) * HEIGHT_EXAGGERATION
}

/** Where a vehicle is drawn in 3D, in rendered metres (the camera aims here to follow it). */
export function drawnHeight(v: Vehicle) {
  return z(v.alt_msl)
}

function iconFor(v: Vehicle, labels = true) {
  const stale = isStale(v)
  // no heading arrow in 3D: the 3D trail shows direction, and a stable icon per vehicle
  // keeps deck.gl from rebuilding its icon texture on every turn
  const sym = symbolSvg(v, { stale, heading: false, labels })
  return {
    url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sym.svg)}`,
    id: `${v.uid}|${stale}|${labels}`,
    width: Math.ceil(sym.size.width),
    height: Math.ceil(sym.size.height),
    anchorX: sym.anchor.x,
    anchorY: sym.anchor.y,
  }
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

/** visible=false keeps the layers (and their GPU resources) alive while the 2D view is shown.
 *  `bare`: vehicles drawn without labels (crowded; see declutter.ts). */
export function build3dLayers(
  vehicles: Vehicle[],
  selected: Set<number>,
  visible = true,
  bridges: LiftedBridge[] = [],
  bare: Set<number> = new Set(),
) {
  const placed = vehicles.filter((v) => v.position && v.alt_msl != null)
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
      data: placed.filter((v) => v.trail.length > 1),
      getPath: (v) => [...v.trail.map(([lon, lat, alt]): XYZ => [lon, lat, z(alt)]), at(v)],
      getColor: (v) => colour(v, 140),
      getWidth: 2,
      widthUnits: 'pixels',
      updateTriggers: { getPath: placed.map((v) => v.trail.length).join() },
    }),
    // stem from sea level to the vehicle: shows height (or depth) at a glance
    new LineLayer<Vehicle>({
      id: 'stems',
      visible,
      data: placed,
      getSourcePosition: (v) => [v.position![0], v.position![1], 0],
      getTargetPosition: at,
      getColor: (v) => colour(v, 160),
      getWidth: 1.5,
      widthUnits: 'pixels',
    }),
    new ScatterplotLayer<Vehicle>({
      id: 'sea-level-shadow',
      visible,
      data: placed,
      getPosition: (v) => [v.position![0], v.position![1], 0],
      getRadius: 4,
      radiusUnits: 'pixels',
      getFillColor: [0, 0, 0, 110],
    }),
    new ScatterplotLayer<Vehicle>({
      id: 'selection',
      visible,
      data: placed.filter((v) => selected.has(v.uid)),
      getPosition: at,
      getRadius: 24,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: false,
      getLineColor: [255, 179, 71, 255],
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
    }),
    new IconLayer<Vehicle>({
      id: 'vehicles-3d',
      visible,
      data: placed,
      pickable: true,
      getPosition: at,
      getIcon: (v) => iconFor(v, !bare.has(v.uid)),
      getSize: (v) => iconFor(v, !bare.has(v.uid)).height,
      sizeUnits: 'pixels',
      updateTriggers: {
        getIcon: placed.map((v) => `${v.uid}|${isStale(v)}|${bare.has(v.uid)}`).join(),
        getSize: placed.map((v) => bare.has(v.uid)).join(),
      },
    }),
  ]
}
