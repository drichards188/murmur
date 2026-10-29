// Hostile acts our sensors observed (rules 9), drawn on the map as a fire event: a fast streak
// from where our sensors placed the act's author to the victim, a flash on the victim, a label
// that fades, then a faint dashed line and the author's marker kept for a minute. Only what the
// console already receives is drawn: the observed acts in the inbox (`inbox.acts`, the bridge's
// latest twenty), each with its author's fix and that fix's 1-sigma error, never the
// simulator's truth. An act carries no weapon, range or speed, so none is drawn: the streak is a
// display convention, the same for every act.
//
// The victim is drawn where the console had it when the act first came in (our vehicle's
// reported position, or a protected track's predicted one at the act's time); the bridge
// reports an act within a second or two of observing it. An act first seen long after it was
// observed (a console opened later) is not replayed: it is drawn as it would be by then.
//
// Pure functions, no DOM and no framework, so they can be checked on their own
// (hostileAct.check.ts); actLayers.ts draws them, App.tsx shows the banner.
import { sensorsText } from './engageSteps.ts'
import type { Against, ObservedAct } from './ident'
import type { LngLat } from './types'

/** [lon, lat, metres above mean sea level] */
export type Pt3 = [number, number, number]

/** The victim as drawn: its name, and where it was (metres above mean sea level). */
export interface Victim {
  at: LngLat
  alt: number
}

/** One observed act as the map draws it. Times are engine ms (the bridge's clock). */
export interface ActDrawing {
  /** `A-1@<observed_t>`: act ids start again in a new run */
  key: string
  act: string
  observedT: number
  /** when its fire event starts on this console (animStart) */
  start: number
  /** where our sensors placed the author (the best observer's fix), and its 1-sigma error */
  author: LngLat
  sigma_m: number
  /** the track the engine attributed it to, and the attribution; null: unattributed */
  authorTrack: string | null
  attribution: number | null
  /** why it is unattributed, when the engine said ("flag F-2") */
  unattributedNote: string | null
  /** null: no position for the victim (something we hold no track of, or not in the picture) */
  victim: Victim | null
  victimName: string
  /** "HUNTER 1 EO/IR and sonar; REAPER 1 EO/IR" */
  seenBy: string
}

// ---- timing ------------------------------------------------------------------------------------

/** the streak's head goes from the author to the victim in this long, ms */
export const TRAVEL_MS = 700
/** its tail follows this far behind, so the whole streak is gone after TRAVEL_MS + TAIL_LAG_MS */
export const TAIL_LAG_MS = 300
export const STREAK_MS = TRAVEL_MS + TAIL_LAG_MS
/** the flash where the author fired */
export const MUZZLE_MS = 600
/** the flash on the victim, from the streak's arrival */
export const HIT_MS = 1500
/** the full label: held, then faded out */
export const LABEL_HOLD_MS = 6000
export const LABEL_FADE_MS = 3000
/** the faint line and marker after it: kept this long from the start, the last part fading */
export const LINGER_MS = 60_000
export const LINGER_FADE_MS = 15_000
/** the banner */
export const BANNER_MS = 45_000
/** An act first seen within this of its observation plays from when it is first seen (it took
 *  the bridge a moment to report it); one seen later is placed at its observation. */
export const FRESH_MS = 5000
/** at most this many acts drawn at once (the newest), and full labels on at most this many */
export const MAX_DRAWN = 8
export const MAX_LABELS = 2

/** When an act's fire event starts on this console: when it was first seen, if that was soon
 *  after it was observed; else its observation (so an old act is not replayed). */
export function animStart(observedT: number, firstSeen: number): number {
  return firstSeen - observedT <= FRESH_MS ? Math.max(observedT, firstSeen) : observedT
}

/** The streak at `age` ms from the start, as fractions of the way from the author to the victim:
 *  [tail, head]; null before or after it. */
export function streakSpan(age: number): [number, number] | null {
  if (!(age >= 0 && age < STREAK_MS)) return null
  const head = Math.min(1, age / TRAVEL_MS)
  const tail = Math.max(0, Math.min(1, (age - TAIL_LAG_MS) / TRAVEL_MS))
  return head > tail ? [tail, head] : null
}

/** A flash's progress, 0 as it starts to 1 as it ends; null outside it. */
function flash(age: number, from: number, span: number): number | null {
  const p = (age - from) / span
  return p >= 0 && p < 1 ? p : null
}
export const muzzleFlash = (age: number) => flash(age, 0, MUZZLE_MS)
export const hitFlash = (age: number) => flash(age, TRAVEL_MS, HIT_MS)

/** The full label's opacity: 1, then down to 0 over LABEL_FADE_MS. */
export function labelAlpha(age: number): number {
  if (!(age >= 0)) return 0
  if (age < LABEL_HOLD_MS) return 1
  return Math.max(0, 1 - (age - LABEL_HOLD_MS) / LABEL_FADE_MS)
}

/** The lingering line and marker's opacity: 1, the last LINGER_FADE_MS down to 0. */
export function lingerAlpha(age: number): number {
  if (!(age >= 0) || age >= LINGER_MS) return 0
  return Math.min(1, (LINGER_MS - age) / LINGER_FADE_MS)
}

/** Animation frames are wanted: its streak, a flash or the label's fade is under way. */
export function animating(age: number): boolean {
  return age >= 0 && age < LABEL_HOLD_MS + LABEL_FADE_MS
}

// ---- geometry ----------------------------------------------------------------------------------

/** the point a fraction `f` of the way from `a` to `b` (straight in lon, lat and height: a few
 *  kilometres at most) */
export function along(a: Pt3, b: Pt3, f: number): Pt3 {
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f]
}

/** A dashed line from `a` to `b`: `n` periods, the first half of each drawn. */
export function dashes(a: Pt3, b: Pt3, n = 14): [Pt3, Pt3][] {
  const out: [Pt3, Pt3][] = []
  for (let k = 0; k < n; k++) out.push([along(a, b, k / n), along(a, b, (k + 0.5) / n)])
  return out
}

/** The streak's points at `age`, tail first, `n` + 1 of them (per-point colour and width fade
 *  along it); empty when there is no streak or no victim to fly to. */
export function streakPoints(a: Pt3, b: Pt3, age: number, n = 6): Pt3[] {
  const s = streakSpan(age)
  if (!s) return []
  const [f0, f1] = s
  const out: Pt3[] = []
  for (let k = 0; k <= n; k++) out.push(along(a, b, f0 + ((f1 - f0) * k) / n))
  return out
}

// ---- what is drawn -----------------------------------------------------------------------------

/** The acts drawn at `now`: started, within LINGER_MS, not dismissed (an act that started at or
 *  before `dismissedAt`), the newest MAX_DRAWN, newest first. */
export function visibleActs(ds: ActDrawing[], now: number, dismissedAt: number | null): ActDrawing[] {
  return ds
    .filter((d) => now - d.start >= 0 && now - d.start < LINGER_MS && !(dismissedAt != null && d.start <= dismissedAt))
    .sort((a, b) => b.start - a.start)
    .slice(0, MAX_DRAWN)
}

/** Of the acts drawn, those with a full label (the newest MAX_LABELS whose label is still on);
 *  the rest carry only their id. */
export function labelledKeys(visible: ActDrawing[], now: number): Set<string> {
  return new Set(visible.filter((d) => labelAlpha(now - d.start) > 0).slice(0, MAX_LABELS).map((d) => d.key))
}

/** The banner's act: the newest started within BANNER_MS and not dismissed; with how many other
 *  acts started in the last LINGER_MS. */
export function bannerAct(ds: ActDrawing[], now: number, dismissedAt: number | null): { d: ActDrawing; others: number } | null {
  const v = visibleActs(ds, now, dismissedAt)
  const d = v[0]
  if (!d || now - d.start >= BANNER_MS) return null
  return { d, others: v.length - 1 }
}

// ---- words -------------------------------------------------------------------------------------

/** "fires on HUNTER 1" */
export function headline(d: ActDrawing): string {
  return `Act ${d.act}: fires on ${d.victimName}`
}

/** "from T-0006 (0.97) · seen by HUNTER 1 EO/IR and sonar", or "author unattributed (flag F-2) · ..." */
export function subline(d: ActDrawing): string {
  const from = d.authorTrack
    ? `from ${d.authorTrack}${d.attribution != null ? ` (${d.attribution.toFixed(2)})` : ''}`
    : `author unattributed${d.unattributedNote ? ` (${d.unattributedNote})` : ''}`
  return d.seenBy ? `${from} · seen by ${d.seenBy}` : from
}

/** the map's label: two lines */
export function mapLabel(d: ActDrawing): string {
  return `${headline(d)}\n${subline(d)}`
}

/** the banner's line: "T-0006 fired on HUNTER 1", "Unattributed fire on HUNTER 1" */
export function bannerLine(d: ActDrawing): string {
  return d.authorTrack ? `${d.authorTrack} fired on ${d.victimName}` : `Unattributed fire on ${d.victimName}`
}

// ---- from what the bridge sends ----------------------------------------------------------------

/** Who the engine says the author was: the track it declared on; on a recorded act (the rule off,
 *  or the track already declared) its best candidate; a flagged act is unattributed. */
export function attributionOf(a: ObservedAct): { track: string | null; score: number | null; note: string | null } {
  const v = a.verdict
  if (v.verdict === 'declare') return { track: v.track, score: v.attribution, note: null }
  if (v.verdict === 'flag') return { track: null, score: null, note: a.flag ? `flag ${a.flag}` : 'flagged' }
  const best = a.candidates[0]
  return best ? { track: best.track, score: best.confidence, note: null } : { track: null, score: null, note: 'no track near' }
}

/** what the console knew when it first saw an act */
export interface Seen {
  start: number
  victim: Victim | null
}

/** The acts as drawn, from the inbox's `acts`. `seen` keeps, per act, when this console first
 *  saw it and where its victim was then (updated here: a new act is noted at `now`; a victim not
 *  found yet is looked for again while the act is fresh). `victimAt`: where the victim is, at the
 *  act's time where that can be said (a track's prediction); `nameOf`: a vehicle's callsign. */
export function actDrawings(
  acts: ObservedAct[],
  seen: Map<string, Seen>,
  now: number,
  victimAt: (a: Against, t: number) => Victim | null,
  nameOf: (uid: string) => string,
): ActDrawing[] {
  const out: ActDrawing[] = []
  for (const a of acts) {
    const r = a.act
    const key = `${r.act}@${r.observed_t}`
    let s = seen.get(key)
    if (!s) {
      s = { start: animStart(r.observed_t, now), victim: victimAt(r.against, r.observed_t) }
      seen.set(key, s)
    } else if (!s.victim && now - s.start < FRESH_MS) s.victim = victimAt(r.against, r.observed_t)
    const who = attributionOf(a)
    out.push({
      key,
      act: r.act,
      observedT: r.observed_t,
      start: s.start,
      author: r.location,
      sigma_m: r.sigma_m,
      authorTrack: who.track,
      attribution: who.score,
      unattributedNote: who.note,
      victim: s.victim,
      victimName: r.against.kind === 'vehicle' ? nameOf(r.against.uid) : r.against.kind === 'track' ? `protected track ${r.against.track}` : 'something we hold no track of',
      seenBy: sensorsText(r.sensors, nameOf),
    })
  }
  return out
}
