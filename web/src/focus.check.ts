// Checks for focus.ts (no test runner in web/): node web/src/focus.check.ts, as make check does.
// The claims (docs/research/saturation-display.md R1): a track is in focus when it is selected,
// pinned, hostile or suspect, declared (by the engine's rule too), flagged, named by a review item,
// an engagement's target or blocker, or hovered, and only then; the first reason that holds is the
// one given; a lost contact the same by its last affiliation; a hover never puts a label first;
// the review's and the engagements' tracks are gathered from the inbox's shapes, an ended
// engagement's left out. Throws on the first failure, prints "focus: ok" otherwise.
import type { Intent } from './decisions'
import type { KeepOutExplanation } from './engage'
import type { Review } from './ident'
import type { LostContact, Track } from './tracks'
import { engagementTracks, focusMap, labelFirst, lostFocus, reviewTracks, trackFocus, type FocusInput } from './focus.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`focus: ${what}`)
}

type T = Pick<Track, 'id' | 'affiliation' | 'declared' | 'review'>
const track = (id: string, extra: Partial<T> = {}): T => ({ id, affiliation: 'unknown', declared: null, ...extra })
const none: FocusInput = { selected: null, hovered: null, pinned: new Set(), review: new Set(), engagement: new Set() }

// an unknown track nobody looks at is dim
check(trackFocus(track('T-1'), none) === null, 'a plain unknown is out of focus')
check(trackFocus(track('T-1', { affiliation: 'neutral' }), none) === null, 'a plain neutral is out of focus')
// each reason
check(trackFocus(track('T-1'), { ...none, selected: 'T-1' }) === 'selected', 'selected')
check(trackFocus(track('T-1'), { ...none, pinned: new Set(['T-1']) }) === 'pinned', 'pinned')
check(trackFocus(track('T-1', { affiliation: 'hostile' }), none) === 'hostile', 'hostile')
check(trackFocus(track('T-1', { affiliation: 'suspect' }), none) === 'suspect', 'suspect')
const declared = { affiliation: 'neutral' as const, by: 'op-1', t: 1, note: null }
check(trackFocus(track('T-1', { affiliation: 'neutral', declared }), none) === 'declared', 'declared by an operator, whatever the affiliation')
check(trackFocus(track('T-1', { review: [{ flag: 3, kind: 'conflict', status: 'open', t: 1, evidence: 'x' }] }), none) === 'flagged', 'flagged on the track')
check(trackFocus(track('T-1'), { ...none, review: new Set(['T-1']) }) === 'review', 'named by a review item')
check(trackFocus(track('T-1'), { ...none, engagement: new Set(['T-1']) }) === 'engagement', "an engagement's target or blocker")
check(trackFocus(track('T-1'), { ...none, hovered: 'T-1' }) === 'hovered', 'hovered')
// the first reason that holds
check(trackFocus(track('T-1', { affiliation: 'hostile' }), { ...none, selected: 'T-1', hovered: 'T-1' }) === 'selected', 'selected before hostile and hovered')
check(trackFocus(track('T-1', { affiliation: 'hostile' }), { ...none, hovered: 'T-1' }) === 'hostile', 'hostile before hovered')
// only the hover never puts a label first
check(labelFirst('hostile') && labelFirst('engagement') && !labelFirst('hovered') && !labelFirst(null), 'label first for every reason but the hover')

// lost contacts
type L = Pick<LostContact, 'id' | 'affiliation' | 'declared_by'>
const lost = (id: string, extra: Partial<L> = {}): L => ({ id, affiliation: 'unknown', declared_by: null, ...extra })
check(lostFocus(lost('T-9'), none) === null, 'a lost unknown is out of focus')
check(lostFocus(lost('T-9', { affiliation: 'hostile' }), none) === 'hostile', 'a lost hostile is in focus')
check(lostFocus(lost('T-9', { declared_by: 'op-1' }), none) === 'declared', 'a lost declared contact is in focus')
check(lostFocus(lost('T-9'), { ...none, engagement: new Set(['T-9']) }) === 'engagement', 'a lost contact a keep-out names is in focus')

// the map of a picture: only what is in focus, by id
const tracks = [track('T-1'), track('T-2', { affiliation: 'hostile' }), track('T-3')] as Track[]
const lostList = [lost('T-7'), lost('T-8', { affiliation: 'suspect' })] as LostContact[]
const m = focusMap(tracks, lostList, { ...none, selected: 'T-3' })
check(m.size === 3 && m.get('T-2') === 'hostile' && m.get('T-3') === 'selected' && m.get('T-8') === 'suspect' && !m.has('T-1') && !m.has('T-7'), `the focus map (${[...m].join(' ')})`)

// the review's tracks: declarations, flags, what holds up a nomination
const review = {
  declarations: [{ kind: 'declaration', id: 'D-1', track: 'T-8', t: 1, affiliation: 'hostile', declared_by: 'rule', basis: '', declaration: {}, actions: ['confirm', 'revoke'] }],
  flags: [{ kind: 'flag', id: 'F-2', flag: 2, flag_kind: 'conflict', tracks: ['T-4', 'T-5'], related: null, act: null, t: 1, evidence: '', status: { status: 'open' }, raises_suspect: false, actions: [] }],
  resolved: [],
  confirmed: [],
  counts: { declarations: 1, flags: 1 },
} as unknown as Review
const r = reviewTracks(review, [{ track: 'T-6' } as never])
check(r.size === 4 && ['T-8', 'T-4', 'T-5', 'T-6'].every((id) => r.has(id)), `the review's tracks (${[...r].join(' ')})`)
check(reviewTracks(null, null).size === 0, 'no review, no tracks')

// the engagements' tracks: in play or waiting, never ended; the form's and its keep-outs'
const engage = (id: number, state: string, target: string, extra: object = {}) =>
  ({ id, kind: 'engage', state, engage: { track: target, ...extra } }) as unknown as Intent
const intents = [
  engage(1, 'pending_approval', 'T-10', { behaving: [{ track: 'T-11' }], lost_open: [{ track: 'T-12' }] }),
  engage(2, 'dispatched', 'T-13'),
  engage(3, 'done', 'T-14'),
  { id: 4, kind: 'orbit', state: 'dispatched' } as unknown as Intent,
]
const keepOuts = { track: 'T-20', items: [{ subject: 'track', id: 'T-21' }, { subject: 'lost_contact', id: 'T-22' }, { subject: 'vehicle', id: '7' }] } as unknown as KeepOutExplanation
const e = engagementTracks(intents, keepOuts, 'T-20')
const want = ['T-10', 'T-11', 'T-12', 'T-13', 'T-20', 'T-21', 'T-22']
check(e.size === want.length && want.every((id) => e.has(id)), `the engagements' tracks (${[...e].join(' ')})`)
check(!e.has('T-14') && !e.has('7'), 'an ended engagement and a vehicle blocker are not tracks in focus')

console.log('focus: ok')
