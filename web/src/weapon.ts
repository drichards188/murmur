// Opponent weapons (docs/OPPONENT-WEAPONS.md): each weapon as our sensors know it, from the bridge's
// weapon picture (the tracks feed's `weapons`), never from the simulator's truth. What is drawn:
//
// - the estimated flight, when a launch was seen: from the launch fix to the vehicle the launch
//   report says it heads for, at its class's nominal speed (a launch reports the class, its speed
//   and its heading by construction: the design's shortcut, bd murmur-ju1h), dashed and labelled as
//   an estimate, with a countdown to the expected strike; the strike time is the victim's report
//   once it comes;
// - the weapon's own fixes (radar plots, camera fixes), solid, when they come;
// - the strike at the victim when the victim reports it.
//
// With no launch seen, only the fixes and the strike are drawn. Pure functions, no DOM and no
// framework (weapon.check.ts); weaponLayers.ts draws, App.tsx shows the banner, Tutorial.tsx the
// timeline. Times are engine ms (the bridge's clock).
import { sensorsText } from './engageSteps.ts'
import type { Munition } from './munition.ts'
import type { LngLat } from './types.ts'

export interface WeaponFix {
  t: number
  point: LngLat
  sigma_m: number
  /** `"<uid>:<sensor>"` */
  sensor: string
}

export interface WeaponLaunch {
  t: number
  point: LngLat
  sigma_m: number
  sensors: string[]
}

/** One weapon track (the bridge's `tracks::weapons`). */
export interface WeaponTrack {
  id: string
  /** the act report the seen launch became (`A-3`) */
  report: string | null
  /** what a seen launch reports, by construction: the class, its nominal speed, which of ours it heads for */
  class: string | null
  speed_mps: number | null
  aimed_at: string | null
  launch: WeaponLaunch | null
  fixes: WeaponFix[]
  status: 'in_flight' | 'expended' | 'lost'
  ended_t: number | null
  struck: { uid: string; t: number } | null
  /** the strike the launch's report expects (launch time plus distance over nominal speed) */
  eta_t: number | null
  first_cue_t: number | null
  warning_s: number | null
}

/** its flight height above the sea, m (the weapon's figures: docs/OPPONENT-WEAPONS.md) */
export const FLY_ALT_M = 4
/** a weapon track is drawn this long after it ended (the bridge forgets it then too), ms */
export const SHOWN_MS = 60_000
/** the banner holds the strike this long, ms */
export const STRUCK_BANNER_MS = 45_000
/** the strike's flash at the victim, ms */
export const STRIKE_FLASH_MS = 1500

/** "ATGM-class missile" -> "ATGM" */
export function weaponShort(name: string | null): string {
  return name ? (name.trim().split(/[\s-]/)[0] || name).toUpperCase() : 'WEAPON'
}

/** The estimated flight as a munition-shaped line (munition.ts draws and moves it): from the launch
 *  fix to `victim` (where the vehicle it heads for was when this console first had the launch), at
 *  the class's speed; its end the victim's reported strike once there is one. None without a seen
 *  launch, its heading or its speed. */
export function estimate(w: WeaponTrack, victim: LngLat | null): Munition | null {
  const l = w.launch
  if (!l || !victim || !(w.speed_mps && w.speed_mps > 0)) return null
  const tof = distanceM(l.point, victim) / w.speed_mps
  const impact = w.struck?.t ?? l.t + tof * 1000
  return {
    name: w.class ?? 'weapon',
    speed_mps: w.speed_mps,
    launch_t: l.t,
    launch_point: l.point,
    launch_alt_m: FLY_ALT_M,
    impact_t: Math.max(impact, l.t + 1),
    impact_point: victim,
    impact_alt_m: FLY_ALT_M,
    tof_s: tof,
  }
}

/** seconds to the expected strike, one decimal, never below 0; null once struck or with no estimate */
export function countdownS(est: Munition | null, w: WeaponTrack, now: number): number | null {
  if (!est || w.struck) return null
  return Math.max(0, Math.round((est.impact_t - now) / 100) / 10)
}

/** the map's label on the estimate: "EST. ATGM · 2.1 s", "EST. ATGM · due" */
export function estimateMapLabel(est: Munition, w: WeaponTrack, now: number): string {
  const c = countdownS(est, w, now)
  return `EST. ${weaponShort(w.class)} · ${c == null ? 'struck' : c > 0 ? `${c.toFixed(1)} s` : 'due'}`
}

/** "estimated from the launch seen by HUNTER 1 EO/IR: ATGM-class missile, about 4 s" */
export function estimateText(w: WeaponTrack, est: Munition | null, nameOf: (uid: string) => string): string {
  if (!w.launch || !est) return ''
  return `estimated from the launch seen by ${sensorsText(w.launch.sensors, nameOf)}: ${w.class ?? 'weapon'}, about ${Math.max(1, Math.round(est.tof_s))} s`
}

/** what an estimate rests on (the banner's tooltip) */
export const ESTIMATE_NOTE =
  "The dashed flight is an estimate: from where our cameras saw the launch to the vehicle the launch heads for, at the weapon class's nominal speed. The class, speed and heading come with the launch observation by construction (a simulation shortcut standing for a threat-warning system); the solid marks are the weapon's own fixes by our radars and cameras."

/** The weapon tracks to draw at `now`: in flight, or ended within SHOWN_MS. */
export function shownWeapons(ws: WeaponTrack[], now: number): WeaponTrack[] {
  return ws.filter((w) => w.ended_t == null || now - w.ended_t < SHOWN_MS)
}

/** the strike flash's progress at the victim, 0 to 1; null outside it */
export function strikeFlash(w: WeaponTrack, now: number): number | null {
  if (!w.struck) return null
  const p = (now - w.struck.t) / STRIKE_FLASH_MS
  return p >= 0 && p < 1 ? p : null
}

/** "1:20.4" from a run's start (engine ms) */
export function clockText(t: number, start: number | null): string {
  if (start == null) return new Date(t).toISOString().slice(11, 21) + 'Z'
  const s = Math.max(0, (t - start) / 1000)
  const m = Math.floor(s / 60)
  return `${m}:${(s - m * 60).toFixed(1).padStart(4, '0')}`
}

/** who fixed it: "REAPER 1 radar", each sensor once, in the order they first fixed it */
export function fixedBy(w: WeaponTrack, nameOf: (uid: string) => string): string {
  const seen: string[] = []
  for (const f of w.fixes) if (!seen.includes(f.sensor)) seen.push(f.sensor)
  return sensorsText(seen, nameOf)
}

/** One attack's cues as a person reads them: "launch seen by HUNTER 1 EO/IR at 1:20.4; weapon fixed
 *  by REAPER 1 radar from 1:22.0; HUNTER 1 struck at 1:24.4; 4.0 s of warning". `start`: the run's
 *  start (engine ms), for the clock. */
export function timeline(w: WeaponTrack, nameOf: (uid: string) => string, start: number | null): string {
  const parts: string[] = []
  parts.push(w.launch ? `launch seen by ${sensorsText(w.launch.sensors, nameOf)} at ${clockText(w.launch.t, start)}` : 'launch not seen')
  parts.push(w.fixes.length ? `weapon fixed by ${fixedBy(w, nameOf)} from ${clockText(w.fixes[0].t, start)}` : 'weapon not fixed in flight')
  if (w.struck) parts.push(`${nameOf(w.struck.uid)} struck at ${clockText(w.struck.t, start)}`)
  else if (w.status === 'in_flight') parts.push('strike to come')
  if (w.warning_s != null) parts.push(`${w.warning_s.toFixed(1)} s of warning`)
  return parts.join('; ')
}

export interface WeaponBanner {
  tone: 'incoming' | 'struck'
  w: WeaponTrack
  head: string
  text: string
}

/** The banner while one of ours is under a detected attack, then its strike: the newest weapon in
 *  flight with a seen launch or a fix (INCOMING, with the countdown when there is an estimate),
 *  else the newest strike within STRUCK_BANNER_MS. `victimOf`: the estimate's end for a weapon. */
export function weaponBanner(ws: WeaponTrack[], now: number, nameOf: (uid: string) => string, victimOf: (w: WeaponTrack) => LngLat | null): WeaponBanner | null {
  const flying = ws
    .filter((w) => w.status === 'in_flight' && !w.struck && (w.launch || w.fixes.length))
    .sort((a, b) => (b.first_cue_t ?? 0) - (a.first_cue_t ?? 0))[0]
  if (flying) {
    const est = estimate(flying, victimOf(flying))
    const c = countdownS(est, flying, now)
    const at = flying.aimed_at ? nameOf(flying.aimed_at) : 'one of ours'
    const seen = flying.launch ? `launch seen by ${sensorsText(flying.launch.sensors, nameOf)}` : 'launch not seen'
    const fixed = flying.fixes.length ? `weapon fixed by ${fixedBy(flying, nameOf)}` : 'weapon not tracked'
    const when = c == null ? '' : c > 0 ? `, strike expected in ${c.toFixed(1)} s` : ', strike due'
    return { tone: 'incoming', w: flying, head: 'INCOMING', text: `${weaponShort(flying.class)} at ${at}${when} · ${seen} · ${fixed}` }
  }
  const struck = ws.filter((w) => w.struck && now - w.struck.t >= 0 && now - w.struck.t < STRUCK_BANNER_MS).sort((a, b) => b.struck!.t - a.struck!.t)[0]
  if (struck) {
    const who = nameOf(struck.struck!.uid)
    const how = struck.launch
      ? `launch seen by ${sensorsText(struck.launch.sensors, nameOf)}`
      : struck.fixes.length
        ? `launch not seen; weapon fixed by ${fixedBy(struck, nameOf)}`
        : 'no sensor saw the launch or the weapon'
    const warning = struck.warning_s != null ? ` · ${struck.warning_s.toFixed(1)} s of warning` : ''
    return { tone: 'struck', w: struck, head: `${who.toUpperCase()} STRUCK`, text: `${how}${warning} · no damage is modelled` }
  }
  return null
}

/** The act reports that launched a weapon (their drawing is the weapon's, not an arrow). */
export function weaponReports(ws: WeaponTrack[]): Set<string> {
  return new Set(ws.map((w) => w.report).filter((r): r is string => !!r))
}

/** metres between two [lon, lat] points (flat; a few kilometres at most) */
export function distanceM(a: LngLat, b: LngLat): number {
  const k = 111_320
  const dx = (b[0] - a[0]) * k * Math.cos((((a[1] + b[1]) / 2) * Math.PI) / 180)
  const dy = (b[1] - a[1]) * k
  return Math.hypot(dx, dy)
}
