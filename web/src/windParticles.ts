// Wind streaks for the weather layer (bd murmur-vr17): short-lived particles carried by the surface
// wind the sampler gives where each one is (weatherField.ts: its nearest model point's, so a
// streak turns where it crosses into another point's region, and nowhere else). Pure: positions
// in, segments out; weatherMap.ts draws them with deck.gl and runs the frames.
//
// Speed is exaggerated so that motion reads at map scale: a streak moves PX_PER_MPS screen pixels
// a second for each m/s of wind at the view's centre's scale. Direction is the wind's (toward
// where it blows); length and pace grow with its speed.
import type { LngLat } from './types'
import { nearestRegion, type WeatherRegions } from './weatherField.ts'

/** screen pixels a second per m/s of wind (5 m/s: 20 px/s): slow beside the symbols, still showing which way it blows */
export const PX_PER_MPS = 4
/** a particle's trail: a point every TRAIL_EVERY_S, the last TRAIL_POINTS kept */
export const TRAIL_EVERY_S = 0.07
export const TRAIL_POINTS = 10
const FADE_IN_S = 0.4
const FADE_OUT_S = 0.7
const M_PER_DEG = 111_320

export interface Particle {
  lon: number
  lat: number
  z: number
  age: number
  life: number
  /** recorded points, oldest first: lon, lat, z for each */
  trail: number[]
  sinceTrail: number
}

export interface WindStep {
  regions: WeatherRegions
  /** metres a screen pixel at the view's centre */
  mPerPx: number
  /** how many particles to keep */
  count: number
  /** a new particle's place (somewhere in view), or null for none now */
  spawn: () => LngLat | null
  /** still worth drawing (in view) */
  inView: (lon: number, lat: number) => boolean
  /** the surface's height to draw at, rendered metres */
  zOf: (lon: number, lat: number) => number
  rng: () => number
}

/** The wind toward east and north (m/s) where a position takes its values, or null (none
 *  known there, or calm). */
export function windAt(r: WeatherRegions, lon: number, lat: number): [number, number] | null {
  const i = nearestRegion(r, lon, lat)
  const v = i < 0 ? r.outside : r.points[i].values
  if (v.wind_east_mps == null || v.wind_north_mps == null) return null
  if (Math.hypot(v.wind_east_mps, v.wind_north_mps) < 0.05) return null
  return [v.wind_east_mps, v.wind_north_mps]
}

/** How many particles a view of `w` by `h` CSS pixels gets: one per 3,600 px², 60 to 450. */
export function particleCount(w: number, h: number): number {
  return Math.max(60, Math.min(450, Math.round((w * h) / 3600)))
}

function born(at: LngLat, s: WindStep): Particle {
  const z = s.zOf(at[0], at[1])
  // staggered ages, so a fresh field does not pulse in step
  return { lon: at[0], lat: at[1], z, age: 0, life: 2.4 + 2.2 * s.rng(), trail: [at[0], at[1], z], sinceTrail: 0 }
}

/** Advances every particle by `dt` seconds (capped at 0.1: a stalled tab does not fling them),
 *  retires the old, the stalled and those out of view, and keeps `count` alive. */
export function stepWind(ps: Particle[], dt: number, s: WindStep): Particle[] {
  const step = Math.max(0, Math.min(0.1, dt))
  const metres = PX_PER_MPS * s.mPerPx * step
  const out: Particle[] = []
  for (const p of ps) {
    if (out.length >= s.count) break
    p.age += step
    const v = windAt(s.regions, p.lon, p.lat)
    if (p.age >= p.life || !v || !s.inView(p.lon, p.lat)) {
      const at = s.spawn()
      if (at && windAt(s.regions, at[0], at[1])) out.push(born(at, s))
      continue
    }
    p.lat += (v[1] * metres) / M_PER_DEG
    p.lon += (v[0] * metres) / (M_PER_DEG * Math.cos((p.lat * Math.PI) / 180))
    p.z = s.zOf(p.lon, p.lat)
    p.sinceTrail += step
    if (p.sinceTrail >= TRAIL_EVERY_S) {
      p.sinceTrail = 0
      p.trail.push(p.lon, p.lat, p.z)
      if (p.trail.length > TRAIL_POINTS * 3) p.trail.splice(0, p.trail.length - TRAIL_POINTS * 3)
    }
    out.push(p)
  }
  // top up: a few a step, so a new field fills in over a second or so instead of all at once
  for (let n = 0; out.length < s.count && n < Math.max(8, s.count / 20); n++) {
    const at = s.spawn()
    if (!at || !windAt(s.regions, at[0], at[1])) continue
    const p = born(at, s)
    p.age = s.rng() * p.life * 0.5
    out.push(p)
  }
  return out
}

/** A particle's opacity now: fading in when born and out before it is retired. */
export function particleAlpha(p: Particle): number {
  return Math.max(0, Math.min(1, p.age / FADE_IN_S, (p.life - p.age) / FADE_OUT_S))
}

export interface Segments {
  length: number
  /** lon, lat, z for each segment's two ends */
  from: Float64Array
  to: Float64Array
  /** RGBA bytes */
  color: Uint8Array
}

/** The particles' trails as line segments, each fading toward its tail: the head's segment at
 *  the particle's opacity, the oldest near none. */
export function windSegments(ps: Particle[], rgb: [number, number, number], maxAlpha: number): Segments {
  let n = 0
  for (const p of ps) n += p.trail.length / 3
  const from = new Float64Array(n * 3), to = new Float64Array(n * 3), color = new Uint8Array(n * 4)
  let k = 0
  for (const p of ps) {
    const a = particleAlpha(p) * maxAlpha
    const t = p.trail
    const m = t.length / 3
    for (let j = 0; j < m; j++) {
      // segment j runs from trail point j to the next one, the last to the head
      const nx = j + 1 < m ? t[3 * j + 3] : p.lon
      const ny = j + 1 < m ? t[3 * j + 4] : p.lat
      const nz = j + 1 < m ? t[3 * j + 5] : p.z
      from[3 * k] = t[3 * j]
      from[3 * k + 1] = t[3 * j + 1]
      from[3 * k + 2] = t[3 * j + 2]
      to[3 * k] = nx
      to[3 * k + 1] = ny
      to[3 * k + 2] = nz
      color[4 * k] = rgb[0]
      color[4 * k + 1] = rgb[1]
      color[4 * k + 2] = rgb[2]
      color[4 * k + 3] = Math.round(255 * a * ((j + 1) / m) ** 1.5)
      k++
    }
  }
  return { length: k, from, to, color }
}

/** Static arrows instead of streaks (prefers-reduced-motion): one at each region's point in
 *  view, toward where the wind blows, its length growing with the speed (14 to 44 px). Returns
 *  each arrow's shaft and head as paths of [lon, lat]. */
export function windArrows(r: WeatherRegions, inView: (lon: number, lat: number) => boolean, mPerPx: number, center: LngLat): LngLat[][] {
  const places: LngLat[] = r.points.length ? r.points.map((p) => [p.lon, p.lat]) : [center]
  const out: LngLat[][] = []
  for (const [lon, lat] of places) {
    if (!inView(lon, lat)) continue
    const v = windAt(r, lon, lat)
    if (!v) continue
    const speed = Math.hypot(v[0], v[1])
    const px = Math.max(14, Math.min(44, 14 + speed * 3))
    const ux = v[0] / speed, uy = v[1] / speed
    const m = px * mPerPx
    const k = Math.cos((lat * Math.PI) / 180)
    const at = (east: number, north: number): LngLat => [lon + east / (M_PER_DEG * k), lat + north / M_PER_DEG]
    const tail = at((-ux * m) / 2, (-uy * m) / 2), head = at((ux * m) / 2, (uy * m) / 2)
    const h = 0.35 * m
    // the head's two barbs, 30 degrees off the shaft
    const c = Math.cos(Math.PI / 6), sn = Math.sin(Math.PI / 6)
    const b1 = at((ux * m) / 2 - h * (ux * c - uy * sn), (uy * m) / 2 - h * (uy * c + ux * sn))
    const b2 = at((ux * m) / 2 - h * (ux * c + uy * sn), (uy * m) / 2 - h * (uy * c - ux * sn))
    out.push([tail, head], [b1, head, b2])
  }
  return out
}
