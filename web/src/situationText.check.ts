// Checks for situationText.ts (no test runner in web/): node web/src/situationText.check.ts, as
// make check does. Throws on the first failure, prints "situationText: ok" otherwise. The records
// are as eval/proxy.py writes them; the confirmation's reason is the one the panel showed in
// hostile-act-engagement (logs/ui-sweep, T+8:29). Not part of the bundle.
import assert from 'node:assert/strict'
import { opensInTopView, proposalState, scriptedLine, situationTime, situationZero, STALE_S, whatHappened, withoutCommands } from './situationText.ts'

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

// the clock on this console: run on from the last status while the runner runs, held otherwise
const clock = { elapsed_s: 100, duration_s: 720 }
assert.deepEqual(situationTime(clock, 'running', { elapsed: 100, at: 50 }, 53), { elapsed: 103, left: 617, stale: false, running: true, sincePost: 3 })
assert.equal(situationTime(clock, 'running', { elapsed: 100, at: 50 }, 50 + STALE_S + 1).stale, true, 'no status for STALE_S: not reporting')
assert.equal(situationTime(clock, 'running', { elapsed: 100, at: 50 }, 50 + STALE_S + 1).elapsed, 100, 'and its clock is held')
assert.equal(situationTime(clock, 'ended', { elapsed: 720, at: 50 }, 90).left, 0, 'ended: nothing left')
assert.equal(situationTime({ elapsed_s: 715, duration_s: 720 }, 'running', { elapsed: 715, at: 0 }, 9).elapsed, 720, 'never past its duration')
assert.equal(situationTime({ elapsed_s: null, duration_s: 720 }, 'starting', null, 9).elapsed, null, 'before it starts')

// the end line without the command
assert.equal(
  withoutCommands("the situation's time is up; the stack stays up for inspection until you end it (End in the menu, or make down)"),
  "the situation's time is up; the stack stays up for inspection until you end it (End in the menu)",
)
assert.equal(withoutCommands('stopping the runner; the stack keeps running: make down stops it'), 'stopping the runner; the stack keeps running')

// what happened: the timeline, the scripts' decisions and the AI's proposals, the newest first
const happened = whatHappened(
  [{ t_s: 230, kind: 'autoplay', text: 'op-auto (scripted) confirmed the rule declaration D-1 on T-0008' }, { t_s: 210, kind: 'red', text: 'T-0008 fires on our vehicle 1' }],
  [{ id: 4, kind: 'orbit', created_t: (zero + 60) * 1000, state: 'dispatched', uids: ['1'], approval: { by: 'op-auto' }, rationale: 'cover the channel' }],
  zero,
  'Playbook',
  new Set(['op-auto']),
  nameOf,
)
assert.deepEqual(
  happened.map((h) => [h.t_s, h.kind, h.text]),
  [
    [230, 'timeline', 'op-auto (scripted) confirmed the rule declaration D-1 on T-0008'],
    [210, 'timeline', 'T-0008 fires on HUNTER 1'],
    [60, 'ai', 'The AI (Playbook) proposed orbit i4 for HUNTER 1: approved by op-auto (scripted)'],
  ],
  'merged by time, in plain words, each scripted decision once (the timeline says it)',
)
assert.equal(happened[2].title, 'cover the channel', "the AI's rationale in the hover")
assert.equal(proposalState({ state: 'pending_approval', approval: null }, new Set()), 'waiting for a decision')
assert.equal(proposalState({ state: 'dispatched', approval: null }, new Set()), 'sent by policy')
assert.equal(proposalState({ state: 'queued', approval: { by: 'op-1' } }, new Set()), 'approved by op-1, held for its link')

// the map's first view: a situation with an armed platform opens in top view; the open world does not
assert.equal(opensInTopView(true, [{ identity: { armed: false } }, { identity: { armed: true } }]), true)
assert.equal(opensInTopView(true, [{ identity: { armed: false } }, { identity: null }]), false, 'no armed platform: as before')
assert.equal(opensInTopView(false, [{ identity: { armed: true } }]), false, 'the open world: as before')

console.log('situationText: ok')
