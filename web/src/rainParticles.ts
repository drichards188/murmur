// Rain for the weather layer (bd murmur-vr17): drops that fall where the sampler gives a
// precipitation above zero (weatherField.ts: each model point's region), as many as its
// intensity asks, slanted by that region's surface wind. Pure: weatherMap.ts draws them.
//
// Drawn to be seen, not to scale: a drop falls from about 110 screen pixels' worth of height in
// under a second and drifts with the wind at the streaks' pace. Precipitation is information
// only: the sensing model does not read it.
import type { LngLat } from './types'
import { nearestRegion, type WeatherRegions } from './weatherField.ts'
import { PX_PER_MPS } from './windParticles.ts'

const M_PER_DEG = 111_320
/** the height a drop falls from, in screen pixels at the view's centre's scale */
const FALL_PX = 110
const FALL_S = 0.85
/** the tail behind a drop, in seconds of its motion */
const TAIL_S = 0.06

export interface Drop {
  lon: number
  lat: number
  /** the surface under it, and its height above that (rendered metres) */
  ground: number
  above: number
  /** its drift toward east and north (m/s, the region's wind) */
  east: number
  north: number
}

/** How strongly it rains in a region, 0 to 1: a share of the most drops, from the hour's total
 *  (mm): 0 for none or unknown, 1 from 8 mm. */
export function rainWeight(precipitation_mm: number | null): number {
  if (precipitation_mm == null || !Number.isFinite(precipitation_mm) || precipitation_mm <= 0) return 0
  return Math.min(1, precipitation_mm / 8) ** 0.6
}

export function rainAt(r: WeatherRegions, lon: number, lat: number): { weight: number; east: number; north: number } {
  const i = nearestRegion(r, lon, lat)
  const v = i < 0 ? r.outside : r.points[i].values
  return { weight: rainWeight(v.precipitation_mm), east: v.wind_east_mps ?? 0, north: v.wind_north_mps ?? 0 }
}

export interface RainStep {
  regions: WeatherRegions
  mPerPx: number
  /** the most drops at once (where it rains hardest everywhere in view) */
  max: number
  spawn: () => LngLat | null
  inView: (lon: number, lat: number) => boolean
  zOf: (lon: number, lat: number) => number
  rng: () => number
}

/** Advances the drops by `dt` (capped at 0.1 s), retires those that land or leave the view, and
 *  starts new ones: each place is tried at the same rate and kept by its region's weight, so the
 *  drops in view settle at `max` times the weight where they fall. */
export function stepRain(ds: Drop[], dt: number, s: RainStep): Drop[] {
  const step = Math.max(0, Math.min(0.1, dt))
  const height = FALL_PX * s.mPerPx
  const fall = (height / FALL_S) * step
  const drift = PX_PER_MPS * s.mPerPx * step
  const out: Drop[] = []
  for (const d of ds) {
    d.above -= fall
    d.lat += (d.north * drift) / M_PER_DEG
    d.lon += (d.east * drift) / (M_PER_DEG * Math.cos((d.lat * Math.PI) / 180))
    if (d.above > 0 && s.inView(d.lon, d.lat)) out.push(d)
  }
  // a drop lives FALL_S: max / FALL_S tries a second keeps max where every try is kept
  let tries = (s.max / FALL_S) * step
  while (tries > 0 && out.length < s.max) {
    if (tries < 1 && s.rng() > tries) break
    tries -= 1
    const at = s.spawn()
    if (!at) continue
    const r = rainAt(s.regions, at[0], at[1])
    if (r.weight <= 0 || s.rng() > r.weight) continue
    out.push({ lon: at[0], lat: at[1], ground: s.zOf(at[0], at[1]), above: height * (0.3 + 0.7 * s.rng()), east: r.east, north: r.north })
  }
  return out
}

export interface DropSegments {
  length: number
  from: Float64Array
  to: Float64Array
}

/** Each drop as a short line from where it was TAIL_S ago to where it is: slanted by its drift,
 *  upright in a tilted view, a dash in the wind's direction from above. */
export function rainSegments(ds: Drop[], mPerPx: number): DropSegments {
  const n = ds.length
  const from = new Float64Array(n * 3), to = new Float64Array(n * 3)
  const height = FALL_PX * mPerPx
  const back = PX_PER_MPS * mPerPx * TAIL_S
  // a dash at least this long from above even when calm (a dot is lost under the fog)
  const minDash = 3 * mPerPx
  ds.forEach((d, i) => {
    const speed = Math.hypot(d.east, d.north)
    const ex = speed > 0.05 ? d.east / speed : 0.6, ny = speed > 0.05 ? d.north / speed : -0.8
    const dashM = Math.max(minDash, speed * back)
    const k = Math.cos((d.lat * Math.PI) / 180)
    to[3 * i] = d.lon
    to[3 * i + 1] = d.lat
    to[3 * i + 2] = d.ground + d.above
    from[3 * i] = d.lon - (ex * dashM) / (M_PER_DEG * k)
    from[3 * i + 1] = d.lat - (ny * dashM) / M_PER_DEG
    from[3 * i + 2] = d.ground + d.above + (height / FALL_S) * TAIL_S * 2
  })
  return { length: n, from, to }
}
