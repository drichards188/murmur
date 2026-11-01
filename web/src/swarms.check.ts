// Checks for swarmsModel.ts (no test runner in web/): node web/src/swarms.check.ts, as make check
// does. The claims, on a fixture in the bridge's shapes (GET /api/swarms, the inbox's
// swarm_alerts): a member alert folded into a swarm alert is left out of the Inbox's own list and
// found under its swarm card; a fleet without folding keeps every alert; a selection is a swarm's
// only when it is exactly its members; the fleet list groups by swarm in the summary's order, with
// the vehicles of none last, and stays by domain for a fleet without swarms; each swarm with a hull
// gets one outline at sea level and a label (how many are unheard when any); the task line and
// the tone say what the panel shows. Throws on the first failure, prints "swarms: ok" otherwise.
import type { Alert } from './decisions'
import { causeLines, foldedInto, selectedSwarm, swarmGroups, swarmOutlines, swarmTone, taskLine, unfolded, type Swarm, type SwarmsDoc } from './swarmsModel.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`swarms: ${what}`)
}

const swarm = (id: string, members: number[], extra: Partial<Swarm> = {}): Swarm => ({
  id,
  name: `${id} name`,
  role: 'picket',
  color: null,
  count: members.length,
  members,
  ready: members.length,
  lost_link: 0,
  low_battery: 0,
  centroid: [-122.47, 37.83],
  hull: [[-122.48, 37.82], [-122.46, 37.82], [-122.47, 37.84]],
  tasks: [{ intent: 7, kind: 'orbit', count: members.length }],
  ...extra,
})

const doc: SwarmsDoc = {
  t: 1,
  swarms: [
    swarm('PICKET-1', [1, 2, 3, 4], { lost_link: 3, ready: 1 }),
    swarm('SEARCH-1', [5, 6], { role: 'search', color: '#f2c14e', tasks: [{ intent: 9, kind: 'patrol', count: 1 }, { intent: null, kind: 'hold', count: 1 }], hull: null }),
  ],
}

const alert = (id: string, uid: number, folded?: string): Alert => ({
  id, kind: 'lost_link', severity: 'high', uid, callsign: `SKY ${uid}`, detail: `no message for 12 s`, since_t: 100, ...(folded ? { folded_into: folded } : {}),
})

// folding
const alerts = [alert('link:1', 1, 'swarm:PICKET-1:lost_link'), alert('link:2', 2, 'swarm:PICKET-1:lost_link'), alert('link:9', 9)]
check(unfolded(alerts).map((a) => a.id).join() === 'link:9', 'folded member alerts are left out of the list')
const plain = [alert('link:9', 9), alert('link:8', 8)]
check(unfolded(plain) === plain, 'nothing folded: the list as it was')
const under = foldedInto(alerts, 'swarm:PICKET-1:lost_link')
check(under.size === 2 && under.get(1)?.[0].id === 'link:1' && !under.has(9), 'the members under their swarm card')
check(causeLines(['jammed', { cause: 'lost link', count: 3 }]).join('|') === 'jammed|lost link (3)', 'causes as lines from a list')
check(causeLines({ jam: 2 }).join() === 'jam (2)' && causeLines(undefined).length === 0, 'causes from a map, none')

// selection
check(selectedSwarm(doc.swarms, new Set([1, 2, 3, 4])) === 'PICKET-1', 'a swarm selected whole')
check(selectedSwarm(doc.swarms, new Set([1, 2, 3])) === null, 'part of a swarm is not the swarm')
check(selectedSwarm(doc.swarms, new Set([1, 2, 3, 4, 5])) === null, 'more than a swarm is not the swarm')

// the fleet list
const v = (uid: number, swarm: string | null) => ({ uid, swarm, identity: null })
const groups = swarmGroups([v(6, 'SEARCH-1'), v(1, 'PICKET-1'), v(10, null), v(2, 'PICKET-1'), v(11, 'NEW-1')], doc.swarms)!
check(groups.map((g) => g.id).join() === 'PICKET-1,SEARCH-1,NEW-1,', 'groups in the summary order, an unknown swarm after, none last')
check(groups[0].title === 'PICKET-1 · PICKET-1 name' && groups[0].vehicles.map((x) => x.uid).join() === '1,2', 'a group keeps its members in list order')
check(swarmGroups([v(1, null)], doc.swarms) === null, 'a fleet without swarms keeps its domain groups')

// the map
const { outlines, labels } = swarmOutlines(doc, 'PICKET-1')
check(outlines.length === 1 && outlines[0].id === 'PICKET-1', 'one outline per swarm with a hull')
check(outlines[0].ring.every((p) => p[2] === 0), 'outlines at sea level')
check(outlines[0].line[3] === 255, 'the selected swarm drawn stronger')
check(labels.map((l) => l.text).join('|') === 'PICKET-1 · 3 lost|SEARCH-1', 'labels say how many are unheard')
check(swarmOutlines(null, null).outlines.length === 0, 'no summary, nothing drawn')

// the panel's words
check(taskLine(doc.swarms[0]) === 'orbit i7', 'a task all run')
check(taskLine(doc.swarms[1]) === 'patrol i9 (1), hold (1)', 'tasks some run, with how many')
check(taskLine(swarm('X', [1], { tasks: [] })) === 'no task', 'no task')
check(swarmTone(doc.swarms[0]) === 'bad' && swarmTone(doc.swarms[1]) === 'ok', 'tone from lost links')
check(swarmTone(swarm('Y', Array.from({ length: 100 }, (_, k) => k), { lost_link: 2 })) === 'warn', 'a few unheard: a warning')

console.log('swarms: ok')
