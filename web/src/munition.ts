// Munitions in flight (bd murmur-64z): a standoff platform's munition from its release to its
// impact, as the bridge models it. The model is the one the engine, the truth side and the
// console share: a straight slant line from the platform (its position and altitude at release)
// to the effect point at constant speed, so the position at a time is linear in longitude,
// latitude and altitude between the two ends. Pure functions, no DOM and no framework, so they
// can be checked on their own (munition.check.ts); munitionLayers.ts draws them.
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

/** Where it is at `t`: at its launch point before the launch, at the impact point after. */
export function munitionAt(m: Munition, t: number): Pt3 {
  return pointAlong(m, flightFraction(m, t))
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

/** how long a flash marks the launch, ms */
export const LAUNCH_FLASH_MS = 900

/** The launch flash's progress at `t`, 0 at the launch to 1 as it ends; null outside it. */
export function launchFlash(m: Munition, t: number): number | null {
  const age = t - m.launch_t
  return age >= 0 && age < LAUNCH_FLASH_MS ? age / LAUNCH_FLASH_MS : null
}

/** Something of it is drawn at `t`: it is in flight, or its launch flash is still on. */
export function munitionShown(m: Munition, t: number): boolean {
  return inFlight(m, t) || launchFlash(m, t) != null
}

/** how much flight the trail behind it covers, ms */
export const TRAIL_MS = 3000

/** The trail behind it at `t`, oldest point first, ending at the munition: the last TRAIL_MS of
 *  its flight (less just after the launch); empty when it has not moved yet. */
export function trailPoints(m: Munition, t: number, n = 8): Pt3[] {
  const f1 = flightFraction(m, t)
  const f0 = flightFraction(m, t - TRAIL_MS)
  if (!(f1 > f0)) return []
  const out: Pt3[] = []
  for (let k = 0; k <= n; k++) out.push(pointAlong(m, f0 + ((f1 - f0) * k) / n))
  return out
}

/** dash periods over the whole flight (each period's half nearer the impact drawn) */
export const DASHES = 16

/** The dashed line from the munition on to the impact point: the dashes are laid out from the
 *  impact back, over the whole flight, so they stay put while the munition flies and it eats
 *  into them; the one it is in is cut at it. */
export function dashesAhead(m: Munition, t: number): [Pt3, Pt3][] {
  const f = flightFraction(m, t)
  const out: [Pt3, Pt3][] = []
  for (let k = 0; k < DASHES; k++) {
    const hi = 1 - k / DASHES
    if (hi <= f) break
    out.push([pointAlong(m, Math.max(f, hi - 0.5 / DASHES)), pointAlong(m, hi)])
  }
  return out
}

/** The IconLayer angle (degrees, counter-clockwise) that turns an icon drawn nose up to point
 *  along (dx, dy), in screen pixels with y down. */
export function screenAngle(dx: number, dy: number): number {
  return (Math.atan2(-dx, -dy) * 180) / Math.PI
}

/** A munition with what the console knows of it besides its flight (bd murmur-ma92): the
 *  weapon's catalog kind (`engagement.weapon.kind`), the launcher's callsign and the target
 *  track, so it is drawn and labelled as what it is. */
export interface MunitionFlight {
  m: Munition
  kind?: string | null
  launcher?: string
  target?: string
}

/** Its label on the map: "HELLFIRE · REAPER 1 > T-12 · 9 s PRED". No munition of ours reports
 *  its position in flight: where it is drawn is the model's, and the label says so (PRED). */
export function flightLabel(f: MunitionFlight, t: number): string {
  const who = f.launcher ? (f.target ? `${f.launcher} > ${f.target}` : f.launcher) : f.target
  return [munitionShort(f.m.name), who, `${secondsToImpact(f.m, t)} s PRED`].filter(Boolean).join(' · ')
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
