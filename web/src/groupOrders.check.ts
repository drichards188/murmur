// Checks for groupOrders.ts (no test runner in web/): node web/src/groupOrders.check.ts, as make
// check does. The claims, on fixtures in the bridge's shapes (a group order's checks, each member's
// failing one carrying `member`; a dry run's `group`): a member is excluded when any check says
// so, waiting when one says so and none excludes it, else sent; a hundred members' checks fold to
// one line a rule, each reason once with the members it holds for, never a line per member per
// rule (bd murmur-3kqi fault 12); the bottom line counts what is sent, waiting and left out against
// the minimum and says whether it re-forms; the layout reads in words; each slot of a small group
// is labelled, a large group's not; a group in force reads as its strength against its minimum.
// Throws on the first failure, prints "groupOrders: ok" otherwise.
import type { Check } from './decisions'
import {
  approvalMeans,
  counts,
  defaultMin,
  foldByRule,
  foldLine,
  foldTone,
  headline,
  layoutLine,
  memberClasses,
  reformBody,
  slotMarks,
  strengthLine,
  strengthTone,
  type GroupPreview,
} from './groupOrders.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`groupOrders: ${what}`)
}

// a hundred quadcopters' orbit: three waiting (stale), two left out (reserves), the rest sent
const uids = Array.from({ length: 100 }, (_, k) => String(k + 1))
const checks: Check[] = []
for (const rule of ['capability', 'readiness', 'platform_limits', 'domain', 'geofence', 'reserves']) {
  for (const u of uids) {
    const n = Number(u)
    if (rule === 'readiness' && n <= 3) {
      checks.push({ rule, uid: u, outcome: 'fail', detail: `cannot evaluate: navigation status is ${50 + n}.2 s old (limit 30 s)`, member: 'waiting' })
    } else if (rule === 'reserves' && (n === 50 || n === 51)) {
      checks.push({ rule, uid: u, outcome: 'fail', detail: `${n - 40} min left after task, reserve 8 min (20%)`, member: 'excluded' })
    } else checks.push({ rule, uid: u, outcome: 'pass', detail: 'fine' })
  }
}
checks.push({ rule: 'separation', uid: null, outcome: 'pass', detail: 'ring: 100 air 25 m apart; at least 15 m apart' })
checks.push({ rule: 'strength', uid: null, outcome: 'pass', detail: '98 of 100 can take it (95 sent, 3 waiting for fresh facts (1, 2, 3), 2 excluded (reserves 2)); the minimum is 80' })

const lines = memberClasses(uids, checks)
check(lines.filter((l) => l.class === 'waiting').map((l) => l.uid).join() === '1,2,3', 'the three stale members wait')
check(lines.filter((l) => l.class === 'excluded').map((l) => l.uid).join() === '50,51', 'the two short of reserve are left out')
check(lines.filter((l) => l.class === 'sent').length === 95, 'the rest are sent')
// a member waiting on one rule and excluded on another is excluded
const both = memberClasses(['9'], [
  { rule: 'readiness', uid: '9', outcome: 'fail', detail: 'stale', member: 'waiting' },
  { rule: 'geofence', uid: '9', outcome: 'fail', detail: 'no route', member: 'excluded' },
])
check(both[0].class === 'excluded' && both[0].why.length === 2, 'excluded wins over waiting')

const folds = foldByRule(checks)
check(folds.map((f) => f.rule).join() === 'capability,readiness,platform_limits,domain,geofence,reserves,separation,strength', 'one fold a rule, in the checks order')
const readiness = folds.find((f) => f.rule === 'readiness')!
check(readiness.pass === 97 && readiness.waiting === 3 && readiness.excluded === 0, 'readiness counts')
check(readiness.reasons.length === 1 && readiness.reasons[0].uids.length === 3, 'the three stale reasons are one reason (numbers aside)')
check(foldLine(readiness) === '97 pass, 3 waiting', `readiness line: ${foldLine(readiness)}`)
check(foldTone(readiness) === 'warn', 'a waiting member is a warning')
const reserves = folds.find((f) => f.rule === 'reserves')!
check(foldTone(reserves) === 'bad' && reserves.reasons.length === 1, 'two left out on reserves: one reason, bad')
check(foldTone(folds.find((f) => f.rule === 'capability')!) === 'ok', 'all pass: ok')
const sep = folds.find((f) => f.rule === 'separation')!
check(foldLine(sep).startsWith('ring: 100 air'), 'a group check reads as its own detail')
const shownLines = folds.reduce((n, f) => n + 1 + f.reasons.length, 0)
check(shownLines < 15, `a hundred members fold to a few lines (${shownLines}), not ${checks.length}`)

const c = counts(lines, { spread: 'ring', min_members: 80, reform: true, wait_s: 120 }, 'orbit')
check(c.sent === 95 && c.waiting === 3 && c.excluded === 2 && c.min === 80, 'counts')
const h = headline(c, { min_members: 80, reform: true, wait_s: 120 }, 'orbit', 'PICKET-1')
check(h === '95 of 100 of PICKET-1 now; 3 join if heard within 120 s; 2 left out; re-forms itself above 80', `headline: ${h}`)
check(headline(counts(lines, { min_members: 1 }, 'transit'), { min_members: 1, reform: false }, 'transit', null).endsWith('minimum 1'), 'a transit states its minimum, no re-form')
check(approvalMeans(c, { reform: true }, 'orbit').includes('below 80 it stops and asks you'), 'what approving a re-forming group does')
check(approvalMeans(c, { reform: false }, 'transit').includes('does not re-form'), 'what approving a transit does')
check(defaultMin('orbit', 10) === 8 && defaultMin('orbit', 4) === 4 && defaultMin('hold', 30) === 1, 'the default minimum: 80 % rounded up, one for a hold')

// the dry run's group: five stations, and a hundred
const five: GroupPreview = {
  spec: { spread: 'stations', min_members: 4, reform: false },
  layout: {
    spread: 'stations',
    shares: Object.fromEntries(['1', '2', '3', '4', '5'].map((u, k) => [u, { slot: k, layer: k % 2, alt_m: 30 + 10 * (k % 2), entry_m: 220 * k, at: [-122.45 - 0.0025 * k, 37.81] }])),
    domains: [{ domain: 'air', members: 5, spacing_m: 220, layers: 2, lateral_m: 15, vertical_m: 10, path_m: 880 }],
  },
  tasks: {},
}
check(layoutLine(five.layout) === 'stations: 5 air 220 m apart in 2 layers 10 m apart', `layout line: ${layoutLine(five.layout)}`)
const marks = slotMarks(five, memberClasses(['1', '2', '3', '4', '5'], []), (u) => (u === '1' ? [-122.44, 37.80] : null), (u) => `SKY ${u}`)
check(marks.length === 5 && marks[0].label === 'SKY 1 30 m' && marks[1].label === 'SKY 2 40 m', 'a small group labels each slot with its name and altitude')
check(marks[0].from != null && marks[1].from == null, 'the way from a member known to be somewhere')
const viaBlock = slotMarks(
  { ...five, tasks: { '1': { kind: 'transit', params: { route: [[-122.46, 37.80], [-122.45, 37.81]] } } } },
  memberClasses(['1'], []),
  () => null,
  (u) => u,
)
check(viaBlock[0].via.length === 1 && viaBlock[0].via[0][0] === -122.46 && marks[2].via.length === 0, 'a transit member’s way goes through its own route')
const hundred: GroupPreview = {
  ...five,
  layout: { ...five.layout, shares: Object.fromEntries(uids.map((u, k) => [u, { slot: k, at: [-122.45, 37.81 + k * 1e-4] }])) },
}
check(slotMarks(hundred, lines, () => null, (u) => u).every((m) => m.label === ''), 'a hundred slots are not labelled')
check(slotMarks(hundred, lines, () => null, (u) => u).find((m) => m.uid === '2')?.class === 'waiting', 'a waiting member’s slot says so')

// a group in force
check(strengthLine({ intent: 41, kind: 'orbit', strength: 90, min: 80, reforms: 1, suspended: false, dropped: 10, back: 0 }) === 'i41 orbit: 90 of a minimum 80, 10 dropped, re-formed 1x', 'strength line')
check(strengthTone({ strength: 70, min: 80, suspended: true, dropped: 30 }) === 'bad', 'under strength is bad')
check(strengthTone({ strength: 90, min: 80, suspended: false, dropped: 10 }) === 'warn', 'drops are a warning')
check(strengthTone({ strength: 100, min: 80, suspended: false, dropped: 0 }) === 'ok', 'full strength is ok')

// re-forming a suspended group: the same task to the members heard, the minimum the engine's anew
const suspended = { id: 79, kind: 'orbit', params: { center: [-122.458, 37.8125] as [number, number], radius_m: 120 }, uids: ['61', '62', '63', '64', '65', '66', '67', '68'], group: { spread: 'ring' as const, min_members: 7, reform: true } }
const again = reformBody(suspended, { dropped: { '62': 'silent', '65': 'silent', '68': 'silent' }, back: [65] })
check(again != null && JSON.stringify(again.uids) === JSON.stringify([61, 63, 64, 65, 66, 67]), `re-form: the members heard (${JSON.stringify(again?.uids)})`)
check(again!.task === 'orbit' && again!.radius === 120 && JSON.stringify(again!.center) === JSON.stringify([-122.458, 37.8125]), 're-form: the same task')
check(JSON.stringify(again!.group) === JSON.stringify({ spread: 'ring', reform: true }), 're-form: the spec without its minimum')
check(JSON.stringify(reformBody(suspended, { dropped: { '62': 'silent', '65': 'silent', '68': 'silent' }, back: [] }, (u) => u !== '68')!.uids) === JSON.stringify([61, 62, 63, 64, 65, 66, 67]), 're-form: a dropped member heard now goes too')
check(reformBody(suspended, { dropped: Object.fromEntries(suspended.uids.map((u) => [u, 'silent'])), back: [] }) === null, 're-form: none heard, nothing to send')
check(reformBody({ ...suspended, uids: ['61', '62'] }, { dropped: { '62': 'silent' }, back: [] })!.group === undefined, 're-form: one member heard is an order of its own')

console.log('groupOrders: ok')
