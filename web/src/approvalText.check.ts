// Checks for approvalText.ts: the ask sentence per kind, the button names.
import assert from 'node:assert/strict'
import { askSentence, DONT_SEND, placeLine, sendLabel, subjectOf, type AskFleet } from './approvalText.ts'

const fleet: AskFleet = {
  callsign: (u) => `V${u}`,
  model: () => 'PUMA',
  swarm: (u) => (Number(u) < 10 ? 'PICKET' : null),
}
const ring: [number, number][] = [
  [-122.45, 37.8],
  [-122.45, 37.81],
  [-122.44, 37.81],
]
const many = (n: number) => Array.from({ length: n }, (_, k) => String(k))

assert.equal(subjectOf({ kind: 'hold', params: {}, uids: ['3'] }, fleet), 'V3')
assert.equal(subjectOf({ kind: 'hold', params: {}, uids: ['1', '2', '3'] }, fleet), 'V1, V2, V3')
assert.equal(subjectOf({ kind: 'hold', params: {}, uids: many(8) }, fleet), '8 PICKET vehicles')
assert.equal(subjectOf({ kind: 'hold', params: {}, uids: ['20', '21', '22', '23'] }, fleet), '4 PUMA vehicles')
assert.equal(subjectOf({ kind: 'hold', params: {}, uids: many(5), group: { swarm: 'CRISSY' } }, fleet), '5 CRISSY vehicles')

assert.equal(askSentence({ kind: 'orbit', params: { radius_m: 2000, center: ring[0] }, uids: ['1'] }, fleet), 'Send V1 to orbit, 2.0 km radius')
assert.equal(askSentence({ kind: 'orbit', params: { radius_m: 250 }, uids: ['1'] }, fleet, true), 'Sent V1 to orbit, 250 m radius')
assert.match(askSentence({ kind: 'patrol', params: { route: ring }, uids: many(10) }, fleet), /^Send 10 PICKET vehicles to patrol a [\d.]+ km loop of 3 waypoints$/)
assert.match(askSentence({ kind: 'transit', params: { route: ring }, uids: ['1'] }, fleet), /^Send V1 along a .* route of 3 points/)
assert.equal(askSentence({ kind: 'rtl', params: {}, uids: ['1', '2'] }, fleet), 'Send V1, V2 to return to launch')
assert.equal(askSentence({ kind: 'identify', params: { track: 'T-0008' }, uids: ['1'] }, fleet), 'Send V1 to identify T-0008')
assert.equal(askSentence({ kind: 'collect', params: {}, uids: [] }, fleet), 'Task a satellite collection')
assert.equal(askSentence({ kind: 'engage', params: { track: 'T-0008' }, uids: ['4'] }, fleet), 'Engage T-0008 with V4')
assert.equal(askSentence({ kind: 'lay', params: { count: 6, weapon: 'TM-62M' }, uids: ['4'] }, fleet, true), 'Laid 6 TM-62M with V4')

assert.equal(placeLine({ kind: 'orbit', params: { center: [-122.458, 37.8125] }, uids: [] }), 'centre 37.8125, -122.4580')
assert.equal(placeLine({ kind: 'hold', params: {}, uids: [] }), null)

assert.equal(sendLabel('patrol'), 'Send the patrol')
assert.equal(sendLabel('something'), 'Send the order')
assert.equal(DONT_SEND, "Don't send")
console.log('approvalText.check ok')
