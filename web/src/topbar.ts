// The top bar's glance (bd murmur-iobd, the cutdown's ledger 1 section 1): the Sitrep's bottom line
// as one quiet line (how many of ours, how many contacts and how many of them matter) and one count
// of ours in trouble, made by the same function as each fleet row's and map symbol's exception slot
// (vehicleException.ts `exceptionOf`), so the count and the slots never disagree. Quiet when normal:
// the trouble count shows only above 0, and only hostile and suspect contacts take colour.
//
// Display only. Pure (no browser): topbar.check.ts runs it under node.
import { headline } from './sitrep.ts'
import type { Track } from './tracks'
import type { Vehicle } from './types'
import { callsign } from './types.ts'
import { exceptionOf, type ExceptionContext, type ExceptionTone } from './vehicleException.ts'

/** Group scale: more of ours than this and the Sitrep's button shows (its report summarises what
 *  the lists cannot be read for at a glance). The open world's 22 vehicles stay under it. */
export const GROUP_SCALE = 40

export function groupScale(vehicles: number): boolean {
  return vehicles > GROUP_SCALE
}

/** Ours in trouble: how many, the worst tone among them (the slot's own), and which, in fleet order. */
export interface Trouble {
  n: number
  tone: ExceptionTone | null
  uids: number[]
  /** for the hover: each one and its slot's words */
  says: string
}

const TONE_RANK: Record<ExceptionTone, number> = { bad: 0, warn: 1, settled: 2 }

/** At most this many named in the hover; the rest counted. */
const NAMED = 6

export function troubleOf(vehicles: readonly Vehicle[], ctx: ExceptionContext): Trouble {
  const uids: number[] = []
  const lines: string[] = []
  let tone: ExceptionTone | null = null
  for (const v of vehicles) {
    const x = exceptionOf(v, ctx)
    // a degraded link (measured loss, an emulator's degradation) is the row's warning, not trouble
    // the whole bar should carry: the vehicle is heard and does what it is told (seen live: 12 to
    // 20 % loss on a busy host kept the bar amber with nothing to act on)
    if (!x || x.kind === 'degraded') continue
    uids.push(v.uid)
    if (lines.length < NAMED) lines.push(`${callsign(v)}: ${x.text.toLowerCase()}`)
    if (tone == null || TONE_RANK[x.tone] < TONE_RANK[tone]) tone = x.tone
  }
  const more = uids.length - lines.length
  const says = uids.length
    ? `${uids.length} of ours in trouble: ${lines.join('; ')}${more > 0 ? `; ${more} more` : ''}. A click shows the next one in the Fleet list.`
    : 'None of ours in trouble.'
  return { n: uids.length, tone, uids, says }
}

/** The one in trouble a click shows: the one after `last` in fleet order, from the first again
 *  after the last; null with none. */
export function nextTrouble(uids: readonly number[], last: number | null): number | null {
  if (!uids.length) return null
  const k = last == null ? -1 : uids.indexOf(last)
  return uids[(k + 1) % uids.length]
}

/** The glance line: ours, the contacts, and how many of them are hostile or suspect. */
export interface Glance {
  ours: number
  contacts: number
  hostile: number
  suspect: number
}

export function glanceOf(vehicles: readonly Pick<Vehicle, 'link'>[], tracks: readonly Pick<Track, 'affiliation' | 'declared' | 'state'>[]): Glance {
  const h = headline({ scenario: null, vehicles, tracks, counts: null, blocking: null })
  return { ours: h.total, contacts: h.contacts, hostile: h.hostile, suspect: h.suspect }
}

/** "1 hostile, 2 suspect", or "" when none matters. */
export function mattersText(g: Pick<Glance, 'hostile' | 'suspect'>): string {
  return [g.hostile ? `${g.hostile} hostile` : '', g.suspect ? `${g.suspect} suspect` : ''].filter(Boolean).join(', ')
}

/** The contacts the glance's problem counts link to: the hostile and the suspect ones' ids, in
 *  picture order, by `glanceOf`'s own rule (a declaration's affiliation over the tracker's), so
 *  each list is as long as its count. */
export function mattersOf(tracks: readonly Pick<Track, 'id' | 'affiliation' | 'declared'>[]): { hostile: string[]; suspect: string[] } {
  const hostile: string[] = []
  const suspect: string[] = []
  for (const t of tracks) {
    const a = t.declared?.affiliation ?? t.affiliation
    if (a === 'hostile') hostile.push(t.id)
    else if (a === 'suspect') suspect.push(t.id)
  }
  return { hostile, suspect }
}

/** The one after `last` in a list, from the first again after the last; null with none: a click
 *  on a problem count shows the next one it counts. */
export function nextOf<T>(list: readonly T[], last: T | null): T | null {
  if (!list.length) return null
  const k = last == null ? -1 : list.indexOf(last)
  return list[(k + 1) % list.length]
}

/** One piece of the glance line: a count and the words after it, or the punctuation between two.
 *  A problem (`trouble`, `hostile`, `suspect`) links to what it counts; a total (`ours`,
 *  `contacts`) does not. */
export interface GlancePiece {
  kind: 'ours' | 'trouble' | 'contacts' | 'hostile' | 'suspect' | 'sep'
  /** the count (none on punctuation) */
  n?: number
  /** the words after the count, or the punctuation itself */
  text: string
}

/** `groups` one after another, `sep` between each two. */
function joinPieces(groups: GlancePiece[][], sep: string): GlancePiece[] {
  return groups.flatMap((g, k) => (k ? [{ kind: 'sep' as const, text: sep }, ...g] : g))
}

/** The glance line in words (the cockpit layout's top bar): "105 vehicles · 3 of ours in trouble ·
 *  2 contacts, 1 hostile". Folded, for a bar with no room for all of it, only the problems: "3 of
 *  ours in trouble · 1 hostile", and nothing while nothing is wrong. */
export function glanceWords(g: Glance, trouble: number, folded: boolean): GlancePiece[] {
  const ours: GlancePiece[] = folded ? [] : [{ kind: 'ours', n: g.ours, text: g.ours === 1 ? 'vehicle' : 'vehicles' }]
  if (trouble > 0) ours.push({ kind: 'trouble', n: trouble, text: 'of ours in trouble' })
  const theirs: GlancePiece[] = folded ? [] : [{ kind: 'contacts', n: g.contacts, text: g.contacts === 1 ? 'contact' : 'contacts' }]
  if (g.hostile > 0) theirs.push({ kind: 'hostile', n: g.hostile, text: 'hostile' })
  if (g.suspect > 0) theirs.push({ kind: 'suspect', n: g.suspect, text: 'suspect' })
  const sides = [joinPieces(ours.map((x) => [x]), ' · '), joinPieces(theirs.map((x) => [x]), ', ')]
  return joinPieces(sides.filter((s) => s.length > 0), ' · ')
}

/** Where a situation stands, as the top bar says it beside the run's title. */
export interface RunStanding {
  label: string
  /** the hover: what it means for the person */
  title: string
  /** quiet, or amber when the run is not as it should be (its runner silent, or failed) */
  tone: 'quiet' | 'warn'
}

/** A situation's standing (its runner's phase and modes, `GET /api/situation`): who decides while it
 *  runs ("You decide", "Autoplay", "Tutorial"), its phase when it does not ("Starting", "Ended",
 *  "Stopped", "Failed"), "Not reporting" once its runner has gone quiet. */
export function runStanding(r: { operator: 'live' | 'autoplay'; tutorial: boolean; phase: string; stale: boolean }): RunStanding {
  if (r.stale) return { label: 'Not reporting', tone: 'warn', title: "The situation's runner has stopped reporting: what shows is its last status. The stack may still be running." }
  if (r.phase === 'starting') return { label: 'Starting', tone: 'quiet', title: 'The situation is starting: its stack, its fleet and its opening orders' }
  if (r.phase === 'ended') return { label: 'Ended', tone: 'quiet', title: "The situation's time is up. Its stack stays up for you to look around until you end it." }
  if (r.phase === 'interrupted') return { label: 'Stopped', tone: 'quiet', title: 'The situation was stopped before its time was up. Its stack stays up until you end it.' }
  if (r.phase === 'failed') return { label: 'Failed', tone: 'warn', title: 'The situation failed: the Situation panel says why' }
  if (r.tutorial) return { label: 'Tutorial', tone: 'quiet', title: 'The tutorial: you decide at this console, and a card walks you through each step' }
  return r.operator === 'live'
    ? { label: 'You decide', tone: 'quiet', title: 'You decide at this console: every approval, review and signature asked of you is yours' }
    : { label: 'Autoplay', tone: 'quiet', title: 'Scripts decide: a scripted operator approves and scripted signers sign, and you watch. Scripts, never people.' }
}
