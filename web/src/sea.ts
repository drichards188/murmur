// The 3D sea surface's outline, from the basemap's own water polygons (murmur-5xl follow-up).
// Only the sea is a surface at 0 m: OpenMapTiles' `ocean` class, the water inside the coastline
// (the Pacific, the bay, Richardson Bay, the tidal sloughs the coastline takes in). Every other
// water polygon (lakes, reservoirs, ponds, salt ponds, rivers, docks) lies at its own height, a
// metre or more above 0 m for most of them, where a plane at 0 m loses to the terrain over it
// seen from above and wins once tilted, so its tint depended on the view. That water is tinted
// by the style's own water fill instead, draped on the terrain in 3D (drapedWater, MapView).
// MapLibre hands the water of each loaded vector tile clipped to the tile plus a buffer, so
// neighbouring tiles overlap by the buffer, and while children load it keeps their parent as a
// placeholder (the same water twice, at two zooms). The sea layer tints each pixel once whatever
// covers it (a stencil pass, map3d.ts), so the polygons go to it as MapLibre gives them, buffers
// and all: cutting each tile's water to its own square was tried, cost the same and left a few
// untinted pixels along tile borders where the cuts of two tiles met at different points. A tile
// under another tile in the set is still left out, which only saves drawing the same water twice.
// Also which vehicles' trails and stems draw under the surface. Pure functions (no MapLibre or
// deck.gl at run time): checked by sea.check.ts.

import { domainOf, type Vehicle } from './types.ts'

type RGBA = [number, number, number, number]

/** The sea surface's colour (map3d.ts), and the tint the draped water is given to match it. */
export const SEA_TINT: RGBA = [40, 110, 160, 70]

/** The water class drawn as the sea surface at 0 m. Measured against the console's own
 *  elevation tiles (Terrarium, the 3D terrain) under the basemap's water around the whole bay
 *  and the Golden Gate (2026-10-09): `ocean` is 93 % below 0 m (98 % around the Golden Gate,
 *  median -19 m); lakes and reservoirs 2 % (median +1.8 m, reservoirs to +150 m); ponds and salt
 *  ponds 6 % (median +1.1 m); rivers 42 % (the North Bay's tidal rivers and sloughs reach -6 m,
 *  the South Bay's creeks sit near +1 m), so a river's polygon does not say which; docks are a
 *  dozen basins, most inside the ocean polygon. Only `ocean` is water at sea level by class.
 *  Not all of it, though: in the far South Bay its tidal flats, marsh and sloughs read up to
 *  +1.3 m (in the Mountain View tiles 53 % of the ocean is above 0 m, 24 % above +0.5 m), and
 *  there the surface still loses to the terrain seen from above and wins once tilted. */
export const SEA_CLASS = 'ocean'

/** Water the draped fill leaves at the style's own colour in 3D: the sea (the surface tints it,
 *  and a second tint would double it) and swimming pools (many, in cities, never tinted before). */
export const UNTINTED_CLASSES = [SEA_CLASS, 'swimming_pool']

/** longitude, latitude */
export type Pt = [number, number]
/** a GeoJSON ring (closed or not) */
export type Ring = Pt[]
/** outer ring, then holes (islands) */
export type Polygon = Ring[]

export interface TileId {
  z: number
  x: number
  y: number
}

export interface TileWater extends TileId {
  polygons: Polygon[]
}

/** one sea polygon for deck.gl's SolidPolygonLayer (outer ring, then holes) */
export interface SeaPolygon {
  polygon: Pt[][]
}

export const tileKey = (t: TileId) => `${t.z}/${t.x}/${t.y}`

/** A feature as MapLibre's querySourceFeatures returns it: the tile it came from (`_z`, `_x`,
 *  `_y`, declared in maplibre-gl's types) and its decoded vector tile feature (`type`: 3 for a
 *  polygon) and properties. Its geometry is converted only when read. */
export interface TileFeature {
  _z: number
  _x: number
  _y: number
  _vectorTileFeature: { type: number }
  properties: Record<string, unknown>
}

/** The sea's water among the query results, grouped by the tile it came from: only what
 *  isSeaWater keeps counts, so a tile of nothing but lakes or pools neither enters the rebuild
 *  key nor, as a parent, hides a child's sea from outermost. Features without tile coordinates
 *  (a MapLibre that stopped setting them) are skipped and counted in `missing`. */
export function byTile<F extends TileFeature>(features: F[]) {
  const groups = new Map<string, TileId & { feats: F[] }>()
  let missing = 0
  for (const f of features) {
    if (!isSeaWater(f)) continue
    if (!Number.isInteger(f._z) || !Number.isInteger(f._x) || !Number.isInteger(f._y)) {
      missing++
      continue
    }
    const id = { z: f._z, x: f._x, y: f._y }
    const k = tileKey(id)
    const g = groups.get(k)
    if (g) g.feats.push(f)
    else groups.set(k, { ...id, feats: [f] })
  }
  return { groups, missing }
}

/** What the sea was built from: each tile and how many of the sea's water features it holds, in
 *  a fixed order. Equal keys, same sea: no rebuild. */
export function tilesKey(groups: Map<string, { feats: unknown[] }>) {
  return [...groups.entries()]
    .map(([k, g]) => `${k}:${g.feats.length}`)
    .sort()
    .join(' ')
}

export const seaKey = (features: TileFeature[]) => tilesKey(byTile(features).groups)

/** The water the sea surface is drawn over: the sea's polygons (SEA_CLASS), not in a tunnel. */
export function isSeaWater(f: Pick<TileFeature, '_vectorTileFeature' | 'properties'>) {
  return f._vectorTileFeature?.type === 3 && f.properties?.brunnel !== 'tunnel' && f.properties?.class === SEA_CLASS
}

/** The style's water fill in 3D, for a layer whose colour is `base` (2D keeps the style's own).
 *  `color`: the sea and pools keep `base`; every other water polygon gets SEA_TINT composited over
 *  `base`, which is what the sea surface makes of the same fill where it draws (the surface tests
 *  depth but never writes it, so it blends over the draped water as a translucent fill does), and
 *  the two meet without a seam. Draped, the tint follows the terrain: it shows at any height and
 *  view angle, needs no depth offset and costs nothing per frame. `sortKey` draws the sea's
 *  polygons last within the layer, over any other water polygon inside the coastline (Mission
 *  Creek, Islais Creek Channel, Richmond's Santa Fe and Harbor Channels, the Pier 70 docks and a
 *  salt pond by Alviso are both): there the draped tint is covered, and the surface alone tints
 *  it, once, as it tints the sea around it. The composite is left to MapLibre (`to-rgba`), so it
 *  holds for whatever colour the style gives its water; a `base` that is not a plain colour string
 *  (an expression or a zoom function, none in the style now) cannot be nested in a match, and
 *  gives null: the caller leaves the layer as it is. */
export function drapedWater(base: unknown, tint: RGBA = SEA_TINT) {
  if (typeof base !== 'string') return null
  const a = tint[3] / 255
  const over = [
    'let',
    'water',
    ['to-rgba', ['to-color', base]],
    ['rgba', ...[0, 1, 2].map((k) => ['+', ['*', ['at', k, ['var', 'water']], 1 - a], tint[k] * a]), ['at', 3, ['var', 'water']]],
  ]
  return {
    color: ['match', ['get', 'class'], UNTINTED_CLASSES, base, over] as unknown[],
    sortKey: ['match', ['get', 'class'], SEA_CLASS, 1, 0] as unknown[],
  }
}

// --- what draws under the sea surface (map3d.ts, trailLayers.ts) -------------------------------

/** Seen through the water: a submarine's trail and stem (the subsurface domain), and anything
 *  more than a metre under the surface. A surface vessel's GPS altitude wanders around 0, and
 *  the margin keeps its trail from flipping between tinted and not. A surfaced submarine counts
 *  as under water on purpose: its trail (mostly a dive) does not move between layers each time
 *  it comes up. */
export function underWater(v: Pick<Vehicle, 'kind' | 'identity' | 'alt_msl'>) {
  return domainOf(v) === 'subsurface' || (v.alt_msl != null && v.alt_msl < -1)
}

/** The uids of the vehicles under water, and a signature that changes only when that set does. */
export function belowOf(vehicles: Pick<Vehicle, 'uid' | 'kind' | 'identity' | 'alt_msl'>[]) {
  const below = new Set(vehicles.filter(underWater).map((v) => v.uid))
  return { below, sig: [...below].join() }
}

export interface UnderSplit<T> {
  list: T[]
  sig: string
  under: T[]
  over: T[]
}

/** A list split into what is under water (`below`) and the rest. The previous split comes back
 *  as it was while the list and the signature are the same, so deck.gl gets the same data arrays
 *  and does not redo the layers; with nothing under water `over` is the list itself. */
export function splitUnder<T extends { uid: number }>(prev: UnderSplit<T> | null, list: T[], below: Set<number>, sig: string): UnderSplit<T> {
  if (prev && prev.list === list && prev.sig === sig) return prev
  if (!below.size) return { list, sig, under: [], over: list }
  return { list, sig, under: list.filter((v) => below.has(v.uid)), over: list.filter((v) => !below.has(v.uid)) }
}

/** The tiles to draw: duplicates collapse, and a tile with an ancestor in the set (a child
 *  loaded while MapLibre still keeps its parent) is left out: the ancestor covers its area, and
 *  drawing both would only cost vertices. Tiles at different zooms side by side (a tilted view:
 *  finer near the camera) are all kept. Order is kept. */
export function outermost<T extends TileId>(tiles: T[]): T[] {
  const keys = new Set(tiles.map(tileKey))
  const seen = new Set<string>()
  return tiles.filter((t) => {
    const k = tileKey(t)
    if (seen.has(k)) return false
    seen.add(k)
    for (let z = t.z - 1; z >= 0; z--) {
      const s = t.z - z
      if (keys.has(`${z}/${Math.floor(t.x / 2 ** s)}/${Math.floor(t.y / 2 ** s)}`)) return false
    }
    return true
  })
}

/** The sea surface: the water of every kept tile (outermost), polygons as they come, outer ring
 *  then holes. Rings with fewer than 3 points are dropped, and a polygon goes with its outer
 *  ring. */
export function assembleSea(tiles: TileWater[]): SeaPolygon[] {
  const out: SeaPolygon[] = []
  for (const t of outermost(tiles)) {
    for (const poly of t.polygons) {
      if (!poly.length || poly[0].length < 3) continue
      out.push({ polygon: poly.filter((r) => r.length >= 3) })
    }
  }
  return out
}

/** Layer parameters for gl.polygonOffset(factor, units). luma.gl 9.4's WebGL adapter calls
 *  gl.polygonOffset(depthBias, depthBiasSlopeScale), so there depthBias is the factor (scaled by
 *  the depth slope) and depthBiasSlopeScale the constant units: the reverse of their WebGPU
 *  meaning. sea.check.ts pins that line of luma.gl, so a version that swaps them fails make check
 *  instead of quietly swapping the sea's constant and slope-scaled offsets. */
export function polygonOffset(factor: number, units: number) {
  return { depthBias: factor, depthBiasSlopeScale: units }
}

/** the polygons of a GeoJSON geometry (other types: none) */
export function polygonsOf(g: { type: string; coordinates?: unknown }): Polygon[] {
  if (g.type === 'Polygon') return [g.coordinates as Polygon]
  if (g.type === 'MultiPolygon') return g.coordinates as Polygon[]
  return []
}
