// Checks for sea.ts: there is no test runner in web/, so run them with Node 22.18 or later, which
// strips the types itself (make check does this):
//   node web/src/sea.check.ts
// Throws on the first failure, prints "sea: ok" otherwise. Not part of the bundle.
import { createPropertyExpression, latest, type StylePropertySpecification } from '@maplibre/maplibre-gl-style-spec'
import { assembleSea, belowOf, byTile, drapedWater, isSeaWater, outermost, polygonOffset, polygonsOf, SEA_CLASS, SEA_TINT, seaKey, splitUnder, tileKey, tilesKey, underWater, type Polygon, type Pt, type TileFeature, type TileWater } from './sea.ts'
import type { Identity, Kind, Vehicle } from './types.ts'

// node's own fs, without @types/node in this (browser) project
declare const process: { getBuiltinModule(id: string): unknown }
const { readFileSync } = process.getBuiltinModule('node:fs') as { readFileSync(path: URL, encoding: 'utf8'): string }

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`sea check failed: ${what}`)
}

// --- which tiles: outermost ---------------------------------------------------------------
const T = { z: 10, x: 163, y: 395 }
check(tileKey(T) === '10/163/395', 'tile key')
// a child under its parent goes, a grandchild too; neighbours at other zooms stay
const parent = { z: 9, x: 81, y: 197 }
const kept = outermost([T, parent, { z: 11, x: 327, y: 790 }, { z: 9, x: 82, y: 197 }, { z: 12, x: 4 * 164, y: 4 * 395 }, T])
check(kept.length === 2 && kept[0] === parent && tileKey(kept[1]) === '9/82/197', `children of a kept tile dropped, neighbours kept (${kept.map(tileKey)})`)
check(outermost([{ z: 11, x: 326, y: 790 }, { z: 10, x: 164, y: 395 }, { z: 12, x: 652, y: 1580 }]).length === 2, 'a z12 under the z11 goes; the z10 next to them stays')
check(outermost([T, T]).length === 1, 'duplicates collapse')

// --- query results by tile, and the key that decides a rebuild ------------------------------
const feat = (z: number, x: number, y: number, type = 3, props: Record<string, unknown> = { class: 'ocean' }): TileFeature => ({
  _z: z,
  _x: x,
  _y: y,
  _vectorTileFeature: { type },
  properties: props,
})
const fs = [feat(10, 163, 395), feat(10, 164, 395), feat(10, 163, 395), feat(9, 81, 197)]
const { groups, missing } = byTile(fs)
check(groups.size === 3 && groups.get('10/163/395')!.feats.length === 2 && missing === 0, 'grouped by tile')
check(seaKey(fs) === seaKey([fs[3], fs[1], fs[2], fs[0]]), 'the key does not depend on the order MapLibre returns features in')
check(seaKey(fs) !== seaKey(fs.slice(0, 3)), 'a tile gone changes the key')
check(seaKey(fs) !== seaKey([...fs, feat(10, 164, 395)]), 'a sea feature more in a tile changes the key (a reloaded tile)')
check(seaKey([]) === '' && tilesKey(new Map()) === '', 'no water tiles: the empty key, the sea before any tile loads')
const noIds = { ...feat(10, 1, 1), _z: undefined } as unknown as TileFeature
const withMissing = byTile([...fs, noIds])
check(withMissing.missing === 1 && withMissing.groups.size === 3, 'a feature without its tile is counted, not grouped (map3d.ts warns)')
// only the sea counts: other water, pools, lines and tunnels neither make a tile nor change its
// count (a lake's tile loading is no reason to tessellate the sea again)
const pool = (z: number, x: number, y: number) => feat(z, x, y, 3, { class: 'swimming_pool' })
const lake = (z: number, x: number, y: number) => feat(z, x, y, 3, { class: 'lake' })
check(seaKey([...fs, pool(10, 165, 395), pool(10, 165, 395)]) === seaKey(fs), 'a tile of nothing but pools leaves the key alone (no rebuild that changes nothing)')
check(seaKey([...fs, lake(10, 165, 395), feat(10, 165, 395, 3, { class: 'pond', intermittent: 1 })]) === seaKey(fs), 'nor does a tile of nothing but lakes and ponds')
check(seaKey([...fs, pool(10, 163, 395), lake(10, 163, 395), feat(10, 163, 395, 3, { class: 'river' })]) === seaKey(fs), 'a pool, lake or river more in a sea tile leaves the key alone')
check(seaKey([...fs, feat(10, 163, 395, 2, { class: 'ocean' }), feat(10, 163, 395, 3, { class: 'ocean', brunnel: 'tunnel' })]) === seaKey(fs), 'nor do lines or water in a tunnel')
check(byTile([pool(9, 81, 197), lake(9, 81, 197)]).groups.size === 0 && seaKey([pool(9, 81, 197), lake(9, 81, 197)]) === '', 'lakes and pools alone: no tiles, the empty key')
// a parent holding only lakes or pools must not hide its child's sea from outermost
const keptKids = outermost([...byTile([pool(9, 81, 197), lake(9, 81, 197), feat(10, 163, 395)]).groups.values()])
check(keptKids.length === 1 && tileKey(keptKids[0]) === '10/163/395', "a parent of lakes and pools does not drop its child's sea")

// --- which water: the sea surface takes the sea, the draped fill the rest -------------------------
check(SEA_CLASS === 'ocean' && isSeaWater(feat(10, 1, 1)), 'an ocean polygon is the sea')
for (const cls of ['lake', 'pond', 'river', 'dock', 'swimming_pool', 'something_new']) {
  check(!isSeaWater(feat(10, 1, 1, 3, { class: cls })), `a ${cls} polygon is not on the sea surface (it lies at its own height)`)
}
check(!isSeaWater(feat(10, 1, 1, 3, { class: 'lake', intermittent: 1 })), 'nor a salt pond (an intermittent lake or pond)')
check(!isSeaWater(feat(10, 1, 1, 3, {})), 'nor water with no class')
check(!isSeaWater(feat(10, 1, 1, 2, { class: 'ocean' })), 'not a line')
check(!isSeaWater(feat(10, 1, 1, 3, { class: 'ocean', brunnel: 'tunnel' })), 'not water in a tunnel')

// The draped fill (MapView.setDrapedWater), evaluated by MapLibre's own expression engine (the
// style spec maplibre-gl itself depends on): every water polygon but the sea and pools comes out
// as the sea surface's tint composited over the style's water colour, the colour the surface
// gives the sea where it draws over that same fill, so the two meet without a seam.
const STYLE_WATER = 'rgb(27 ,27 ,29)' // OpenFreeMap dark's `water` fill-color
const draped = drapedWater(STYLE_WATER)!
const colourExpr = createPropertyExpression(draped.color, latest.paint_fill['fill-color'] as StylePropertySpecification)
const sortExpr = createPropertyExpression(draped.sortKey, latest.layout_fill['fill-sort-key'] as StylePropertySpecification)
check(colourExpr.result === 'success' && sortExpr.result === 'success', `the draped fill parses as a style expression (${JSON.stringify(colourExpr.value)})`)
const evalAs = <T>(e: typeof colourExpr, props: Record<string, unknown>) =>
  (e.value as unknown as { evaluate(g: { zoom: number }, f: { type: string; properties: Record<string, unknown> }): T }).evaluate({ zoom: 13 }, { type: 'Polygon', properties: props })
const rgb255 = (c: { r: number; g: number; b: number; a: number }) => [c.r, c.g, c.b].map((v) => (v / c.a) * 255)
const near = (a: number[], b: number[], eps: number) => a.every((v, k) => Math.abs(v - b[k]) <= eps)
// the surface's blend (deck.gl's default: src alpha, one minus src alpha) over the style's water
const a = SEA_TINT[3] / 255
const surfaceOverWater = [27, 27, 29].map((w, k) => SEA_TINT[k] * a + w * (1 - a))
for (const cls of ['lake', 'pond', 'river', 'dock', 'something_new']) {
  const c = evalAs<{ r: number; g: number; b: number; a: number }>(colourExpr, { class: cls, intermittent: 1 })
  check(c.a === 1 && near(rgb255(c), surfaceOverWater, 0.01), `a ${cls} is draped in the tint the surface gives the sea (${rgb255(c).map((v) => v.toFixed(2))} against ${surfaceOverWater.map((v) => v.toFixed(2))})`)
}
check(near(rgb255(evalAs(colourExpr, {})), surfaceOverWater, 0.01), 'water with no class too')
for (const cls of ['ocean', 'swimming_pool']) {
  const c = evalAs<{ r: number; g: number; b: number; a: number }>(colourExpr, { class: cls })
  check(near(rgb255(c), [27, 27, 29], 0.01), `the ${cls} keeps the style's own colour (the surface tints the sea; pools stay as they were)`)
}
check(evalAs<number>(sortExpr, { class: 'ocean' }) > evalAs<number>(sortExpr, { class: 'lake' }) && evalAs<number>(sortExpr, { class: 'lake' }) === evalAs<number>(sortExpr, { class: 'river' }), 'the sea draws last, over a lake or river inside the coastline (tinted once, by the surface)')
// another style colour: the composite follows it (MapLibre does the arithmetic)
const other = evalAs<{ r: number; g: number; b: number; a: number }>(createPropertyExpression(drapedWater('#102030')!.color, latest.paint_fill['fill-color'] as StylePropertySpecification), { class: 'lake' })
check(near(rgb255(other), [0x10, 0x20, 0x30].map((w, k) => SEA_TINT[k] * a + w * (1 - a)), 0.01), 'the composite follows the style colour')
check(drapedWater(['interpolate', ['linear'], ['zoom'], 5, '#000', 10, '#111']) === null && drapedWater({ stops: [[5, '#000']] }) === null && drapedWater(undefined) === null, 'a colour that is not a plain string: null (the layer is left as it is)')

// --- the polygons: as MapLibre gives them -------------------------------------------------------
// lon/lat of a point given in the tile's own coordinates (0..1 across, buffer outside)
const n = 2 ** T.z
const P = (u: number, v: number): Pt => [((T.x + u) / n) * 360 - 180, (360 / Math.PI) * Math.atan(Math.exp((1 - (2 * (T.y + v)) / n) * Math.PI)) - 90]
const close = (r: Pt[]) => [...r, r[0]]
// the cases a cut to the tile square would have had to get right, which now pass through whole:
// a U leaving through the east edge and coming back, a hole across the east edge, a hole across a
// corner, and a ring that goes round the north-east corner outside the tile
const u = close([P(0.2, 0.2), P(1.05, 0.2), P(1.05, 0.4), P(0.6, 0.4), P(0.6, 0.6), P(1.05, 0.6), P(1.05, 0.8), P(0.2, 0.8)])
const all = close([P(-0.03, -0.03), P(1.03, -0.03), P(1.03, 1.03), P(-0.03, 1.03)])
const holeEdge = close([P(0.8, 0.4), P(0.8, 0.6), P(1.02, 0.6), P(1.02, 0.4)])
const holeCorner = close([P(0.9, -0.02), P(0.9, 0.1), P(1.02, 0.1), P(1.02, -0.02)])
const corner = close([P(0.2, 0.2), P(0.9, 0.2), P(1.03, 0.5), P(0.5, -0.03), P(0.5, 0.1), P(0.2, 0.1)])
const polys: Polygon[] = [[u], [all, holeEdge, holeCorner], [corner]]
const tiles: TileWater[] = [
  { ...T, polygons: polys },
  // a child of T: its water is T's again, at a finer zoom, and is not drawn
  { z: 11, x: 2 * T.x, y: 2 * T.y, polygons: [[close([P(0, 0), P(0.5, 0), P(0.5, 0.5), P(0, 0.5)])]] },
  // a neighbour whose buffer overlaps T's (the stencil tints the overlap once)
  { z: 10, x: T.x + 1, y: T.y, polygons: [[close([P(0.97, 0.3), P(2, 0.3), P(2, 0.7), P(0.97, 0.7)])]] },
]
const sea = assembleSea(tiles)
check(sea.length === 4, `T's three polygons and the neighbour's, not the child's (${sea.length})`)
check(sea[0].polygon[0] === u && sea[1].polygon.length === 3 && sea[1].polygon[1] === holeEdge && sea[1].polygon[2] === holeCorner && sea[2].polygon[0] === corner, 'rings pass through unchanged, holes and all')
check(sea[3].polygon[0][0][0] < P(1, 0)[0], "the neighbour's buffer reaching into T is kept")
// degenerate rings
const tiny: Pt[] = [P(0.1, 0.1), P(0.2, 0.1)]
check(assembleSea([{ ...T, polygons: [[tiny, holeEdge]] }]).length === 0, 'an outer ring of fewer than 3 points drops its polygon with its holes')
check(assembleSea([{ ...T, polygons: [[all, tiny]] }])[0].polygon.length === 1, 'a hole of fewer than 3 points is dropped alone')
check(assembleSea([]).length === 0 && assembleSea([{ ...T, polygons: [] }]).length === 0, 'no water: no sea')

// GeoJSON geometries
check(polygonsOf({ type: 'Polygon', coordinates: polys[1] }).length === 1, 'a Polygon')
check(polygonsOf({ type: 'MultiPolygon', coordinates: polys }).length === 3, 'a MultiPolygon')
check(polygonsOf({ type: 'LineString', coordinates: [] }).length === 0, 'not a polygon')

// --- what draws under the sea surface -----------------------------------------------------------
const veh = (uid: number, kind: Kind, alt: number | null, domain?: Identity['domain']) =>
  ({ uid, kind, alt_msl: alt, identity: domain ? ({ domain } as Identity) : null }) as Pick<Vehicle, 'uid' | 'kind' | 'identity' | 'alt_msl'>
check(underWater(veh(1, 'sub', -20)) && underWater(veh(1, 'sub', 0)), 'a submarine, dived or surfaced (its trail stays in one layer)')
check(!underWater(veh(2, 'boat', 0)) && !underWater(veh(2, 'boat', -0.8)), 'a boat at the surface, GPS wobble and all')
check(underWater(veh(2, 'boat', -1.5)), 'anything more than a metre down')
check(!underWater(veh(3, 'plane', 120)) && !underWater(veh(4, 'boat', null)), 'an aircraft; a boat with no altitude')
check(underWater(veh(5, 'unknown', 0, 'subsurface')) && !underWater(veh(6, 'sub', 0, 'surface')), "the fleet manifest's domain wins over the kind")
// the trails' split keeps its arrays while nothing changed, so deck.gl does not redo the layers
const boat = veh(2, 'boat', 0)
const sub = veh(1, 'sub', -20)
const list = [boat, sub, veh(3, 'plane', 120)]
const b1 = belowOf(list)
const s1 = splitUnder(null, list, b1.below, b1.sig)
check(s1.under.length === 1 && s1.under[0] === sub && s1.over.length === 2, 'split: the submarine under, the rest over')
const b2 = belowOf(list)
check(b2.sig === b1.sig && splitUnder(s1, list, b2.below, b2.sig) === s1, 'the same list and signature: the same arrays')
const dived = list.map((v) => (v === boat ? { ...boat, alt_msl: -1.5 } : v))
const b3 = belowOf(dived)
check(b3.sig !== b1.sig, 'a vehicle crossing -1 m changes the signature')
const s3 = splitUnder(s1, list, b3.below, b3.sig)
check(s3 !== s1 && s3.under !== s1.under && s3.under.length === 2 && s3.over.length === 1, 'and the same list then gives new arrays, the boat under')
check(splitUnder(s3, dived, b3.below, b3.sig) !== s3, 'a new list gives new arrays too')
const none = belowOf([boat])
const s4 = splitUnder(null, [boat], none.below, none.sig)
check(s4.under.length === 0 && s4.over === s4.list, 'nothing under water: over is the list itself')

// --- what map3d.ts relies on in luma.gl 9.4's WebGL adapter -----------------------------------
const po = polygonOffset(-1, -4)
check(po.depthBias === -1 && po.depthBiasSlopeScale === -4, 'factor goes in depthBias, units in depthBiasSlopeScale')
const lumaFile = (path: string) => readFileSync(new URL(`../node_modules/@luma.gl/webgl/dist/adapter/${path}`, import.meta.url), 'utf8')
const params = lumaFile('converters/device-parameters.js')
const renderPass = lumaFile('resources/webgl-render-pass.js')
const pin = (text: string, pattern: RegExp, what: string) => check(pattern.test(text), `luma.gl changed: ${what}. Check map3d.ts (polygonOffset, the sea's stencil passes) against the new luma.gl before updating this pin.`)
pin(params, /gl\.polygonOffset\(parameters\.depthBias, parameters\.depthBiasSlopeScale/, 'gl.polygonOffset(depthBias, depthBiasSlopeScale): which is the factor and which the units')
pin(params, /gl\.stencilFuncSeparate\(1028, glValue, 0, mask\);\s*gl\.stencilFuncSeparate\(1029, glValue, 0, mask\);/, 'the stencil reference is no longer 0')
pin(params, /const mask = parameters\.stencilReadMask \|\| 0xffffffff;/, 'stencilReadMask is no longer the compare mask')
pin(params, /parameters\.stencilCompare !== 'always'\s*\?\s*gl\.enable\(2960\)\s*:\s*gl\.disable\(2960\)/, "a stencilCompare of 'always' no longer turns the stencil test off")
pin(renderPass, /glParameters\.stencilMask = this\.props\.stencilReadOnly \? 0 : 1;/, 'the render pass no longer forces the stencil write mask to 1')

console.log('sea: ok')
