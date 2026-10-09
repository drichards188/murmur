// Checks for sea.ts: there is no test runner in web/, so run them with Node 22.18 or later, which
// strips the types itself (make check does this):
//   node web/src/sea.check.ts
// Throws on the first failure, prints "sea: ok" otherwise. Not part of the bundle.
import { assembleSea, clipRing, outermost, polygonOffset, polygonsOf, tileBox, tileKey, type Polygon, type Pt, type TileWater } from './sea.ts'

// node's own fs, without @types/node in this (browser) project
declare const process: { getBuiltinModule(id: string): unknown }
const { readFileSync } = process.getBuiltinModule('node:fs') as { readFileSync(path: URL, encoding: 'utf8'): string }

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`sea check failed: ${what}`)
}
const near = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps * Math.max(1, Math.abs(a), Math.abs(b))

// Web Mercator, as sea.ts cuts in it
const mx = (lon: number) => (lon + 180) / 360
const my = (lat: number) => {
  const s = Math.sin((lat * Math.PI) / 180)
  return 0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)
}
const lonAt = (x: number) => x * 360 - 180
const latAt = (y: number) => (360 / Math.PI) * Math.atan(Math.exp((1 - 2 * y) * Math.PI)) - 90
/** shoelace area of an open ring, in Web Mercator units (absolute) */
const area = (ring: Pt[]) => {
  let a = 0
  for (let i = 0; i < ring.length; i++) {
    const [x0, y0] = [mx(ring[i][0]), my(ring[i][1])]
    const [x1, y1] = [mx(ring[(i + 1) % ring.length][0]), my(ring[(i + 1) % ring.length][1])]
    a += x0 * y1 - x1 * y0
  }
  return Math.abs(a) / 2
}
/** a closed GeoJSON rectangle ring, given in Web Mercator */
const rect = (x0: number, y0: number, x1: number, y1: number): Pt[] => [
  [lonAt(x0), latAt(y0)],
  [lonAt(x1), latAt(y0)],
  [lonAt(x1), latAt(y1)],
  [lonAt(x0), latAt(y1)],
  [lonAt(x0), latAt(y0)],
]

// a tile around the Golden Gate, z10 (x 163, y 395), and its neighbours and children
const T = { z: 10, x: 163, y: 395 }
const B = tileBox(T)
const W = B.x1 - B.x0
check(near(B.x0, 163 / 1024) && near(B.y1, 396 / 1024), 'tile box in Web Mercator')
check(tileKey(T) === '10/163/395', 'tile key')

// outermost: a child under its parent goes, a grandchild too; neighbours at other zooms stay
const parent = { z: 9, x: 81, y: 197 }
const kept = outermost([T, parent, { z: 11, x: 327, y: 790 }, { z: 9, x: 82, y: 197 }, { z: 12, x: 4 * 164, y: 4 * 395 }, T])
check(kept.length === 2 && kept[0] === parent && tileKey(kept[1]) === '9/82/197', `children of a kept tile dropped, neighbours kept (${kept.map(tileKey)})`)
check(outermost([{ z: 11, x: 326, y: 790 }, { z: 10, x: 164, y: 395 }, { z: 12, x: 652, y: 1580 }]).length === 2, 'a z12 under the z11 goes; the z10 next to them stays')
check(outermost([T, T]).length === 1, 'duplicates collapse')

// a ring inside the tile comes back open, untouched
const inner = rect(B.x0 + 0.2 * W, B.y0 + 0.2 * W, B.x0 + 0.4 * W, B.y0 + 0.4 * W)
const ci = clipRing(inner, B)
check(ci.length === 4 && ci[0] === inner[0], 'an inner ring comes back as it was, without the closing point')
check(clipRing(rect(B.x1 + W, B.y0, B.x1 + 2 * W, B.y1), B).length === 0, 'a ring outside the tile: nothing')

// a ring over the tile's east edge (the buffer of a vector tile): cut exactly at the edge
const over = rect(B.x0 + 0.5 * W, B.y0 + 0.25 * W, B.x1 + 0.1 * W, B.y0 + 0.75 * W)
const co = clipRing(over, B)
const east = Math.max(...co.map((p) => p[0]))
check(near(east, lonAt(B.x1), 1e-12), 'cut at the east edge')
check(near(area(co), 0.5 * W * 0.5 * W, 1e-6), `cut area is the part inside (${area(co)} vs ${0.25 * W * W})`)

// the same water seen by two neighbouring tiles: the two cuts meet on the shared edge with no
// gap and no overlap (the translucent fill must not blend twice there)
const R = { z: 10, x: 164, y: 395 }
const across = rect(B.x0 + 0.6 * W, B.y0 + 0.3 * W, B.x1 + 0.4 * W, B.y0 + 0.7 * W)
const left = clipRing(across, B)
const right = clipRing(across, tileBox(R))
const lonEdge = lonAt(tileBox(R).x0)
check(left.some((p) => p[0] === lonEdge) && right.some((p) => p[0] === lonEdge), 'both cuts put their edge points exactly on the shared meridian')
check(Math.max(...left.map((p) => p[0])) <= lonEdge && Math.min(...right.map((p) => p[0])) >= lonEdge, 'no overlap across the edge')
check(near(area(left) + area(right), area(across.slice(0, 4)), 1e-6), 'the two cuts add up to the whole: no gap')

// a concave ring (a U) that leaves the tile and comes back: area kept, seam adds none
const u: Pt[] = [
  [lonAt(B.x0 + 0.2 * W), latAt(B.y0 + 0.2 * W)],
  [lonAt(B.x1 + 0.5 * W), latAt(B.y0 + 0.2 * W)],
  [lonAt(B.x1 + 0.5 * W), latAt(B.y0 + 0.4 * W)],
  [lonAt(B.x0 + 0.6 * W), latAt(B.y0 + 0.4 * W)],
  [lonAt(B.x0 + 0.6 * W), latAt(B.y0 + 0.6 * W)],
  [lonAt(B.x1 + 0.5 * W), latAt(B.y0 + 0.6 * W)],
  [lonAt(B.x1 + 0.5 * W), latAt(B.y0 + 0.8 * W)],
  [lonAt(B.x0 + 0.2 * W), latAt(B.y0 + 0.8 * W)],
]
const cu = clipRing(u, B)
// inside the tile: the U's spine 0.2..0.6 x 0.2..0.8, plus both arms 0.6..1 x 0.2 high
check(near(area(cu), (0.4 * 0.6 + 2 * 0.4 * 0.2) * W * W, 1e-6), `a concave ring keeps its area (${area(cu) / (W * W)})`)

// a parent's water cut four ways by its children adds up to the parent's cut
const big = rect(B.x0 - 0.3 * W, B.y0 + 0.1 * W, B.x1 + 0.3 * W, B.y1 - 0.2 * W)
const kids = [0, 1].flatMap((dx) => [0, 1].map((dy) => ({ z: 11, x: 2 * T.x + dx, y: 2 * T.y + dy })))
const sum = kids.reduce((a, k) => a + area(clipRing(big, tileBox(k))), 0)
check(near(sum, area(clipRing(big, B)), 1e-6), 'children cut the same water as their parent')

// assembleSea: holes (islands) kept inside, dropped when outside; an outer ring outside drops the
// polygon with its holes; a child under a kept parent adds nothing
const island = rect(B.x0 + 0.3 * W, B.y0 + 0.3 * W, B.x0 + 0.35 * W, B.y0 + 0.35 * W)
const farHole = rect(B.x1 + 0.1 * W, B.y0 + 0.3 * W, B.x1 + 0.2 * W, B.y0 + 0.35 * W)
const bay: Polygon = [rect(B.x0 - 0.1 * W, B.y0 - 0.1 * W, B.x1 + 0.3 * W, B.y1 + 0.1 * W), island, farHole]
const offTile: Polygon = [rect(B.x1 + 0.05 * W, B.y0, B.x1 + 0.1 * W, B.y1), farHole]
const tiles: TileWater[] = [
  { ...T, polygons: [bay, offTile] },
  { z: 11, x: 2 * T.x, y: 2 * T.y, polygons: [[rect(B.x0, B.y0, B.x0 + 0.5 * W, B.y0 + 0.5 * W)]] },
]
const sea = assembleSea(tiles)
check(sea.length === 1, `one polygon: the child under its parent and the off-tile water add none (${sea.length})`)
check(sea[0].polygon.length === 2, 'the bay keeps its island, not the hole beyond the tile')
check(near(area(sea[0].polygon[0]), W * W, 1e-6), 'the bay fills its tile exactly')
check(assembleSea([]).length === 0, 'no water tiles: no sea')

// GeoJSON geometries
check(polygonsOf({ type: 'Polygon', coordinates: bay }).length === 1, 'a Polygon')
check(polygonsOf({ type: 'MultiPolygon', coordinates: [bay, offTile] }).length === 2, 'a MultiPolygon')
check(polygonsOf({ type: 'LineString', coordinates: [] }).length === 0, 'not a polygon')

// the depth offset: what polygonOffset relies on in luma.gl's WebGL adapter
const po = polygonOffset(0, -4)
check(po.depthBias === 0 && po.depthBiasSlopeScale === -4, 'factor goes in depthBias, units in depthBiasSlopeScale')
const luma = readFileSync(new URL('../node_modules/@luma.gl/webgl/dist/adapter/converters/device-parameters.js', import.meta.url), 'utf8')
check(
  luma.includes('gl.polygonOffset(parameters.depthBias, parameters.depthBiasSlopeScale'),
  'luma.gl no longer calls gl.polygonOffset(depthBias, depthBiasSlopeScale): check which is the factor and which the units, then fix polygonOffset in sea.ts',
)

console.log('sea: ok')
