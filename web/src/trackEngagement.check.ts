// Checks for trackEngagement.ts (no test runner in web/): node web/src/trackEngagement.check.ts,
// as make check does. Throws on the first failure, prints "trackEngagement: ok" otherwise. Not
// part of the bundle.
import assert from 'node:assert/strict'
import { trackEngagement } from './trackEngagement.ts'

const nameOf = (uid: string) => ({ '2': 'REAPER 1' })[uid] ?? `vehicle ${uid}`

const intent = (id: number, state: string, extra: Record<string, unknown> = {}) => ({ id, kind: 'engage', state, uids: ['2'], params: { track: 'T-0008' }, ...extra })
assert.equal(trackEngagement([], 'T-0008', nameOf), null, 'none: the card offers Nominate as before')
assert.deepEqual(trackEngagement([intent(2, 'dispatched')], 'T-0008', nameOf), { id: 2, inPlay: true, line: 'Engagement i2 under way: REAPER 1 en route' })
assert.equal(trackEngagement([intent(2, 'pending_approval')], 'T-0008', nameOf)?.inPlay, true, 'waiting for signatures is in play')
assert.equal(trackEngagement([intent(2, 'dispatched')], 'T-0009', nameOf), null, 'another track')
assert.equal(trackEngagement([{ ...intent(2, 'dispatched'), kind: 'orbit' }], 'T-0008', nameOf), null, 'not an engagement')
const assessed = intent(2, 'done', { engagement: { snapshot: { track_id: 'T-0008' }, assessment: { result: 'persists' } } })
assert.deepEqual(trackEngagement([assessed], 'T-0008', nameOf), { id: 2, inPlay: false, line: 'Engagement i2 assessed: persists' })
assert.equal(trackEngagement([intent(3, 'rejected'), assessed], 'T-0008', nameOf)?.line, 'Engagement i3 rejected', 'the newest that ended')
assert.equal(trackEngagement([assessed, intent(5, 'effected')], 'T-0008', nameOf)?.id, 5, 'one in play wins over a newer one that ended')
assert.equal(
  trackEngagement([{ ...intent(4, 'approved'), params: {}, engage: { track: 'T-0008' } }], 'T-0008', nameOf)?.line,
  'Engagement i4 approved: REAPER 1 sets off',
  "the bridge's view names the track",
)

console.log('trackEngagement: ok')
