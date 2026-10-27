// Predicted link coverage (docs/LINK-COVERAGE.md, bd murmur-hei): the bridge's answers from the
// link emulator's own geometry, as the console shows them.
//
// - A grid for the map (`GET /api/links/coverage?class=..&alt=..&bbox=..&res=..`): one letter a
//   cell for the state and the reason, and the loss. Drawn as one image draped on the terrain
//   (MapView), read under the pointer for the reason.
// - A route answer for an order (`link_coverage` in the dry run, `POST /api/intents/preview`):
//   per vehicle its legs (transit, task path, return) with their stretches up, degraded and down
//   by distance along the leg, `first_down` and `trip_down`, the failsafe's chance and whether it
//   `holds`, and a sentence. The legs are drawn on the map from the order's own geometry
//   (`murmur_decide::route::route_for` lays them out the same way), each stretch placed at its
//   share of the leg's length.
//
// Predicted, never observed: the emulator's random draws and scripted events (jammers, cuts) are
// not in it, and nothing here blocks an order.
import { circle, distanceM } from './geo.ts'
import type { LngLat, TaskKind } from './types.ts'

export type LinkState = 'up' | 'degraded' | 'down'
export type LinkReason = 'loss' | 'range' | 'terrain' | 'no_sat' | 'submerged' | 'surfaced'
export type RoutePart = 'transit' | 'task' | 'return' | 'here'

export interface CoverageSources {
  links: string
  terrain: string
  terrain_cell_m: number
  terrain_alt_step_m: number
  ground_station: [number, number, number] | null
  ground_station_check: string | null
  /** what the SATCOM prediction rests on; null where no satellite was looked at */
  satcom: string | null
}

/** A grid answer: rows south to north, each west to east; cell (i, j) is at lat0 + i dlat,
 *  lon0 + j dlon. */
export interface CoverageGrid {
  predicted: boolean
  profile: string
  alt_m: number
  alt_ref: 'surface' | 'msl'
  at: string
  bbox: [number, number, number, number]
  res_m: number
  coarsened: boolean
  lat0: number
  lon0: number
  dlat: number
  dlon: number
  nlat: number
  nlon: number
  /** one letter a cell: u up, d degraded, x down, ? terrain not answered yet */
  state: string
  /** one letter a cell: - none, l loss, r range, t terrain, s no satellite, w submerged, a surfaced */
  reason: string
  p_loss: number[]
  counts: { up: number; degraded: number; down: number; unknown: number }
  thresholds: { degraded: number; down: number }
  failsafe_s: number | null
  sources: CoverageSources
  note: string
  /** a vehicle's own profile (asked with `uid`) */
  uid?: number
  callsign?: string
}

export interface Stretch {
  state: LinkState
  from_m: number
  to_m: number
  seconds: number
  reason: LinkReason | null
  max_p_loss: number
}

export interface FirstDown {
  along_m: number
  at: LngLat
  alt_m: number
  reason: LinkReason | null
  seconds: number
  trips: boolean
}

export interface LegAnswer {
  part: RoutePart
  name: string
  looped: boolean
  length_m: number
  duration_s: number
  max_p_loss: number
  stretches: Stretch[]
  first_down: FirstDown | null
  trip_down: FirstDown | null
  down_s: number
  p_failsafe: number | null
  terrain_unknown: number
}

export interface RouteDown extends FirstDown {
  part: RoutePart
  leg: string
}

/** One vehicle's predicted link for an order, or why there is none (`error`). */
export interface VehicleLink {
  uid: number
  callsign?: string
  error?: string
  profile?: string
  active_profile?: string
  alt_m?: number | null
  alt_basis?: string
  legs?: LegAnswer[]
  speed_mps?: number
  failsafe_s?: number | null
  heartbeat_s?: number | null
  p_failsafe?: number | null
  holds?: boolean
  first_down?: RouteDown | null
  trip_down?: RouteDown | null
  likely_trip?: { part: RoutePart; leg: string; along_m: number } | null
  sentence?: string
}

/** `link_coverage` in a dry run. */
export interface OrderLink {
  predicted: boolean
  vehicles?: VehicleLink[]
  holds_when?: string
  sources?: CoverageSources
  note?: string
  error?: string
}

// ---- words and colours ----------------------------------------------------------------------

export const STATE_LETTER: Record<string, LinkState | 'unknown'> = { u: 'up', d: 'degraded', x: 'down', '?': 'unknown' }
export const REASON_LETTER: Record<string, LinkReason | null> = {
  '-': null,
  l: 'loss',
  r: 'range',
  t: 'terrain',
  s: 'no_sat',
  w: 'submerged',
  a: 'surfaced',
}

/** As the predictor words them (`Reason::words`), for the map's hover and the order form. */
export const REASON_WORDS: Record<LinkReason, string> = {
  loss: "the link's own loss",
  range: 'range from the ground station',
  terrain: 'behind terrain from the ground station',
  no_sat: 'no satellite of its class in view',
  submerged: 'submerged',
  surfaced: 'at the surface',
}

export type RGBA = [number, number, number, number]
/** The console's ok, warn and bad (index.css), and its muted grey. */
export const STATE_RGB: Record<LinkState | 'unknown', [number, number, number]> = {
  up: [95, 211, 141],
  degraded: [242, 193, 78],
  down: [255, 93, 93],
  unknown: [125, 139, 154],
}
/** The grid's fill alpha (0-255): up faint, so what is degraded or down stands out. */
const GRID_ALPHA: Record<LinkState | 'unknown', number> = { up: 38, degraded: 100, down: 125, unknown: 64 }

// ---- link classes and altitudes -------------------------------------------------------------

/** The link classes of sim/links.toml an operator picks from, and the altitude each starts at. */
export const CLASSES: { id: string; label: string; alt: number; above: boolean }[] = [
  { id: 'los', label: 'LOS radio (los)', alt: 150, above: false },
  { id: 'mesh', label: 'Mesh radio (mesh)', alt: 2, above: true },
  { id: 'satcom', label: 'SATCOM, military GEO (satcom)', alt: 100, above: false },
  { id: 'satcom_ku_geo', label: 'SATCOM, Ku GEO (satcom_ku_geo)', alt: 100, above: false },
  { id: 'satcom_leo', label: 'SATCOM, LEO (satcom_leo)', alt: 2, above: true },
  { id: 'satcom_iridium', label: 'SATCOM, Iridium (satcom_iridium)', alt: 2, above: true },
  { id: 'acoustic', label: 'Acoustic modem (acoustic)', alt: -10, above: false },
]

/** Altitude presets: metres, above mean sea level unless `above` (above the terrain or water). */
export const ALT_PRESETS: { label: string; alt: number; above: boolean; title: string }[] = [
  { label: 'surface', alt: 2, above: true, title: '2 m above the ground or the water: a boat, a ground vehicle' },
  { label: '85 m', alt: 85, above: false, title: '85 m above sea level' },
  { label: '150 m', alt: 150, above: false, title: "150 m above sea level (a Puma's mission altitude at the Golden Gate)" },
  { label: '400 m', alt: 400, above: false, title: '400 m above sea level' },
  { label: '1500 m', alt: 1500, above: false, title: '1500 m above sea level' },
  { label: '-10 m', alt: -10, above: false, title: '10 m below sea level: a submerged vehicle' },
]

// ---- the grid request -----------------------------------------------------------------------

const M_PER_DEG = 111_320
/** Cells a grid request asks for at most (the bridge's limit is 40 000; fewer answer sooner). */
export const GRID_BUDGET = 12_000
/** The same where satellites decide the link: the bridge looks at the constellation for every
 *  cell (about 2 ms each), and what is in view changes over tens of kilometres, not hundreds of
 *  metres. */
export const SAT_GRID_BUDGET = 600

/** Whether satellites decide a link class (or a vehicle's catalog link): sim/links.toml's
 *  SATCOM profiles. */
export function satcomClass(link: string | null | undefined): boolean {
  return !!link && link.startsWith('satcom')
}
/** The farthest a grid reaches from the view's centre, m: a tilted view's far edge can be at
 *  the horizon. */
export const GRID_REACH_M = 30_000
/** The bbox is snapped outward to blocks this many cells wide, so a small pan asks for the same
 *  grid (the bridge caches it) and the cells do not shift under the operator. */
const SNAP_CELLS = 8

export interface GridRequest {
  bbox: [number, number, number, number]
  res: number
}

/** The grid to ask for a view: its bounds (w, s, e, n) clamped to GRID_REACH_M around the centre,
 *  with a margin, at the finest resolution on a ladder from `cellM` (the emulator's terrain cell:
 *  finer says nothing more about terrain) that keeps it within `budget` cells, snapped outward to
 *  blocks of cells. */
export function gridRequest(bounds: [number, number, number, number], center: LngLat, cellM = 250, budget = GRID_BUDGET): GridRequest {
  const [cx, cy] = center
  const cos = (lat: number) => Math.max(0.05, Math.cos((lat * Math.PI) / 180))
  const rLat = GRID_REACH_M / M_PER_DEG
  const rLon = rLat / cos(cy)
  let [w, s, e, n] = bounds
  w = Math.max(w, cx - rLon)
  e = Math.min(e, cx + rLon)
  s = Math.max(s, cy - rLat)
  n = Math.min(n, cy + rLat)
  if (!(e > w && n > s)) [w, s, e, n] = [cx - rLon / 10, cy - rLat / 10, cx + rLon / 10, cy + rLat / 10]
  // a margin, so a small pan stays inside
  const mx = (e - w) * 0.1
  const my = (n - s) * 0.1
  w -= mx
  e += mx
  s = Math.max(-85, s - my)
  n = Math.min(85, n + my)
  let res = cellM
  for (let k = 0; k < 12; k++) {
    const dlat = res / M_PER_DEG
    const dlon = dlat / cos((s + n) / 2)
    if (Math.ceil((n - s) / dlat + SNAP_CELLS) * Math.ceil((e - w) / dlon + SNAP_CELLS) <= budget) break
    res *= 2
  }
  const dlat = res / M_PER_DEG
  const by = SNAP_CELLS * dlat
  const s2 = Math.floor(s / by) * by
  const n2 = Math.ceil(n / by) * by
  // the bridge's columns are as wide as at the bbox's middle latitude: snap with the same
  const bx = (SNAP_CELLS * dlat) / cos((s2 + n2) / 2)
  const w2 = Math.floor(w / bx) * bx
  const e2 = Math.ceil(e / bx) * bx
  // a hair inside the far edges, so the bridge's ceil does not add a row or column of rounding
  const eps = dlat * 1e-6
  return { bbox: [round6(w2), round6(s2), round6(e2 - eps), round6(n2 - eps)], res }
}

function round6(x: number) {
  return Math.round(x * 1e6) / 1e6
}

/** What a grid is asked for: a link class, or a vehicle's own profile (and, unless `alt` is
 *  given, the altitude its orders fly at). */
export type GridTarget = { class: string } | { uid: number }

export function gridUrl(target: GridTarget, alt: number | null, above: boolean, req: GridRequest): string {
  const q = new URLSearchParams()
  if ('uid' in target) q.set('uid', String(target.uid))
  else q.set('class', target.class)
  if (alt != null) q.set('alt', String(alt))
  if (above && alt != null) q.set('above_surface', '1')
  q.set('bbox', req.bbox.join(','))
  q.set('res', String(req.res))
  return `/api/links/coverage?${q}`
}

// ---- the grid as an image -------------------------------------------------------------------

/** The grid's outer edges: west, south, east, north. */
export function gridEdges(g: CoverageGrid): [number, number, number, number] {
  return [g.lon0 - g.dlon / 2, g.lat0 - g.dlat / 2, g.lon0 + (g.nlon - 0.5) * g.dlon, g.lat0 + (g.nlat - 0.5) * g.dlat]
}

const mercY = (lat: number) => Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360))
const invMercY = (y: number) => (360 / Math.PI) * Math.atan(Math.exp(y)) - 90

/** The grid's pixels, one column a cell, top row north: rows spaced evenly in Web Mercator (how
 *  MapLibre stretches an image between its corners), each taking the cell its latitude falls in,
 *  `rowsPerCell` rows a cell so none is skipped. RGBA, not premultiplied. */
export function gridPixels(g: CoverageGrid, rowsPerCell = 2): { data: Uint8ClampedArray<ArrayBuffer>; width: number; height: number } {
  const width = g.nlon
  const height = Math.max(1, Math.min(8192, g.nlat * rowsPerCell))
  const data = new Uint8ClampedArray(new ArrayBuffer(width * height * 4))
  const [, south, , north] = gridEdges(g)
  const yN = mercY(north)
  const yS = mercY(south)
  const colours: Record<string, RGBA> = {}
  for (const [letter, st] of Object.entries(STATE_LETTER)) colours[letter] = [...STATE_RGB[st], GRID_ALPHA[st]]
  for (let r = 0; r < height; r++) {
    const lat = invMercY(yN + ((r + 0.5) / height) * (yS - yN))
    const i = Math.min(g.nlat - 1, Math.max(0, Math.floor((lat - south) / g.dlat)))
    for (let j = 0; j < width; j++) {
      const c = colours[g.state[i * g.nlon + j]] ?? colours['?']
      data.set(c, (r * width + j) * 4)
    }
  }
  return { data, width, height }
}

/** The grid as an image source's url and corners (top left, top right, bottom right, bottom
 *  left), or null without a document. */
export interface CoverageImage {
  url: string
  coordinates: [LngLat, LngLat, LngLat, LngLat]
  grid: CoverageGrid
}

export function gridImage(g: CoverageGrid): CoverageImage | null {
  if (typeof document === 'undefined' || g.nlat < 1 || g.nlon < 1) return null
  const { data, width, height } = gridPixels(g)
  const canvas = document.createElement('canvas')
  canvas.width = width
  canvas.height = height
  const ctx = canvas.getContext('2d')
  if (!ctx) return null
  ctx.putImageData(new ImageData(data, width, height), 0, 0)
  const [w, s, e, n] = gridEdges(g)
  return { url: canvas.toDataURL('image/png'), coordinates: [[w, n], [e, n], [e, s], [w, s]], grid: g }
}

/** The cell under a point, as the hover reads it; null outside the grid. */
export function cellAt(g: CoverageGrid, lon: number, lat: number): { state: LinkState | 'unknown'; reason: LinkReason | null; p_loss: number } | null {
  const i = Math.round((lat - g.lat0) / g.dlat)
  const j = Math.round((lon - g.lon0) / g.dlon)
  if (i < 0 || j < 0 || i >= g.nlat || j >= g.nlon) return null
  const k = i * g.nlon + j
  return { state: STATE_LETTER[g.state[k]] ?? 'unknown', reason: REASON_LETTER[g.reason[k]] ?? null, p_loss: g.p_loss[k] ?? 0 }
}

export function pct(p: number): string {
  if (p > 0 && p < 0.005) return '< 1 %'
  if (p < 1 && p > 0.995) return '> 99 %'
  return `${Math.round(p * 100)} %`
}

/** Why a cell is as it is: "behind terrain from the ground station · loss 100 %". */
export function cellDetail(c: NonNullable<ReturnType<typeof cellAt>>): string {
  if (c.state === 'unknown') return 'terrain not answered yet (counted open, as the emulator does)'
  return `${c.reason ? `${REASON_WORDS[c.reason]} · ` : ''}loss ${pct(c.p_loss)}`
}

// ---- the order's legs on the map ------------------------------------------------------------

/** The order the dry run was asked about, as the task panel sends it. */
export interface OrderShape {
  /** rules 21: an identify order's orbit is the engine's (none drawn from the request) */
  task: TaskKind | 'identify'
  center?: LngLat
  radius?: number
  route?: LngLat[]
}

/** Where a vehicle was, and its home, when the dry run was asked. */
export interface VehicleAt {
  position: LngLat | null
  home: LngLat | null
}

/** Each part of a vehicle's route as the decision engine lays it out (route.rs `route_for`):
 *  the transit from where it is to the task's first point, the task path (one lap, closed), the
 *  return from where the task ends (its first point, a loop) to home; an rtl's return from where
 *  it is; a hold or a track where it is. The orbit is drawn as the circle the console draws
 *  everywhere (its first point due north of the centre, as the engine's ring of 12 starts). */
export function legPaths(order: OrderShape, v: VehicleAt): Partial<Record<RoutePart, LngLat[]>> {
  const out: Partial<Record<RoutePart, LngLat[]>> = {}
  let task: LngLat[] | null = null
  if (order.task === 'orbit' && order.center && order.radius) task = circle(order.center, order.radius, 72)
  else if (order.task === 'patrol' && order.route && order.route.length >= 2) task = [...order.route, order.route[0]]
  if (task) {
    out.task = task
    if (v.position) out.transit = [v.position, task[0]]
    if (v.home) out.return = [task[0], v.home]
  } else if (order.task === 'rtl') {
    if (v.position && v.home) out.return = [v.position, v.home]
  } else if (v.position) out.here = [v.position]
  return out
}

/** The piece of `path` from `a` to `b` metres along it (`cum`: the distance to each point). */
export function slicePath(path: LngLat[], cum: number[], a: number, b: number): LngLat[] {
  const total = cum[cum.length - 1] ?? 0
  const at = (d: number): { k: number; p: LngLat } => {
    const x = Math.min(total, Math.max(0, d))
    let k = 1
    while (k < path.length - 1 && cum[k] < x) k++
    const span = cum[k] - cum[k - 1]
    const f = span > 0 ? (x - cum[k - 1]) / span : 0
    const p0 = path[k - 1]
    const p1 = path[k]
    return { k, p: [p0[0] + (p1[0] - p0[0]) * f, p0[1] + (p1[1] - p0[1]) * f] }
  }
  if (path.length < 2) return path.slice()
  const s = at(a)
  const e = at(b)
  const out: LngLat[] = [s.p]
  for (let k = s.k; k < e.k; k++) out.push(path[k])
  out.push(e.p)
  return out
}

export function cumulative(path: LngLat[]): number[] {
  const cum = [0]
  for (let k = 1; k < path.length; k++) cum.push(cum[k - 1] + distanceM(path[k - 1], path[k]))
  return cum
}

export interface RouteLine {
  path: LngLat[]
  state: LinkState
  part: RoutePart
  uid: number
}

export interface RouteMark {
  at: LngLat
  /** where the link first goes down on the way, or where the failsafe trips */
  kind: 'down' | 'trip'
  text: string
}

export interface RouteDot {
  at: LngLat
  state: LinkState
}

/** What MapView draws for an order's predicted link. */
export interface LinkRouteDrawing {
  lines: RouteLine[]
  marks: RouteMark[]
  dots: RouteDot[]
}

export const EMPTY_ROUTE: LinkRouteDrawing = { lines: [], marks: [], dots: [] }

function secs(s: number): string {
  return s >= 120 ? `${Math.round(s / 60)} min` : `${Math.max(1, Math.round(s))} s`
}

/** An order's legs coloured by the predicted link: each stretch at its share of the leg (the
 *  bridge measures along the engine's legs; the drawn ones can differ by a few per cent, an
 *  orbit's circle against the engine's ring of 12), with marks where the link first goes down on
 *  the way and where the failsafe trips. */
export function routeDrawing(answer: OrderLink | null, order: OrderShape, at: Map<number, VehicleAt>): LinkRouteDrawing {
  if (!answer?.vehicles?.length) return EMPTY_ROUTE
  const lines: RouteLine[] = []
  const marks: RouteMark[] = []
  const dots: RouteDot[] = []
  for (const v of answer.vehicles) {
    if (v.error || !v.legs) continue
    const paths = legPaths(order, at.get(v.uid) ?? { position: null, home: null })
    for (const leg of v.legs) {
      const path = paths[leg.part]
      if (!path?.length) continue
      if (leg.part === 'here' || path.length === 1) {
        const worst = leg.stretches.reduce<LinkState>((w, s) => (rank(s.state) > rank(w) ? s.state : w), 'up')
        dots.push({ at: path[0], state: worst })
        continue
      }
      const cum = cumulative(path)
      const scale = leg.length_m > 0 ? cum[cum.length - 1] / leg.length_m : 1
      for (const s of leg.stretches) {
        if (s.to_m <= s.from_m) continue
        lines.push({ path: slicePath(path, cum, s.from_m * scale, s.to_m * scale), state: s.state, part: leg.part, uid: v.uid })
      }
    }
    const name = v.callsign ?? `uid ${v.uid}`
    const fd = v.first_down
    const td = v.trip_down
    const same = fd && td && Math.abs(fd.at[0] - td.at[0]) < 1e-7 && Math.abs(fd.at[1] - td.at[1]) < 1e-7
    const down = (x: RouteDown) => `${name} · LINK DOWN ~${secs(x.seconds)}${x.reason ? ` (${shortReason(x.reason)})` : ''}`
    if (fd && !same) marks.push({ at: fd.at, kind: 'down', text: down(fd) })
    if (td) marks.push({ at: td.at, kind: 'trip', text: `${down(td)} · FAILSAFE TRIPS` })
  }
  return { lines, marks, dots }
}

const rank = (s: LinkState) => (s === 'down' ? 2 : s === 'degraded' ? 1 : 0)

export function shortReason(r: LinkReason): string {
  return r === 'no_sat' ? 'no satellite' : r === 'loss' ? 'own loss' : r
}
