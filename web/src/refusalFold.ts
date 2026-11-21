// A group order's refused or failing checks, folded (bd murmur-4nik): the bottom line first, from
// the group-level results (strength, separation, depth: uid null), then one line per rule with
// the members it holds for and the range of its figures, the members behind a toggle, and, where
// the figures allow, what would fix it (a hint, never a change). One helper for every place the
// console lists a group order's checks (the task panel's refusal and dry run, the inbox card, the
// decision's detail). A one-vehicle order is not folded: `folded` is false and the caller keeps
// its own list.
import type { Check } from './decisions'

export interface FoldMember {
  uid: string
  detail: string
}

export interface FoldLine {
  rule: string
  class: 'excluded' | 'waiting' | 'other'
  count: number
  total: number
  /** "reserves, 60 of 60: 5 to 7 min left after a 28 min task, below the 8 min (20 %) reserve" */
  text: string
  hint: string | null
  members: FoldMember[]
}

export interface RefusalFold {
  folded: boolean
  /** a group check fails: the order is refused (or would be) */
  refused: boolean
  bottom: string
  lines: FoldLine[]
}

const RULE_WORDS: Record<string, string> = { reserves: 'battery reserve' }

export function ruleWords(rule: string): string {
  return RULE_WORDS[rule] ?? rule.replace(/_/g, ' ')
}

const NUM = /-?\d+(?:\.\d+)?/g

const num = (x: number) => String(Math.round(x * 10) / 10)

function span(lo: number, hi: number, unit = ''): string {
  return lo === hi ? `${num(lo)}${unit}` : `${num(lo)} to ${num(hi)}${unit}`
}

/** Details that differ only in their numbers, as one: each number position by its range. */
export function mergeDetails(details: string[]): string {
  const first = details[0]
  const parts = first.split(NUM)
  const cols = details.map((d) => (d.match(NUM) ?? []).map(Number))
  let out = parts[0]
  for (let k = 0; k < parts.length - 1; k++) {
    const xs = cols.map((c) => c[k]).filter((x) => x != null)
    out += span(Math.min(...xs), Math.max(...xs)) + parts[k + 1]
  }
  return out
}

const RESERVES =
  /^(\d+(?:\.\d+)?) min left after task \(task (\d+(?:\.\d+)?) min, battery (\d+(?:\.\d+)?)% of (\d+(?:\.\d+)?) min\), reserve (\d+(?:\.\d+)?) min \((\d+(?:\.\d+)?)%\)$/

interface Reserves {
  left: number
  task: number
  reserve: number
  pct: number
}

function parseReserves(d: string): Reserves | null {
  const m = RESERVES.exec(d)
  return m ? { left: +m[1], task: +m[2], reserve: +m[5], pct: +m[6] } : null
}

function reservesLine(rs: Reserves[], cruiseMps: number | null): { text: string; hint: string } {
  const left = span(Math.min(...rs.map((r) => r.left)), Math.max(...rs.map((r) => r.left)))
  const task = span(Math.min(...rs.map((r) => r.task)), Math.max(...rs.map((r) => r.task)))
  const reserve = span(Math.min(...rs.map((r) => r.reserve)), Math.max(...rs.map((r) => r.reserve)))
  const pct = span(Math.min(...rs.map((r) => r.pct)), Math.max(...rs.map((r) => r.pct)))
  const short = Math.max(...rs.map((r) => r.reserve - r.left))
  const dist = cruiseMps ? ` (about ${num(Math.round((short * 60 * cruiseMps) / 100) / 10)} km at ${num(cruiseMps)} m/s, the type's cruise)` : ''
  return {
    text: `${left} min left after a ${task} min task, below the ${reserve} min (${pct} %) reserve`,
    hint:
      `Cut at least ${num(short)} min of flying${dist}: a shorter route, one nearer the members' home, or fewer or fresher members. ` +
      'The order form has no duration; the time on task is one lap or the policy minimum, whichever is longer.',
  }
}

function countPhrase(n: number, total: number, cls: FoldLine['class']): string {
  return `${n} of ${total}${cls === 'waiting' ? ' waiting' : ''}`
}

/** Fold a group order's checks. `cruiseOf` gives a member's type's cruise speed (m/s) when known. */
export function foldRefusal(checks: Check[], uids: string[], cruiseOf: (uid: string) => number | null = () => null): RefusalFold {
  const total = uids.length
  const group = checks.filter((c) => c.uid == null)
  const groupFail = group.filter((c) => c.outcome === 'fail')
  const refused = groupFail.length > 0
  if (total <= 1) return { folded: false, refused, bottom: '', lines: [] }

  const buckets = new Map<string, { rule: string; cls: FoldLine['class']; key: string; items: { uid: string; detail: string }[] }>()
  for (const c of checks) {
    if (c.uid == null || c.outcome === 'pass') continue
    const cls: FoldLine['class'] = c.member ?? 'other'
    const key = `${c.rule}|${cls}|${c.outcome}|${c.detail.replace(NUM, 'n')}`
    let b = buckets.get(key)
    if (!b) buckets.set(key, (b = { rule: c.rule, cls, key, items: [] }))
    b.items.push({ uid: c.uid, detail: c.detail })
  }
  // one line per rule, class and reason (reasons that differ in more than numbers stay apart)
  const lines: FoldLine[] = []
  for (const b of buckets.values()) {
    const details = b.items.map((x) => x.detail)
    const rs = b.rule === 'reserves' ? details.map(parseReserves) : []
    let text: string
    let hint: string | null = null
    if (rs.length > 0 && rs.every((r) => r != null)) {
      const cruise = cruiseOf(b.items[0].uid)
      const r = reservesLine(rs as Reserves[], cruise)
      text = r.text
      hint = r.hint
    } else {
      text = mergeDetails(details)
      if (text !== details[0]) hint = `One member's own figures: ${details[0]}`
    }
    lines.push({
      rule: b.rule,
      class: b.cls,
      count: b.items.length,
      total,
      text: `${b.rule}, ${countPhrase(b.items.length, total, b.cls)}: ${text}`,
      hint,
      members: b.items.map((x) => ({ uid: x.uid, detail: x.detail })),
    })
  }

  const strength = group.find((c) => c.rule === 'strength')
  const rest = group.filter((c) => c.rule !== 'strength' && c.outcome === 'fail')
  const sm = strength ? /^(\d+) of (\d+) can take it.*?minimum is (\d+)/.exec(strength.detail) : null
  let bottom = ''
  if (strength) {
    bottom = sm ? `${sm[1]} of ${sm[2]} can take it, the minimum is ${sm[3]}.` : `${strength.detail}.`
    if (strength.outcome === 'fail') bottom = `Refused: ${bottom}`
  } else if (refused) {
    bottom = 'Refused.'
  }
  const reasonsOf = (cls: 'excluded' | 'waiting') => {
    const n = new Map<string, Set<string>>()
    for (const l of lines) if (l.class === cls) n.set(l.rule, new Set([...(n.get(l.rule) ?? []), ...l.members.map((m) => m.uid)]))
    return [...n.entries()].sort((a, b) => b[1].size - a[1].size).map(([r]) => ruleWords(r)).join(', ')
  }
  const ex = new Set(lines.filter((l) => l.class === 'excluded').flatMap((l) => l.members.map((m) => m.uid))).size
  const wa = new Set(lines.filter((l) => l.class === 'waiting').flatMap((l) => l.members.map((m) => m.uid))).size
  if (ex > 0) bottom += ` ${ex === total ? `All ${total}` : ex} left out: ${reasonsOf('excluded')}.`
  if (wa > 0) bottom += ` ${wa} waiting: ${reasonsOf('waiting')}.`
  for (const c of rest) bottom += ` ${ruleWords(c.rule)}: ${c.detail}.`
  return { folded: true, refused, bottom: bottom.trim(), lines }
}
