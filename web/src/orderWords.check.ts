// Checks for orderWords.ts (no test runner in web/): node web/src/orderWords.check.ts, as make
// check does. The claims (R3, ledger 1 "Needs a new piece" 1): without an order a vehicle is moving
// or idle, returning home by its mode; with one, the order's words by its task and its op's state,
// with the distance while en route; an engagement and an identify by their track; a group order
// names its group; a mode that takes it home overrides an order that does not; a failed order says
// nothing of what it does; and the command log's one word per op. Throws on the first failure,
// prints "orderWords: ok" otherwise. Not part of the bundle.
import assert from 'node:assert/strict'
import { doingNow, doingWords, latestOrders, orderOver, opWord, opWordIsBad, type OrderIntent, type OrderRef } from './orderWords.ts'
import type { LngLat } from './types.ts'

const here: LngLat = [-122.4783, 37.8199]
const north = (m: number): LngLat => [here[0], here[1] + m / 111_195]
const v = (o: { mode?: string; groundspeed?: number; position?: LngLat } = {}) => ({ mode: o.mode ?? 'AUTO', groundspeed: o.groundspeed ?? 0, position: o.position ?? here })
const name = (uid: number) => ({ 1: 'HUNTER 1', 2: 'REAPER 1' })[uid] ?? `vehicle ${uid}`
const ref = (task: string, state: string, params: Record<string, unknown> = {}, intent?: OrderIntent, parent?: OrderIntent): OrderRef => ({
  tasking: { id: 1, intent_id: intent?.id, task: task as never, params },
  op: { state: state as never, detail: '' },
  intent,
  parent,
})

// no order: vehicleState's words
assert.equal(doingWords(v(), undefined, name), 'idle')
assert.equal(doingWords(v({ groundspeed: 12 }), undefined, name), 'moving')
assert.equal(doingWords(v({ mode: 'RTL', groundspeed: 12 }), undefined, name), 'returning home')

// by task, running
assert.equal(doingWords(v(), ref('patrol', 'active', { route: [north(500), north(900)] }), name), 'patrolling')
assert.equal(doingWords(v(), ref('hold', 'done'), name), 'holding')
assert.equal(doingWords(v(), ref('rtl', 'active'), name), 'returning home')
assert.equal(doingWords(v(), ref('rtl', 'done'), name), 'home')
assert.equal(doingWords(v(), ref('track', 'active', { target: 2 }), name), 'pointing at REAPER 1')
// an orbit: en route with the distance to its ring, then orbiting
assert.equal(doingWords(v(), ref('orbit', 'active', { center: north(2500), radius: 400 }), name), 'en route to its orbit, 2.1 km')
assert.equal(doingWords(v({ position: north(2100) }), ref('orbit', 'active', { center: north(2500), radius: 400 }), name), 'orbiting')
assert.equal(doingWords(v(), ref('orbit', 'done', { center: north(2500), radius: 400 }), name), 'orbiting', 'an orbit done is established')
// a transit: the way to its end, then holding there
assert.equal(doingWords(v(), ref('transit', 'active', { route: [north(300), north(800)] }), name), 'en route, 800 m to go')
assert.equal(doingWords(v(), ref('transit', 'done', { route: [north(300), north(800)] }), name), 'holding at the end of its route')

// before it runs
assert.equal(doingWords(v(), ref('patrol', 'sent'), name), 'starting its patrol')
assert.equal(doingWords(v(), ref('orbit', 'queued'), name), 'waiting for a position fix to start its orbit')

// an engagement and an identify, by their track
const engage: OrderIntent = { id: 2, kind: 'engage', params: { track: 'T-0008' }, uids: ['2'] }
assert.equal(doingWords(v(), ref('orbit', 'active', { center: north(3000), radius: 200 }, engage), name), 'en route to strike T-0008, 2.8 km')
assert.equal(doingWords(v(), ref('orbit', 'done', { center: north(3000), radius: 200 }, engage), name), 'striking T-0008')
assert.equal(doingWords(v(), ref('orbit', 'starting', { center: north(3000), radius: 200 }, engage), name), 'starting its strike on T-0008')
// released, then assessed: the platform waits on station, it is no longer striking
assert.equal(doingWords(v(), ref('orbit', 'done', { center: north(3000), radius: 200 }, { ...engage, state: 'effected' }), name), 'on station, the strike on T-0008 being assessed')
assert.equal(doingWords(v(), ref('orbit', 'done', { center: north(3000), radius: 200 }, { ...engage, state: 'done' }), name), 'on station after the strike on T-0008')
const identify: OrderIntent = { id: 5, kind: 'identify', params: {}, uids: ['1'] }
assert.equal(doingWords(v({ position: north(2700) }), ref('orbit', 'active', { center: north(3000), radius: 300, look: 'T-0003' }, identify), name), 'identifying T-0003')
// and once its look ended, the order it went back to, under the identify's number
assert.equal(doingWords(v(), ref('patrol', 'active', {}, identify), name), 'patrolling')

// a group order names its group, a member's share its group order's
const group: OrderIntent = { id: 78, kind: 'patrol', params: {}, uids: ['1', '2'], group: { swarm: 'North picket' } }
const share: OrderIntent = { id: 80, kind: 'patrol', params: {}, uids: ['1'], parent: 78 }
assert.equal(doingWords(v(), ref('patrol', 'active', {}, share, group), name), 'patrolling · North picket')

// a mode that takes it home overrides an order that does not
assert.equal(doingWords(v({ mode: 'RTL' }), ref('patrol', 'active'), name), 'returning home')
// an order that failed or was replaced no longer says what it does
assert.equal(doingWords(v({ groundspeed: 5 }), ref('orbit', 'failed'), name), 'moving')
assert.equal(doingWords(v(), ref('orbit', 'superseded'), name), 'idle')
assert.equal(doingWords(v(), ref('orbit', 'requeued'), name), 'idle; its orbit back in the queue')

// the latest order per vehicle: of the newest tasking, with its intent and group order
const intents = new Map<number, OrderIntent>([[78, group], [80, share], [2, engage]])
const latest = latestOrders(
  [
    { id: 3, intent_id: 80, task: 'patrol', params: {}, ops: [{ id: 1, tasking: 3, task: 'patrol', uid: 1, sysid: 1, state: 'active', detail: '', progress: null, history: [] }] },
    { id: 1, intent_id: 2, task: 'orbit', params: {}, ops: [{ id: 2, tasking: 1, task: 'orbit', uid: 1, sysid: 1, state: 'done', detail: '', progress: null, history: [] }] },
  ],
  intents,
)
assert.equal(latest.get(1)?.tasking.id, 3, 'the newest tasking wins')
assert.equal(latest.get(1)?.parent?.id, 78, 'with its group order')

// the command log's word
assert.equal(opWord({ state: 'active', detail: '' }, 'orbit'), 'running')
assert.equal(opWord({ state: 'done', detail: '' }, 'orbit'), 'running', 'an orbit done goes on')
assert.equal(opWord({ state: 'done', detail: '' }, 'rtl'), 'done')
assert.equal(opWord({ state: 'acked', detail: '' }, 'patrol'), 'starting')
assert.equal(opWord({ state: 'superseded', detail: '' }, 'patrol'), 'replaced')
assert.equal(opWord({ state: 'failed', detail: 'not sent before the bridge restarted' }, 'patrol'), 'not sent')
assert.equal(opWord({ state: 'unconfirmed', detail: 'the vehicle does not run it now' }, 'patrol'), 'not running')
assert.ok(opWordIsBad('failed') && !opWordIsBad('running') && !opWordIsBad('replaced'))

// what is wrong with it wins over what its order says (HUNTER 1 read "DAMAGED: CANNOT MOVE"
// above "patrolling" in hostile-act-engagement)
{
  const still = { mode: 'AUTO', groundspeed: 0, position: null } as unknown as Parameters<typeof doingNow>[0]
  assert.equal(doingNow(still, undefined, () => '', { cannotMove: true }), 'stopped')
  assert.equal(doingNow(still, undefined, () => '', { silent: true }), 'last heard idle')
  assert.equal(doingNow(still, undefined, () => ''), 'idle')
}

// an engagement's orbit runs on after the release: its card and details say the order is over
assert.equal(orderOver('engage', 'effected'), 'released')
assert.equal(orderOver('engage', 'done'), 'over')
assert.equal(orderOver('engage', 'dispatched'), null)
assert.equal(orderOver('orbit', 'done'), null)

console.log('orderWords: ok')
