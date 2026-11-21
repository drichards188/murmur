// Checks for tasking.ts: node web/src/tasking.check.ts (prints "tasking: ok").
import assert from 'node:assert/strict'
import { ago, BOUNDARY, deactivated, split, textProblem, TEXT_MAX, type TaskingItem } from './tasking.ts'

const it = (id: string, at_t: number, active = true): TaskingItem => ({ id, text: id, by: 'op-1', at_t, active })

assert.equal(textProblem('   '), 'Write the guidance first.')
assert.equal(textProblem('x'.repeat(TEXT_MAX + 1)), `Guidance is at most 500 characters (501).`)
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

// the boundary is said in words wherever the form shows
assert.ok(/not an order and not a decision/.test(BOUNDARY) && /never signs, declares, approves or releases/.test(BOUNDARY))
console.log('tasking: ok')
