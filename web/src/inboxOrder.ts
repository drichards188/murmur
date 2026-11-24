// The Inbox's two lists, "Needs you" and "For information" (ledger 2 section 2, D3, requirements
// R8 to R10, rule 4: no alarm styling without a response expected). One function says where each
// item goes and in what order, so the drawer's list, its tab, the Inbox button's badge and the
// waiting strip count the same things. Display only: nothing here decides, resolves or hides
// anything in the bridge or the engine. Pure: inboxOrder.check.ts.

/** The order of "Needs you", most consequential first; items of one rank go by time left or, with
 *  none, by how long they have waited. */
export const RANK = {
  /** an engagement waiting for signatures */
  signing: 1,
  /** a review item that alone holds up a nomination */
  blocking: 2,
  /** one of ours damaged, silent or presumed lost */
  ours: 3,
  /** an assessed engagement whose target persists or whose verdict is unknown */
  again: 4,
  /** a running engagement whose picture changed, or a gun still firing */
  picture: 5,
  /** other review items: declarations, flags, claims */
  review: 6,
  /** approvals, soonest expiry first */
  approval: 7,
  /** high and medium alerts, a group's that stops and asks included */
  alert: 8,
  /** for information, in this order */
  running: 9,
  note: 10,
  settled: 11,
  acknowledged: 12,
  lost: 13,
} as const

/** Where an item sits: in "Needs you" up to rank 8, in "For information" past it. */
export const NEEDS_UP_TO = RANK.alert

export interface Ranked {
  rank: number
  /** within a rank: smaller first (time left, or minus how long it has waited); none last */
  sort: number | null
}

export function isNeeds(r: Ranked): boolean {
  return r.rank <= NEEDS_UP_TO
}

/** Items in the order the drawer shows them: by rank, then by `sort` (none last), stable. */
export function inboxOrder<T extends Ranked>(items: readonly T[]): { needs: T[]; info: T[] } {
  const key = (x: T) => x.sort ?? Number.POSITIVE_INFINITY
  const sorted = items.map((x, k) => ({ x, k })).sort((a, b) => a.x.rank - b.x.rank || key(a.x) - key(b.x) || a.k - b.k).map((e) => e.x)
  return { needs: sorted.filter(isNeeds), info: sorted.filter((x) => !isNeeds(x)) }
}

/** The engagement fields standing needs, as the intents list carries them. */
export interface EngageLike {
  state: string
  engagement?: {
    watch?: { holds: boolean } | null
    fire?: { ended?: unknown } | null
    assessment?: { result: string } | null
  } | null
}

/** Rank of an engagement that is not waiting for signatures (a running, assessed or ended one):
 *  the picture changed, a gun or salvo still firing, or a verdict of persists or unknown asks the
 *  person; every other one is for information (settled for the build, ledger 2). */
export function engagementRank(i: EngageLike): number {
  const e = i.engagement
  const abortable = i.state === 'approved' || i.state === 'dispatched' || (i.state === 'effected' && !!e?.fire && !e.fire.ended)
  if (abortable && (e?.watch ? !e.watch.holds : false)) return RANK.picture
  if (i.state === 'effected' && !!e?.fire && !e.fire.ended) return RANK.picture
  const verdict = e?.assessment?.result
  if (verdict === 'persists' || verdict === 'unknown') return RANK.again
  return RANK.running
}

/** An alert (a group's alert too, one card for its members): ours in trouble first (a lost link or one that cannot move), else by severity;
 *  an info alert asks nothing. */
const OURS_TROUBLE = new Set(['damaged', 'lost_link', 'not_moving'])

export function alertRank(kind: string, severity: string): number {
  if (severity === 'info') return RANK.note
  if (severity === 'high' && OURS_TROUBLE.has(kind)) return RANK.ours
  return RANK.alert
}

/** Secondary sort for alerts: high before medium, then the oldest first. */
export function alertSort(severity: string, sinceT: number): number {
  return (severity === 'high' ? 0 : severity === 'medium' ? 1 : 2) * 1e12 + sinceT
}

/** The engagements the Inbox shows beside the ones waiting for signatures: every one in play
 *  (approved, en route, assessing) and the last three that ended with a record. The one list the
 *  drawer draws and the badge counts, so they cannot disagree. */
export function shownEngagements<T extends { kind: string; state: string; engagement?: { snapshot?: unknown } | null }>(intents: readonly T[]): T[] {
  const all = intents.filter((i) => i.kind === 'engage')
  const running = all.filter((i) => i.state === 'approved' || i.state === 'dispatched' || i.state === 'effected')
  const ended = all.filter((i) => ['aborted', 'done', 'voided'].includes(i.state) && i.engagement?.snapshot).slice(0, 3)
  return [...running, ...ended]
}
