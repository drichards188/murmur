// Checks for needsYou.ts (no test runner in web/): node web/src/needsYou.check.ts, as make check
// does. Throws on the first failure, prints "needsYou: ok" otherwise. The inbox is the one seen in
// hostile-act-engagement at T+3:33 and T+8:00 (logs/ui-sweep2 hae-03, hae-06). Not part of the bundle.
import assert from 'node:assert/strict'
import { needsYou, settledFlags } from './needsYou.ts'
import type { Alert } from './decisions'

const flag = (id: string, tracks: string[], t: number, status = 'open') => ({ id, tracks, t, status: { status } })
const alert = (id: string, severity: Alert['severity'], extra: Partial<Alert> = {}) => ({ id, kind: 'damaged', severity, uid: 1, callsign: 'HUNTER 1', detail: '', since_t: 0, ...extra }) as unknown as Alert
const ruleDecl = (t: number, confirmed_by?: string) => ({ affiliation: 'hostile', by: 'rule:hostile_act', t, id: 1, rule: true, ...(confirmed_by ? { confirmed_by } : {}) })

// ---- a flag a later declaration settled ------------------------------------------------------------
const f1 = flag('F-1', ['T-0008'], 213)
const confirmed = [{ id: 'T-0008', declared: ruleDecl(225, 'op-auto') }]
assert.deepEqual(
  [...settledFlags([f1], confirmed, [])],
  [['F-1', "T-0008 declared hostile by the engine's rule (D-1) after it was raised, and op-auto confirmed it"]],
  'the case seen: F-1 below threshold at 3:33, D-1 by rule at 3:45, confirmed',
)
assert.equal(settledFlags([f1], [{ id: 'T-0008', declared: ruleDecl(225) }], []).size, 0, 'a rule declaration nobody confirmed settles nothing: a person has not decided')
assert.equal(settledFlags([f1], [{ id: 'T-0008', declared: ruleDecl(200, 'op-1') }], []).size, 0, 'declared before the flag was raised: the flag asks something new')
assert.equal(settledFlags([f1], confirmed, [{ id: 'F-1' }]).size, 0, 'never one holding up a nomination (the urgent strip has it)')
assert.equal(settledFlags([flag('F-1', ['T-0008'], 213, 'confirmed')], confirmed, []).size, 0, 'only open flags wait on the person')
assert.equal(settledFlags([flag('F-2', [], 213)], confirmed, []).size, 0, 'a flag naming no track stays')
assert.equal(settledFlags([f1], [], []).size, 0, 'its track gone from the picture: it stays')
const two = flag('F-3', ['T-0008', 'T-0009'], 213)
assert.equal(settledFlags([two], confirmed, []).size, 0, 'two tracks named, one settled: it stays')
const op = { id: 'T-0009', declared: { affiliation: 'neutral', by: 'op-1', t: 300, id: 4 } }
assert.equal(settledFlags([two], [...confirmed, op], []).get('F-3'), "T-0008 declared hostile by the engine's rule (D-1) after it was raised, and op-auto confirmed it; op-1 declared T-0009 neutral (D-4) after it was raised")
assert.equal(settledFlags([flag('F-4', ['T-0009'], 213)], [{ id: 'T-0009', declared: { affiliation: 'suspect', by: 'op-1', t: 300 } }], []).size, 0, 'a mark that restricts answers nothing')

// ---- one count -----------------------------------------------------------------------------------------
// T+3:33: the flag holding up a nomination, HUNTER 1's propulsion lost (high), one for information
const at333 = {
  pending: [],
  alerts: [alert('damaged:1', 'high'), alert('identify_meanwhile:2', 'info')],
  review: { declarations: [], flags: [f1] },
  blocking: [{ id: 'F-1' }],
}
const n = needsYou(at333, new Set(), new Set())
assert.equal(n.total, 2, 'the flag once (not again as waiting) and the high alert')
assert.deepEqual([n.review, n.waiting, n.high, n.info], [1, 1, 1, 1])
assert.equal(n.says, '2 need you: 1 item for review (1 holding up a nomination), 1 high alert. Not counted: 1 alert for information.')
// T+8:00: the flag settled, an engagement awaiting signatures
const at800 = { ...at333, pending: [{}], blocking: [] }
const m = needsYou(at800, new Set(), new Set(['F-1']))
assert.deepEqual([m.total, m.approvals, m.review, m.settled], [2, 1, 0, 1])
assert.match(m.says, /Not counted: 1 alert for information, 1 flag a later declaration settled\.$/)
// acknowledged in this browser: not counted, said (there is no snooze)
const k = needsYou(at333, new Set(['damaged:1']), new Set())
assert.deepEqual([k.total, k.high, k.muted], [1, 0, 1])
// a swarm's member alerts count once, as the swarm's
const folded = { pending: [], alerts: [alert('link:1', 'high', { folded_into: 'sw:PICKET' } as Partial<Alert>), alert('link:2', 'high', { folded_into: 'sw:PICKET' } as Partial<Alert>)], swarm_alerts: [{ severity: 'high' as const }] }
assert.equal(needsYou(folded, new Set(), new Set()).total, 1)
assert.deepEqual(needsYou(null, new Set(), new Set()), { total: 0, approvals: 0, review: 0, waiting: 0, engagements: 0, high: 0, medium: 0, info: 0, settled: 0, muted: 0, says: 'Nothing needs you.' })
// lost-contact claims and presumed losses are review items too; a confirmed flag is not pending
const rest = needsYou({ pending: [], alerts: [], review: { declarations: [{}], flags: [flag('F-5', ['T-1'], 1, 'confirmed')], lost_claims: [{}], node_losses: [{}] } }, new Set(), new Set())
assert.deepEqual([rest.total, rest.review], [3, 3])

// an engagement under way counts only when it asks: its picture changed, or a verdict of persists
const running = (holds: boolean) => ({ state: 'dispatched', engagement: { watch: { holds } } })
assert.equal(needsYou({ pending: [], alerts: [] }, new Set(), new Set(), [running(true)]).total, 0, 'a running engagement whose picture holds is for information')
const changed = needsYou({ pending: [], alerts: [] }, new Set(), new Set(), [running(false)])
assert.deepEqual([changed.total, changed.engagements], [1, 1])
assert.match(changed.says, /1 engagement to look at/)
assert.equal(needsYou({ pending: [], alerts: [] }, new Set(), new Set(), [{ state: 'done', engagement: { assessment: { result: 'persists' } } }]).total, 1, 'assessed and persists asks')
assert.equal(needsYou({ pending: [], alerts: [] }, new Set(), new Set(), [{ state: 'done', engagement: { assessment: { result: 'ceased' } } }]).total, 0, 'assessed and ceased is for information')

console.log('needsYou: ok')
