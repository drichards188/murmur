// Checks for situationText.ts (no test runner in web/): node web/src/situationText.check.ts, as
// make check does. Throws on the first failure, prints "situationText: ok" otherwise. The records
// are as eval/proxy.py writes them; the confirmation's reason is the one the panel showed in
// hostile-act-engagement (logs/ui-sweep, T+8:29). Not part of the bundle.
import assert from 'node:assert/strict'
import { scriptedLine, situationZero } from './situationText.ts'

const nameOf = (uid: string) => ({ '1': 'HUNTER 1', '2': 'REAPER 1' })[uid] ?? `vehicle ${uid}`
const zero = 1795517866.0
const reason =
  'scripted (situation hostile-act-engagement, autoplay): op-auto (not a person): confirms every rule declaration (confirm_rule_declarations): ' +
  'declared hostile by rule hostile_act (D-1) on act A-2 against our vehicle 1, observed by 1:eo_ir at t=1795518091266, attribution 1.00 >= 0.90; not confirmed by an operator'
const confirm = { t: zero + 230, intent_id: null, action: 'confirm', by: 'op-auto', declaration: 'D-1', track: 'T-0008', result: 'confirmed', reason }
const l = scriptedLine(confirm, zero, nameOf)
assert.equal(l.at, 'T+3:50')
assert.equal(l.text, 'op-auto confirmed D-1 on T-0008', 'said once, as what it was')
assert.equal(l.why, 'scripted (situation hostile-act-engagement, autoplay): op-auto (not a person): confirms every rule declaration (confirm_rule_declarations)')
assert.ok(!l.why!.includes('not confirmed'), 'never also said as unconfirmed')
assert.equal(
  l.read,
  'As op-auto read it before confirming: declared hostile by rule hostile_act (D-1) on act A-2 against HUNTER 1, observed by HUNTER 1 EO/IR at T+3:45, attribution 1.00 >= 0.90; not confirmed by an operator',
  'what it read stays reachable, said as read before, its epoch on the situation clock',
)
assert.equal(scriptedLine({ ...confirm, result: 'error 409: no rule declaration in force' }, zero, nameOf).text, 'op-auto confirmed D-1 on T-0008: error 409: no rule declaration in force')
assert.equal(scriptedLine(confirm, null, nameOf).at, null, 'no clock yet: no time')

// the rest of what the scripts decide
assert.equal(scriptedLine({ t: zero, intent_id: 9, action: 'approve', by: 'op-auto', granted: true, result: 'dispatched' }, zero, nameOf).text, 'op-auto approved i9 (it grants every request of the AI): dispatched')
assert.equal(scriptedLine({ t: zero, intent_id: 10, action: 'sign', by: 'op-2', result: 'pending_approval' }, zero, nameOf).text, 'op-2 signed i10: pending_approval')
assert.equal(scriptedLine({ t: zero, intent_id: 10, action: 'sign_failed', by: 'op-3', result: 'error 409: stale' }, zero, nameOf).text, 'op-3 could not sign i10: error 409: stale')
assert.equal(
  scriptedLine({ t: zero, intent_id: null, action: 'confirm_lost', by: 'op-2', uid: 1, callsign: null, result: 'confirmed lost (ended [1])' }, zero, nameOf).text,
  'op-2 confirmed HUNTER 1 lost: confirmed lost (ended [1])',
  'a loss confirmed: never "confirm_lost inull"',
)
assert.equal(situationZero({ t: zero + 509, elapsed_s: 509 }), zero)
assert.equal(situationZero({ t: zero, elapsed_s: null }), null)

console.log('situationText: ok')
