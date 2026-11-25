// Checks for railTabs.ts: node web/src/layouts/railTabs.check.ts (prints "railTabs: ok"), as make
// check does. Throws on the first failure.
import assert from 'node:assert/strict'
import {
  RAIL_DEFAULT,
  RAIL_TABS,
  clickTab,
  commandFace,
  fleetFace,
  offerOrder,
  orderTally,
  parseRailState,
  railStateText,
  revealed,
  tabAfterKey,
  tracksFace,
  unseenOrders,
  type RailState,
} from './railTabs.ts'

// ---- the faces ----------------------------------------------------------------------------------

// Fleet: ours, marked by ours in trouble in the slot's own tone; the glance's button sentence goes
{
  const quiet = fleetFace({ ours: 22 }, { n: 0, tone: null, says: 'None of ours in trouble.' })
  assert.equal(quiet.count, 22)
  assert.equal(quiet.mark, null, 'nothing in trouble: no mark')
  assert.equal(quiet.label, 'Fleet')
  const bad = fleetFace({ ours: 2 }, { n: 1, tone: 'bad', says: '1 of ours in trouble: HUNTER 1: damaged: cannot move. A click shows the next one in the Fleet list.' })
  assert.deepEqual(bad.mark, { n: 1, tone: 'bad', says: '1 of ours in trouble: HUNTER 1: damaged: cannot move.' })
  assert.equal(fleetFace({ ours: 5 }, { n: 2, tone: 'warn', says: 'x' }).mark?.tone, 'warn')
  assert.equal(fleetFace({ ours: 5 }, { n: 2, tone: null, says: 'x' }).mark?.tone, 'warn', 'a count without a tone is amber, never red')
  assert.equal(fleetFace(undefined, undefined).count, null, 'not known: no count')
  assert.equal(fleetFace(undefined, undefined).mark, null)
}

// Tracks: the contacts, marked red by the hostile, else amber by the suspect
{
  assert.equal(tracksFace({ contacts: 8, hostile: 0, suspect: 0 }).mark, null)
  const h = tracksFace({ contacts: 8, hostile: 1, suspect: 2 })
  assert.deepEqual(h.mark, { n: 1, tone: 'bad', says: '1 hostile, 2 suspect' })
  assert.equal(h.count, 8)
  assert.deepEqual(tracksFace({ contacts: 3, hostile: 0, suspect: 2 }).mark, { n: 2, tone: 'warn', says: '2 suspect' })
  assert.equal(tracksFace({ contacts: 1, hostile: 0, suspect: 0 }).counts, '1 contact our sensors hold')
  assert.equal(tracksFace(null).count, null)
}

// ---- the command log ------------------------------------------------------------------------------

type T = Parameters<typeof orderTally>[0][number]
function op(tasking: number, id: number, state: string, detail = '') {
  return { id, tasking, task: 'orbit' as const, uid: id, sysid: id, state: state as never, detail, progress: null, history: [] }
}
function tasking(id: number, ops: ReturnType<typeof op>[], extra: Partial<T> = {}): T {
  return { id, task: 'orbit', params: {}, ops, ...extra }
}

// an orbit that runs (done is still running for an orbit), a hold starting, a patrol done for good
{
  const t = orderTally(
    [tasking(1, [op(1, 1, 'done')]), tasking(2, [op(2, 2, 'sent')], { task: 'hold' }), tasking(3, [op(3, 3, 'superseded')])],
    [],
  )
  assert.equal(t.running, 2)
  assert.deepEqual(t.failures, [])
}
// a failure, and a start never sent before a restart, each by its own key on its card
{
  const t = orderTally([tasking(4, [op(4, 10, 'failed', 'no ack'), op(4, 11, 'active')]), tasking(5, [op(5, 12, 'failed', 'not sent before the bridge restarted')])], null)
  assert.equal(t.running, 1, 'one vehicle still running it: the order is under way')
  assert.deepEqual(t.failures, [
    { key: '4/10', order: 4 },
    { key: '5/12', order: 5 },
  ])
}
// a group order's members are one order (orderCard.ts oneCardPerOrder)
{
  const intents = [
    { id: 70, kind: 'orbit', state: 'dispatched' },
    { id: 71, kind: 'orbit', state: 'dispatched', parent: 70 },
    { id: 72, kind: 'orbit', state: 'dispatched', parent: 70 },
  ]
  const t = orderTally([tasking(8, [op(8, 20, 'active')], { intent_id: 71 }), tasking(9, [op(9, 21, 'failed')], { intent_id: 72 })], intents)
  assert.equal(t.running, 1, 'one order, under way')
  assert.deepEqual(t.failures, [{ key: '9/21', order: 8 }], "the failure is on the group order's card")
}
// an identify order: its newest tasking only
{
  const intents = [{ id: 30, kind: 'identify', state: 'dispatched' }]
  const t = orderTally(
    [tasking(11, [op(11, 40, 'failed')], { intent_id: 30, params: { look: 'T-0003' } }), tasking(12, [op(12, 41, 'active')], { intent_id: 30, params: { look: 'T-0003' } })],
    intents,
  )
  assert.equal(t.running, 1)
  assert.deepEqual(t.failures, [], "an older orbit of the identify is not on the log")
}
// an engagement over by its own state is not under way, though its orbit runs on
{
  const t = orderTally([tasking(13, [op(13, 50, 'active')], { intent_id: 2 })], [{ id: 2, kind: 'engage', state: 'done' }])
  assert.equal(t.running, 0)
}

// unseen: counted by order, a failure seen once the log was in front of the person
{
  const failures = [
    { key: '4/10', order: 4 },
    { key: '4/11', order: 4 },
    { key: '5/12', order: 5 },
  ]
  assert.equal(unseenOrders(failures, new Set()), 2)
  assert.equal(unseenOrders(failures, new Set(['4/10'])), 2, 'another failure on the same order still shows it')
  assert.equal(unseenOrders(failures, new Set(['4/10', '4/11'])), 1)
  assert.equal(unseenOrders(failures, new Set(['4/10', '4/11', '5/12'])), 0)
}
{
  const f = commandFace({ running: 3, failures: [] }, 0)
  assert.equal(f.count, 3)
  assert.equal(f.mark, null)
  assert.equal(f.counts, '3 orders under way')
  assert.deepEqual(commandFace({ running: 1, failures: [] }, 1).mark, { n: 1, tone: 'warn', says: '1 order failed since you last read the command log' })
  assert.equal(commandFace(null, 0).count, null)
}

// ---- the remembered state -----------------------------------------------------------------------

assert.deepEqual(RAIL_DEFAULT, { tab: 'fleet', open: false, needs: true }, 'first time: the lists folded to their tabs, Needs you shown')
assert.deepEqual(parseRailState(null), RAIL_DEFAULT)
assert.deepEqual(parseRailState(''), RAIL_DEFAULT)
assert.deepEqual(parseRailState('not json'), RAIL_DEFAULT)
assert.deepEqual(parseRailState('[1,2]'), RAIL_DEFAULT)
assert.deepEqual(parseRailState('"tracks"'), RAIL_DEFAULT)
assert.deepEqual(parseRailState('{"tab":"tracks","open":false,"needs":false}'), { tab: 'tracks', open: false, needs: false })
assert.deepEqual(parseRailState('{"tab":"radar","open":"no","needs":0}'), RAIL_DEFAULT, 'each part not understood is the default')
assert.deepEqual(parseRailState('{"tab":"command"}'), { ...RAIL_DEFAULT, tab: 'command' }, 'each part on its own')
assert.deepEqual(parseRailState('{"tab":"toString"}'), RAIL_DEFAULT)
for (const tab of RAIL_TABS)
  for (const open of [true, false])
    for (const needs of [true, false]) {
      const s: RailState = { tab, open, needs }
      assert.deepEqual(parseRailState(railStateText(s)), s, 'read back as written')
    }

// ---- the tabs -----------------------------------------------------------------------------------

{
  const s: RailState = { tab: 'fleet', open: true, needs: true }
  assert.deepEqual(clickTab(s, 'tracks'), { ...s, tab: 'tracks' }, 'another tab: forward')
  assert.deepEqual(clickTab(s, 'fleet'), { ...s, open: false }, 'the tab in front: hides the lists')
  assert.deepEqual(clickTab({ ...s, open: false }, 'command'), { ...s, tab: 'command', open: true }, 'hidden: shows them on that tab')
  assert.deepEqual(clickTab({ ...s, open: false }, 'fleet'), { ...s, open: true })
}
assert.equal(tabAfterKey('fleet', 'ArrowDown'), 'tracks')
assert.equal(tabAfterKey('command', 'ArrowDown'), 'fleet', 'wraps')
assert.equal(tabAfterKey('fleet', 'ArrowUp'), 'command', 'wraps')
assert.equal(tabAfterKey('tracks', 'Home'), 'fleet')
assert.equal(tabAfterKey('tracks', 'End'), 'command')
assert.equal(tabAfterKey('tracks', 'Enter'), null)

// a request to show rows
{
  const fleet: RailState = { tab: 'fleet', open: true, needs: true }
  const shut: RailState = { ...fleet, open: false }
  assert.deepEqual(revealed(fleet, 'tracks'), { ...fleet, tab: 'tracks' }, "a track's card: Tracks forward")
  assert.deepEqual(revealed(shut, 'tracks'), { ...fleet, tab: 'tracks' }, 'and the lists shown')
  const onTracks: RailState = { ...fleet, tab: 'tracks' }
  assert.equal(revealed(onTracks, 'tracks'), onTracks, 'already in front: unchanged')
  assert.deepEqual(revealed(onTracks, 'fleet'), fleet, 'one of ours: Fleet forward')
  assert.deepEqual(revealed({ ...shut, tab: 'tracks' }, 'fleet'), { ...shut, tab: 'fleet' }, 'hidden lists stay hidden')
  const ordering: RailState = { ...fleet, tab: 'command' }
  assert.equal(revealed(ordering, 'fleet'), ordering, 'the order form in front stays')
  assert.equal(revealed(fleet, 'fleet'), fleet)
}

// the way to the order form: only with vehicles selected and the form out of sight
assert.equal(offerOrder({ tab: 'fleet', open: true, needs: true }, 0), false)
assert.equal(offerOrder({ tab: 'fleet', open: true, needs: true }, 3), true)
assert.equal(offerOrder({ tab: 'command', open: true, needs: true }, 3), false)
assert.equal(offerOrder({ tab: 'command', open: false, needs: true }, 3), true)

console.log('railTabs: ok')
