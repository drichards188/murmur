// The command surface at scale, the pure part: how a selection reads, how it
// grows (a range, all of a kind), what the order says it will do, and where it goes. Display only:
// nothing here decides, routes or sends; the routing words are read from the dry run's answer
// (`would_reach`, `policy`), never worked out here. Checked by commandModel.check.ts.
import { DOMAIN_LABEL, domainOf, type Vehicle } from './types.ts'

/** What an order is, in a phrase: "Orbit 400 m", "Patrol 4 waypoints". */
export function orderPhrase(kind: string, o: { radius?: number; points?: number }): string {
  switch (kind) {
    case 'orbit':
      return o.radius != null ? `Orbit ${Math.round(o.radius)} m` : 'Orbit'
    case 'patrol':
      return `Patrol ${o.points ?? 0} waypoint${o.points === 1 ? '' : 's'}`
    case 'transit':
      return `Transit ${o.points ?? 0} point${o.points === 1 ? '' : 's'}`
    case 'hold':
      return 'Hold'
    case 'rtl':
      return 'Return to launch'
    default:
      return kind.charAt(0).toUpperCase() + kind.slice(1)
  }
}

/** Where a dry run says the order goes. `tone` for the colour: pass (out at once), wait (a person
 *  decides, or the sender's submission is their approval), fail (refused). */
export interface Routing {
  tone: 'pass' | 'wait' | 'fail'
  /** a few words for the send button: "goes at once", "waits for approval" */
  short: string
  /** the sentence the dry run shows */
  text: string
}

interface AnswerLike {
  passed: boolean
  would_reach: string
  submission_authorizes?: boolean
  policy: { rule: string; reason: string } | null
}

export function routing(a: AnswerLike, tier: (rule: string) => string): Routing {
  if (!a.passed) return { tone: 'fail', short: 'the rules would refuse it', text: 'The rules would refuse it:' }
  const why = a.policy ? ` (${tier(a.policy.rule)}: ${a.policy.reason})` : ''
  if (a.would_reach === 'approved' || a.submission_authorizes) return { tone: 'wait', short: 'your sending is the approval', text: `Checks pass. Sending it is your approval${why}.` }
  if (a.would_reach === 'auto_approved') return { tone: 'pass', short: 'goes at once', text: 'Checks pass. It would go at once (auto).' }
  if (a.would_reach === 'pending_approval') return { tone: 'wait', short: 'waits for approval', text: `Checks pass. It would wait for approval${a.policy ? `: ${a.policy.reason}` : ''}.` }
  return { tone: 'wait', short: `reaches ${a.would_reach}`, text: `Checks pass; it would reach ${a.would_reach}.` }
}

/** Where the order stands, for the three steps it shows: composing (not complete), checking (the
 *  dry run is out), ready (the dry run answered, nothing refuses), blocked (it would be refused), sent. */
export type Stage = 'compose' | 'checking' | 'ready' | 'blocked' | 'sent'

export function stage(o: { complete: boolean; loading: boolean; answered: boolean; passed: boolean; sent: boolean }): Stage {
  if (o.sent) return 'sent'
  if (!o.complete) return 'compose'
  if (!o.answered) return 'checking'
  return o.passed ? 'ready' : 'blocked'
}

export const STAGE_WORDS: Record<Stage, string> = {
  compose: 'Set the order up: where, how it is shared.',
  checking: 'Asking the rules what would happen. Nothing is sent.',
  ready: 'Checked. Send it when it reads right.',
  blocked: 'The rules would refuse it as it stands. Change it, or send to see the full refusal.',
  sent: 'Sent. Follow it in the command log.',
}

/** The selection in a line: its name when it is a group of yours (or a swarm), then its size and
 *  what is in it by domain, as the console names domains (types.ts `domainOf`, `DOMAIN_LABEL`):
 *  "North picket · 100: 94 air, 6 water". Not 100 callsigns. */
export function selectionLine(vehicles: readonly Pick<Vehicle, 'kind' | 'identity'>[], name: string | null): string {
  const by = new Map<string, number>()
  for (const v of vehicles) {
    const d = DOMAIN_LABEL[domainOf(v)].toLowerCase()
    by.set(d, (by.get(d) ?? 0) + 1)
  }
  const parts = [...by.entries()].sort((a, b) => b[1] - a[1]).map(([d, n]) => `${n} ${d}`)
  return `${name ? `${name} · ` : ''}${vehicles.length}${parts.length > 1 || name ? `: ${parts.join(', ')}` : ''}`
}

/** The uids from `a` to `b` inclusive in the order the list shows them (shift-click: a range). An
 *  anchor not in the list, or `b` not in it, is just `b`. */
export function rangeBetween(order: readonly number[], a: number | null, b: number): number[] {
  const i = a == null ? -1 : order.indexOf(a)
  const j = order.indexOf(b)
  if (i < 0 || j < 0) return [b]
  return order.slice(Math.min(i, j), Math.max(i, j) + 1)
}

/** The kinds of vehicle in the fleet, to select all of one: by platform, its short model name,
 *  most numerous first. */
export function kindsOf(vehicles: readonly Pick<Vehicle, 'uid' | 'identity'>[]): { id: string; label: string; uids: number[] }[] {
  const by = new Map<string, { id: string; label: string; uids: number[] }>()
  for (const v of vehicles) {
    const id = v.identity?.platform
    if (!id) continue
    const e = by.get(id) ?? { id, label: v.identity?.short || id, uids: [] }
    e.uids.push(v.uid)
    by.set(id, e)
  }
  return [...by.values()].sort((a, b) => b.uids.length - a.uids.length || a.label.localeCompare(b.label))
}

/** A command log card's label: "ORBIT · North picket" when the order went to a named group. */
export function cardLabel(task: string, group: string | null): string {
  return group ? `${task.toUpperCase()} · ${group}` : task.toUpperCase()
}
