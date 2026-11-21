// Checks for sitrep.ts: node web/src/sitrep.check.ts (prints "sitrep: ok").
import assert from 'node:assert/strict'
import { ANY, contactSummary, filterNodes, groupRows, headline, linkHealth, nearest, nodeRows, sitrepText, strengthWords, tally, windowOf, worstFirst, type SitrepContext } from './sitrep.ts'
import type { Swarm } from './swarmsModel.ts'
import type { Vehicle } from './types.ts'

const v = (uid: number, o: { lost?: boolean; mode?: string; speed?: number; battery?: number | null; domain?: string; short?: string } = {}): Vehicle =>
  ({
    uid, sysid: uid, kind: 'copter', mode: o.mode ?? 'GUIDED', groundspeed: o.speed ?? 0, battery_pct: o.battery === undefined ? 80 : o.battery,
    link: { stale: o.lost ?? false, since_last_ms: o.lost ? 90_000 : 200, expected_interval_ms: 1000 },
    identity: { callsign: `V${uid}`, platform: 'quad', short: o.short ?? 'QUAD', domain: o.domain ?? 'air' },
  }) as unknown as Vehicle
const fleet = [v(1), v(2, { lost: true }), v(3, { mode: 'RTL' }), v(4, { battery: 10 }), v(5, { speed: 5, domain: 'surface', short: 'USV' }), v(6)]
const by = new Map(fleet.map((x) => [x.uid, x]))
const ctx: SitrepContext = { latest: new Map([[1, { state: 'active', task: 'orbit' }]]), damaged: new Set([6]) }

// the header
const tr = (affiliation: string, declared?: string, state = 'active') => ({ affiliation, declared: declared ? { affiliation: declared } : null, state, domain: 'surface', review: [] }) as never
const h = headline({
  scenario: { title: 'Harbor watch', phase: 'T+4' }, vehicles: fleet, tracks: [tr('unknown'), tr('unknown', 'hostile'), tr('suspect')],
  counts: { pending: 2, review: 1, queued: 0 }, blocking: [{ text: 'Holding up: REAPER 1' }], oldest: 'D-3 waiting',
})
assert.deepEqual([h.up, h.total, h.lost, h.contacts, h.hostile, h.suspect, h.waiting], [5, 6, 1, 3, 1, 1, 3])
assert.equal(h.scenario, 'Harbor watch: T+4')
assert.equal(h.urgent, 'Holding up: REAPER 1', 'the bridge\'s blocking item first')
assert.equal(headline({ scenario: null, vehicles: [], tracks: [], counts: null, blocking: [], oldest: 'D-3 waiting' }).urgent, 'D-3 waiting')
assert.equal(headline({ scenario: null, vehicles: [], tracks: [], counts: null, blocking: null }).urgent, null)

// strength by state
const t = tally([1, 2, 3, 4, 6, 99], by, ctx)
assert.equal(t.here, 5, 'a member not in the fleet is not counted')
assert.deepEqual([t.counts.station, t.counts.lost, t.counts.returning, t.counts.damaged, t.counts.idle], [1, 1, 1, 1, 1])
assert.equal(t.lowBattery, 1)
const swarm = { id: 'PICKET-1', name: 'Gate', role: 'picket', color: null, count: 4, members: [1, 2, 3, 4], ready: 3, lost_link: 1, low_battery: 1, centroid: null, hull: null, tasks: [{ intent: 12, kind: 'orbit', count: 4 }] } as Swarm
const rows = groupRows([swarm], [{ id: 'og-1', name: 'Mine', color: '#c49bff', members: [5, 6], created_t: 1, updated_t: 1 }], by, ctx, (k, i) => `${k}${i}`)
assert.deepEqual(rows.map((r) => r.key), ['g:og-1', 's:PICKET-1'], 'the operator\'s groups first')
assert.equal(rows[0].tone, 'bad', 'a damaged member')
assert.equal(rows[1].tone, 'bad', 'a quarter lost: the bridge\'s own tone')
assert.equal(rows[1].task, 'orbit i12')
assert.equal(strengthWords(rows[1]), '3 of 4: 1 on station, 1 returning, 1 idle; 1 lost link')
assert.equal(rows[1].name, 'PICKET-1 · Gate', 'a swarm named as the fleet list and the strip name it')
// under a group order in force, a swarm reads as its chip does: the order and its strength, its tone
const calm = { ...swarm, lost_link: 0, low_battery: 0, members: [1, 3], count: 2 } as Swarm
const mark = { doing: 'orbit i12', parts: [{ text: 'SUSPENDED · 1 of min 2', tone: 'bad' as const }], tone: 'bad' as const, intent: 12 }
const under = groupRows([calm], [], by, ctx, () => null, (id) => (id === 'PICKET-1' ? mark : undefined))
assert.equal(under[0].task, 'orbit i12: SUSPENDED · 1 of min 2')
assert.equal(under[0].tone, 'bad', 'the group order suspended: bad, though every member is heard')
assert.equal(groupRows([calm], [], by, ctx, () => null)[0].tone, 'ok', 'without it the members alone')

// nodes: filters and the worst first
const nodes = nodeRows(fleet, ctx, new Map([[1, ['Mine']]]))
assert.equal(nodes.length, 6)
assert.equal(nodes.find((n) => n.uid === 4)!.lowBattery, true)
assert.equal(nodes.find((n) => n.uid === 1)!.order, 'orbit active')
assert.deepEqual(filterNodes(nodes, { ...ANY, unheard: true }).map((n) => n.uid), [2])
assert.deepEqual(filterNodes(nodes, { ...ANY, domain: 'surface' }).map((n) => n.uid), [5])
assert.deepEqual(filterNodes(nodes, { ...ANY, model: 'USV' }).map((n) => n.uid), [5])
assert.deepEqual(filterNodes(nodes, { ...ANY, low: true }).map((n) => n.uid), [4])
assert.deepEqual(filterNodes(nodes, { ...ANY, text: 'v3' }).map((n) => n.uid), [3])
assert.deepEqual(filterNodes(nodes, { ...ANY, within: new Set([1, 2]) }).map((n) => n.uid), [1, 2])
assert.deepEqual(worstFirst(nodes).slice(0, 3).map((n) => n.uid), [6, 2, 4], 'damaged, lost, low battery')
assert.equal(linkHealth(v(1, { lost: true })), 'bad')

// the window: only what is in view, with a margin, never past the ends
assert.deepEqual(windowOf(1000, 0, 300, 30), { from: 0, to: 16 })
assert.deepEqual(windowOf(1000, 3000, 300, 30), { from: 94, to: 116 })
assert.deepEqual(windowOf(10, 0, 300, 30), { from: 0, to: 10 })
assert.deepEqual(windowOf(0, 0, 300, 30), { from: 0, to: 0 })

// contacts
const c = contactSummary([tr('unknown'), tr('unknown', 'hostile'), tr('neutral', undefined, 'stale')])
assert.deepEqual(c.byAffiliation, { unknown: 1, hostile: 1, neutral: 1 }, 'a declaration over the tracker\'s call')
assert.equal(c.declared, 1)
assert.equal(c.stale, 1)
const near = nearest([{ position: [-122.4, 37.8] as [number, number], id: 'far' }, { position: [-122.5, 37.81] as [number, number], id: 'near' }], [-122.5, 37.8], 1)
assert.equal(near[0].track.id, 'near')

// the export reads the same numbers
const text = sitrepText(h, rows, c, '10:00Z')
assert.ok(text.includes('5 of 6 vehicles heard') && text.includes('3 waiting for a person') && text.includes('(yours) **Mine**'))
console.log('sitrep: ok')
