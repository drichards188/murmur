// Checks for inboxOrder.ts: what goes in "Needs you", what in "For information", and in what order.
import assert from 'node:assert/strict'
import { alertRank, alertSort, engagementRank, inboxOrder, RANK } from './inboxOrder.ts'

// the order of the Needs you list (ledger 2): signatures, a blocking review item, ours in trouble,
// an assessed engagement that persists, a running one whose picture changed, other review items,
// approvals by expiry, then high and medium alerts
const items = [
  { id: 'alert-med', rank: alertRank('left_task', 'medium'), sort: alertSort('medium', 5) },
  { id: 'approval-late', rank: RANK.approval, sort: 300 },
  { id: 'decl', rank: RANK.review, sort: -20 },
  { id: 'approval-soon', rank: RANK.approval, sort: 40 },
  { id: 'picture', rank: RANK.picture, sort: null },
  { id: 'again', rank: RANK.again, sort: null },
  { id: 'ours', rank: alertRank('lost_link', 'high'), sort: alertSort('high', 9) },
  { id: 'blocking', rank: RANK.blocking, sort: -5 },
  { id: 'signing', rank: RANK.signing, sort: 100 },
  { id: 'alert-high', rank: alertRank('task_failed', 'high'), sort: alertSort('high', 8) },
]
const { needs, info } = inboxOrder(items)
assert.deepEqual(
  needs.map((x) => x.id),
  ['signing', 'blocking', 'ours', 'again', 'picture', 'decl', 'approval-soon', 'approval-late', 'alert-high', 'alert-med'],
)
assert.equal(info.length, 0)

// for information, in its own order: running, info alerts, settled flags, acknowledged, confirmed losses
const quiet = inboxOrder([
  { id: 'lost', rank: RANK.lost, sort: null },
  { id: 'ack', rank: RANK.acknowledged, sort: null },
  { id: 'info-alert', rank: alertRank('identify_meanwhile', 'info'), sort: alertSort('info', 3) },
  { id: 'settled', rank: RANK.settled, sort: null },
  { id: 'running', rank: RANK.running, sort: null },
])
assert.deepEqual(quiet.needs, [])
assert.deepEqual(
  quiet.info.map((x) => x.id),
  ['running', 'info-alert', 'settled', 'ack', 'lost'],
)

// same rank and no time: the order they came in
assert.deepEqual(
  inboxOrder([
    { id: 'a', rank: RANK.review, sort: null },
    { id: 'b', rank: RANK.review, sort: null },
  ]).needs.map((x) => x.id),
  ['a', 'b'],
)

// an engagement under way asks only when its picture changed, a gun still fires, or the verdict
// is persists or unknown (settled for the build, 2026-11-24)
assert.equal(engagementRank({ state: 'dispatched', engagement: { watch: { holds: true } } }), RANK.running)
assert.equal(engagementRank({ state: 'dispatched', engagement: { watch: { holds: false } } }), RANK.picture)
assert.equal(engagementRank({ state: 'effected', engagement: { fire: {} } }), RANK.picture, 'a gun firing')
assert.equal(engagementRank({ state: 'effected', engagement: { fire: { ended: {} }, watch: { holds: true } } }), RANK.running, 'a fire that ended')
assert.equal(engagementRank({ state: 'done', engagement: { assessment: { result: 'persists' } } }), RANK.again)
assert.equal(engagementRank({ state: 'done', engagement: { assessment: { result: 'unknown' } } }), RANK.again)
assert.equal(engagementRank({ state: 'done', engagement: { assessment: { result: 'ceased' } } }), RANK.running)
assert.equal(engagementRank({ state: 'aborted', engagement: null }), RANK.running)
// a watch that failed after the engagement ended asks nothing
assert.equal(engagementRank({ state: 'done', engagement: { watch: { holds: false }, assessment: { result: 'ceased' } } }), RANK.running)

console.log('inboxOrder: ok')
