// Checks for tasking.ts: node web/src/tasking.check.ts (prints "tasking: ok").
import assert from 'node:assert/strict'
import { ago, awaitsReply, BOUNDARY, deactivated, deliveryOf, inFlight, lastReply, officerOf, outcomeView, REPLY_WAIT_MS, split, textProblem, TEXT_MAX, type Reply, type TaskingItem } from './tasking.ts'

const it = (id: string, at_t: number, active = true): TaskingItem => ({ id, text: id, by: 'op-1', at_t, active })

assert.equal(textProblem('   '), 'Write the request first.')
assert.equal(textProblem('x'.repeat(TEXT_MAX + 1)), `A request is at most 500 characters (501).`)
assert.equal(textProblem(' ' + 'x'.repeat(TEXT_MAX) + ' '), null, 'trimmed before it is counted, as the bridge does')
assert.equal(textProblem('Watch the ferry lane.'), null)
assert.equal(textProblem('🛥'.repeat(TEXT_MAX)), null, 'characters, as the bridge counts, not UTF-16 units')

const items = [it('tk-1', 100), it('tk-3', 300), it('tk-2', 200, false), it('tk-4', 400, false)]
const s = split(items)
assert.deepEqual(s.active.map((i) => i.id), ['tk-3', 'tk-1'], 'active, newest first')
assert.deepEqual(s.retired.map((i) => i.id), ['tk-4', 'tk-2'], 'retired, newest first')
assert.deepEqual(split(deactivated(items, 'tk-3')).active.map((i) => i.id), ['tk-1'], 'deactivated: out of the active list')
assert.equal(items.find((i) => i.id === 'tk-3')!.active, true, 'the list itself is not changed')

assert.equal(ago(0, 40_000), '40 s')
assert.equal(ago(0, 300_000), '5 min')
assert.equal(ago(0, 7_200_000), '2 h')
assert.equal(ago(5000, 1000), '0 s', 'never negative')

// the boundary is said in words wherever the form shows: a request it carries out, approving nothing
assert.ok(/not an approval and not a decision/.test(BOUNDARY) && /never signs, declares, approves or releases/.test(BOUNDARY))
assert.ok(/those that need you wait in your Inbox/.test(BOUNDARY) && /replies to each/.test(BOUNDARY))
// how far the officer has an item (its acknowledgements): nothing from an older bridge
const base = it('tk-5', 100_000)
assert.equal(deliveryOf(base, 160_000), null, 'a bridge without delivery fields: no chip')
const sent = deliveryOf({ ...base, delivery: 'posted' }, 160_000)!
assert.deepEqual([sent.label, sent.ticks, sent.tone], ['Sent', 1, 'muted'])
assert.match(sent.title, /Stored by the bridge 60 s ago; the watch officer has not been given it yet/)
const waiting = deliveryOf({ ...base, delivery: 'posted', waiting: { t: 150_000, why: "the hour's wakes are spent" } }, 160_000)!
assert.deepEqual([waiting.label, waiting.tone], ['Sent, waiting', 'warn'])
assert.match(waiting.title, /has it but has not been given it yet: the hour's wakes are spent \(10 s ago\)/)
const seen = { ...base, delivery: 'seen' as const, seen_t: 104_000, seen_wake: 7, officer: 'agent-watch' }
const s1 = deliveryOf(seen, 160_000)!
assert.deepEqual([s1.label, s1.ticks, s1.tone], ['Seen', 2, 'muted'])
assert.match(s1.title, /^Given to the watch officer \(agent-watch\) in update 7, 56 s ago; it has not finished that turn yet\.$/)
const f = deliveryOf({ ...seen, failed: { t: 110_000, wake: 7, why: 'the turn ended error' } }, 160_000)!
assert.deepEqual([f.label, f.tone], ['Seen, turn failed', 'warn'])
assert.match(f.title, /The turn on update 7 did not finish \(the turn ended error\), 50 s ago: the next update gives it again\./)
const done = deliveryOf({ ...seen, delivery: 'processed', processed_t: 130_000, processed_wake: 8 }, 160_000)!
assert.deepEqual([done.label, done.ticks, done.tone], ['Processed', 2, 'ok'])
assert.match(done.title, /^The watch officer \(agent-watch\) finished update 8, which carried this request, 30 s ago\. Given to the watch officer \(agent-watch\) in update 7, 56 s ago\.$/)
assert.equal(deliveryOf({ ...seen, delivery: 'processed', processed_wake: 8, active: false }, 160_000)!.label, 'Processed', 'a retired item keeps its last state')
assert.ok(inFlight([{ ...base, delivery: 'posted' }]) && inFlight([seen]) && !inFlight([{ ...seen, delivery: 'processed' }]))
assert.ok(!inFlight([{ ...seen, active: false }]) && !inFlight([base]), 'a retired item or an older bridge: the usual poll')

// whether an officer reads the tasking now (bridge ms on both sides)
assert.equal(officerOf({ t: 50_000 }), null, 'a bridge that does not say')
assert.deepEqual(officerOf({ t: 50_000, officer: null }), { connected: false, text: 'No watch officer is connected: requests wait as Sent until one connects.' })
assert.equal(officerOf({ t: 50_000, officer: { agent: 'agent-watch', read_t: 48_000 } })!.connected, true)
const gone = officerOf({ t: 90_000, officer: { agent: 'agent-watch', read_t: 48_000 } })!
assert.equal(gone.connected, false)
assert.match(gone.text, /No watch officer has read this for 42 s \(agent-watch was the last\)/)
// the officer's answers: the latest shown, its outcome as a chip; nothing from an older bridge
const reply = (status: Reply['status'], t: number): Reply => ({ t, agent: 'agent-watch', status, text: 'Proposed two patrols\n- i41 west\n- i42 east', intents: [41, 42], groups: ['og-1'] })
const processed = { ...base, delivery: 'processed' as const, seen_t: 104_000, processed_t: 130_000, processed_wake: 8, officer: 'agent-watch' }
assert.equal(lastReply(processed), null)
assert.equal(lastReply({ replies: [reply('partial', 140_000), reply('done', 150_000)] })!.status, 'done', 'the latest')
const doneView = outcomeView(reply('done', 150_000), 160_000)
assert.deepEqual([doneView.label, doneView.tone], ['Done', 'ok'])
assert.match(doneView.title, /^agent-watch replied 10 s ago: every order it needs is proposed, or its groups made\. Orders it proposed wait in the Inbox where they need you\.$/)
assert.deepEqual(['partial', 'blocked', 'declined'].map((x) => outcomeView(reply(x as Reply['status'], 150_000), 160_000).tone), ['warn', 'bad', 'muted'])
const odd = outcomeView({ ...reply('done', 150_000), status: 'rerouted' as Reply['status'] }, 160_000)
assert.deepEqual([odd.label, odd.tone], ['rerouted', 'muted'], "a newer bridge's outcome, shown as it is")
// "no reply yet": the officer had it (its turn finished, or failed) and has not answered
assert.equal(awaitsReply(processed, true), true)
assert.equal(awaitsReply({ ...seen, failed: { t: 110_000, wake: 7, why: 'the turn ended error' } }, true), true, 'a failed turn without an answer')
assert.equal(awaitsReply(seen, true), false, 'its turn is still running')
assert.equal(awaitsReply({ ...processed, replies: [reply('blocked', 150_000)] }, true), false, 'answered')
assert.equal(awaitsReply({ ...processed, active: false }, true), false, 'retired')
assert.equal(awaitsReply(processed, undefined), false, "an older bridge's officer cannot answer: its silence means nothing")
// the drawer polls quickly for a minute after a turn finished without an answer
assert.ok(inFlight([processed], 130_000 + REPLY_WAIT_MS - 1) && !inFlight([processed], 130_000 + REPLY_WAIT_MS))
assert.ok(!inFlight([{ ...processed, replies: [reply('done', 140_000)] }], 131_000), 'answered: the usual poll')
console.log('tasking: ok')
