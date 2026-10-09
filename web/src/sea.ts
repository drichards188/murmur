// The 3D sea surface's outline, from the basemap's own water polygons (murmur-5xl follow-up).
// MapLibre hands the water of each loaded vector tile clipped to the tile plus a buffer, so
// neighbouring tiles overlap by the buffer, and while children load it keeps their parent as a
// placeholder (the same water twice, at two zooms). Drawn as they come, a translucent fill
// would blend twice on every overlap: darker seams along tile edges, darker squares where a
// parent and a child are both kept. So each tile's water is cut to the tile's own square, and a
// tile under another tile in the set is left out (the coarser one covers its area).
// Pure functions (no MapLibre at run time): checked by sea.check.ts.

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

/** one sea polygon for deck.gl's SolidPolygonLayer (outer ring, then holes; open rings) */
export interface SeaPolygon {
  polygon: Pt[][]
}

export const tileKey = (t: TileId) => `${t.z}/${t.x}/${t.y}`

/** The tiles to draw: duplicates collapse, and a tile with an ancestor in the set (a child
 *  loaded while MapLibre still keeps its parent) is left out, so no area is drawn twice and the
 *  ancestor keeps all of it covered. Tiles at different zooms side by side (a tilted view:
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

// Web Mercator in [0, 1] (x east, y south), as MapLibre's tiles are laid out. Tile edges are
// straight lines there, and a feature's edges are straight lines there too, so cutting in
// these coordinates is exact.
const mercX = (lon: number) => (lon + 180) / 360
const mercY = (lat: number) => {
  const s = Math.sin((lat * Math.PI) / 180)
  return 0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)
}
const lonOf = (x: number) => x * 360 - 180
const latOf = (y: number) => (360 / Math.PI) * Math.atan(Math.exp((1 - 2 * y) * Math.PI)) - 90

interface Box {
  x0: number
  y0: number
  x1: number
  y1: number
}

/** a tile's square in Web Mercator [0, 1] */
export function tileBox(t: TileId): Box {
  const n = 2 ** t.z
  return { x0: t.x / n, y0: t.y / n, x1: (t.x + 1) / n, y1: (t.y + 1) / n }
}

type XY = [number, number]

/** One Sutherland-Hodgman pass against the half plane `axis >= at` (keep 1) or `<= at` (-1).
 *  The cut points lie exactly on the line, so two tiles sharing an edge agree on it. */
function cutHalf(pts: XY[], axis: 0 | 1, at: number, keep: 1 | -1): XY[] {
  const out: XY[] = []
  const n = pts.length
  if (!n) return out
  const inside = (p: XY) => (keep === 1 ? p[axis] >= at : p[axis] <= at)
  let s = pts[n - 1]
  let sIn = inside(s)
  for (const e of pts) {
    const eIn = inside(e)
    if (eIn !== sIn) {
      const t = (at - s[axis]) / (e[axis] - s[axis])
      const o = 1 - axis
      const p: XY = [0, 0]
      p[axis] = at
      p[o] = s[o] + t * (e[o] - s[o])
      out.push(p)
    }
    if (eIn) out.push(e)
    s = e
    sIn = eIn
  }
  return out
}

/** A ring cut to a box (Web Mercator), back in longitude and latitude; open (no repeated first
 *  point). A concave ring that leaves and re-enters the box keeps a zero-width seam along the
 *  edge: it adds no area, so a fill draws the same. Fewer than 3 points: nothing left. */
export function clipRing(ring: Ring, box: Box): Pt[] {
  let pts: XY[] = ring.map(([lon, lat]) => [mercX(lon), mercY(lat)])
  if (pts.length > 1 && pts[0][0] === pts[pts.length - 1][0] && pts[0][1] === pts[pts.length - 1][1]) pts.pop()
  // entirely inside (most rings): nothing to cut
  if (pts.every(([x, y]) => x >= box.x0 && x <= box.x1 && y >= box.y0 && y <= box.y1)) return ring.slice(0, pts.length)
  pts = cutHalf(pts, 0, box.x0, 1)
  pts = cutHalf(pts, 0, box.x1, -1)
  pts = cutHalf(pts, 1, box.y0, 1)
  pts = cutHalf(pts, 1, box.y1, -1)
  if (pts.length < 3) return []
  return pts.map(([x, y]) => [lonOf(x), latOf(y)])
}

/** The sea surface: every kept tile's water, cut to its square. A polygon whose outer ring
 *  falls outside its tile is dropped with its holes; a hole that does is dropped alone. (Water
 *  areas of one tile can still overlap each other, as OSM maps them: the layer draws each pixel
 *  once with a stencil test, map3d.ts.) */
export function assembleSea(tiles: TileWater[]): SeaPolygon[] {
  const out: SeaPolygon[] = []
  for (const t of outermost(tiles)) {
    const box = tileBox(t)
    for (const poly of t.polygons) {
      const rings: Pt[][] = []
      for (let i = 0; i < poly.length; i++) {
        const r = clipRing(poly[i], box)
        if (r.length >= 3) rings.push(r)
        else if (i === 0) break
      }
      if (rings.length) out.push({ polygon: rings })
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
