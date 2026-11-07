// Checks for blocking.ts (no test runner in web/): node web/src/blocking.check.ts, as make check
// does. Throws on the first failure, prints "blocking: ok" otherwise. Not part of the bundle.
import { type Blocking, holdingFor, holdingOnTrack, namedBlocking, stripLine, waited, waitingItems } from './blocking.ts'

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

console.log('blocking: ok')
