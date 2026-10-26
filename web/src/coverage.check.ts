// Checks for coverage.ts: there is no test runner in web/, so run them with Node 22.18 or later,
// which strips the types itself (make check does this):
//   node web/src/coverage.check.ts
// Throws on the first failure, prints "coverage: ok" otherwise. Not part of the bundle.
import { cellAt, gridPixels, gridRequest, legPaths, routeDrawing, slicePath, cumulative, GRID_BUDGET, type CoverageGrid, type OrderLink } from './coverage.ts'
import { distanceM } from './geo.ts'
import type { LngLat } from './types.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`coverage check failed: ${what}`)
}
const nearM = (a: LngLat, b: LngLat, m: number) => distanceM(a, b) <= m

// --- the grid request: within the budget, at the terrain's cell or coarser, the same after a small pan
const gate: [number, number, number, number] = [-122.56, 37.77, -122.40, 37.86]
const c: LngLat = [-122.48, 37.815]
const r = gridRequest(gate, c)
const cells = (q: typeof r) => {
  const dlat = q.res / 111_320
  const dlon = dlat / Math.cos((((q.bbox[1] + q.bbox[3]) / 2) * Math.PI) / 180)
  return Math.ceil((q.bbox[3] - q.bbox[1]) / dlat) * Math.ceil((q.bbox[2] - q.bbox[0]) / dlon)
}
check(r.res === 250, `a 14 km view asks at the 250 m terrain cell (${r.res})`)
check(cells(r) <= GRID_BUDGET, `within the budget (${cells(r)})`)
check(r.bbox[0] <= gate[0] && r.bbox[1] <= gate[1] && r.bbox[2] >= gate[2] && r.bbox[3] >= gate[3], 'covers the view')
const panned = gridRequest([gate[0] + 0.001, gate[1] + 0.0008, gate[2] + 0.001, gate[3] + 0.0008], [c[0] + 0.001, c[1] + 0.0008])
check(JSON.stringify(panned) === JSON.stringify(r), 'a small pan asks for the same grid')
const wide = gridRequest([-123.5, 37.0, -121.5, 38.6], [-122.5, 37.8])
check(wide.res > 250 && cells(wide) <= GRID_BUDGET, `a wide view coarsens within the budget (${wide.res} m, ${cells(wide)})`)
check(wide.bbox[2] - wide.bbox[0] < 1.0, 'and reaches only so far from the centre')

// --- a grid: the cell under a point, and its pixels north up
const g: CoverageGrid = {
  predicted: true, profile: 'los', alt_m: 85, alt_ref: 'msl', at: '2026-06-14T10:00:00Z', bbox: [-122.5, 37.8, -122.47, 37.82],
  res_m: 1000, coarsened: false, lat0: 37.8045, lon0: -122.4943, dlat: 0.009, dlon: 0.0114, nlat: 2, nlon: 3,
  // south row: up, degraded, down; north row: down (terrain), unknown, up
  state: 'udxx?u', reason: '-rrt--', p_loss: [0.01, 0.3, 0.97, 1, 0, 0.02],
  counts: { up: 2, degraded: 1, down: 2, unknown: 1 }, thresholds: { degraded: 0.2, down: 0.95 }, failsafe_s: 5,
  sources: { links: 'sim/links.toml', terrain: 'test', terrain_cell_m: 250, terrain_alt_step_m: 10, ground_station: null, ground_station_check: null, satcom: null },
  note: '',
}
check(cellAt(g, g.lon0, g.lat0)?.state === 'up', 'cell (0, 0) at its centre')
check(cellAt(g, g.lon0 + 2 * g.dlon, g.lat0)?.reason === 'range', 'cell (0, 2): down by range')
check(cellAt(g, g.lon0 + 0.4 * g.dlon, g.lat0 + 0.6 * g.dlat)?.reason === 'terrain', 'nearer the north row: (1, 0), terrain')
check(cellAt(g, g.lon0 + 0.0114, g.lat0 + 0.009)?.state === 'unknown', 'cell (1, 1) not answered')
check(cellAt(g, g.lon0 - g.dlon, g.lat0) === null && cellAt(g, g.lon0, g.lat0 + 2 * g.dlat) === null, 'outside the grid: nothing')
const px = gridPixels(g)
check(px.width === 3 && px.height === 4, `a column a cell, two rows a cell (${px.width} x ${px.height})`)
const red = (row: number, col: number) => px.data[(row * px.width + col) * 4]
check(red(0, 0) === 255 && red(px.height - 1, 0) === 95, 'top row north (down), bottom row south (up)')

// --- an order's legs, coloured by the stretches' share of each leg
const pos: LngLat = [-122.446, 37.8068]
const home: LngLat = [-122.446, 37.8068]
const orbit = { task: 'orbit' as const, center: [-122.49, 37.83] as LngLat, radius: 500 }
const paths = legPaths(orbit, { position: pos, home })
check(nearM(paths.task![0], [-122.49, 37.83 + 500 / 111_320], 0.5), "the task path starts due north of the centre, as the engine's ring")
check(nearM(paths.transit![1], paths.task![0], 0.01) && nearM(paths.return![0], paths.task![0], 0.01), 'transit ends and return starts there')
check(legPaths({ task: 'rtl' }, { position: pos, home: [-122.45, 37.80] }).return?.length === 2, 'an rtl: the return from where it is')
check(legPaths({ task: 'hold' }, { position: pos, home }).here?.length === 1, 'a hold: where it is')
const line: LngLat[] = [[-122.5, 37.8], [-122.48, 37.8], [-122.48, 37.82]]
const cum = cumulative(line)
const half = slicePath(line, cum, cum[2] / 4, (cum[2] * 3) / 4)
check(nearM(half[0], [-122.5 + 0.02 * ((cum[2] / 4) / cum[1]), 37.8], 1), 'a slice starts a quarter along')
check(half.length === 3 && nearM(half[1], line[1], 0.01), 'and keeps the corner it passes')
const transitLen = distanceM(pos, paths.task![0])
const answer: OrderLink = {
  predicted: true,
  vehicles: [{
    uid: 3, callsign: 'PUMA 1', failsafe_s: 5, holds: false, p_failsafe: 0.9,
    legs: [{
      part: 'transit', name: 'transit leg (position to waypoint 1)', looped: false,
      // the bridge's leg is 2 % longer than the one drawn: stretches are placed by share
      length_m: transitLen * 1.02, duration_s: 100, max_p_loss: 1, down_s: 50, p_failsafe: 0.9, terrain_unknown: 0,
      stretches: [
        { state: 'up', from_m: 0, to_m: transitLen * 0.51, seconds: 50, reason: null, max_p_loss: 0.05 },
        { state: 'down', from_m: transitLen * 0.51, to_m: transitLen * 1.02, seconds: 50, reason: 'terrain', max_p_loss: 1 },
      ],
      first_down: null, trip_down: null,
    }],
    first_down: { part: 'transit', leg: 'transit leg', along_m: transitLen * 0.51, at: [-122.47, 37.82], alt_m: 85, reason: 'terrain', seconds: 50, trips: true },
    trip_down: { part: 'transit', leg: 'transit leg', along_m: transitLen * 0.51, at: [-122.47, 37.82], alt_m: 85, reason: 'terrain', seconds: 50, trips: true },
  }],
}
const d = routeDrawing(answer, orbit, new Map([[3, { position: pos, home }]]))
check(d.lines.length === 2 && d.lines[0].state === 'up' && d.lines[1].state === 'down', 'two stretches, in order')
const mid: LngLat = [(pos[0] + paths.task![0][0]) / 2, (pos[1] + paths.task![0][1]) / 2]
check(nearM(d.lines[0].path[d.lines[0].path.length - 1], mid, 5), 'the change at half the drawn leg')
check(nearM(d.lines[1].path[d.lines[1].path.length - 1], paths.task![0], 1), 'down to its end')
check(d.marks.length === 1 && d.marks[0].kind === 'trip' && d.marks[0].text.includes('FAILSAFE TRIPS'), 'one mark where it goes down and trips')
check(routeDrawing({ predicted: true, vehicles: [{ uid: 3, error: 'no position' }] }, orbit, new Map()).lines.length === 0, 'an error draws nothing')

console.log('coverage: ok')
