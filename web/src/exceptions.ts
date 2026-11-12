// Lists by exception (docs/research/saturation-display.md R6): the fleet list by group, the track
// list with what is in focus pinned on top and the rest folded by affiliation and kind, the command
// log one card per order with its vehicles' states counted. In Saturation the fleet list was 3,602
// identical five-line rows, the track list some 500 near-identical "small drone UNKNOWN" rows and
// the command log one line per vehicle per group order (P9 to P11). What a fold holds is counted
// on it and one click away; exceptions are never folded. A fleet without groups and a picture of a
// few tracks are listed as before. Display only: no ranking here changes an affiliation or a state.
//
// Pure (no browser): exceptions.check.ts runs it under node.
import type { Op, Tasking, Vehicle } from './types'
import { isStale } from './types.ts'

// ---- the fleet list by group --------------------------------------------------------------------

/** A vehicle's state as its group's row counts it: the first that holds of damaged (an inbox
 *  `damaged` alert), lost link, returning (RTL, landing), on station (its order's op active, or
 *  done for an orbit, patrol or hold: an orbit is done once established, a patrol after its
 *  first lap, and both go on), moving, idle. */
export type VehicleState = 'damaged' | 'lost' | 'returning' | 'station' | 'moving' | 'idle'

/** The row's order: the normal states, then the exceptions (drawn in the alert colour). */
export const STATE_ORDER: VehicleState[] = ['station', 'moving', 'returning', 'idle', 'lost', 'damaged']

export const STATE_WORDS: Record<VehicleState, string> = {
  station: 'on station',
  moving: 'moving',
  returning: 'returning',
  idle: 'idle',
  lost: 'lost link',
  damaged: 'damaged',
}

/** A mode that takes the vehicle home or down: RTL, SMART_RTL, QRTL, AUTOLAND, LAND. */
const RETURNING = /RTL|RETURN|LAND/

export interface LatestOp {
  state: string
  task: string
}

/** Each vehicle's latest op: of the newest tasking that names it. */
export function latestOps(taskings: readonly Pick<Tasking, 'id' | 'task' | 'ops'>[]): Map<number, LatestOp> {
  const out = new Map<number, LatestOp>()
  for (const t of [...taskings].sort((a, b) => a.id - b.id)) for (const o of t.ops) out.set(o.uid, { state: o.state, task: t.task })
  return out
}

/** On its order's station: the op running, or done for a task that goes on once done. */
function onStation(op: LatestOp | undefined): boolean {
  return op != null && (op.state === 'active' || (op.state === 'done' && op.task !== 'rtl'))
}

type V = Pick<Vehicle, 'uid' | 'link' | 'mode' | 'groundspeed'>

export function vehicleState(v: V, latest: ReadonlyMap<number, LatestOp>, damaged: ReadonlySet<number>): VehicleState {
  if (damaged.has(v.uid)) return 'damaged'
  if (isStale(v)) return 'lost'
  if (v.mode && RETURNING.test(v.mode.toUpperCase())) return 'returning'
  if (onStation(latest.get(v.uid))) return 'station'
  if ((v.groundspeed ?? 0) >= 1) return 'moving'
  return 'idle'
}

export interface GroupContext {
  latest: ReadonlyMap<number, LatestOp>
  damaged: ReadonlySet<number>
  /** vehicles with an alert of their own (not folded into their group's) */
  alerted: ReadonlySet<number>
  selected: ReadonlySet<number>
  /** its details open */
  open: number | null
  /** just revealed from outside the list (a map click) */
  flash: number | null
}

/** A folded group lists at most this many members on their own (the open and the revealed one
 *  always); the rest are counted. */
export const GROUP_SHOWN = 5

export interface GroupView<T> {
  counts: Record<VehicleState, number>
  /** listed on their own: everyone when open, else the exceptions, the most pressing first */
  shown: T[]
  /** members not listed */
  folded: number
  /** of the folded, how many are exceptions or selected (said on the "more" line) */
  foldedExceptions: number
  selected: number
}

/** Why a member is listed on its own in a folded group, the lower the more pressing; null: it
 *  is not an exception. */
function exceptionRank(v: V, state: VehicleState, ctx: GroupContext): number | null {
  if (v.uid === ctx.open) return 0
  if (v.uid === ctx.flash) return 1
  if (state === 'damaged') return 2
  if (state === 'lost') return 3
  if (ctx.alerted.has(v.uid)) return 4
  if (ctx.selected.has(v.uid)) return 5
  return null
}

/** A group's row: its members counted by state, and who is listed on their own. */
export function groupView<T extends V>(members: readonly T[], ctx: GroupContext, expanded: boolean): GroupView<T> {
  const counts = Object.fromEntries(STATE_ORDER.map((s) => [s, 0])) as Record<VehicleState, number>
  const ranked: { v: T; rank: number }[] = []
  const rest: T[] = []
  let selected = 0
  for (const v of members) {
    const state = vehicleState(v, ctx.latest, ctx.damaged)
    counts[state]++
    if (ctx.selected.has(v.uid)) selected++
    const rank = exceptionRank(v, state, ctx)
    if (rank == null) rest.push(v)
    else ranked.push({ v, rank })
  }
  ranked.sort((a, b) => a.rank - b.rank)
  if (expanded) return { counts, shown: [...ranked.map((r) => r.v), ...rest], folded: 0, foldedExceptions: 0, selected }
  const shown = ranked.filter((r, k) => r.rank <= 1 || k < GROUP_SHOWN).map((r) => r.v)
  return { counts, shown, folded: members.length - shown.length, foldedExceptions: ranked.length - shown.length, selected }
}

// ---- the track list: focus on top, the rest folded by affiliation and kind -----------------------

/** A kind of track this common is folded into one row (fewer are listed on their own). */
export const FOLD_MIN = 5

interface T {
  id: string
  affiliation: string
  identity: { kind: string | null; level?: string | null }
}

export interface TrackFold<X extends T> {
  /** `<affiliation>|<kind>` */
  key: string
  affiliation: string
  kind: string | null
  tracks: X[]
  /** how many at each identity level ("identify", "recognise", "detect", "" for none) */
  levels: Record<string, number>
}

export interface TrackList<X extends T> {
  /** in focus, in the list's order, on top */
  pinned: X[]
  folds: TrackFold<X>[]
  /** the rest, listed on their own in the list's order */
  loose: X[]
}

const AFF_RANK: Record<string, number> = { hostile: 0, suspect: 1, unknown: 2, neutral: 3, assumed_friend: 4 }

/** The track list (`sorted`: the list's order) by exception: what is in focus pinned on top, the
 *  rest folded by affiliation and kind (this is not raid grouping: nothing by position or
 *  velocity), a kind with fewer than FOLD_MIN tracks listed on its own. Not `crowded`: listed as
 *  before. */
export function foldTracks<X extends T>(sorted: readonly X[], inFocus: (t: X) => boolean, crowded: boolean): TrackList<X> {
  if (!crowded) return { pinned: [], folds: [], loose: [...sorted] }
  const pinned: X[] = []
  const by = new Map<string, X[]>()
  for (const t of sorted) {
    if (inFocus(t)) {
      pinned.push(t)
      continue
    }
    const key = `${t.affiliation}|${t.identity.kind ?? ''}`
    const list = by.get(key)
    if (list) list.push(t)
    else by.set(key, [t])
  }
  const folds: TrackFold<X>[] = []
  const looseIds = new Set<string>()
  for (const [key, tracks] of by) {
    if (tracks.length < FOLD_MIN) {
      for (const t of tracks) looseIds.add(t.id)
      continue
    }
    const levels: Record<string, number> = {}
    for (const t of tracks) levels[t.identity.level ?? ''] = (levels[t.identity.level ?? ''] ?? 0) + 1
    folds.push({ key, affiliation: tracks[0].affiliation, kind: tracks[0].identity.kind, tracks, levels })
  }
  folds.sort((a, b) => (AFF_RANK[a.affiliation] ?? 9) - (AFF_RANK[b.affiliation] ?? 9) || b.tracks.length - a.tracks.length || a.key.localeCompare(b.key))
  return { pinned, folds, loose: sorted.filter((t) => looseIds.has(t.id)) }
}

// ---- the command log: one card per order, its vehicles counted -----------------------------------

/** An order to more vehicles than this is summarised (fewer are listed vehicle by vehicle, as before). */
export const OPS_FOLD = 5
/** An op waiting this long in a step before active (sent, accepted...) is stuck: listed, s. */
export const STUCK_S = 60

/** The op states as the summary counts them, in the lifecycle's order, then the endings. */
export const OP_ORDER = ['queued', 'sent', 'starting', 'acked', 'active', 'done', 'requeued', 'unconfirmed', 'superseded', 'failed']
const WAITING = new Set(['queued', 'sent', 'starting', 'acked'])
const ENDED_SHORT = new Set(['failed', 'superseded', 'unconfirmed', 'requeued'])

export interface OpException {
  /** the op state, or `stuck` */
  state: string
  detail: string
  uids: number[]
}

export interface OrderSummary {
  total: number
  /** [state, count] in OP_ORDER, the states present only */
  counts: [string, number][]
  /** every op that failed, was superseded, may be running, went back to the queue or is stuck,
   *  grouped by state and reason, the largest group first: always listed */
  exceptions: OpException[]
}

/** An order's ops counted by state, with its exceptions grouped (`nowS`: the bridge's clock, s). */
export function orderSummary(ops: readonly Pick<Op, 'uid' | 'state' | 'detail' | 'history'>[], nowS: number): OrderSummary {
  const counts = new Map<string, number>()
  const groups = new Map<string, OpException>()
  for (const o of ops) {
    counts.set(o.state, (counts.get(o.state) ?? 0) + 1)
    const since = o.history.length ? o.history[o.history.length - 1].t : nowS
    const state = ENDED_SHORT.has(o.state) ? o.state : WAITING.has(o.state) && nowS - since > STUCK_S ? 'stuck' : null
    if (!state) continue
    const key = `${state}|${o.detail}`
    const g = groups.get(key)
    if (g) g.uids.push(o.uid)
    else groups.set(key, { state, detail: o.detail, uids: [o.uid] })
  }
  return {
    total: ops.length,
    counts: OP_ORDER.flatMap((s) => (counts.get(s) ? [[s, counts.get(s)!] as [string, number]] : [])),
    exceptions: [...groups.values()].sort((a, b) => b.uids.length - a.uids.length || a.state.localeCompare(b.state)),
  }
}
