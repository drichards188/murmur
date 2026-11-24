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
    if (!x) continue
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
