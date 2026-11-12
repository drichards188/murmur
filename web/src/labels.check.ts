// Checks for the label budget in declutter.ts (no test runner in web/): node web/src/labels.check.ts,
// as make check does. The claims (docs/research/saturation-display.md R2): no two labels placed
// overlap, unless one is forced (selected, pinned, details open); priority 1 is placed first and in
// full, the rest short, except while few labels are in view (then everyone in full, as before);
// a full block that does not fit falls back to the short label, then to none; ties keep the
// caller's order; each priority's zoom floor holds; an obstacle (a cluster marker) takes room and
// is never counted; labels off the screen are not counted; Show all draws every one in full; the
// counts add up. Throws on the first failure, prints "labels: ok" otherwise.
import { FEW, LABEL_MIN_ZOOM, planLabels, rightOf, type Box, type LabelCandidate, type Priority } from './declutter.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`labels: ${what}`)
}

const W = 1800
const H = 1000
const opts = { zoom: 12, width: W, height: H }
// a symbol 12 px wide either side: "T-0001" short, "SM AIR (DET) T-0001 · alt ?" in full
const SHORT: Box = rightOf(12, 'T-0001', 11)
const FULL: Box = [-90, -10, 120, 10]
function cand(key: string, x: number, y: number, priority: Priority = 4, extra: Partial<LabelCandidate> = {}): LabelCandidate {
  return { key, at: [x, y], short: SHORT, full: FULL, priority, ...extra }
}

// a few labels: each in full where it fits (the small situations look as before)
{
  const p = planLabels([cand('a', 100, 100), cand('b', 600, 100), cand('c', 100, 400)], opts)
  check(['a', 'b', 'c'].every((k) => p.level(k) === 'full'), 'few labels, all in full')
  check(!p.stats.budgeted && p.stats.full === 3 && p.stats.inView === 3, `few labels: counts ${JSON.stringify(p.stats)}`)
}
// two in full would overlap: the second falls back to short where that fits, else none
{
  const p = planLabels([cand('a', 300, 100), cand('b', 300 + 150, 100), cand('c', 300 + 20, 100)], opts)
  check(p.level('a') === 'full', 'the first keeps its full block')
  check(p.level('b') === 'short', `the second falls back to short (${p.level('b')})`)
  check(p.level('c') === 'none', `the third has no room (${p.level('c')})`)
}

// many labels: priority 1 in full, the rest short; never two overlapping
const crowd: LabelCandidate[] = []
let seed = 7
const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647)
for (let i = 0; i < 1500; i++) crowd.push(cand(`t:${i}`, 300 + rnd() * 600, 200 + rnd() * 400, i % 97 === 0 ? 1 : 4))
crowd.push(cand('v:sel', 600, 400, 1, { force: true }))
crowd.push(cand('v:sel2', 605, 402, 1, { force: true }))
{
  const p = planLabels(crowd, opts)
  check(p.stats.budgeted, 'over FEW labels the budget is in force')
  check(p.level('v:sel') === 'full' && p.level('v:sel2') === 'full', 'forced labels are placed in full, even overlapping each other')
  const boxes: { key: string; b: number[]; force: boolean }[] = []
  for (const c of crowd) {
    const l = p.level(c.key)
    if (l === 'none') continue
    if (c.priority !== 1) check(l === 'short', `a label not in focus is short over FEW (${c.key} ${l})`)
    const b = l === 'full' ? c.full : c.short
    boxes.push({ key: c.key, b: [c.at![0] + b[0], c.at![1] + b[1], c.at![0] + b[2], c.at![1] + b[3]], force: !!c.force })
  }
  for (let i = 0; i < boxes.length; i++)
    for (let j = i + 1; j < boxes.length; j++) {
      const [a, b] = [boxes[i], boxes[j]]
      if (a.force || b.force) continue
      const over = a.b[0] < b.b[2] && b.b[0] < a.b[2] && a.b[1] < b.b[3] && b.b[1] < a.b[3]
      check(!over, `labels ${a.key} and ${b.key} overlap`)
    }
  check(boxes.length < 200, `the budget is bounded by the screen, not the crowd (${boxes.length} placed of ${crowd.length})`)
  check(p.stats.full + p.stats.short + p.stats.none === p.stats.inView && p.stats.inView === crowd.length, `the counts add up ${JSON.stringify(p.stats)}`)
}
// priority before order: a priority-1 label wins its place over an earlier priority-4 one
{
  const many = Array.from({ length: FEW }, (_, i) => cand(`far:${i}`, 50 + (i % 10) * 170, 700 + Math.floor(i / 10) * 60))
  const p = planLabels([...many, cand('rest', 500, 100, 4), cand('focus', 505, 100, 1)], opts)
  check(p.level('focus') === 'full' && p.level('rest') === 'none', `priority 1 first (${p.level('focus')}, ${p.level('rest')})`)
}
// ties keep the caller's order
{
  const many = Array.from({ length: FEW }, (_, i) => cand(`far:${i}`, 50 + (i % 10) * 170, 700 + Math.floor(i / 10) * 60))
  const p = planLabels([...many, cand('first', 500, 100), cand('second', 505, 100)], opts)
  check(p.level('first') === 'short' && p.level('second') === 'none', 'equal priority: the earlier one wins')
}
// zoom floors: below LABEL_MIN_ZOOM the rest go bare, focus and group names stay
{
  const low = { ...opts, zoom: LABEL_MIN_ZOOM - 1 }
  const p = planLabels([cand('rest', 100, 100, 4), cand('alert', 600, 100, 3), cand('group', 100, 400, 2), cand('focus', 600, 400, 1)], low)
  check(p.level('rest') === 'none' && p.level('alert') === 'none', 'below the floor, priorities 3 and 4 go bare')
  check(p.level('group') !== 'none' && p.level('focus') === 'full', 'group names and focus keep their labels at any zoom')
}
// an obstacle takes room, is never counted and never drawn as a label
{
  const p = planLabels([cand('c:1', 300, 100, 2, { obstacle: true, short: [-40, -40, 40, 40], full: [-40, -40, 40, 40] }), cand('near', 310, 100)], opts)
  check(p.level('near') === 'none', 'a label under a cluster marker is not drawn')
  check(p.stats.inView === 1 && p.level('c:1') === 'none', `the marker is not a label (${JSON.stringify(p.stats)})`)
}
// off the screen: not counted
{
  const p = planLabels([cand('in', 100, 100), cand('out', -500, 100), { ...cand('behind', 0, 0), at: null }], opts)
  check(p.stats.inView === 1 && p.level('out') === 'none' && p.level('behind') === 'none', 'labels off the screen are not counted')
}
// Show all: every label in view in full
{
  const p = planLabels(crowd, { ...opts, all: true })
  check(crowd.every((c) => p.level(c.key) === 'full') && p.stats.full === crowd.length && p.stats.all, 'Show all draws every label in full')
}

console.log('labels: ok')
