// Checks for blocking.ts (no test runner in web/): node web/src/blocking.check.ts, as make check
// does. Throws on the first failure, prints "blocking: ok" otherwise. Not part of the bundle.
import { agoWords, type Blocking, holdingFor, holdingOnTrack, namedBlocking, pendingItems, stripLine, waited, waitingItems } from './blocking.ts'

function eq(a: unknown, b: unknown, what: string) {
  const x = JSON.stringify(a)
  const y = JSON.stringify(b)
  if (x !== y) throw new Error(`${what}: got ${x}, want ${y}`)
}

function item(id: string, track: string, since: number, kind: Blocking['kind'] = 'declaration'): Blocking {
  return {
    id,
    kind,
    track,
    holds_up: 'nomination',
    platforms: [{ uid: 3, callsign: 'REAPER 1' }],
    raised_t: since - 5,
    since_t: since,
    act: 'A-2',
    waits_for: `${track} declared hostile by the engine's rule on A-2, not confirmed by an operator`,
    unblocks: `Confirming ${id} lets a nomination on ${track} go ahead`,
    who: 'any operator',
    text: `Holding up: REAPER 1's nomination on ${track}. Every other check passes.`,
    failing: 'the policy needs an operator confirmation before a nomination',
  }
}

const d1 = item('D-1', 'T-0008', 1000)
const f2 = item('F-2', 'T-0009', 990, 'flag')
const f2b = item('F-2', 'T-0010', 995, 'flag')

// the one waiting longest first, one per review item
eq(waitingItems([d1, f2b, f2]).map((b) => `${b.id}@${b.track}`), ['F-2@T-0009', 'D-1@T-0008'], 'order and one per id')
eq(waitingItems(undefined), [], 'none')
eq(holdingFor([d1, f2], 'D-1')?.track, 'T-0008', 'by id')
eq(holdingFor([d1], 'D-2'), undefined, 'no such id')
eq(holdingOnTrack([d1, f2], 'T-0008')?.id, 'D-1', 'a declaration on the track')
eq(holdingOnTrack([d1, f2], 'T-0009'), undefined, 'a flag holds no declaration')

// how long it waited, on the bridge clock
eq(waited(d1, 1000_000), '0:00', 'just now')
eq(waited(d1, 1041_400), '0:41', 'seconds')
eq(waited(d1, (1000 + 3723) * 1000), '1:02:03', 'hours')
eq(waited(d1, 900_000), '0:00', 'never negative')

eq(stripLine(d1), "D-1: T-0008 declared hostile by the engine's rule on A-2, not confirmed by an operator. Holding up: REAPER 1's nomination on T-0008. Every other check passes.", 'strip line')

// a note names an item by id as a word, or its writer attached it
const note = (text: string, blocking?: string[]) => ({ text, blocking })
eq(namedBlocking(note("I can't nominate on T-0008 until you confirm D-1."), [d1]).map((b) => b.id), ['D-1'], 'named in the text')
eq(namedBlocking(note('waiting on D-12 and XD-1'), [d1]), [], 'not D-12, not a longer word')
eq(namedBlocking(note('D-1, then'), [d1]).length, 1, 'at the start')
eq(namedBlocking(note('No action.', ['F-2']), [d1, f2]).map((b) => b.id), ['F-2'], 'attached by the driver')
eq(namedBlocking(note('Confirm D-1.', ['D-1']), []), [], 'nothing holds anything up now: not urgent')

// the calm pointer's items (R4): every rule declaration and open flag, oldest first, except what the
// urgent strip already has; a flag no longer open is not pending
const review = {
  declarations: [
    { id: 'D-3', track: 'T-0011', t: 1200, affiliation: 'hostile' },
    { id: 'D-1', track: 'T-0008', t: 1000, affiliation: 'hostile' },
  ],
  flags: [
    { id: 'F-4', tracks: ['T-0012', 'T-0013'], t: 1100, flag_kind: 'conflict', status: { status: 'open' } },
    { id: 'F-5', tracks: ['T-0014'], t: 900, flag_kind: 'attribution', status: { status: 'dismissed' } },
  ],
}
eq(pendingItems(review, []).map((p) => `${p.id}:${p.tracks.join('+')}`), ['D-1:T-0008', 'F-4:T-0012+T-0013', 'D-3:T-0011'], 'pending: oldest first, open flags only')
eq(pendingItems(review, [d1]).map((p) => p.id), ['F-4', 'D-3'], 'pending: what holds up a nomination is the urgent strip\'s')
eq(pendingItems(undefined, undefined), [], 'pending: none')
eq(pendingItems(review, [])[0].affiliation, 'hostile', 'pending: a declaration says its affiliation')
eq(pendingItems(review, [])[1].flag, 'conflict', "pending: a flag says its kind")
eq([agoWords(1000, 1_040_000), agoWords(1000, 1_300_000), agoWords(1000, 1_000_000 + 3_900_000)], ['40 s', '5 min', '1 h 05 min'], 'pending: how long ago')

console.log('blocking: ok')
