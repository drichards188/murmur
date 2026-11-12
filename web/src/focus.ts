// What is in focus on the map (docs/research/saturation-display.md R1, R2): the few tracks, lost
// contacts and vehicles the person must see in full, the rest drawn dim. Display only: nothing
// here changes a decision, the audit log or replay, and nothing in it ranks or recolours a track
// (the reason is said wherever it is used, and the affiliation stays the tracker's and the
// person's).
//
// In focus, a track is: selected or hovered; pinned by the person; hostile or suspect; declared
// (by anyone, the engine's rule included); flagged for review, or named by a review item waiting
// on the person; an engagement's target or a blocker of its keep-out (in play, waiting, or on the
// nomination form). A lost contact the same, by its last affiliation. Our vehicle: selected, or
// its details open.
//
// Pure (no browser, no milsymbol): focus.check.ts runs it under node.
import type { Intent } from './decisions'
import type { KeepOutExplanation } from './engage'
import type { Review } from './ident'
import type { Blocking } from './blocking'
import type { LostContact, Track } from './tracks'

export type FocusReason = 'selected' | 'pinned' | 'hostile' | 'suspect' | 'declared' | 'flagged' | 'review' | 'engagement' | 'hovered'

/** In words, for a label's or a legend's title: why it is drawn in full. */
export const FOCUS_WORDS: Record<FocusReason, string> = {
  selected: 'selected',
  pinned: 'pinned by you',
  hostile: 'hostile',
  suspect: 'suspect',
  declared: 'declared',
  flagged: 'flagged for review',
  review: 'a review item waits on you',
  engagement: "an engagement's target or blocker",
  hovered: 'under the pointer',
}

export interface FocusInput {
  selected: string | null
  hovered?: string | null
  pinned: ReadonlySet<string>
  /** tracks a review item names (declarations to confirm or revoke, open flags, what holds up a nomination) */
  review: ReadonlySet<string>
  /** tracks an engagement in play or on the form rests on: its target, its keep-out's blockers */
  engagement: ReadonlySet<string>
}

/** Why a track is in focus, the first reason that holds, or null (drawn dim). */
export function trackFocus(t: Pick<Track, 'id' | 'affiliation' | 'declared' | 'review'>, f: FocusInput): FocusReason | null {
  if (t.id === f.selected) return 'selected'
  if (f.pinned.has(t.id)) return 'pinned'
  if (t.affiliation === 'hostile') return 'hostile'
  if (t.affiliation === 'suspect') return 'suspect'
  if (t.declared) return 'declared'
  if ((t.review?.length ?? 0) > 0) return 'flagged'
  if (f.review.has(t.id)) return 'review'
  if (f.engagement.has(t.id)) return 'engagement'
  if (t.id === f.hovered) return 'hovered'
  return null
}

/** Why a lost contact (rules 16) is in focus, or null: its possible area then drawn faint. */
export function lostFocus(l: Pick<LostContact, 'id' | 'affiliation' | 'declared_by'>, f: FocusInput): FocusReason | null {
  if (l.id === f.selected) return 'selected'
  if (f.pinned.has(l.id)) return 'pinned'
  if (l.affiliation === 'hostile') return 'hostile'
  if (l.affiliation === 'suspect') return 'suspect'
  if (l.declared_by) return 'declared'
  if (f.review.has(l.id)) return 'review'
  if (f.engagement.has(l.id)) return 'engagement'
  if (l.id === f.hovered) return 'hovered'
  return null
}

/** The focus of every track and lost contact in focus, by id (the rest are absent). */
export function focusMap(tracks: readonly Track[], lost: readonly LostContact[], f: FocusInput): Map<string, FocusReason> {
  const out = new Map<string, FocusReason>()
  for (const t of tracks) {
    const r = trackFocus(t, f)
    if (r) out.set(t.id, r)
  }
  for (const l of lost) {
    const r = lostFocus(l, f)
    if (r && !out.has(l.id)) out.set(l.id, r)
  }
  return out
}

/** A focus that puts its label first (R2's priority 1): any but the pointer's (a hover shows the
 *  full block in a tooltip, and must not move the other labels about). */
export function labelFirst(r: FocusReason | null | undefined): boolean {
  return r != null && r !== 'hovered'
}

/** The tracks the review names: rule declarations awaiting confirmation, open flags, and what
 *  holds up a nomination. */
export function reviewTracks(review: Review | null | undefined, blocking: readonly Blocking[] | null | undefined): Set<string> {
  const out = new Set<string>()
  for (const d of review?.declarations ?? []) out.add(d.track)
  for (const fl of review?.flags ?? []) for (const t of fl.tracks) out.add(t)
  for (const b of blocking ?? []) out.add(b.track)
  return out
}

const ENGAGE_IN_PLAY = new Set(['pending_approval', 'approved', 'dispatched', 'effected'])

/** The tracks an engagement rests on: each engagement waiting or in play, its target and the
 *  blockers its card names (accepted as behaving, lost contacts open to accept); the nomination
 *  form's target and every track or lost contact its keep-out explanation lists. */
export function engagementTracks(intents: readonly Intent[] | null | undefined, keepOuts: KeepOutExplanation | null | undefined, formTrack?: string | null): Set<string> {
  const out = new Set<string>()
  if (formTrack) out.add(formTrack)
  if (keepOuts) {
    out.add(keepOuts.track)
    for (const it of keepOuts.items) if (it.subject !== 'vehicle') out.add(it.id)
  }
  for (const i of intents ?? []) {
    if (i.kind !== 'engage' || !ENGAGE_IN_PLAY.has(i.state)) continue
    const target = i.engagement?.snapshot?.track_id ?? i.engage?.track
    if (target) out.add(target)
    for (const b of i.engage?.behaving ?? []) if (b.track) out.add(b.track)
    for (const b of i.engage?.accepted_behaving ?? []) if (b.track) out.add(b.track)
    for (const l of i.engage?.lost_open ?? []) out.add(l.track)
  }
  return out
}
