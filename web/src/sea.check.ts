// Checks for sea.ts: there is no test runner in web/, so run them with Node 22.18 or later, which
// strips the types itself (make check does this):
//   node web/src/sea.check.ts
// Throws on the first failure, prints "sea: ok" otherwise. Not part of the bundle.
import { createPropertyExpression, latest, type StylePropertySpecification } from '@maplibre/maplibre-gl-style-spec'
import { belowOf, drapedWater, SEA_TINT, seenThroughWater, splitUnder, underWater, type RGBA } from './sea.ts'
import type { Identity, Kind, Vehicle } from './types.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`sea check failed: ${what}`)
}
const near = (a: number[], b: number[], eps: number) => a.every((v, k) => Math.abs(v - b[k]) <= eps)
const a = SEA_TINT[3] / 255
/** `top` with alpha `alpha` blended over an opaque `under` (deck.gl's and MapLibre's blend: src
 *  alpha, one minus src alpha) */
const over = (top: number[], alpha: number, under: number[]) => under.map((u, k) => top[k] * alpha + u * (1 - alpha))

// --- the water: the style's fill, tinted and draped, every class but pools -------------------------
// Evaluated by MapLibre's own expression engine (the style spec maplibre-gl itself depends on):
// every water polygon comes out as the old sea surface's tint over the style's water colour, the
// colour the surface gave the water where it won the depth test, now at any height and angle.
const STYLE_WATER = 'rgb(27 ,27 ,29)' // OpenFreeMap dark's `water` fill-color
const draped = drapedWater(STYLE_WATER)!
const colourExpr = createPropertyExpression(draped.color, latest.paint_fill['fill-color'] as StylePropertySpecification)
check(colourExpr.result === 'success', `the draped fill parses as a style expression (${JSON.stringify(colourExpr.value)})`)
type Colour = { r: number; g: number; b: number; a: number }
const evalAs = (e: typeof colourExpr, props: Record<string, unknown>) =>
  (e.value as unknown as { evaluate(g: { zoom: number }, f: { type: string; properties: Record<string, unknown> }): Colour }).evaluate({ zoom: 13 }, { type: 'Polygon', properties: props })
const rgb255 = (c: Colour) => [c.r, c.g, c.b].map((v) => (v / c.a) * 255)
const tintedWater = over(SEA_TINT, a, [27, 27, 29])
for (const cls of ['ocean', 'lake', 'pond', 'river', 'dock', 'something_new']) {
  const c = evalAs(colourExpr, { class: cls, intermittent: 1 })
  check(c.a === 1 && near(rgb255(c), tintedWater, 0.01), `the ${cls} is draped in the tint (${rgb255(c).map((v) => v.toFixed(2))} against ${tintedWater.map((v) => v.toFixed(2))})`)
}
check(near(rgb255(evalAs(colourExpr, {})), tintedWater, 0.01), 'water with no class too')
check(near(rgb255(evalAs(colourExpr, { class: 'swimming_pool' })), [27, 27, 29], 0.01), "a swimming pool keeps the style's own colour")
// another style colour: the composite follows it (MapLibre does the arithmetic)
const other = evalAs(createPropertyExpression(drapedWater('#102030')!.color, latest.paint_fill['fill-color'] as StylePropertySpecification), { class: 'lake' })
check(near(rgb255(other), over(SEA_TINT, a, [0x10, 0x20, 0x30]), 0.01), 'the composite follows the style colour')
check(
  drapedWater(['interpolate', ['linear'], ['zoom'], 5, '#000', 10, '#111']) === null && drapedWater({ stops: [[5, '#000']] }) === null && drapedWater(undefined) === null,
  'a colour that is not a plain string: null (the layer is left as it is)',
)

// --- under water: drawn seen through the tint ----------------------------------------------------
// A trail (alpha 140) or stem (alpha 160) drawn in seenThroughWater's colour over the tinted water
// gives exactly what the sea surface gave by tinting the trail drawn over the untinted water, for
// any colour, alpha and water under it.
const samples: [RGBA, number][] = [
  [[111, 224, 196, 140], 140],
  [[127, 184, 255, 160], 160],
  [[214, 196, 140, 255], 255],
  [[0, 0, 0, 1], 1],
]
for (const [c, alpha] of samples) {
  for (const water of [[27, 27, 29], [0, 0, 0], [255, 255, 255], [60, 20, 90]]) {
    const surface = over(SEA_TINT, a, over(c, alpha / 255, water))
    const seen = seenThroughWater(c)
    const drawn = over(seen, alpha / 255, over(SEA_TINT, a, water))
    check(seen[3] === c[3] && near(drawn, surface, 1e-9), `seen through the water: ${c} over ${water} (${drawn.map((v) => v.toFixed(3))} against ${surface.map((v) => v.toFixed(3))})`)
  }
}

// --- what is drawn as under water ----------------------------------------------------------------
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

console.log('sea: ok')
