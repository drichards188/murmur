// Checks for floatPlace.ts: node web/src/floatPlace.check.ts (prints "floatPlace: ok").
import assert from 'node:assert/strict'
import {
  BAR,
  CASCADE,
  cascadeSpot,
  clampBar,
  cornerAt,
  freeSpot,
  GAP,
  inside,
  layoutOf,
  MARGIN,
  overlaps,
  place,
  sameRect,
  spotAt,
  spotOf,
  type Rect,
} from './floatPlace.ts'

// a map 1180 x 760, as beside a 340 px fleet list and a 400 px rail on a 1920 x 804 window
const area: Rect = { x: 0, y: 0, w: 1180, h: 760 }
const rect = (x: number, y: number, w: number, h: number): Rect => ({ x, y, w, h })
const at = (p: { x: number; y: number }, w: number, h: number) => rect(p.x, p.y, w, h)

// what is fixed on the map (MapView, WeatherLayer, maplibre): the toolbar, the zoom buttons and the
// attribution, the scale, the weather bar
const toolbar = rect(12, 12, 92, 28)
const zoom = rect(1141, 640, 29, 90)
const attribution = rect(1146, 736, 24, 24)
const scale = rect(10, 736, 100, 18)
const legend = rect(14, 680, 360, 30)
const fixed = [toolbar, zoom, attribution, scale, legend]

// ---- geometry -----------------------------------------------------------------------------------
assert.equal(overlaps(rect(0, 0, 10, 10), rect(10, 0, 10, 10)), false, 'edge to edge is not an overlap')
assert.equal(overlaps(rect(0, 0, 10, 10), rect(9, 0, 10, 10)), true)
assert.equal(overlaps(rect(0, 0, 10, 10), rect(10 + GAP, 0, 10, 10), GAP), false, 'exactly GAP apart is clear')
assert.equal(overlaps(rect(0, 0, 10, 10), rect(10 + GAP - 1, 0, 10, 10), GAP), true, 'nearer than GAP is not')
assert.deepEqual(cornerAt({ w: 300, h: 400 }, 'top-right', area), { x: 1180 - MARGIN - 300, y: MARGIN })
assert.deepEqual(cornerAt({ w: 300, h: 200 }, 'bottom-left', area), { x: MARGIN, y: 760 - MARGIN - 200 })
assert.deepEqual(inside({ x: -50, y: 900 }, { w: 300, h: 200 }, area), { x: MARGIN, y: 760 - MARGIN - 200 }, 'moved inside with the margin')
assert.equal(inside({ x: 0, y: 0 }, { w: 1300, h: 200 }, area).x, 0, 'wider than the area: at its left, not off it')
assert.equal(inside({ x: 0, y: 0 }, { w: 1000, h: 200 }, { x: 0, y: 0, w: 1010, h: 760 }).x, 5, 'too wide for the margins: centred')
assert.equal(inside({ x: 0, y: 300 }, { w: 300, h: 900 }, area).y, MARGIN, 'taller than the area: at the top margin (its height gives way)')

// ---- a panel alone opens on its own corner -------------------------------------------------------
const weather = { w: 300, h: 380 }
const w1 = place(weather, 'top-right', area, fixed, [])
assert.equal(w1.free, true)
assert.deepEqual(w1.at, { x: 1180 - MARGIN - 300, y: MARGIN }, 'Weather: top right, where it was')
const sit = place({ w: 360, h: 420 }, 'top-left', area, fixed, [])
assert.deepEqual(sit.at, { x: MARGIN, y: toolbar.y + toolbar.h + GAP }, 'Situation: top left, under the map toolbar (it sat at 52 under it)')
const links = place({ w: 300, h: 230 }, 'bottom-left', area, fixed, [])
assert.deepEqual(links.at, { x: MARGIN, y: legend.y - GAP - 230 }, 'Links: bottom left, above the weather bar (it lay under it)')

// ---- open together, none overlaps another or anything fixed ------------------------------------
const globe = { w: 280, h: 300 }
const g = place(globe, 'top-right', area, fixed, [])
const gR = at(g.at, globe.w, globe.h)
const w2 = place(weather, 'top-right', area, fixed, [gR])
const w2R = at(w2.at, weather.w, weather.h)
assert.equal(w2.free, true)
assert.equal(overlaps(w2R, gR), false, 'Weather and the globe no longer lie on each other (both sat top right)')
assert.equal(w2.at.y, gR.y + gR.h + GAP, 'it stacks down the right edge, under the globe, before it takes the middle')
assert.equal(w2R.x + w2R.w, zoom.x - GAP, 'as far right as the zoom buttons below leave it')
assert.equal(overlaps(w2R, zoom), false)

// vehicle details and the Situation chip both prefer the top left: the second goes under the first
const chip = place({ w: 420, h: 30 }, 'top-left', area, fixed, [])
const chipR = at(chip.at, 420, 30)
const vd = place({ w: 330, h: 480 }, 'top-left', area, fixed, [chipR])
const vdR = at(vd.at, 330, 480)
assert.equal(overlaps(vdR, chipR), false)
assert.equal(overlaps(vdR, toolbar), false, 'the details no longer cover the map toolbar (it opened at 12, 12 on it)')
assert.equal(vd.at.x, MARGIN, 'still on the left edge')

// a whole set of panels opened one after another: every pair clear, every one clear of the fixed
const opened: Rect[] = []
const open = (w: number, h: number, c: Parameters<typeof place>[1]) => {
  const p = place({ w, h }, c, area, fixed, opened)
  const r = at(p.at, w, h)
  opened.push(r)
  return p
}
open(280, 300, 'top-right') // globe
open(300, 380, 'top-right') // weather
open(300, 230, 'bottom-left') // links
open(420, 30, 'top-left') // situation chip
open(330, 480, 'top-left') // vehicle details
for (const [i, a] of opened.entries()) {
  for (const b of opened.slice(i + 1)) assert.equal(overlaps(a, b), false, `panels ${JSON.stringify(a)} and ${JSON.stringify(b)} overlap`)
  for (const f of fixed) assert.equal(overlaps(a, f), false, `panel ${JSON.stringify(a)} covers ${JSON.stringify(f)}`)
  assert.ok(a.x >= MARGIN && a.y >= MARGIN && a.x + a.w <= area.w - MARGIN && a.y + a.h <= area.h - MARGIN, 'inside with the margin')
}

// the coach card (tutorial) at the bottom left takes the Links corner: Links goes elsewhere
const coach = rect(12, 300, 390, 372)
const l2 = place({ w: 300, h: 230 }, 'bottom-left', area, [...fixed, coach], [])
assert.equal(l2.free, true)
assert.equal(overlaps(at(l2.at, 300, 230), coach), false, 'clear of the coach card')

// a banner at the top centre and an open dock (the area stops at the drawer: the caller's measure)
const banner = rect(300, 12, 580, 40)
const docked: Rect = { x: 0, y: 0, w: 1180 - 430, h: 760 }
const w3 = place(weather, 'top-right', docked, [...fixed, banner], [])
assert.equal(w3.at.x + weather.w, docked.w - MARGIN, 'beside the drawer, not under it')
assert.equal(overlaps(at(w3.at, weather.w, weather.h), banner), false, 'clear of the banner')

// ---- no free spot: cascade, never exactly on another ----------------------------------------------
const small: Rect = { x: 0, y: 0, w: 500, h: 400 }
const big = { w: 400, h: 300 }
const first = place(big, 'top-left', small, [], [])
assert.equal(first.free, true)
const firstR = at(first.at, big.w, big.h)
const second = place(big, 'top-left', small, [], [firstR])
assert.equal(second.free, false, 'nothing free on a small map')
assert.deepEqual(second.at, { x: first.at.x + CASCADE, y: first.at.y + CASCADE }, 'offset from the one at its corner')
const third = place(big, 'top-left', small, [], [firstR, at(second.at, big.w, big.h)])
assert.notDeepEqual(third.at, second.at, 'a third is not stacked exactly on the second')
// from the right: the cascade steps left and down
const r1 = cascadeSpot(big, 'top-right', small, [])
const r2 = cascadeSpot(big, 'top-right', small, [at(r1, big.w, big.h)])
assert.ok(r2.x <= r1.x && r2.y > r1.y, 'a right corner cascades inward (left) and down')
// a cascade starts clear of what is fixed when it can: a tall panel on a map whose top left holds
// the toolbar starts beside it, not on it
const tall = { w: 400, h: 760 - 2 * MARGIN }
const tallFirst = place(tall, 'top-left', area, [toolbar], [])
assert.equal(tallFirst.free, true)
assert.deepEqual(tallFirst.at, { x: toolbar.x + toolbar.w + GAP, y: MARGIN }, 'beside the toolbar')
const tallNext = place(tall, 'top-left', area, [toolbar], [at(tallFirst.at, tall.w, tall.h), rect(520, 12, 640, 736)])
assert.equal(tallNext.free, false)
assert.equal(overlaps(at(tallNext.at, tall.w, tall.h), toolbar), false, 'the cascade keeps off the toolbar too')
assert.deepEqual(tallNext.at, { x: tallFirst.at.x + CASCADE, y: MARGIN }, 'one step across (no room to step down)')
// a cascade that runs out of room stops inside the area
const many = Array.from({ length: 30 }, (_, k) => rect(MARGIN + k * CASCADE, MARGIN + k * CASCADE, big.w, big.h))
const last = cascadeSpot(big, 'top-left', small, many)
assert.ok(last.x + big.w <= small.w && last.y + big.h <= small.h, 'never pushed off the map')

// freeSpot itself: nothing to avoid, the corner
assert.deepEqual(freeSpot({ w: 100, h: 100 }, 'bottom-right', area, []), { x: 1180 - MARGIN - 100, y: 760 - MARGIN - 100 })
// a panel taller than the area has no free spot
assert.equal(freeSpot({ w: 100, h: 800 }, 'top-left', area, []), null)

// ---- the drag: the bar stays inside the area -------------------------------------------------------
const size = { w: 300, h: 400 }
assert.deepEqual(clampBar({ x: -200, y: -50 }, size, area), { x: 0, y: 0 }, 'not past the left or the top')
assert.deepEqual(clampBar({ x: 2000, y: 2000 }, size, area), { x: 1180 - 300, y: 760 - BAR }, 'the whole bar inside on the right; at least BAR of it above the bottom')
assert.deepEqual(clampBar({ x: 500, y: 300 }, size, area), { x: 500, y: 300 }, 'inside: unchanged')
assert.equal(clampBar({ x: 50, y: 0 }, { w: 1300, h: 100 }, area).x, 0, 'wider than the area: at its left')

// ---- spots: kept from the nearer edges --------------------------------------------------------------
const tr = spotOf({ x: 868, y: 12 }, weather, area, false)
assert.deepEqual(tr, { ax: 'r', x: 12, ay: 't', y: 12 }, 'a top right panel is kept from the right')
const bl = spotOf({ x: 12, y: 442 }, { w: 300, h: 230 }, area, true)
assert.deepEqual(bl, { ax: 'l', x: 12, ay: 'b', y: 760 - 442 - 230 }, 'a bottom panel kept from the bottom grows upward')
assert.equal(spotOf({ x: 12, y: 442 }, { w: 300, h: 230 }, area, false).ay, 't', 'only when asked')
assert.equal(spotOf({ x: 12, y: 600 }, { w: 300, h: 230 }, area, true).ay, 't', 'one hanging below the area is kept from the top (its height gives way)')
assert.equal(spotOf({ x: 12, y: 100 }, { w: 300, h: 230 }, area, true).ay, 't', 'one in the upper half is kept from the top')
assert.deepEqual(spotOf({ x: 12.4, y: 99.6 }, { w: 300, h: 230 }, area, false), { ax: 'l', x: 12, ay: 't', y: 100 }, 'whole px')
// a spot comes back where it was
for (const [p, s, b] of [
  [{ x: 868, y: 12 }, weather, false],
  [{ x: 12, y: 442 }, { w: 300, h: 230 }, true],
  [{ x: 600, y: 500 }, { w: 300, h: 200 }, true],
  [{ x: 100, y: 50 }, { w: 330, h: 480 }, true],
] as const) {
  assert.deepEqual(spotAt(spotOf(p, s, area, b), s, area), p, `round trip ${JSON.stringify(p)}`)
}

// the area narrows (a drawer opens on the right): a right-kept panel moves with the edge, a
// left-kept one stays unless it would go under the drawer
assert.deepEqual(spotAt(tr, weather, docked), { x: docked.w - 12 - 300, y: 12 }, 'beside the drawer')
const leftKept = spotOf({ x: 500, y: 100 }, { w: 300, h: 200 }, { x: 0, y: 0, w: 1300, h: 760 }, false)
assert.equal(leftKept.ax, 'l')
assert.equal(spotAt(leftKept, { w: 300, h: 200 }, docked).x, docked.w - 300, 'clamped back inside, not under the drawer')
// the window shrinks: a remembered spot is clamped back into the map
const far: ReturnType<typeof spotOf> = { ax: 'l', x: 1500, ay: 't', y: 900 }
const shrunk = spotAt(far, size, area)
assert.ok(shrunk.x + size.w <= area.w && shrunk.y <= area.h - BAR, 'inside again')

// ---- laid out in the layer ---------------------------------------------------------------------------
const layer = { w: 1180, h: 760 }
const lw = layoutOf(tr, weather, area, layer)
assert.equal(lw.right, 12, 'kept from the right: CSS right, so it grows leftward (the globe enlarged)')
assert.equal(lw.left, undefined)
assert.equal(lw.top, 12)
assert.equal(lw.maxHeight, 760 - 12, 'it may grow down to the area bottom')
const lb = layoutOf(bl, { w: 300, h: 230 }, area, layer)
assert.equal(lb.bottom, 760 - 442 - 230)
assert.equal(lb.top, undefined)
assert.equal(lb.maxHeight, 442 + 230, 'kept from the bottom: it may grow up to the area top')
// with a drawer open the layer is wider than the area: CSS right counts from the layer's edge
const ld = layoutOf(tr, weather, docked, layer)
assert.equal(ld.right, 430 + 12)
// a panel dragged low is kept from the top and gives way in height, never below BAR
const low = layoutOf({ ax: 'l', x: 12, ay: 't', y: 740 }, size, area, layer)
assert.equal(low.top, 760 - BAR, 'its bar stays inside')
assert.equal(low.maxHeight, BAR)
// the nomination form's layer reaches over the map and the rail (1180 + 400): dragged out of the
// rail over the map, it is kept from the rail's right edge
const wide: Rect = { x: 0, y: 0, w: 1580, h: 760 }
const lr = layoutOf(spotOf({ x: 680, y: 0 }, { w: 400, h: 760 }, wide, true), { w: 400, h: 760 }, wide, { w: 1580, h: 760 })
assert.equal(lr.right, 500, 'over the map, 500 px in from the rail edge')
assert.equal(lr.top, 0)

assert.equal(sameRect(area, { ...area, w: 1180.2 }), true)
assert.equal(sameRect(area, docked), false)

console.log('floatPlace: ok')
