// Checks for commandModel.ts: node web/src/commandModel.check.ts (prints "commandModel: ok").
import assert from 'node:assert/strict'
import { cardLabel, kindsOf, orderPhrase, rangeBetween, routing, selectionLine, stage } from './commandModel.ts'

assert.equal(orderPhrase('orbit', { radius: 400.4 }), 'Orbit 400 m')
assert.equal(orderPhrase('patrol', { points: 1 }), 'Patrol 1 waypoint')
assert.equal(orderPhrase('transit', { points: 3 }), 'Transit 3 points')
assert.equal(orderPhrase('rtl', {}), 'Return to launch')

// the routing is read off the dry run's answer
const tier = (r: string) => r.toUpperCase()
const base = { passed: true, would_reach: 'auto_approved', policy: null }
assert.equal(routing(base, tier).tone, 'pass')
assert.equal(routing(base, tier).short, 'goes at once')
assert.equal(routing({ ...base, would_reach: 'pending_approval', policy: { rule: 'armed', reason: 'weapons platform' } }, tier).text, 'Checks pass. It would wait for approval: weapons platform.')
assert.equal(routing({ ...base, would_reach: 'approved' }, tier).short, 'your sending is the approval')
assert.equal(routing({ ...base, submission_authorizes: true, would_reach: 'x' }, tier).tone, 'wait')
assert.equal(routing({ ...base, passed: false }, tier).tone, 'fail')
assert.equal(routing({ ...base, would_reach: 'queued' }, tier).short, 'reaches queued', 'anything else is said as it came')

// the steps
assert.equal(stage({ complete: false, loading: false, answered: false, passed: false, sent: false }), 'compose')
assert.equal(stage({ complete: true, loading: true, answered: false, passed: false, sent: false }), 'checking')
assert.equal(stage({ complete: true, loading: false, answered: true, passed: true, sent: false }), 'ready')
assert.equal(stage({ complete: true, loading: false, answered: true, passed: false, sent: false }), 'blocked')
assert.equal(stage({ complete: true, loading: false, answered: true, passed: true, sent: true }), 'sent')

// the selection in a line: not 100 callsigns
const air = { kind: 'copter' as const, identity: null }
const sea = { kind: 'boat' as const, identity: null }
assert.equal(selectionLine([air, air, sea], null), '3: 2 air, 1 water', 'domains as the console names them')
assert.equal(selectionLine([air], null), '1')
assert.equal(selectionLine([air, air], 'North picket'), 'North picket · 2: 2 air')

// shift-click ranges, in the list's order, either way round
const order = [5, 6, 7, 8, 9]
assert.deepEqual(rangeBetween(order, 6, 8), [6, 7, 8])
assert.deepEqual(rangeBetween(order, 8, 6), [6, 7, 8])
assert.deepEqual(rangeBetween(order, null, 7), [7], 'no anchor: just the one')
assert.deepEqual(rangeBetween(order, 99, 7), [7], 'an anchor no longer listed: just the one')

// select all of a kind
const id = (uid: number, platform: string, short: string) => ({ uid, identity: { platform, short } as never })
const kinds = kindsOf([id(1, 'puma', 'Puma'), id(2, 'magura', 'Magura'), id(3, 'puma', 'Puma'), { uid: 4, identity: null }])
assert.deepEqual(kinds.map((k) => [k.id, k.uids]), [['puma', [1, 3]], ['magura', [2]]], 'most numerous first; no identity, no kind')

assert.equal(cardLabel('orbit', 'North picket'), 'ORBIT · North picket')
assert.equal(cardLabel('orbit', null), 'ORBIT')
console.log('commandModel: ok')
