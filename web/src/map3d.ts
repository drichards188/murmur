import { IconLayer, LineLayer, PathLayer, ScatterplotLayer, SolidPolygonLayer } from '@deck.gl/layers'
import { symbolSvg } from './symbols'
import type { Vehicle } from './types'
import { AIR_KINDS, STALE_MS } from './types'

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

function colour(v: Vehicle, alpha = 255): RGBA {
  const c = v.kind && AIR_KINDS.includes(v.kind) ? AIR : SEA
  return [c[0], c[1], c[2], alpha]
}

function z(metresMsl: number | null) {
  return (metresMsl ?? 0) * HEIGHT_EXAGGERATION
}

function iconFor(v: Vehicle) {
  const stale = (v.link.since_last_ms ?? 0) > STALE_MS
  // no heading arrow in 3D: the 3D trail shows direction, and a stable icon per vehicle
  // keeps deck.gl from rebuilding its icon texture on every turn
  const sym = symbolSvg(v, { stale, heading: false })
  return {
    url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sym.svg)}`,
    id: `${v.sysid}|${stale}`,
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

/** visible=false keeps the layers (and their GPU resources) alive while the 2D view is shown. */
export function build3dLayers(vehicles: Vehicle[], selected: Set<number>, visible = true) {
  const placed = vehicles.filter((v) => v.position && v.alt_msl != null)
  const at = (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  return [
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
      data: placed.filter((v) => selected.has(v.sysid)),
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
      getIcon: iconFor,
      getSize: (v) => iconFor(v).height,
      sizeUnits: 'pixels',
      updateTriggers: { getIcon: placed.map((v) => `${v.sysid}|${(v.link.since_last_ms ?? 0) > STALE_MS}`).join() },
    }),
  ]
}
