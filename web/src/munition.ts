// Munitions in flight (bd murmur-64z): a standoff platform's munition from its release to its
// impact, as the bridge models it. The model is the one the engine, the truth side and the
// console share: a straight slant line from the platform (its position and altitude at release)
// to the effect point at constant speed, so the position at a time is linear in longitude,
// latitude and altitude between the two ends. A missile of the catalog's "dive" profile (the
// drone-launcher UGV's rounds, bd murmur-4nik.5) is drawn on a path of its own over the same ends
// and times (divePath): only the drawing, never what the engine or the truth side read. Pure
// functions, no DOM and no framework, so they can be checked on their own (munition.check.ts);
// munitionLayers.ts draws them.
import type { LngLat } from './types.ts'

/** `engagement.munition`, present from the release on: times in engine ms (the clock of
 *  `engagement.effect.t`, which stays the release), points [lon, lat], altitudes metres above
 *  mean sea level (the impact's is the surface's, or the target's). A platform spent by its own
 *  effect (Switchblade-class) is its own munition and has none: its strike is at the release. */
export interface Munition {
  name: string
  speed_mps: number
  launch_t: number
  launch_point: LngLat
  launch_alt_m: number
  impact_t: number
  impact_point: LngLat
  impact_alt_m: number
  tof_s: number
}

/** [lon, lat, metres above mean sea level] */
export type Pt3 = [number, number, number]

/** How far along its flight it is at `t`: 0 at the launch, 1 at the impact (held at either end). */
export function flightFraction(m: Munition, t: number): number {
  const span = m.impact_t - m.launch_t
  if (!(span > 0)) return t >= m.impact_t ? 1 : 0
  return Math.min(1, Math.max(0, (t - m.launch_t) / span))
}

/** the point a fraction `f` of the way from the launch to the impact */
export function pointAlong(m: Munition, f: number): Pt3 {
  const [a, b] = [m.launch_point, m.impact_point]
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, m.launch_alt_m + (m.impact_alt_m - m.launch_alt_m) * f]
}

/** Where it is at `t`: at its launch point before the launch, at the impact point after. On a
 *  shaped path (`path`, divePath) where that path puts it at that time. */
export function munitionAt(m: Munition, t: number, path?: FlightPath): Pt3 {
  return path ? path.point(path.flown(flightFraction(m, t))) : pointAlong(m, flightFraction(m, t))
}

// ---- the path it is drawn on (bd murmur-4nik.5) ----

/** The ground under a point, metres above mean sea level (the water's surface, 0, over the sea),
 *  as the console's terrain gives it; null where it has none. */
export type GroundAt = (lon: number, lat: number) => number | null

/** The path a munition is drawn on from its launch to its impact, and how it is flown along it:
 *  the same two ends and the same two times whatever its shape. */
export interface FlightPath {
  /** a profile's own shape, not the straight slant line */
  shaped: boolean
  /** its length, m */
  length_m: number
  /** the point a fraction `u` of its length from the launch (held at either end) */
  point(u: number): Pt3
  /** the fraction of its length flown a fraction `f` of the flight's time from the launch
   *  (flightFraction): 0 at 0, 1 at 1, never going back */
  flown(f: number): number
}

const M_PER_DEG = 111_320
const clamp01 = (x: number) => Math.min(1, Math.max(0, x))

/** metres east and north of `a` to `b`, on a flat earth about `a` (a few km: plenty) */
function enu(a: LngLat, b: LngLat): [number, number] {
  return [(b[0] - a[0]) * M_PER_DEG * Math.cos((a[1] * Math.PI) / 180), (b[1] - a[1]) * M_PER_DEG]
}

/** The straight slant line at constant speed, the model's own: its point a fraction of the way
 *  is its point a fraction of the time. */
export function straightPath(m: Munition): FlightPath {
  const [e, n] = enu(m.launch_point, m.impact_point)
  return {
    shaped: false,
    length_m: Math.hypot(e, n, m.impact_alt_m - m.launch_alt_m),
    point: (u) => pointAlong(m, clamp01(u)),
    flown: clamp01,
  }
}

// A drone's flight from its tube to a dive onto the point, at a game's fidelity: rounded figures,
// every one an estimate [E] for the look of it (docs/research/hiv3.md 3.1 for the speeds), none
// a real system's.
/** it leaves the tube straight up this far, m [E] */
export const DIVE_POP_M = 15
/** at this speed, m/s, so the pop-up takes about 1.5 s [E] */
export const DIVE_POP_MPS = 10
/** then speeds up to its cruise over this much of its path, m (some 4 s) [E] */
const DIVE_ACCEL_M = 60
/** and climbs at this gradient (about 31 degrees), easing out into the cruise [E] */
const DIVE_CLIMB_GRADE = 0.6
/** its cruise height above the higher of the two ends, m [E] */
export const DIVE_CRUISE_ABOVE_M = 60
/** and at least this far above the terrain along the way, where the console has it, m [E] */
export const DIVE_CLEAR_M = 40
/** what the climb and the dive keep above the terrain (bar the last metres onto the point), m */
export const DIVE_FLOOR_M = 10
/** its cruise speed, the catalog's 20 m/s (hiv3.md 3.1 [E]) */
export const DIVE_CRUISE_MPS = 20
/** its speed onto the point at the end of the dive, m/s (hiv3.md 3.1's dash [E]) */
export const DIVE_END_MPS = 35
/** the dive's gradient at the point (45 degrees), which sets where it pushes over [E] */
const DIVE_END_GRADE = 1
/** the dive covers the last few hundred metres of ground, within these bounds [E] */
export const DIVE_MIN_M = 150
export const DIVE_MAX_M = 500
/** the ground distance within which the dive only stays above the ground, onto the point, m */
export const DIVE_END_M = 30

/** A drone's path from its launch to its impact, as the console draws a "dive" round: straight up
 *  out of the tube (DIVE_POP_M), a climb easing out into a cruise DIVE_CRUISE_ABOVE_M above the
 *  higher end and DIVE_CLEAR_M above the terrain `ground` gives along the way, then a dive over
 *  the last few hundred metres that steepens to 45 degrees onto the point. Along it the speed
 *  rises from the pop-up's to the cruise's on the climb and from the cruise's to DIVE_END_MPS in
 *  the dive, the whole profile scaled so it arrives at `impact_t` exactly: the ends, the times
 *  and the ground track (the straight line between the ends, in plan) are the model's. Nowhere
 *  below the ground the console has, or the sea. A flight with no length or time is straight. */
export function divePath(m: Munition, ground?: GroundAt): FlightPath {
  const [e, n] = enu(m.launch_point, m.impact_point)
  const D = Math.hypot(e, n)
  if (!(D >= 1) || !(m.impact_t > m.launch_t)) return straightPath(m)
  const [a, b] = [m.launch_point, m.impact_point]
  const h0 = m.launch_alt_m
  const h1 = m.impact_alt_m
  const ll = (d: number): LngLat => [a[0] + ((b[0] - a[0]) * d) / D, a[1] + ((b[1] - a[1]) * d) / D]
  // the ground along the track, every ~10 m (fewer on a long flight)
  const steps = Math.max(8, Math.min(800, Math.ceil(D / 10)))
  const ds = Array.from({ length: steps + 1 }, (_, k) => (D * k) / steps)
  const under = ds.map((d) => {
    const [lon, lat] = ll(d)
    const g = ground?.(lon, lat)
    return g == null || !Number.isFinite(g) ? null : Math.max(0, g)
  })
  // the cruise height: above the higher end, and clear of the terrain between them
  let cruise = Math.max(h0, h1) + DIVE_CRUISE_ABOVE_M
  for (let k = 1; k < steps; k++) {
    const g = under[k]
    if (g != null && ds[k] > DIVE_END_M && D - ds[k] > DIVE_END_M) cruise = Math.max(cruise, g + DIVE_CLEAR_M)
  }
  // the climb eases out (a quadratic: DIVE_CLIMB_GRADE at its start, level at the cruise); the
  // dive is a cubic, level at the cruise and DIVE_END_GRADE onto the point; on a short flight they
  // share the ground out, the climb at most 40 % of it and the dive 50 %
  const top = h0 + DIVE_POP_M
  const climbH = Math.max(0, cruise - top)
  const climb = Math.min((2 * climbH) / DIVE_CLIMB_GRADE, 0.4 * D)
  const diveH = cruise - h1
  const dive = Math.min(Math.max((3 * diveH) / DIVE_END_GRADE, DIVE_MIN_M), DIVE_MAX_M, 0.5 * D)
  const height = (d: number) => {
    if (d < climb) return top + climbH * (1 - (1 - d / climb) ** 2)
    if (d > D - dive) return h1 + diveH * (1 - (1 - (D - d) / dive) ** 3)
    return cruise
  }
  // the vertices: the launch, the top of the pop-up, then the track's points; each one's height,
  // never under the ground (the last metres onto the point only kept above it)
  const vd = [0, 0]
  const vh = [h0, top]
  for (let k = 1; k <= steps; k++) {
    let h = k === steps ? h1 : height(ds[k])
    const g = under[k]
    if (k < steps && g != null) h = Math.max(h, D - ds[k] > DIVE_END_M ? g + DIVE_FLOOR_M : g)
    vd.push(ds[k])
    vh.push(h)
  }
  // length along it, and the nominal speed at each vertex: the pop-up's, rising to the cruise's
  // over the first DIVE_ACCEL_M of the climb, the cruise's, rising to the end's over the dive
  // (each by length along it)
  const vs = [0]
  for (let i = 1; i < vd.length; i++) vs.push(vs[i - 1] + Math.hypot(vd[i] - vd[i - 1], vh[i] - vh[i - 1]))
  const L = vs[vs.length - 1]
  const sAt = (d: number) => {
    const i = vd.findIndex((x, k) => k > 1 && x >= d)
    return i < 0 ? L : vs[i]
  }
  const diveStart = sAt(D - dive)
  const fast = Math.min(vs[1] + DIVE_ACCEL_M, diveStart)
  const speed = (s: number) => {
    if (s <= vs[1]) return DIVE_POP_MPS
    if (s < fast) return DIVE_POP_MPS + ((DIVE_CRUISE_MPS - DIVE_POP_MPS) * (s - vs[1])) / (fast - vs[1])
    if (s <= diveStart) return DIVE_CRUISE_MPS
    return DIVE_CRUISE_MPS + ((DIVE_END_MPS - DIVE_CRUISE_MPS) * (s - diveStart)) / Math.max(1e-9, L - diveStart)
  }
  // the nominal time at each vertex, then the fraction of the flight's time: the whole profile
  // scaled to the time the model flies it in
  const vt = [0]
  for (let i = 1; i < vs.length; i++) vt.push(vt[i - 1] + (vs[i] - vs[i - 1]) / ((speed(vs[i - 1]) + speed(vs[i])) / 2))
  const T = vt[vt.length - 1]
  const vf = vt.map((t) => t / T)
  vf[vf.length - 1] = 1
  // the segment a value falls in, by binary search over an increasing array
  const seg = (xs: number[], x: number) => {
    let lo = 0
    let hi = xs.length - 1
    while (hi - lo > 1) {
      const mid = (lo + hi) >> 1
      if (xs[mid] <= x) lo = mid
      else hi = mid
    }
    return lo
  }
  return {
    shaped: true,
    length_m: L,
    point: (u) => {
      const s = clamp01(u) * L
      const i = seg(vs, s)
      const span = vs[i + 1] - vs[i]
      const w = span > 0 ? (s - vs[i]) / span : 0
      const d = vd[i] + (vd[i + 1] - vd[i]) * w
      const [lon, lat] = ll(d)
      return [lon, lat, vh[i] + (vh[i + 1] - vh[i]) * w]
    },
    flown: (f) => {
      const x = clamp01(f)
      const i = seg(vf, x)
      const span = vf[i + 1] - vf[i]
      return (vs[i] + (vs[i + 1] - vs[i]) * (span > 0 ? (x - vf[i]) / span : 0)) / L
    },
  }
}

/** A catalog weapon of this kind and profile is drawn on a path of its own: a missile flying the
 *  "dive" profile (a loitering munition's terminal dive; there is no drone kind), the
 *  drone-launcher UGV's rounds. Every other is the straight line. */
export function dives(kind: string | null | undefined, profile: string | null | undefined): boolean {
  return (kind ?? '').trim().toLowerCase() === 'missile' && (profile ?? '').trim().toLowerCase() === 'dive'
}

/** The path it is drawn on: divePath for a dive (`dives`), else the straight line. */
export function flightPath(f: { m: Munition; kind?: string | null; profile?: string | null }, ground?: GroundAt): FlightPath {
  return dives(f.kind, f.profile) ? divePath(f.m, ground) : straightPath(f.m)
}

/** how far along its heading the point that turns its icon lies, m: far enough to be pixels
 *  from it on the screen at any zoom the console shows */
export const HEADING_REACH_M = 200

/** A point HEADING_REACH_M on along the path's direction where it is (a fraction `u` along it),
 *  for its heading on the screen: a straight path's is on the line to its impact. */
export function headingPoint(path: FlightPath, u: number): Pt3 {
  const du = Math.min(0.5, 5 / Math.max(1, path.length_m))
  const [p, q] = u + du <= 1 ? [path.point(u), path.point(u + du)] : [path.point(u - du), path.point(u)]
  const [e, n] = enu([p[0], p[1]], [q[0], q[1]])
  const up = q[2] - p[2]
  const len = Math.hypot(e, n, up)
  if (!(len > 0)) return path.point(1)
  const k = HEADING_REACH_M / len
  const here = path.point(u)
  const cos = Math.cos((here[1] * Math.PI) / 180)
  return [here[0] + (e * k) / (M_PER_DEG * cos), here[1] + (n * k) / M_PER_DEG, here[2] + up * k]
}

/** The profile of the weapon `name` in a platform's catalog loadout (the console's copy of
 *  sim/platforms.toml, /api/platforms): what the intent record does not carry. */
export function loadoutProfile(loadout: { name: string; profile?: string | null }[] | null | undefined, name: string | null | undefined): string | undefined {
  return (loadout ?? []).find((w) => w.name === name)?.profile ?? undefined
}

/** on its way: launched and not landed */
export function inFlight(m: Munition, t: number): boolean {
  return t >= m.launch_t && t < m.impact_t
}

/** whole seconds to the impact, rounded up (so 0 only at the impact) */
export function secondsToImpact(m: Munition, t: number): number {
  return Math.max(0, Math.ceil((m.impact_t - t) / 1000))
}

/** "Hellfire-class missile" -> "HELLFIRE": the name's first word, for the map */
export function munitionShort(name: string): string {
  return (name.trim().split(/[\s-]/)[0] || name).toUpperCase()
}

/** its label on the map: "HELLFIRE · 9 s" */
export function munitionLabel(m: Munition, t: number): string {
  return `${munitionShort(m.name)} · ${secondsToImpact(m, t)} s`
}

/** how long a flash marks an opponent weapon's launch, ms (weaponLayers.ts; our own munition's
 *  launch has none) */
export const LAUNCH_FLASH_MS = 900

/** dash periods over the whole flight (each period's half nearer the impact drawn) */
export const DASHES = 16
/** points along each dash of a shaped path (a straight one's are its two ends) */
const DASH_POINTS = 7

/** The dashed line from the munition on to the impact point: the dashes are laid out from the
 *  impact back, over the whole flight, so they stay put while the munition flies and it eats
 *  into them; the one it is in is cut at it. On a shaped path (`path`) they are laid out along
 *  its length and bend with it. */
export function dashesAhead(m: Munition, t: number, path?: FlightPath): Pt3[][] {
  const f = flightFraction(m, t)
  const u = path ? path.flown(f) : f
  const at = (x: number) => (path ? path.point(x) : pointAlong(m, x))
  const n = path?.shaped ? DASH_POINTS : 2
  const out: Pt3[][] = []
  for (let k = 0; k < DASHES; k++) {
    const hi = 1 - k / DASHES
    if (hi <= u) break
    const lo = Math.max(u, hi - 0.5 / DASHES)
    out.push(Array.from({ length: n }, (_, j) => at(lo + ((hi - lo) * j) / (n - 1))))
  }
  return out
}

/** The IconLayer angle (degrees, counter-clockwise) that turns an icon drawn nose up to point
 *  along (dx, dy), in screen pixels with y down. */
export function screenAngle(dx: number, dy: number): number {
  return (Math.atan2(-dx, -dy) * 180) / Math.PI
}

/** A munition with what the console knows of it besides its flight (bd murmur-ma92): the
 *  weapon's catalog kind (`engagement.weapon.kind`), its catalog profile (the launcher's loadout
 *  in /api/platforms, loadoutProfile; bd murmur-4nik.5), the launcher's callsign and the target
 *  track, so it is drawn and labelled as what it is. */
export interface MunitionFlight {
  m: Munition
  kind?: string | null
  profile?: string | null
  launcher?: string
  target?: string
}

/** "Hellfire-class missile" -> "Hellfire": the name's first word as the catalog writes it */
function munitionName(name: string): string {
  return name.trim().split(/[\s-]/)[0] || name
}

/** Its label on the map, in words: "Hellfire to T-12, 9 s" (the launcher is on the map beside
 *  it; ledger 1, change 21). No munition of ours reports its position in flight: where it is
 *  drawn is the model's. */
export function flightLabel(f: MunitionFlight, t: number): string {
  return `${munitionName(f.m.name)}${f.target ? ` to ${f.target}` : ''}, ${secondsToImpact(f.m, t)} s`
}

/** A round's own line when hovered: "Hellfire from REAPER 1 to T-12, 9 s". */
export function flightTip(f: MunitionFlight, t: number): string {
  return `${munitionName(f.m.name)}${f.launcher ? ` from ${f.launcher}` : ''}${f.target ? ` to ${f.target}` : ''}, ${secondsToImpact(f.m, t)} s`
}

/** Rounds in flight with the same weapon, launcher and target (a salvo's, rules 35, bd
 *  murmur-4nik.2: one plan, its rounds a few seconds apart) are one group on the map, labelled once
 *  (`flightGroupLabel`) rather than a label a round piled on its neighbours'. */
export function flightGroupKey(f: MunitionFlight): string {
  return `${f.m.name}|${f.launcher ?? ''}|${f.target ?? ''}`
}

/** A group's one label: "HIV3 ×6 to T-0001, 20–39 s", the rounds in flight and the seconds to the
 *  first and the last impact; a group of one is `flightLabel`'s. */
export function flightGroupLabel(fs: readonly MunitionFlight[], t: number): string {
  if (fs.length === 1) return flightLabel(fs[0], t)
  const secs = fs.map((f) => secondsToImpact(f.m, t))
  const lo = Math.min(...secs)
  const hi = Math.max(...secs)
  return `${munitionName(fs[0].m.name)} ×${fs.length}${fs[0].target ? ` to ${fs[0].target}` : ''}, ${lo === hi ? lo : `${lo}–${hi}`} s`
}

// ---- gun bursts (rules 26) ----

/** how long a burst lasts, s: the M2's 6 rounds at its 525 rounds a minute (about 0.7 s). The
 *  intent record carries no burst duration (`engage.burst` logs `duration_s`, the feed does
 *  not), so the console assumes the one gun there is. */
export const BURST_S = 0.7
/** how long a tracer round takes to cross the line, s: the drawing's, not the bullet's */
export const TRACER_FLIGHT_S = 0.3

/** the gun's last reported burst, as `engagement.fire` gives it */
export interface BurstFire {
  bursts: number
  rounds: number
  last_t: number
}

/** Tracer rounds on the line from the gun to its target at `t`: each a fraction along the line
 *  (0 muzzle, 1 target), its head; the burst's rounds leave evenly over BURST_S from `last_t`.
 *  Empty when no burst has been reported or the last one is over: nothing is drawn for a burst
 *  the bridge did not report. `still` (reduced motion): the line shows for the burst's duration
 *  as one round at the middle. */
export function tracerHeads(f: BurstFire, t: number, still = false): number[] {
  if (!(f.bursts > 0) || !(f.rounds > 0)) return []
  const age = (t - f.last_t) / 1000
  if (age < 0 || age >= BURST_S + TRACER_FLIGHT_S) return []
  if (still) return age < BURST_S ? [0.5] : []
  const n = Math.max(1, Math.min(6, Math.round(f.rounds / f.bursts)))
  const out: number[] = []
  for (let k = 0; k < n; k++) {
    const p = (age - (k * BURST_S) / n) / TRACER_FLIGHT_S
    if (p >= 0 && p < 1) out.push(p)
  }
  return out
}

/** a burst is drawn now or within a second (for animation frames) */
export function burstShown(f: BurstFire, t: number): boolean {
  const age = (t - f.last_t) / 1000
  return f.bursts > 0 && age >= -1 && age < BURST_S + TRACER_FLIGHT_S
}
