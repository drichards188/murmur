// Checks for refusalFold.ts (no test runner in web/): node web/src/refusalFold.check.ts, as make
// check does. The claims, on fixtures in the bridge's shapes: a group refused for every member's
// battery reserve (intent 163: 60 quadcopters, a patrol) reads bottom line first, then one folded
// line with the range of its figures and what to cut; a mixed group (some excluded, some waiting)
// folds each rule and class apart; a one-vehicle order is not folded.
// Throws on the first failure, prints "refusalFold: ok" otherwise.
import type { Check } from './decisions'
import { foldRefusal } from './refusalFold.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`refusalFold: ${what}`)
}

const uids = Array.from({ length: 60 }, (_, k) => String(k + 1))
const res = (left: number, task: number) => `${left} min left after task (task ${task} min, battery 89% of 39 min), reserve 8 min (20%)`
const reserves: Check[] = uids.map((u, k) => ({
  rule: 'reserves',
  uid: u,
  outcome: 'fail',
  detail: res(5 + (k % 3), 28 + (k % 2)),
  member: 'excluded',
}))
const group: Check[] = [
  { rule: 'separation', uid: null, outcome: 'pass', detail: 'along: 60 air 201 m apart' },
  { rule: 'strength', uid: null, outcome: 'fail', detail: '0 of 60 can take it (0 sent, 60 excluded (reserves 60)); the minimum is 48' },
]

const a = foldRefusal([...reserves, ...group], uids, () => 10)
check(a.folded && a.refused, 'a group refused folds')
check(a.bottom === 'Refused: 0 of 60 can take it, the minimum is 48. All 60 left out: battery reserve.', `bottom line: ${a.bottom}`)
check(a.lines.length === 1, 'sixty reserve failures are one line')
check(
  a.lines[0].text === 'reserves, 60 of 60: 5 to 7 min left after a 28 to 29 min task, below the 8 min (20 %) reserve',
  `the line: ${a.lines[0].text}`,
)
check(a.lines[0].members.length === 60, 'the members stay behind the line')
check(a.lines[0].hint?.includes('Cut at least 3 min of flying (about 1.8 km at 10 m/s') === true, `the hint: ${a.lines[0].hint}`)
check(a.lines[0].hint?.includes('shorter route') === true && a.lines[0].hint.includes('fresher'), 'the hint names the levers')
check(!/—/.test(a.bottom + a.lines[0].text + a.lines[0].hint), 'no em dashes')
check(foldRefusal([...reserves, ...group], uids).lines[0].hint?.includes('km') === false, 'no cruise speed, no distance')

// a mixed group: 2 excluded for reserves, 3 waiting on readiness, the rest sent; it holds
const mixed: Check[] = [
  { rule: 'reserves', uid: '1', outcome: 'fail', detail: res(6, 28), member: 'excluded' },
  { rule: 'reserves', uid: '2', outcome: 'fail', detail: res(7, 28), member: 'excluded' },
  ...['3', '4', '5'].map((u): Check => ({ rule: 'readiness', uid: u, outcome: 'fail', detail: 'navigation status stale', member: 'waiting' })),
  { rule: 'strength', uid: null, outcome: 'pass', detail: '58 of 60 can take it (55 sent, 3 waiting, 2 excluded (reserves 2)); the minimum is 48' },
]
const m = foldRefusal(mixed, uids)
check(!m.refused, 'a group that holds is not refused')
check(m.bottom.startsWith('58 of 60 can take it, the minimum is 48.'), `mixed bottom: ${m.bottom}`)
check(m.bottom.includes('2 left out: battery reserve.') && m.bottom.includes('3 waiting: readiness.'), `mixed counts: ${m.bottom}`)
check(m.lines.length === 2, 'two rules, two lines')
check(m.lines[1].text === 'readiness, 3 of 60 waiting: navigation status stale', `waiting line: ${m.lines[1].text}`)

// a one-vehicle order keeps its own list
const one = foldRefusal([{ rule: 'reserves', uid: '1', outcome: 'fail', detail: res(6, 28) }], ['1'])
check(!one.folded && one.lines.length === 0, 'one vehicle is not folded')
console.log('refusalFold: ok')
