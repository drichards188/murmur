// What in the Inbox needs the person, counted once (the Inbox button and its Pending tab show the
// same number, and their titles say what it counts), and which open review flags a later decision
// by a person has made moot. Display only: nothing here resolves, dismisses or hides a flag in the
// bridge or the engine (resolving one stays the person's act, on its card); a settled flag only
// moves out of the pending strip and below the Inbox's live items, as a quiet line that still opens
// its card. Pure: needsYou.check.ts.
import { engagementRank, NEEDS_UP_TO, type EngageLike } from './inboxOrder.ts'
import { unfolded } from './swarmsModel.ts'
import type { Alert } from './decisions'
import type { SwarmAlert } from './swarmsModel'

interface FlagLike {
  id: string
  tracks: readonly string[]
  /** when it was raised, engine seconds */
  t: number
  status: { status: string } | string
}

interface DeclaredLike {
  affiliation: string
  /** an operator, or `rule:<name>` */
  by: string
  /** when it was declared, engine seconds */
  t: number
  id?: number
  rule?: boolean
  confirmed_by?: string
}

interface TrackLike {
  id: string
  declared: DeclaredLike | null
}

/** A declaration that answers what a flag asks: it says what the track is (an engagement target, or
 *  not one). Suspect and unknown are marks that restrict, not answers. */
const SETTLING = new Set(['hostile', 'neutral', 'assumed_friend'])

function statusOf(f: FlagLike): string {
  return typeof f.status === 'string' ? f.status : f.status?.status
}

/** The open flags a person has since settled, each with what settled it in words. A flag is
 *  settled when every track it names has a declaration in force that a person made or confirmed
 *  (an operator's own, or a rule declaration an operator confirmed), declared after the flag was
 *  raised and saying what the track is. Never one that holds up a nomination now (`blocking`,
 *  blocking.ts: the urgent strip's), never one naming no track or a track no longer in the
 *  picture: those stay where they were. */
export function settledFlags(
  flags: readonly FlagLike[] | null | undefined,
  tracks: readonly TrackLike[] | null | undefined,
  blocking: readonly { id: string }[] | null | undefined,
): Map<string, string> {
  const out = new Map<string, string>()
  const urgent = new Set((blocking ?? []).map((b) => b.id))
  const open = (flags ?? []).filter((f) => statusOf(f) === 'open' && !urgent.has(f.id) && f.tracks.length > 0)
  if (!open.length) return out
  // the picture by id only when a flag asks (thousands of tracks in Saturation, every message)
  const byId = new Map((tracks ?? []).map((t) => [t.id, t]))
  for (const f of open) {
    const why: string[] = []
    for (const id of f.tracks) {
      const d = byId.get(id)?.declared
      if (!d || !SETTLING.has(d.affiliation) || d.t < f.t) break
      const person = d.rule ? d.confirmed_by : d.by
      if (!person) break
      const aff = d.affiliation.replace(/_/g, ' ')
      const num = d.id != null ? ` (D-${d.id})` : ''
      why.push(d.rule ? `${id} declared ${aff} by the engine's rule${num} after it was raised, and ${person} confirmed it` : `${person} declared ${id} ${aff}${num} after it was raised`)
    }
    if (why.length === f.tracks.length) out.set(f.id, why.join('; '))
  }
  return out
}

interface InboxLike {
  pending: readonly unknown[]
  alerts: readonly Alert[]
  swarm_alerts?: readonly Pick<SwarmAlert, 'severity'>[]
  review?: {
    declarations?: readonly unknown[]
    flags?: readonly FlagLike[]
    lost_claims?: readonly unknown[]
    node_losses?: readonly unknown[]
  }
  blocking?: readonly { id: string }[]
}

export interface NeedsYou {
  /** everything below that needs the person, each item once */
  total: number
  /** approvals and engagements waiting for signatures */
  approvals: number
  /** review items: rule declarations to confirm, open flags not settled, lost-contact claims and
   *  presumed losses of ours */
  review: number
  /** of `review`, those holding up a nomination now (blocking.ts) */
  waiting: number
  high: number
  medium: number
  /** engagements under way or assessed that ask something: the picture changed, a gun firing, a
   *  verdict of persists or unknown (inboxOrder.ts `engagementRank`) */
  engagements: number
  /** not counted: informational alerts, flags settled by a later decision, alerts acknowledged
   *  in this browser */
  info: number
  settled: number
  muted: number
  /** what the total counts, and what it leaves out, in a sentence for a title */
  says: string
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`

/** The one count of what needs the person. `hidden`: alerts acknowledged here; `settled`:
 *  settledFlags' ids; `engagements`: the run's engagements (the intents' engage kinds), of which
 *  those that ask something count. An alert folded into a swarm's counts once, as the swarm's. */
export function needsYou(inbox: InboxLike | null | undefined, hidden: ReadonlySet<string>, settled: ReadonlySet<string>, engagements: readonly EngageLike[] = []): NeedsYou {
  const approvals = inbox?.pending.length ?? 0
  const r = inbox?.review
  const openFlags = (r?.flags ?? []).filter((f) => statusOf(f) === 'open')
  const settledN = openFlags.filter((f) => settled.has(f.id)).length
  const review = (r?.declarations?.length ?? 0) + openFlags.length - settledN + (r?.lost_claims?.length ?? 0) + (r?.node_losses?.length ?? 0)
  const waiting = new Set((inbox?.blocking ?? []).map((b) => b.id)).size
  const all = unfolded([...(inbox?.alerts ?? [])])
  const live = all.filter((a) => !hidden.has(a.id))
  const sev = [...live.map((a) => a.severity), ...(inbox?.swarm_alerts ?? []).map((a) => a.severity)]
  const high = sev.filter((s) => s === 'high').length
  const medium = sev.filter((s) => s === 'medium').length
  const info = sev.length - high - medium
  const muted = all.length - live.length
  const asking = engagements.filter((e) => engagementRank(e) <= NEEDS_UP_TO).length
  const total = approvals + review + high + medium + asking
  const counted = [
    approvals ? plural(approvals, 'approval or signature', 'approvals or signatures') : '',
    asking ? plural(asking, 'engagement') + ' to look at' : '',
    review ? `${plural(review, 'item')} for review${waiting ? ` (${waiting} holding up a nomination)` : ''}` : '',
    high ? plural(high, 'high alert') : '',
    medium ? plural(medium, 'medium alert') : '',
  ].filter(Boolean)
  const left = [
    info ? `${plural(info, 'alert')} for information` : '',
    settledN ? `${plural(settledN, 'flag')} a later declaration settled` : '',
    muted ? `${plural(muted, 'alert')} acknowledged here` : '',
  ].filter(Boolean)
  const says = `${total ? `${total} need${total === 1 ? 's' : ''} you: ${counted.join(', ')}` : 'Nothing needs you'}${left.length ? `. Not counted: ${left.join(', ')}` : ''}.`
  return { total, approvals, review, waiting, engagements: asking, high, medium, info, settled: settledN, muted, says }
}
