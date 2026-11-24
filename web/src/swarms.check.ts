// Checks for swarmsModel.ts (no test runner in web/): node web/src/swarms.check.ts, as make check
// does. The claims, on a fixture in the bridge's shapes (GET /api/swarms, the inbox's
// swarm_alerts): a member alert folded into a swarm alert is left out of the Inbox's own list and
// found under its swarm card; a fleet without folding keeps every alert; a selection is a swarm's
// only when it is exactly its members; the fleet list groups by swarm in the summary's order, with
// the vehicles of none last, and stays by domain for a fleet without swarms; the task line and the
// tone say what the panel shows. The map's groups (bd murmur-4nik): no group colour is an
// affiliation's; codes are as short as keep them apart; a chip's strength is honest (under its
// minimum and suspended told apart, drops and returns counted, who is missing named); an outline
// goes only round members heard; members crowding each other are a pile; chips that would overlap
// stack; a cluster tile names its groups. Throws on the first failure, prints "swarms: ok" otherwise.
import type { Alert } from './decisions'
import { alarmKey, causeLines, chipStatus, convexHull, GROUP_STATE, groupState, heldAlarms, shownTaskings, stepAlarms, stripParts, taskingGroup, type GroupMark, type GroupMarks, foldedInto, GROUP_PALETTE, groupCodes, groupColor, groupMarks, chipExceptions, selectedSwarm, swarmGroups, swarmOutlines, swarmTone, taskLine, unfolded, type Swarm, type SwarmsDoc } from './swarmsModel.ts'
import { cellChipText, cellGroups, chipSize, groupsOnScreen, stackChips, tileOf, type MemberOnScreen } from './groupScreen.ts'
import { textWidth } from './declutter.ts'
import type { GroupInForce } from './groupOrders.ts'
import type { LngLat } from './types.ts'

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

// the map: colours, codes, chips, outlines (bd murmur-4nik)
const AFFILIATION = ['#7fb8ff', '#f2c14e', '#5fd38d', '#ff5d5d', '#ffb347', '#6fe0c4']
check(GROUP_PALETTE.every((g) => !AFFILIATION.includes(g.color.toLowerCase())), 'no group colour is an affiliation, selection or sea colour')
const hue = (hex: string) => {
  const [r, g, b] = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255)
  const max = Math.max(r, g, b)
  const min = Math.min(r, g, b)
  if (max - min < 0.2) return null // near grey: no hue to mistake
  const h = max === r ? ((g - b) / (max - min)) % 6 : max === g ? (b - r) / (max - min) + 2 : (r - g) / (max - min) + 4
  return (h * 60 + 360) % 360
}
// friendly blue round 215, hostile red 0, neutral green 145, unknown yellow 45, selection 35
for (const { color } of GROUP_PALETTE) {
  const h = hue(color)
  check(h == null || (h > 240 && h < 340), `${color} sits outside the affiliation hues (${h})`)
}
check(groupColor(0).color === GROUP_PALETTE[0].color && groupColor(GROUP_PALETTE.length).color === GROUP_PALETTE[0].color, 'colours by place in the summary, repeating')
const sat = groupCodes(['PICKET-1', 'PICKET-12', 'SEARCH-3', 'SURFACE-3', 'RELAY-1', 'RESERVE', 'UNDERSEA-1'])
check([...sat.values()].join() === 'PI1,PI12,SE3,SU3,RE1,RE,UN1', `codes as few letters as keep them apart (${[...sat.values()]})`)
const gc = groupCodes(['PICKET', 'CRISSY', 'SEARCH', 'STRAIT', 'MARINA'])
check([...gc.values()].join() === 'PI,CR,SE,ST,MA', `Group command's codes (${[...gc.values()]})`)
check([...groupCodes(['PICKET-1', 'SEARCH-1']).values()].join() === 'P1,S1', 'one letter where one keeps them apart')
check(new Set(groupCodes(['A-1', 'A_1', 'A.1']).values()).size === 3, 'codes stay unique however the ids collide')

const inForce: GroupInForce[] = [
  { intent: 41, kind: 'orbit', swarm: 'PICKET-1', state: 'dispatched', selection: 60, members: [], on_share: 44, dropped: { '7': 'lost link: silent 30 s', '8': 'lost link: silent 31 s', '9': 'ended: its member order failed' }, back: [12], taken: 0, strength: 45, min: 48, reforming: true, reforms: 1, suspended: false },
]
const sg = (over: Partial<NonNullable<Swarm['groups']>[number]> = {}) => ({ intent: 41, kind: 'orbit', members_here: 60, strength: 60, min: 48, dropped: 0, back: 0, reforms: 0, suspended: false, ...over })
const full = chipStatus(swarm('PICKET-1', [1, 2], { groups: [sg()] }), inForce)
check(full.doing === 'orbit' && full.parts.map((p) => p.text).join(' · ') === '60/60 · min 48' && full.tone === 'ok', `a group at full strength, its order in words without its raw id (${JSON.stringify(full.parts)})`)
const belowMin = chipStatus(swarm('PICKET-1', [1, 2], { groups: [sg({ strength: 45, dropped: 3, back: 1 })] }), inForce)
check(belowMin.parts[0].text === "45/60 · under min 48" && belowMin.tone === "bad" && !belowMin.suspended, 'under its minimum: bad, not suspended')
check(belowMin.parts.some((p) => p.text === "3 dropped") && belowMin.parts.some((p) => p.text === '1 back'), 'drops and returns counted, not pooled')
const susp = chipStatus(swarm('PICKET-1', [1, 2], { groups: [sg({ strength: 5, min: 7, suspended: true })] }), inForce)
check(susp.suspended && susp.parts[0].text === 'SUSPENDED · 5 of min 7', 'suspended said apart from under strength')
const short = chipStatus(swarm('PICKET-1', [1, 2], { groups: [sg({ strength: 55, dropped: 5 })] }), inForce)
check(short.tone === 'warn' && short.parts[0].text === '55/60', 'short of its selection, above its minimum: a warning')
const lost = chipStatus(swarm('X', [1, 2, 3, 4], { lost_link: 3, tasks: [{ intent: 9, kind: 'patrol', count: 4 }] }))
check(lost.doing === 'patrol' && lost.parts.map((p) => p.text).join() === '4,3 lost link' && lost.tone === 'bad', `no group order: its task and its lost links (${JSON.stringify(lost.parts)})`)
check(chipStatus(swarm('X', [1, 2], { tasks: [] })).doing === 'no order', 'no order')

const marks = groupMarks({ ...doc, groups: inForce })
check(marks.list.map((m) => m.code).join() === 'P1,S1' && marks.ofUid.get(5)?.id === 'SEARCH-1', 'marks: codes and members')
check(marks.byId.get('PICKET-1')!.color === GROUP_PALETTE[0].color, "marks: the summary's order sets the colour")
// the map's chip: only what is wrong (the strip holds the order and the strength)
check(chipExceptions(full.parts).length === 0, 'a group at full strength: nothing under its name')
check(chipExceptions(belowMin.parts).map((p) => p.text).join(' · ') === '45/60 · under min 48 · 3 dropped', `under its minimum: that and its drops (${JSON.stringify(belowMin.parts)})`)
check(chipExceptions(lost.parts).map((p) => p.text).join() === '3 lost link', 'its lost links, not its count')

// the group strip's state: its order's first, else what most members do
const base = { suspended: false, under: false, reformedT: null as number | null }
check(groupState({ ...base, suspended: true, under: true }, { station: 9 }, 0) === 'suspended', 'suspended before under strength')
check(groupState({ ...base, under: true }, { station: 9 }, 0) === 'under', 'under strength before what members do')
check(groupState({ ...base, reformedT: 100_000 }, { station: 9 }, 110_000) === 'reforming' && groupState({ ...base, reformedT: 100_000 }, { station: 9 }, 200_000) === 'station', 're-forming for a while after a re-form')
check(groupState(base, { station: 3, moving: 5, lost: 1 }, 0) === 'moving', 'else what most members do')
check(Object.values(GROUP_STATE).every((x) => x.icon && x.word), 'every state an icon and a word, never colour alone')
check(belowMin.under && !belowMin.suspended && susp.suspended && !full.under, 'chip status flags')

// a group's orders on demand
const tk = (id: number, uids: number[]) => ({ id, ops: uids.map((uid) => ({ uid })) })
const tks = [tk(1, [1, 2]), tk(2, [5]), tk(3, [1, 5]), tk(4, [77])]
check(taskingGroup(tks[0], marks.ofUid) === 'PICKET-1' && taskingGroup(tks[2], marks.ofUid) === null && taskingGroup(tks[3], marks.ofUid) === null, "a tasking is a group's when all its vehicles are")
check(shownTaskings(tks, marks.ofUid, new Set(), new Set(), false).map((t) => t.id).join() === '3,4', "groups' orders hidden until asked for")
check(shownTaskings(tks, marks.ofUid, new Set(['PICKET-1']), new Set(), false).map((t) => t.id).join() === '1,3,4', 'an open group shows its orders')
check(shownTaskings(tks, marks.ofUid, new Set(), new Set([5]), false).map((t) => t.id).join() === '2,3,4', "a selected member shows its own order")
check(shownTaskings(tks, marks.ofUid, new Set(), new Set(), true).length === 4, 'Space shows them all')

// group alarms: only a group that stopped and waits (suspended); raised once held, a blip never;
// the oldest first; dismissed until a new one
const mk = (id: string, over: Partial<GroupMark>): GroupMark => ({ ...marks.list[0], id, suspended: false, under: false, refused: null, ...over })
const am = (list: GroupMark[]): GroupMarks => ({ list, byId: new Map(list.map((m) => [m.id, m])), ofUid: new Map() })
check(stepAlarms(new Map(), [mk('U', { under: true }), mk('R', { refused: 'no room' })], 0).size === 0, 'under strength while it settles, or a re-form refused: no alarm (the strip says it)')
const blip = [mk('A', { suspended: true })]
let seen = stepAlarms(new Map(), blip, 1000)
check(heldAlarms(seen, am(blip), 3000, new Set()).length === 0, 'an alarm not yet held is not raised')
seen = stepAlarms(seen, [mk('A', {})], 3000)
check(seen.size === 0, 'a blip that clears is forgotten')
seen = stepAlarms(seen, blip, 4000)
check(seen.get('A')!.since === 4000 && heldAlarms(stepAlarms(seen, blip, 9000), am(blip), 9000, new Set()).length === 1, 'raised once held five seconds, from its own start')
const two = [mk('A', { suspended: true }), mk('B', { suspended: true })]
const s2 = stepAlarms(stepAlarms(new Map(), [two[1]], 0), two, 1000)
const held = heldAlarms(s2, am(two), 7000, new Set())
check(held.map((h) => `${h.m.id}:${h.seen.alarm}`).join() === 'B:suspended,A:suspended', 'the oldest first')
check(heldAlarms(s2, am(two), 7000, new Set([alarmKey('B', s2.get('B')!)])).map((h) => h.m.id).join() === 'A', 'a dismissed alarm stays down')
check(stepAlarms(stepAlarms(s2, [], 7000), two, 8000).get('B')!.since === 8000, 'an alarm that clears and comes back starts again')

// outlines: round the members heard now, a stale one left out
const heard = new Map<number, LngLat>([[1, [-122.48, 37.82]], [2, [-122.46, 37.82]], [3, [-122.47, 37.84]]])
const outlines = swarmOutlines(marks, (u) => heard.get(u) ?? null, 'PICKET-1')
check(outlines.length === 1 && outlines[0].id === 'PICKET-1' && outlines[0].ring.length === 3, 'one outline per group with three heard')
check(outlines[0].ring.every((p) => p[2] === 0) && outlines[0].line[3] === 255 && outlines[0].width > 2.5, 'at sea level, the selected one stronger')
heard.delete(3)
check(swarmOutlines(marks, (u) => heard.get(u) ?? null, null).length === 0, 'no tidy outline round a member not heard')
check(convexHull([[0, 0], [2, 0], [1, 1], [2, 2], [0, 2]]).length === 4, 'the hull leaves inner points out')

// on the screen: apart, or a pile; the chip over the one drawn highest
const at = (uid: number, x: number, y: number, group = 'G'): MemberOnScreen => ({ uid, group, x, y, lon: x / 1000, lat: y / 1000, z: uid, halfH: 14 })
const apart = groupsOnScreen([at(1, 100, 100), at(2, 300, 100), at(3, 500, 200)]).groups.get('G')!
check(apart.lod === 'apart' && apart.n === 3, 'members far apart stand apart')
const onPile = groupsOnScreen([at(1, 100, 100), at(2, 110, 105), at(3, 120, 90), at(4, 600, 600)])
const pile = onPile.groups.get('G')!
check(pile.lod === 'pile', 'members crowding each other are a pile')
check([...onPile.crowded].join() === '1,2,3', 'the one standing apart is not crowded')
check(pile.yAt[2] === 3 && pile.lift === 24, 'the chip over the member drawn highest')
check(groupsOnScreen([at(1, 100, 100), at(2, 105, 100, 'H')]).groups.get('G')!.lod === 'apart', "another group's member does not crowd")
const st = stackChips([{ x: 100, y: 100, w: 120, h: 43 }, { x: 110, y: 100, w: 120, h: 43 }, { x: 600, y: 100, w: 120, h: 43 }])
check(st[0].up === 0 && st[1].up === 47 && st[1].free && st[2].up === 0, `chips that would overlap stack (${JSON.stringify(st)})`)
check(chipSize(marks.list[0]).w >= textWidth('P1', 11) + textWidth('PICKET-1', 14), 'a chip is as wide as its words')
check(chipSize({ ...marks.list[0], parts: full.parts }).h < chipSize({ ...marks.list[0], parts: belowMin.parts }).h, 'a chip with nothing wrong is one line, its exceptions a second')

// far out: the groups in each cluster tile
const [tx, ty] = tileOf(-122.47, 37.82, 12)
check(tx === 654 && ty === 1582, `tiles as the bridge counts them (${tx}, ${ty})`)
const cells = cellGroups(
  [{ uid: 1, position: [-122.47, 37.82] }, { uid: 2, position: [-122.4701, 37.8201] }, { uid: 5, position: [-122.4702, 37.82] }, { uid: 99, position: [-122.47, 37.82] }, { uid: 3, position: null }],
  (u) => marks.ofUid.get(u)?.id ?? null,
  12,
)
const cell = cells.get(`${tx}:${ty}`)!
check(cell.total === 4 && cell.none === 1 && cell.groups.map(([id, n]) => `${id}:${n}`).join() === 'PICKET-1:2,SEARCH-1:1', 'a tile names its groups, the largest first')
check(cellChipText(cell, (id) => marks.byId.get(id)!.code).head === 'PICKET-1 2' && cellChipText(cell, (id) => marks.byId.get(id)!.code).rest === 'S1 1 · 1 ungrouped', 'a tile chip names its groups by code')

// the panel's words
check(taskLine(doc.swarms[0]) === 'orbit', 'a task all run')
check(taskLine(doc.swarms[1]) === 'patrol (1), hold (1)', 'tasks some run, with how many')
check(taskLine(swarm('X', [1], { tasks: [] })) === 'no task', 'no task')
check(swarmTone(doc.swarms[0]) === 'bad' && swarmTone(doc.swarms[1]) === 'ok', 'tone from lost links')
check(swarmTone(swarm('Y', Array.from({ length: 100 }, (_, k) => k), { lost_link: 2 })) === 'warn', 'a few unheard: a warning')

// the strip's room: the strength, then what is wrong, never cut off (bd murmur-iobd.19)
{
  const parts = [{ text: '60/60', tone: 'ok' as const }, { text: 'min 45' }, { text: '10 lost link', tone: 'warn' as const }]
  check(stripParts(parts).map((p) => p.text).join(' · ') === '60/60 · 10 lost link', `a lost link is never cut off the strip (${stripParts(parts).map((p) => p.text)})`)
  check(stripParts(parts.slice(0, 2)).map((p) => p.text).join() === '60/60,min 45', 'nothing wrong: as the chip begins')
  // after the re-form: its strength down, the ten dropped and still unheard
  const reformed = [{ text: '50/60', tone: 'warn' as const }, { text: 'min 45' }, { text: '10 dropped', tone: 'warn' as const }, { text: '10 lost link', tone: 'warn' as const }]
  check(stripParts(reformed).map((p) => p.text).join() === '50/60,10 dropped,10 lost link', `both wrong parts beside the strength (${stripParts(reformed).map((p) => p.text)})`)
  const under = [{ text: '41/60 · under min 45', tone: 'bad' as const }, { text: '2 back' }, { text: '3 dropped', tone: 'warn' as const }, { text: '10 lost link', tone: 'bad' as const }]
  check(stripParts(under).map((p) => p.text).join() === '41/60 · under min 45,3 dropped,10 lost link', 'the wrong parts in the chip\'s order, the plain ones left to the chip')
}

console.log('swarms: ok')
