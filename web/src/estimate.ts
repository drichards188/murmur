// Dead reckoning for silent vehicles (murmur-4y9), the console half: the bridge sends a motion
// model frozen at the vehicle's last report (types.ts `Estimate`); this evaluates it at any
// time, the same sums as the bridge's `estimate::eval` (flat earth around the vehicle), and
// turns the region into something deck.gl can draw and the details card can say.
import { ellipseRing } from './engage'
import { circle } from './geo'
import type { Estimate, FailsafeBranch, LngLat, Motion, Vehicle } from './types'
import { isStale } from './types'

export type Region =
  | { kind: 'circle'; center: LngLat; radius_m: number }
  | { kind: 'ellipse'; center: LngLat; semi_major_m: number; semi_minor_m: number; bearing_deg: number }
  /** a ring (radius +- width / 2) within `spread` degrees of `angle` (180: the whole ring) */
  | { kind: 'arc'; center: LngLat; radius_m: number; width_m: number; angle_deg: number; spread_deg: number }
  /** the whole route, this wide */
  | { kind: 'band'; route: LngLat[]; width_m: number }

export interface Evaluated {
  position: LngLat
  region: Region
}

const DEG = Math.PI / 180

function mPerDegLon(lat: number) {
  return 111_320 * Math.cos(lat * DEG)
}

/** (east, north) metres of p from origin */
function enu(origin: LngLat, p: LngLat): [number, number] {
  return [(p[0] - origin[0]) * mPerDegLon(origin[1]), (p[1] - origin[1]) * 111_320]
}

export function flatDistance(a: LngLat, b: LngLat) {
  const [e, n] = enu(a, b)
  return Math.hypot(e, n)
}

export function flatBearing(a: LngLat, b: LngLat) {
  const [e, n] = enu(a, b)
  return ((Math.atan2(e, n) / DEG) % 360 + 360) % 360
}

/** `dist` metres from p along `bearing` */
export function step(p: LngLat, bearingDeg: number, dist: number): LngLat {
  const a = bearingDeg * DEG
  return [p[0] + (dist * Math.sin(a)) / mPerDegLon(p[1]), p[1] + (dist * Math.cos(a)) / 111_320]
}

function segments(route: LngLat[]) {
  return route.map((p, i) => flatDistance(p, route[(i + 1) % route.length]))
}

function alongRoute(route: LngLat[], seg: number, frac: number): [LngLat, number] {
  const a = route[seg % route.length]
  const b = route[(seg + 1) % route.length]
  const bearing = flatBearing(a, b)
  return [step(a, bearing, frac * flatDistance(a, b)), bearing]
}

/** Where the model puts the vehicle `dt` seconds after its start, and the region around it,
 *  bounded by what the platform can reach (`reach x dt + s0`) and the cap. */
export function evalMotion(m: Motion, dtIn: number, reachMps: number, capM: number): Evaluated {
  const dt = Math.max(0, dtIn)
  const bound = (x: number, s0: number) => Math.min(x, capM, reachMps * dt + s0)
  switch (m.kind) {
    case 'ring': {
      const angle = (((m.angle0_deg + m.rate_dps * dt) % 360) + 360) % 360
      const position = step(m.center, angle, m.radius_m)
      const reachDeg = m.radius_m > 0 ? (reachMps * dt) / m.radius_m / DEG : 180
      const spread = Math.min(m.spread0_deg + m.spread_dps * dt, m.spread0_deg + reachDeg, 180)
      return { position, region: { kind: 'arc', center: m.center, radius_m: m.radius_m, width_m: m.width_m, angle_deg: angle, spread_deg: spread } }
    }
    case 'route': {
      const segs = segments(m.route)
      const loop = segs.reduce((a, b) => a + b, 0)
      let s = m.speed_mps * dt
      let seg = m.seg0
      let frac = m.frac0
      let position: LngLat | null = null
      let bearing = 0
      if (m.lead) {
        const leadM = flatDistance(m.lead, m.route[0])
        const b = flatBearing(m.lead, m.route[0])
        if (s < leadM) {
          position = step(m.lead, b, s)
          bearing = b
        } else {
          s -= leadM
          seg = 0
          frac = 0
        }
      }
      if (!position) {
        if (loop <= 0) position = m.route[0]
        else {
          let rem = (((s % loop) + loop) % loop) + frac * segs[seg % segs.length]
          let i = seg % segs.length
          for (;;) {
            const len = segs[i]
            if (rem <= len || len <= 0) {
              const [p, b] = alongRoute(m.route, i, len > 0 ? rem / len : 0)
              position = p
              bearing = b
              break
            }
            rem -= len
            i = (i + 1) % segs.length
          }
        }
      }
      const along = bound(m.along0_m + m.along_mps * dt, m.along0_m)
      const across = bound(m.across0_m + m.across_mps * dt, m.across0_m)
      if (!m.lead && loop > 0 && along >= loop / 2) return { position: position!, region: { kind: 'band', route: m.route, width_m: 2 * across } }
      return { position: position!, region: { kind: 'ellipse', center: position!, semi_major_m: along, semi_minor_m: across, bearing_deg: bearing } }
    }
    case 'line': {
      let d = m.speed_mps * dt
      let arrived = false
      if (m.stop_m != null && d >= m.stop_m) {
        d = m.stop_m
        arrived = true
      }
      const position = step(m.from, m.bearing_deg, d)
      const along = bound(m.along0_m + m.along_mps * dt, m.along0_m)
      const across = bound(m.across0_m + m.across_mps * dt, m.across0_m)
      const a = m.arrive
      if (a && arrived && a.ring) {
        return { position, region: { kind: 'arc', center: a.center, radius_m: a.radius_m, width_m: a.width_m, angle_deg: flatBearing(a.center, position), spread_deg: 180 } }
      }
      if (a && arrived) return { position, region: { kind: 'circle', center: a.center, radius_m: a.radius_m + m.along0_m } }
      return { position, region: { kind: 'ellipse', center: position, semi_major_m: along, semi_minor_m: across, bearing_deg: m.bearing_deg } }
    }
    case 'point':
      return { position: m.at, region: { kind: 'circle', center: m.at, radius_m: bound(m.radius0_m + m.rate_mps * dt, m.radius0_m) } }
  }
}

/** The estimate now (bridge clock, ms). */
export function estimateAt(e: Estimate, nowMs: number): Evaluated {
  return evalMotion(e.motion, nowMs / 1000 - e.t0, e.reach_mps, e.cap_m)
}

/** The lost-link branch now, once its timer has fired; null before. */
export function failsafeAt(e: Estimate, nowMs: number): Evaluated | null {
  const f = e.failsafe
  if (!f || nowMs / 1000 < f.fires_t) return null
  return evalMotion(f.motion, nowMs / 1000 - f.fires_t, e.reach_mps, e.cap_m)
}

/** Vehicles the map should draw an estimate for: silent, with a model from the bridge. */
export function estimated(vehicles: Vehicle[]): Vehicle[] {
  return vehicles.filter((v) => v.estimate && v.position && isStale(v))
}

/** The region as polygon rings for a PolygonLayer (outer ring, then holes); a band draws as a
 *  path instead (see `bandPath`). */
export function regionRings(r: Region): LngLat[][] {
  switch (r.kind) {
    case 'circle':
      return [circle(r.center, Math.max(r.radius_m, 1), 48)]
    case 'ellipse':
      return [ellipseRing({ center: r.center, semi_major_m: Math.max(r.semi_major_m, 1), semi_minor_m: Math.max(r.semi_minor_m, 1), bearing_deg: r.bearing_deg }, 48)]
    case 'arc': {
      const outer = r.radius_m + r.width_m / 2
      const inner = Math.max(0, r.radius_m - r.width_m / 2)
      if (r.spread_deg >= 180) {
        const o = circle(r.center, outer, 72)
        return inner > 0 ? [o, circle(r.center, inner, 72)] : [o]
      }
      const n = Math.max(8, Math.ceil(r.spread_deg / 4))
      const from = r.angle_deg - r.spread_deg
      const span = 2 * r.spread_deg
      const ring: LngLat[] = []
      for (let k = 0; k <= n; k++) ring.push(step(r.center, from + (span * k) / n, outer))
      if (inner > 0) for (let k = n; k >= 0; k--) ring.push(step(r.center, from + (span * k) / n, inner))
      else ring.push(r.center)
      ring.push(ring[0])
      return [ring]
    }
    case 'band':
      return []
  }
}

export function bandPath(r: Region): LngLat[] | null {
  return r.kind === 'band' ? [...r.route, r.route[0]] : null
}

function fmtM(m: number) {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m)} m`
}

/** One phrase for the details card. */
export function regionText(r: Region): string {
  switch (r.kind) {
    case 'circle':
      return `within ${fmtM(r.radius_m)}`
    case 'ellipse':
      return `±${fmtM(r.semi_major_m)} along its course, ±${fmtM(r.semi_minor_m)} across`
    case 'arc':
      return r.spread_deg >= 180
        ? `anywhere on its ${fmtM(r.radius_m)} ring (±${fmtM(r.width_m / 2)})`
        : `on its ${fmtM(r.radius_m)} ring within ±${Math.round(r.spread_deg)}° of the estimate (±${fmtM(r.width_m / 2)})`
    case 'band':
      return `anywhere along its route (±${fmtM(r.width_m / 2)})`
  }
}

export const BASIS_LABEL: Record<Estimate['basis'], string> = {
  orbit: 'on its orbit',
  patrol: 'on its patrol route',
  hold: 'holding',
  rtl: 'returning home',
  velocity: 'dead reckoned',
  stationary: 'stationary',
}

export const FAILSAFE_LABEL: Record<FailsafeBranch['action'], string> = {
  rtl: 'RTL',
  surface: 'SURFACING',
  land: 'LANDING',
  disarm: 'DISARMED',
}
