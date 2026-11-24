// Checks for approvalModel.ts: the reserve line with its assumption, the callout, Now / After / Holds.
import assert from 'node:assert/strict'
import { afterText, callouts, holdsText, nowRows, parseReserve, reserveLine } from './approvalModel.ts'

// i177 of the group-command run: 9 min left against an 8 min reserve on a 19 min task
const thin = '9 min left after task (task 19 min, battery 73% of 39 min), reserve 8 min (20%)'
const roomy = '20 min left after task (task 19 min, battery 98% of 39 min), reserve 8 min (20%)'
assert.deepEqual(parseReserve(thin), { leftMin: 9, taskMin: 19, batteryPct: 73, enduranceMin: 39, reserveMin: 8, reservePct: 20 })
assert.equal(parseReserve('no surface constraint'), null)
const line = reserveLine([
  { rule: 'reserves', detail: roomy },
  { rule: 'reserves', detail: thin },
  { rule: 'geofence', detail: 'clear' },
])
assert.ok(line)
assert.equal(line.close, true, 'a one-minute margin is close, though every check passes')
assert.equal(line.margin, 1)
assert.match(line.text, /The tightest of them has 9 min left after the task against a 8 min reserve \(assuming the task takes 19 min/)
assert.equal(reserveLine([{ rule: 'reserves', detail: roomy }])?.close, false)
assert.equal(reserveLine([{ rule: 'geofence', detail: 'clear' }]), null, 'no figures, no line')

const names = (u: string) => `V${u}`
assert.deepEqual(callouts({ facts: undefined, extra: [], selfApproval: false, engaged: [], name: names }), [])
assert.deepEqual(
  callouts({ facts: { armed: ['4'], leaves_approved_area: true, area_detail: 'MARLIN 2 leaves it', unverified: ['reserves'] }, extra: [], selfApproval: true, engaged: [2], name: names }),
  [
    'V4 is armed.',
    'It leaves the approved area: MARLIN 2 leaves it.',
    'Could not be verified: reserves. Judge it yourself.',
    'You proposed this: approving it is a self-approval, and the decision log records that.',
    'Approving this stops engagement i2 on the same vehicle (the newest order stands; no fallback is commanded).',
  ],
)

assert.equal(afterText('patrol'), 'Each flies the route and loops it until another order replaces it.')
assert.equal(afterText('weird'), null)
assert.equal(holdsText({ kind: 'orbit', group: true, min: 7, reform: true }), 'Stays together while 7 or more are heard; below that it stops and asks you.')
assert.match(holdsText({ kind: 'transit', group: true, min: 3, reform: false }) ?? '', /Does not re-form/)
assert.equal(holdsText({ kind: 'transit', group: false, min: null, reform: false }), 'Ends at the last point.')
assert.equal(holdsText({ kind: 'orbit', group: false, min: null, reform: false }), null)

const doing = (u: string) => (Number(u) < 6 ? 'patrolling' : 'holding')
assert.deepEqual(nowRows(['1', '7'], doing, names), ['V1: patrolling', 'V7: holding'])
assert.deepEqual(nowRows(['0', '1', '2', '3', '4', '5', '6', '7'], doing, names), ['6 patrolling', '2 holding'])

console.log('approvalModel: ok')
