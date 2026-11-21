import { agoWords, type Pending } from './blocking'
import { FLAG_LABEL, type FlagKind } from './ident'

/** A review item waiting for a person, in a line (the pending strip's and the Sitrep's). */
export function pendingLine(p: Pending, nowMs: number): string {
  const ago = agoWords(p.t, nowMs)
  if (p.kind === 'declaration') return `${p.tracks[0]} declared ${p.affiliation ?? 'hostile'} by rule ${ago} ago`
  const what = (p.flag && FLAG_LABEL[p.flag as FlagKind]) || 'flagged'
  return `${what} on ${p.tracks.join(', ')}, raised ${ago} ago`
}
