// Opponent weapons (docs/OPPONENT-WEAPONS.md): each weapon as our sensors know it, from the bridge's
// weapon picture (the tracks feed's `weapons`), never from the simulator's truth, and only as far as
// its identity has come (bd murmur-ju1h; the stages are the bridge's, tracks/weapons.rs):
//
// - **launch**: a camera saw a launch's flash: a marker where it placed the shooter and the bearing
//   from the observer that saw it. No class, no speed, no target.
// - **weapon**: fixes whose measured speed and origin (or line closing on one of ours) say weapon:
//   a hostile weapon symbol of unknown kind ("weapon, type unknown") with its measured speed. A fast
//   aircraft running at one of ours can be taken for one (`track` names its contact track).
// - **class**: a class assessed on named evidence: "ATGM-class: 250 m/s, from T-0004".
// - the exact type is never shown: nothing modelled can tell it.
//
// The estimated flight is drawn only on a basis: a target and expected strike the bridge derived
// (from the measured line, or with a class from its reach and nominal speed), dashed, with a
// countdown; or, with a measured velocity and no target, its heading a few seconds ahead. The
// weapon's own fixes are solid; the strike is drawn at the victim when the victim reports it.
// Pure functions, no DOM and no framework (weapon.check.ts); weaponLayers.ts draws, App.tsx shows
// the banner, Tutorial.tsx the timeline. Times are engine ms (the bridge's clock).
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
  /** when it was reported (the act report's time) */
  t: number
  /** when the flash was */
  flash_t: number
  /** where the cue placed the shooter, with its 1-sigma */
  point: LngLat
  sigma_m: number
  sensors: string[]
  /** the observer whose fix it is, where it believed it was */
  observer: { uid: string; point: LngLat } | null
  /** from that observer to the launch, degrees true */
  bearing_deg: number | null
}

/** How far a weapon's identity has come: a lone fix (not called a weapon), a seen launch, a weapon by
 *  its kinematics, a class on evidence. */
export type WeaponStage = 'plot' | 'launch' | 'weapon' | 'class'

/** One piece of a class's evidence: its basis, a short form for labels, a sentence. */
export interface WeaponEvidence {
  basis: 'speed' | 'height' | 'reach' | 'shooter'
  short: string
  text: string
}

/** One weapon track (the bridge's `tracks::weapons`; docs/OPPONENT-WEAPONS.md section 5). */
export interface WeaponTrack {
  id: string
  /** the act report the seen launch became (`A-3`) */
  report: string | null
  stage: WeaponStage
  /** the class assessed (the threat library's name, "ATGM-class"); null below the class stage */
  class: string | null
  /** its symbol kind (weaponSymbols.ts: missile, bomb, torpedo, gun) */
  class_symbol: string | null
  /** what the class rests on, each named */
  evidence: WeaponEvidence[]
  /** why it was taken for a weapon: its measured speed over its domain's threshold, from a seen
   *  launch or closing on one of ours */
  weapon_by: { speed_mps: number; over_mps: number; from_launch: boolean; closing_on: string | null } | null
  /** its measured speed and that speed's 1-sigma, m/s; velocity east and north; where and when the
   *  measured line puts it (null without two points in time) */
  speed_mps: number | null
  speed_sigma_mps: number | null
  velocity_mps: [number, number] | null
  kin_t: number | null
  kin_point: LngLat | null
  /** the contact track it is, when a track of the tracker passed the weapon test */
  track: string | null
  /** the contact track the launch was placed on */
  shooter: string | null
  /** the one of ours it heads for and the expected strike, derived (`track`: its measured line;
   *  `reach`: its class's reach from the launch, at the class's nominal speed); null with no basis */
  aimed_at: string | null
  aim_basis: 'track' | 'reach' | null
  eta_t: number | null
  nominal_mps: number | null
  launch: WeaponLaunch | null
  fixes: WeaponFix[]
  status: 'in_flight' | 'expended' | 'lost'
  ended_t: number | null
  struck: { uid: string; t: number } | null
  first_cue_t: number | null
  warning_s: number | null
}

/** its flight height above the sea, m (drawn just over the water) */
export const FLY_ALT_M = 4
/** a weapon track is drawn this long after it ended (the bridge forgets it then too), ms */
export const SHOWN_MS = 60_000
/** the banner holds the strike this long, ms */
export const STRUCK_BANNER_MS = 45_000
/** the strike's flash at the victim, ms */
export const STRIKE_FLASH_MS = 1500
/** a measured velocity with no target is drawn this far ahead of its last point, ms */
export const HEADING_AHEAD_MS = 3000

/** "ATGM-class" -> "ATGM"; no class, "WEAPON" */
export function weaponShort(name: string | null): string {
  return name ? (name.trim().split(/[\s-]/)[0] || name).toUpperCase() : 'WEAPON'
}

/** "ATGM-class: 250 m/s, from T-0004" (the class with its evidence); null below the class stage */
export function classText(w: WeaponTrack): string | null {
  if (!w.class) return null
  const ev = w.evidence.map((e) => e.short).join(', ')
  return ev ? `${w.class}: ${ev}` : w.class
}

/** what the stage says of it, in a few words: "launch, bearing 270° from HUNTER 1",
 *  "weapon, type unknown, 243 m/s", "ATGM-class: 243 m/s, from T-0004" */
export function stageText(w: WeaponTrack, nameOf: (uid: string) => string): string {
  if (w.stage === 'class') return classText(w) ?? 'weapon'
  if (w.stage === 'weapon') {
    const v = w.speed_mps != null ? `, ${Math.round(w.speed_mps)} m/s` : ''
    return `weapon, type unknown${v}${w.track ? ` (track ${w.track})` : ''}`
  }
  if (w.stage === 'launch') {
    const l = w.launch
    return l?.observer && l.bearing_deg != null ? `launch, bearing ${Math.round(l.bearing_deg)}° from ${nameOf(l.observer.uid)}` : 'launch'
  }
  return 'a fix, not yet a weapon'
}

/** The estimated flight as a munition-shaped line (munition.ts draws and moves it), only on a
 *  basis: the target and expected strike the bridge derived. From the launch's flash (else the first
 *  fix) to `victim` (where that vehicle was when this console first had the target), ending at the
 *  expected strike, or the victim's reported strike once there is one. */
export function estimate(w: WeaponTrack, victim: LngLat | null): Munition | null {
  if (!victim || !w.aimed_at || w.eta_t == null) return null
  const from = w.launch ? { t: w.launch.flash_t, point: w.launch.point } : w.fixes[0] ? { t: w.fixes[0].t, point: w.fixes[0].point } : null
  if (!from) return null
  const impact = Math.max(w.struck?.t ?? w.eta_t, from.t + 1)
  const tof = (impact - from.t) / 1000
  return {
    name: w.class ?? 'weapon',
    speed_mps: distanceM(from.point, victim) / tof,
    launch_t: from.t,
    launch_point: from.point,
    launch_alt_m: FLY_ALT_M,
    impact_t: impact,
    impact_point: victim,
    impact_alt_m: FLY_ALT_M,
    tof_s: tof,
  }
}

/** With a measured velocity and no target: where the line puts it at `t` (no further than
 *  HEADING_AHEAD_MS past its last point), and the path from that point to there. */
export function heading(w: WeaponTrack, t: number): { from: LngLat; at: LngLat } | null {
  if (w.aimed_at || !w.velocity_mps || !w.kin_point || w.kin_t == null || w.struck || w.status !== 'in_flight') return null
  const dt = Math.min(Math.max(0, t - w.kin_t), HEADING_AHEAD_MS) / 1000
  return { from: w.kin_point, at: offsetM(w.kin_point, w.velocity_mps[0] * dt, w.velocity_mps[1] * dt) }
}

/** seconds to the expected strike, one decimal, never below 0; null once struck or with no estimate */
export function countdownS(est: Munition | null, w: WeaponTrack, now: number): number | null {
  if (!est || w.struck) return null
  return Math.max(0, Math.round((est.impact_t - now) / 100) / 10)
}

/** the map's label on the estimate: "EST. ATGM · 2.1 s", "EST. WEAPON · due" */
export function estimateMapLabel(est: Munition, w: WeaponTrack, now: number): string {
  const c = countdownS(est, w, now)
  return `EST. ${weaponShort(w.class)} · ${c == null ? 'struck' : c > 0 ? `${c.toFixed(1)} s` : 'due'}`
}

/** what an estimate rests on, as a sentence: "heading for HUNTER 1 on its measured line, strike
 *  expected in about 4 s", "heading for HUNTER 1, the only one of ours within the ATGM-class's reach
 *  of the launch, at its nominal 250 m/s" */
export function estimateText(w: WeaponTrack, est: Munition | null, nameOf: (uid: string) => string): string {
  if (!est || !w.aimed_at) return ''
  const who = nameOf(w.aimed_at)
  return w.aim_basis === 'reach'
    ? `heading for ${who}, the only one of ours within the ${w.class ?? 'class'}'s reach of the launch, at its nominal ${w.nominal_mps ?? '?'} m/s`
    : `heading for ${who} on its measured line, strike expected about ${Math.max(1, Math.round(est.tof_s))} s after the first cue`
}

/** what the drawing rests on (the banner's tooltip) */
export const ESTIMATE_NOTE =
  'Known by stages, as a crew learns it: a launch (a camera saw the flash: where, and the bearing), then a weapon (fixes whose measured speed and origin say so: type unknown), then a class only on named evidence (its measured speed fitting a class, or a shooter identified as a kind known to carry it). The exact type is out of reach. The dashed flight is drawn only on a basis: the target and strike time derived from the measured line, or from the class\'s reach and nominal speed. Solid marks are the weapon\'s own fixes. A fast aircraft running at one of ours can be taken for a weapon.'

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
 *  by REAPER 1 radar from 1:22.0; assessed ATGM-class: 250 m/s, from T-0004; HUNTER 1 struck at
 *  1:24.4; 4.0 s of warning". `start`: the run's start (engine ms), for the clock. */
export function timeline(w: WeaponTrack, nameOf: (uid: string) => string, start: number | null): string {
  const parts: string[] = []
  parts.push(w.launch ? `launch seen by ${sensorsText(w.launch.sensors, nameOf)} at ${clockText(w.launch.t, start)}` : 'launch not seen')
  parts.push(w.fixes.length ? `weapon fixed by ${fixedBy(w, nameOf)} from ${clockText(w.fixes[0].t, start)}` : 'weapon not fixed in flight')
  const c = classText(w)
  if (c) parts.push(`assessed ${c}`)
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

/** The banner while one of ours may be under attack, then its strike: the newest track in flight at
 *  the launch stage or above (LAUNCH with its bearing; INCOMING with what the stage knows and the
 *  countdown when there is an estimate), else the newest strike within STRUCK_BANNER_MS.
 *  `victimOf`: the estimate's end for a weapon. */
export function weaponBanner(ws: WeaponTrack[], now: number, nameOf: (uid: string) => string, victimOf: (w: WeaponTrack) => LngLat | null): WeaponBanner | null {
  const flying = ws
    .filter((w) => w.status === 'in_flight' && !w.struck && w.stage !== 'plot')
    .sort((a, b) => (b.first_cue_t ?? 0) - (a.first_cue_t ?? 0))[0]
  if (flying) {
    const seen = flying.launch ? `seen by ${sensorsText(flying.launch.sensors, nameOf)}` : 'launch not seen'
    if (flying.stage === 'launch') {
      const from = flying.shooter ? ` from ${flying.shooter}` : ''
      return { tone: 'incoming', w: flying, head: 'LAUNCH', text: `${stageText(flying, nameOf)}${from} · ${seen} · weapon not yet tracked: type, speed and target unknown` }
    }
    const est = estimate(flying, victimOf(flying))
    const c = countdownS(est, flying, now)
    const at = flying.aimed_at ? ` at ${nameOf(flying.aimed_at)}` : ''
    const when = c == null ? '' : c > 0 ? `, strike expected in ${c.toFixed(1)} s` : ', strike due'
    const fixed = flying.fixes.length ? `fixed by ${fixedBy(flying, nameOf)}` : 'not tracked in flight'
    return { tone: 'incoming', w: flying, head: 'INCOMING', text: `${stageText(flying, nameOf)}${at}${when} · ${seen} · ${fixed}` }
  }
  const struck = ws.filter((w) => w.struck && now - w.struck.t >= 0 && now - w.struck.t < STRUCK_BANNER_MS).sort((a, b) => b.struck!.t - a.struck!.t)[0]
  if (struck) {
    const who = nameOf(struck.struck!.uid)
    const how = struck.launch
      ? `launch seen by ${sensorsText(struck.launch.sensors, nameOf)}`
      : struck.fixes.length
        ? `launch not seen; weapon fixed by ${fixedBy(struck, nameOf)}`
        : 'no sensor saw the launch or the weapon'
    const c = classText(struck)
    const warning = struck.warning_s != null ? ` · ${struck.warning_s.toFixed(1)} s of warning` : ''
    return { tone: 'struck', w: struck, head: `${who.toUpperCase()} STRUCK`, text: `${how}${c ? ` · ${c}` : ''}${warning} · any damage is its own report (Inbox)` }
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

/** `p` moved `e` metres east and `n` north (flat) */
export function offsetM(p: LngLat, e: number, n: number): LngLat {
  const k = 111_320
  return [p[0] + e / (k * Math.cos((p[1] * Math.PI) / 180)), p[1] + n / k]
}
