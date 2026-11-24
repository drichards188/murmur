// Checks for orderCard.ts (no test runner in web/): node web/src/orderCard.check.ts, as make check
// does. Throws on the first failure, prints "orderCard: ok" otherwise. The case is the one seen in
// hostile-act-engagement (logs/ui-sweep 12-t7): engagement i2, flown as tasking #2, an orbit. Not
// part of the bundle.
import assert from 'node:assert/strict'
import { oneCardPerOrder, orderCard } from './orderCard.ts'

const name = (uid: string) => ({ '1': 'HUNTER 1', '2': 'REAPER 1' })[uid] ?? `vehicle ${uid}`

const engage = { id: 2, kind: 'engage', uids: ['2'], params: { track: 'T-0008' } }
assert.deepEqual(orderCard({ id: 2, intent_id: 2, task: 'orbit' }, engage, undefined, name), {
  label: 'ENGAGE T-0008 with REAPER 1',
  id: 'i2',
  idTitle: 'order i2; tasking #2 in the task layer',
})
// the ids differ when other orders came first: the card says the order's
const patrol = { id: 1, kind: 'patrol', uids: ['1'], params: {} }
assert.equal(orderCard({ id: 7, intent_id: 1, task: 'patrol' }, patrol, undefined, name).id, 'i1')
assert.equal(orderCard({ id: 7, intent_id: 1, task: 'patrol' }, patrol, undefined, name).label, 'PATROL')

// an order no longer among the intents the bridge sends: still its number, the task as it is flown
assert.deepEqual(orderCard({ id: 9, intent_id: 4, task: 'orbit' }, undefined, undefined, name), { label: 'ORBIT', id: 'i4', idTitle: 'order i4; tasking #9 in the task layer' })
// a member's share of a group order: the group order's number and name
const group = { id: 78, kind: 'transit', uids: ['1', '2'], params: {}, group: { swarm: 'STRAIT' } }
const share = { id: 80, kind: 'transit', uids: ['1'], params: {}, parent: 78 }
const m = orderCard({ id: 87, intent_id: 80, task: 'transit' }, share, group, name)
assert.deepEqual([m.label, m.id], ['TRANSIT · STRAIT', 'i78'])
assert.equal(m.idTitle, "order i78 (this vehicle's share is i80); tasking #87 in the task layer")
// a lay order (rules 27)
assert.equal(orderCard({ id: 3, intent_id: 3, task: 'lay' }, { id: 3, kind: 'lay', uids: ['1'], params: { count: 2, weapon: 'TM-62M mine' } }, undefined, name).label, 'LAY 2 TM-62M mine with HUNTER 1')

// one card per order: a group order's members (each its own tasking and intent) are one card
{
  const intents = new Map([[80, { parent: 78 }], [81, { parent: 78 }], [5, {}]])
  const ts = [
    { id: 88, intent_id: 81, task: 'transit', ops: [{ uid: 2 }] },
    { id: 87, intent_id: 80, task: 'transit', ops: [{ uid: 1 }] },
    { id: 90, intent_id: 5, task: 'patrol', ops: [{ uid: 9 }] },
  ]
  const cards = oneCardPerOrder(ts, (id) => intents.get(id))
  assert.deepEqual(cards.map((c) => [c.id, c.intent_id, c.ops.length]), [[87, 80, 2], [90, 5, 1]])
  assert.equal(ts[0].ops.length, 1, 'the bridge\'s taskings are not changed')
}

console.log('orderCard: ok')
