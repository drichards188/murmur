// The weather field as the sampler answers it (bd murmur-vr17; docs/WEATHER.md, first paragraph).
// Pure: no DOM, no map. weatherMap.ts draws it.
//
// The bridge's samplers (crates/murmur-weather) give a position the values of its NEAREST
// archived model point (HRRR through Open-Meteo, a few kilometres apart) and interpolate only in
// time; a scripted value (Fog watch) is overlaid on every point alike. `/api/weather/grid` sends
// those points and their values now (`regions`), so every effect the map draws (the colour
// field, the fog, the cloud shadows, the rain, the wind's streaks) takes each region's own
// values with the region's true edges: the conditions the sensing model senses there, no
// smoother and no sharper. A gradient between points would show conditions the simulation does
// not use.
import type { Bounds } from './viewport'
import { WEATHER_FIELDS, weatherValue, type WeatherField, type WeatherGrid, type WeatherValues } from './weather.ts'

export interface RegionPoint {
  lat: number
  lon: number
  values: WeatherValues
}
/** `Store::regions` (murmur-weather): a position inside `bounds` takes its nearest point's
 *  values, any other position `outside`'s; without an archive `bounds` is null and `outside`
 *  holds everywhere. */
export interface WeatherRegions {
  /** the fields a scripted timeline overrides now (names only; each the same everywhere) */
  scripted?: string[]
  bounds: Bounds | null
  points: RegionPoint[]
  outside: WeatherValues
}

export type RGBA = [number, number, number, number]

/** the most regions the map's shader takes (an archive has 20: 5 by 4) */
export const MAX_REGIONS = 40

/** The grid's regions, or null when there is nothing current to draw (none sent, stale, or more
 *  regions than the map can draw). */
export function regionsOf(g: WeatherGrid | null): WeatherRegions | null {
  if (!g || !g.available || g.stale || !g.regions) return null
  return g.regions.points.length <= MAX_REGIONS ? g.regions : null
}

const FIELD_KEYS: Record<WeatherField, string[]> = {
  visibility: ['visibility_m'],
  wind: ['wind_east_mps', 'wind_north_mps'],
  cloud: ['cloud_cover_pct'],
  precipitation: ['precipitation_mm'],
}

/** Where a field's values come from now: the scenario's script (the same everywhere), the
 *  archive (each model point its own), or nowhere (none known in any region). */
export function fieldSource(r: WeatherRegions, field: WeatherField): 'scripted' | 'archived' | 'none' {
  if (FIELD_KEYS[field].some((k) => r.scripted?.includes(k))) return 'scripted'
  const known = [r.outside, ...r.points.map((p) => p.values)].some((v) => weatherValue(v, field) != null)
  return known ? 'archived' : 'none'
}

/** A field's lowest and highest value over the regions (null where none is known). */
export function fieldRange(r: WeatherRegions, field: WeatherField): [number, number] | null {
  const vs = [...r.points.map((p) => p.values), ...(r.points.length ? [] : [r.outside])].map((v) => weatherValue(v, field)).filter((v): v is number => v != null)
  return vs.length ? [Math.min(...vs), Math.max(...vs)] : null
}

const RAD = Math.PI / 180

/** The index of the point whose values a position takes, or -1 for `outside`: the sampler's
 *  own rule (murmur-weather `Archive::at`), the same distance and the first point on a tie. */
export function nearestRegion(r: WeatherRegions, lon: number, lat: number): number {
  if (!r.bounds || !r.points.length) return -1
  const [w, s, e, n] = r.bounds
  if (lat < s || lat > n || lon < w || lon > e) return -1
  return nearestIn(r.points, lon, lat, Math.cos(lat * RAD))
}

function nearestIn(points: RegionPoint[], lon: number, lat: number, c: number): number {
  let best = 0, bestD = Infinity
  for (let i = 0; i < points.length; i++) {
    const p = points[i]
    const dy = p.lat - lat, dx = (p.lon - lon) * c
    const d = dy * dy + dx * dx
    if (d < bestD) {
      bestD = d
      best = i
    }
  }
  return best
}

export function valuesAt(r: WeatherRegions, lon: number, lat: number): WeatherValues {
  const i = nearestRegion(r, lon, lat)
  return i < 0 ? r.outside : r.points[i].values
}

/** The same regions with values part way from `a` to `b` (0 a, 1 b): for an animated change
 *  between two polls, which the sampler itself makes linearly in time. Only when both hold the
 *  same points; a value missing on either side takes `b`'s. */
export function blendRegions(a: WeatherRegions | null, b: WeatherRegions, f: number): WeatherRegions {
  if (!a || f >= 1 || a.points.length !== b.points.length || a.points.some((p, i) => p.lat !== b.points[i].lat || p.lon !== b.points[i].lon)) return b
  const k = Math.max(0, f)
  return { scripted: b.scripted, bounds: b.bounds, points: b.points.map((p, i) => ({ lat: p.lat, lon: p.lon, values: mixValues(a.points[i].values, p.values, k) })), outside: mixValues(a.outside, b.outside, k) }
}

function mixValues(a: WeatherValues, b: WeatherValues, f: number): WeatherValues {
  const m = (x: number | null, y: number | null) => (x == null || y == null ? y : x + (y - x) * f)
  return {
    visibility_m: m(a.visibility_m, b.visibility_m),
    wind_east_mps: m(a.wind_east_mps, b.wind_east_mps),
    wind_north_mps: m(a.wind_north_mps, b.wind_north_mps),
    cloud_cover_pct: m(a.cloud_cover_pct, b.cloud_cover_pct),
    precipitation_mm: m(a.precipitation_mm, b.precipitation_mm),
  }
}

// ---- colour ramps: one continuous ramp per field, alpha in 0..1 ---------------------------------

interface Ramp {
  /** visibility spans 50 m to 20 km: its stops are spaced by the logarithm */
  log: boolean
  stops: [number, RGBA][]
}
/** Visibility is a pale veil, thicker as it falls (fog looks like fog; clear air nearly
 *  nothing); the others a sequential ramp, transparent where there is nothing to say. Kept light
 *  enough that vehicles, tracks, labels and the reach rings (all drawn above) stay legible. */
export const RAMPS: Record<WeatherField, Ramp> = {
  visibility: { log: true, stops: [[50, [230, 236, 243, 0.55]], [200, [220, 228, 239, 0.47]], [1000, [196, 206, 226, 0.36]], [5000, [140, 170, 214, 0.17]], [20000, [90, 150, 200, 0.04]]] },
  wind: { log: false, stops: [[0, [60, 110, 170, 0.04]], [4, [56, 150, 205, 0.16]], [8, [70, 192, 172, 0.24]], [12, [212, 206, 92, 0.3]], [16, [240, 150, 62, 0.36]], [20, [236, 82, 92, 0.42]]] },
  cloud: { log: false, stops: [[0, [200, 212, 228, 0]], [50, [206, 217, 233, 0.2]], [100, [216, 226, 240, 0.42]]] },
  precipitation: { log: false, stops: [[0, [90, 150, 240, 0]], [0.5, [80, 150, 240, 0.14]], [2, [92, 120, 236, 0.26]], [5, [142, 96, 226, 0.36]], [10, [192, 82, 212, 0.44]]] },
}
/** a place no value is known: a faint grey (the map hatches it faintly too) */
export const MISSING: RGBA = [120, 133, 150, 0.04]

function rampPos(ramp: Ramp, v: number): number {
  const lo = ramp.stops[0][0], hi = ramp.stops[ramp.stops.length - 1][0]
  const x = ramp.log ? Math.log(Math.max(v, 1e-9) / lo) / Math.log(hi / lo) : (v - lo) / (hi - lo)
  return Math.max(0, Math.min(1, x))
}

/** The ramp's colour at a value; MISSING for none. */
export function rampColor(field: WeatherField, value: number | null): RGBA {
  if (value == null || !Number.isFinite(value)) return MISSING
  const { stops } = RAMPS[field]
  const ramp = RAMPS[field]
  const x = rampPos(ramp, value)
  for (let i = 1; i < stops.length; i++) {
    const x1 = rampPos(ramp, stops[i][0])
    if (x <= x1 || i === stops.length - 1) {
      const x0 = rampPos(ramp, stops[i - 1][0])
      const f = x1 > x0 ? Math.max(0, Math.min(1, (x - x0) / (x1 - x0))) : 1
      const a = stops[i - 1][1], b = stops[i][1]
      return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, a[3] + (b[3] - a[3]) * f]
    }
  }
  return stops[stops.length - 1][1]
}

/** The legend's gradient bar, left to right as the ramp runs. */
export function rampCss(field: WeatherField): string {
  const ramp = RAMPS[field]
  const parts = ramp.stops.map(([v, c]) => `rgba(${c[0]},${c[1]},${c[2]},${c[3]}) ${(rampPos(ramp, v) * 100).toFixed(1)}%`)
  return `linear-gradient(to right, ${parts.join(', ')})`
}

/** The legend's ticks: each tick value at its place on the bar (0..1). */
export function rampTicks(field: WeatherField): { value: number; label: string; at: number }[] {
  return WEATHER_FIELDS[field].ticks.map((value) => ({ value, label: value >= 1000 ? `${value / 1000}k` : `${value}`, at: rampPos(RAMPS[field], value) }))
}

/** A field's values now in a few words for the legend: the range over the regions and where it
 *  comes from (`fieldSource`). "scripted" is true per field: only a field the timeline overrides
 *  now is called scripted, and that one is the same everywhere. */
export function describeField(r: WeatherRegions, field: WeatherField): string {
  const source = fieldSource(r, field)
  if (source === 'none') return 'no data'
  const range = fieldRange(r, field)
  const fmt = (v: number) => field === 'visibility' ? (v >= 1000 ? `${+(v / 1000).toFixed(v >= 10_000 ? 0 : 1)} km` : `${Math.round(v)} m`)
    : field === 'wind' ? v.toFixed(1)
    : field === 'cloud' ? `${Math.round(v)}`
    : v.toFixed(1)
  const unit = field === 'wind' ? ' m/s' : field === 'cloud' ? '%' : field === 'precipitation' ? ' mm' : ''
  const value = !range ? 'none known' : field === 'precipitation' && range[1] <= 0 ? 'none'
    : fmt(range[0]) === fmt(range[1]) ? `${fmt(range[0])}${unit}` : `${fmt(range[0])} to ${fmt(range[1])}${unit}`
  return source === 'scripted' ? `${value}, scripted, the same everywhere` : value
}

// ---- haze: the 3D view's horizon -----------------------------------------------------------------

const CLEAR_M = 20_000
const FOG_M = 50

/** How foggy the view's centre is, 0 (20 km or more, or unknown) to 1 (50 m or less), on the
 *  logarithm of the visibility, as the visibility ramp. */
export function hazeStrength(visibility_m: number | null): number {
  if (visibility_m == null || !Number.isFinite(visibility_m)) return 0
  return Math.max(0, Math.min(1, 1 - Math.log(Math.max(visibility_m, 1) / FOG_M) / Math.log(CLEAR_M / FOG_M)))
}

/** MapLibre's `fog-ground-blend` for a visibility: where on the depth scale the fog starts. Its
 *  depth runs from the near plane (`nearPx`, the camera's distance to the sea at the view's
 *  centre, pixels) to the far plane, as a perspective depth: about 1 - near/d. The fog starts
 *  `visibility_m` beyond the view's centre (`pxPerM` pixels a metre there) and thickens to the
 *  horizon. An impression of how far one sees, not to scale: MapLibre's fog grows with depth as
 *  it likes, and only from a pitch of 60 degrees. */
export function fogGroundBlend(visibility_m: number, nearPx: number, pxPerM: number): number {
  if (!(nearPx > 0) || !(pxPerM > 0) || !Number.isFinite(visibility_m)) return 1
  return Math.max(0, Math.min(0.98, 1 - nearPx / (nearPx + Math.max(0, visibility_m) * pxPerM)))
}

/** A CSS colour part way from `a` to `b`. */
export function mixCss(a: RGBA, b: RGBA, f: number): string {
  const k = Math.max(0, Math.min(1, f))
  const c = a.map((x, i) => x + (b[i] - x) * k)
  return `rgba(${Math.round(c[0])},${Math.round(c[1])},${Math.round(c[2])},${c[3].toFixed(3)})`
}
