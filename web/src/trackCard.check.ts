// Checks for trackCard.ts (no test runner in web/): node web/src/trackCard.check.ts, as make check
// does. The claims (ledger 1 section 5, changes 15 and 16): the card leads with one line saying
// what the contact is and who says so, how fresh and where it heads; the estimate's reason names
// kinds in words; the position's uncertainty is a rounded distance; a held-up item's card says
// what deciding lets go on, without its number or a timer. Throws on the first failure, prints
// "trackCard: ok" otherwise. Not part of the bundle.
import assert from 'node:assert/strict'
import { bestLook, type CardTrack, holdingLine, leadLine, motionWords, positionWords, reasonWords } from './trackCard.ts'

const KINDS: Record<string, string> = { small_fast_boat: 'small fast boat', fishing_vessel: 'fishing vessel', cargo_ship: 'cargo ship' }
const kindLabel = (k: string | null) => (k == null ? 'unclassified' : (KINDS[k] ?? k.replace(/_/g, ' ')))
const name = (uid: number) => ({ 1: 'HUNTER 1', 2: 'REAPER 1' })[uid] ?? `vehicle ${uid}`

const boat: CardTrack = {
  affiliation: 'suspect',
  domain: 'surface',
  identity: {
    kind: 'small_fast_boat',
    level: 'identify',
    ais: null,
    supported_by: ['radar', 'eo_ir'],
    looks: [
      { uid: 2, sensor: 'radar', level: 'detect', confidence: 0.99 },
      { uid: 1, sensor: 'eo_ir', level: 'identify', confidence: 0.9 },
    ],
  },
  speed_mps: 15.2,
  course_deg: 82,
  alt_m: null,
  last_seen_t: 998,
}

assert.equal(leadLine(boat, 1000, kindLabel, name), "Suspect: small fast boat, no AIS, identified by HUNTER 1's camera; seen 2 s ago; 82° at 15 m/s")
assert.equal(bestLook(boat.identity.looks)?.uid, 1, 'the look that got furthest, not the surest detection')
// without looks (the legacy model): the sensors behind its identity
assert.equal(
  leadLine({ ...boat, affiliation: 'neutral', identity: { kind: 'fishing_vessel', level: 'identify', ais: { name: 'ANNA MARIE' }, supported_by: ['ais', 'radar'] } }, 1300, kindLabel, name),
  'Neutral: fishing vessel, AIS ANNA MARIE, identified by AIS, radar; seen 5 min ago; 82° at 15 m/s',
)
assert.equal(motionWords({ speed_mps: 0.1, course_deg: 12, alt_m: null, domain: 'surface' }), 'still')
assert.equal(motionWords({ speed_mps: 40, course_deg: 270, alt_m: 450.4, domain: 'air' }), '270° at 40 m/s, 450 m up')
assert.equal(motionWords({ speed_mps: 4, course_deg: 90, alt_m: -31, domain: 'subsurface' }), '90° at 4 m/s, 31 m deep')

assert.equal(reasonWords('fishing_vessel, small craft not required to carry AIS', kindLabel), 'fishing vessel, small craft not required to carry AIS')
assert.equal(reasonWords('cargo_ship not broadcasting AIS', kindLabel), 'cargo ship not broadcasting AIS')
assert.equal(reasonWords('fast small craft, no AIS', kindLabel), 'fast small craft, no AIS', 'words stay words')

assert.equal(positionWords(6.2), 'good to about 10 m')
assert.equal(positionWords(143), 'good to about 140 m')
assert.equal(positionWords(2400), 'good to about 2.4 km')

assert.equal(holdingLine({ kind: 'declaration', track: 'T-0008', platforms: [{ uid: 2, callsign: 'REAPER 1' }] }), "Confirming it lets REAPER 1's nomination on T-0008 go ahead: every other check passes.")
assert.equal(
  holdingLine({ kind: 'flag', track: 'T-0006', platforms: [{ uid: 2, callsign: 'REAPER 1' }, { uid: 3, callsign: null }] }),
  'Only a hostile declaration lets a nomination on T-0006 with REAPER 1 or vehicle 3 go ahead: every other check passes.',
)
assert.ok(!/[DF]-\d|\d:\d\d/.test(holdingLine({ kind: 'declaration', track: 'T-1', platforms: [] })), 'no item number, no timer')

console.log('trackCard: ok')
