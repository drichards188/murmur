// Checks for detailsText.ts (no test runner in web/): node web/src/detailsText.check.ts, as make
// check does. The claims (ledger 1 section 14, changes 33 and 34): a normal link is one line,
// "SATCOM, good"; an abnormal one says what failed, the worst first; a silent vehicle's estimate is
// a distance and a compass point from its last report, not coordinates. Throws on the first failure,
// prints "detailsText: ok" otherwise. Not part of the bundle.
import assert from 'node:assert/strict'
import { awayText, compass, linkLine, type LinkFacts } from './detailsText.ts'
import { LOSS_PCT } from './vehicleException.ts'

const ok: LinkFacts = { type: 'SATCOM', stale: false, sinceMs: 300, expectedMs: 1000, lossPct: 0, emu: { state: 'ok', cause: '' } }

assert.deepEqual(linkLine(ok), { text: 'SATCOM, good', tone: '' }, 'normal: one line')
assert.deepEqual(linkLine({ ...ok, stale: true, sinceMs: 12_400 }), { text: 'SATCOM: silent 12 s', tone: 'bad' })
assert.deepEqual(linkLine({ ...ok, emu: { state: 'down', cause: 'jammer' } }), { text: 'SATCOM: down: jammer', tone: 'bad' })
assert.deepEqual(linkLine({ ...ok, emu: { state: 'degraded', cause: 'out of range' } }), { text: 'SATCOM: degraded: out of range', tone: 'warn' })
assert.equal(linkLine({ ...ok, lossPct: LOSS_PCT + 3 }).text, `SATCOM: ${LOSS_PCT + 3}% loss`)
assert.equal(linkLine({ ...ok, lossPct: LOSS_PCT }).text, 'SATCOM, good', 'loss at the threshold is normal')
assert.equal(linkLine({ ...ok, sinceMs: 2500 }).text, 'SATCOM: last heard 2 s ago', 'late against its expected interval')
assert.equal(linkLine({ ...ok, sinceMs: 25_000, expectedMs: 20_000 }).text, 'SATCOM, good', 'a low-rate link heard on time')
assert.equal(linkLine({ ...ok, stale: true, sinceMs: 9000, emu: { state: 'down', cause: 'jammer' } }).text, 'SATCOM: silent 9 s', 'silent first')
assert.equal(linkLine({ ...ok, emu: null }).text, 'SATCOM, good', 'no emulator: the bridge\'s own numbers')

assert.equal(compass(44), 'NE')
assert.equal(compass(359), 'N')
assert.equal(compass(-90), 'W')
assert.equal(awayText(1234, 45), 'about 1.2 km NE of its last report')
assert.equal(awayText(420, 180), 'about 420 m S of its last report')
assert.equal(awayText(0.4, 10), 'where it last reported')

console.log('detailsText: ok')
