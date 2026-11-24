// Opponent weapons (docs/OPPONENT-WEAPONS.md): each weapon as our sensors know it, from the bridge's
// weapon picture (the tracks feed's `weapons`), never from the simulator's truth, and only as far as
// its identity has come (bd murmur-ju1h; the stages are the bridge's, tracks/weapons.rs):
//
// - **launch**: a camera saw a launch's flash (or a sonar heard a torpedo's launch transient): a
//   marker where it placed the shooter and the bearing from the observer that caught it, and, when
//   told, what kind of launch it was (a missile's, a rocket's, a gun's muzzle flash, a transient).
//   No class, no speed, no target. A gun's burst is a launch of its own.
// - **weapon**: fixes whose measured speed and origin (or line closing on one of ours) say weapon:
//   a hostile weapon symbol of unknown kind ("weapon, type unknown") with its measured speed. A fast
//   aircraft running at one of ours can be taken for one (`track` names its contact track).
// - **class**: a class assessed on named evidence: "ATGM-class: 250 m/s, missile launch, from
//   T-0004", "Gun-class: muzzle flash"; its symbol is the bridge's `class_symbol` (missile for the
//   ATGM, rocket and air-launched classes, gun, torpedo).
// - the exact type is never shown: nothing modelled can tell it.
//
// The estimated flight is drawn only on a basis: a target and expected strike the bridge derived
// (from the measured line, or with a class from its reach and nominal speed), dashed, with a
// countdown; or, with a measured velocity and no target, its heading a few seconds ahead. The
// weapon's own fixes are solid; the strike is drawn at the victim when the victim reports it.
// Pure functions, no DOM and no framework (weapon.check.ts); weaponLayers.ts draws, attackEvents.ts
// joins its strikes into the attack's banner. Times are engine ms (the bridge's clock).
import type { Munition } from './munition.ts'
import type { LngLat } from './types.ts'

export interface WeaponFix {
  t: number
  point: LngLat
  sigma_m: number
  /** `"<uid>:<sensor>"` */
  sensor: string
  /** the height a height-finding radar measured, m (null from any other sensor) */
  alt_m?: number | null
}

/** what kind of launch the sensor that caught it told it as: a missile's, a rocket's, a gun's
 *  muzzle flash, a torpedo's launch transient */
export type LaunchLook = 'missile' | 'rocket' | 'muzzle' | 'transient'

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
  /** what kind of launch it was, when told (null: a launch, no more) */
  look?: LaunchLook | null
}

/** How far a weapon's identity has come: a lone fix (not called a weapon), a seen launch, a weapon by
 *  its kinematics, a class on evidence. */
export type WeaponStage = 'plot' | 'launch' | 'weapon' | 'class'

/** One piece of a class's evidence: its basis, a short form for labels, a sentence. */
export interface WeaponEvidence {
  basis: 'speed' | 'height' | 'launch' | 'reach' | 'shooter'
  short: string
  text: string
}

/** One weapon track (the bridge's `tracks::weapons`; docs/OPPONENT-WEAPONS.md section 5). */
export interface WeaponTrack {
  id: string
  /** the act report the seen launch became (`A-3`) */
  report: string | null
  stage: WeaponStage
  /** the class assessed (the threat library's name: "ATGM-class", "Rocket-class", "Air
   *  missile-class", "Gun-class", "Torpedo-class"); null below the class stage */
  class: string | null
  /** its symbol kind (weaponSymbols.ts: missile, bomb, torpedo, gun) */
  class_symbol: string | null
  /** what the class rests on, each named */
  evidence: WeaponEvidence[]
  /** why it was taken for a weapon: its measured speed over the threshold of the domain it was
   *  judged in (under water, in the air at a measured height, else at the surface), from a seen
   *  launch or closing on one of ours */
  weapon_by: { speed_mps: number; over_mps: number; domain?: 'air' | 'surface' | 'subsurface' | 'ground'; from_launch: boolean; closing_on: string | null } | null
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
/** the strike's flash at the victim, ms */
export const STRIKE_FLASH_MS = 1500
/** a measured velocity with no target is drawn this far ahead of its last point, ms */
export const HEADING_AHEAD_MS = 3000

/** "ATGM-class" -> "ATGM", "Air missile-class" -> "AIR MISSILE"; no class, "WEAPON" */
export function weaponShort(name: string | null): string {
  return name ? (name.trim().replace(/-class$/i, '') || name).toUpperCase() : 'WEAPON'
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

/** what a launch was told as, for the map's line */
const LOOK_WORDS: Record<LaunchLook, string> = { missile: 'missile launch', rocket: 'rocket launch', muzzle: 'muzzle flash', transient: 'launch transient' }

/** a class in words for the map: "ATGM" (an acronym stays), "air missile", "gun"; "weapon" without */
function classWords(name: string | null): string {
  if (!name) return 'weapon'
  const bare = name.trim().replace(/-class$/i, '')
  return /^[A-Z0-9]+$/.test(bare) ? weaponShort(name) : weaponShort(name).toLowerCase()
}

/** A weapon's one line on the map, in words, as far as its stage has come (ledger 1, change 24):
 *  "missile launch" (a launch: its look when told), "ATGM at HUNTER 1, 2 s" (a class, heading for
 *  one of ours, with its countdown), "weapon at HUNTER 1, due", "weapon, 243 m/s" (no target). */
export function weaponLine(w: WeaponTrack, est: Munition | null, now: number, nameOf: (uid: string) => string): string {
  if (w.stage === 'launch') return w.launch?.look ? LOOK_WORDS[w.launch.look] : 'launch'
  const what = w.stage === 'class' ? classWords(w.class) : 'weapon'
  const c = countdownS(est, w, now)
  if (w.aimed_at && c != null) return `${what} at ${nameOf(w.aimed_at)}, ${c > 0 ? `${Math.max(1, Math.round(c))} s` : 'due'}`
  if (w.aimed_at) return `${what} at ${nameOf(w.aimed_at)}`
  return w.speed_mps != null ? `${what}, ${Math.round(w.speed_mps)} m/s` : what
}

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
