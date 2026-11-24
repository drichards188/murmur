// The operator-to-officer loop as the dock shows it (dock.ts): what the person asked the watch
// officer (tasking.ts), what the officer proposed in the turn that read it, and what of that waits
// for a person (Awaiting.tsx, and the line on the Inbox's card); and the rules of the message box
// (OfficerCompose.tsx). Display only: nothing here decides or sends an order. The link is the
// officer's own where it gave one: its answer to a request (reply_tasking) names the intents it
// proposed for it, and the bridge refuses an intent that is not that agent's. Where no answer names
// an order, the link is a matter of time and of who: an order the officer that acknowledged the
// request proposed while the turn that first read it was going on. That says when, never that the
// request caused the order (that turn may have read other things too). Pure: dock.check.ts.
import type { Intent } from './decisions'
import { ago, officerOf, textLength, TEXT_MAX, type TaskingDoc, type TaskingItem } from './tasking.ts'

/** When the turn that first read an item ended: its finish, or the failure that ended it; still
 *  going (Infinity) while the officer has it seen and not finished. */
function turnEnd(it: TaskingItem): number {
  if (it.delivery === 'processed' && it.processed_t != null) return it.processed_t
  if (it.failed && it.seen_t != null && it.failed.t >= it.seen_t) return it.failed.t
  return Infinity
}

/** The intents the proposing agent's answers to an item name, or null where none names any (the
 *  link to that item is then by time). */
function namedBy(it: TaskingItem, agent: string): Set<number> | null {
  const ids = (it.replies ?? []).filter((r) => r.agent === agent).flatMap((r) => r.intents)
  return ids.length ? new Set(ids) : null
}

/** The requests an order was proposed for. Where the proposing agent's answer to a request names
 *  the order, those requests and no others (`explicit`); else, by time, an agent's order created
 *  between the time an item was given to that agent (`seen_t`) and the end of that turn, among the
 *  requests whose answers name none of their orders. Newest first; none for an order of a person,
 *  a script under a person's id, or a handover. */
export function turnItems(i: Pick<Intent, 'id' | 'source' | 'created_t'>, items: readonly TaskingItem[]): TaskingItem[] {
  return turnLinks(i, items).items
}

function turnLinks(i: Pick<Intent, 'id' | 'source' | 'created_t'>, items: readonly TaskingItem[]): { items: TaskingItem[]; explicit: boolean } {
  if (i.source.kind !== 'agent') return { items: [], explicit: false }
  const agent = i.source.id
  const byNew = (a: TaskingItem, b: TaskingItem) => b.at_t - a.at_t
  const named = items.filter((it) => namedBy(it, agent)?.has(i.id))
  if (named.length) return { items: named.sort(byNew), explicit: true }
  const timed = items.filter(
    (it) => namedBy(it, agent) == null && it.seen_t != null && it.seen_t <= i.created_t && i.created_t <= turnEnd(it) && (it.officer == null || it.officer === agent),
  )
  return { items: timed.sort(byNew), explicit: false }
}

/** The request an order's card names: the newest of its requests, how many more, the officer's
 *  update number, and whether the officer's own answer names the order (`explicit`) or only the
 *  time of its turn does. */
export interface TurnLink {
  item: TaskingItem
  also: number
  wake?: number
  explicit?: boolean
}

export function turnOf(i: Pick<Intent, 'id' | 'source' | 'created_t'>, items: readonly TaskingItem[]): TurnLink | null {
  const l = turnLinks(i, items)
  return l.items.length ? { item: l.items[0], also: l.items.length - 1, wake: l.items[0].seen_wake, explicit: l.explicit } : null
}

/** The orders proposed for `item` (the other way round): those its answers name, else those
 *  proposed in the turn that first read it; oldest first. */
export function proposedIn(item: TaskingItem, intents: readonly Intent[]): Intent[] {
  return intents.filter((i) => turnItems(i, [item]).length > 0).sort((a, b) => a.created_t - b.created_t)
}

/** The first `max` characters of a message on one line, for a quote. */
export function excerpt(text: string, max = 90): string {
  const one = text.trim().replace(/\s+/g, ' ')
  const chars = [...one]
  return chars.length <= max ? one : `${chars.slice(0, max - 1).join('').trimEnd()}…`
}

/** One order waiting for a person, as the head of a drawer lists it. */
export interface AwaitRow {
  id: number
  kind: string
  /** a group order's group and size ("CRISSY (8)"), one vehicle's callsign, or a count */
  what: string
  source: Intent['source']
  /** seconds left to decide, or null for an order that does not expire */
  left_s: number | null
  engage: boolean
  turn: TurnLink | null
}

/** Everything that waits for a person's approval (the Inbox's `pending`), soonest expiry first as
 *  the Inbox orders it (one that does not expire last, then by number), each with its turn link. */
export function awaitingRows(pending: readonly Intent[], items: readonly TaskingItem[], nowMs: number, nameOf: (uid: string) => string): AwaitRow[] {
  const rows = pending.map((i): AwaitRow => {
    const n = i.uids.length
    const what = i.group?.swarm ? `${i.group.swarm} (${n})` : n === 1 ? nameOf(i.uids[0]) : `${n} vehicles`
    return {
      id: i.id,
      kind: i.kind,
      what,
      source: i.source,
      left_s: i.expires_t == null ? null : (i.expires_t - nowMs) / 1000,
      engage: i.kind === 'engage',
      turn: turnOf(i, items),
    }
  })
  return rows.sort((a, b) => (a.left_s ?? Infinity) - (b.left_s ?? Infinity) || a.id - b.id)
}

// ---- whether the run has an officer to write to -----------------------------------------------

/** The situation AIs that read requests: the model watch officers. The playbook never reads
 *  /api/tasking, and `none` has no AI. */
const READS_REQUESTS = new Set(['claude', 'codex'])

/** Whether the Officer button and its drawer are shown: only where the run's AI takes requests.
 *  `situationAi`: the situation's `modes.ai` (GET /api/situation), null where no situation runs
 *  (the open world), undefined before the console has heard. In a situation, only a model officer;
 *  in the open world, only once the bridge's tasking answer says an officer has read it (`officer`
 *  with its agent, connected or since gone quiet: the drawer then says how long). A bridge without
 *  the tasking channel shows none. */
export function officerShown(situationAi: string | null | undefined, reader: Pick<TaskingDoc, 'officer'>, status: 'loading' | 'ok' | 'missing' | 'error'): boolean {
  if (status === 'missing') return false
  if (typeof situationAi === 'string') return READS_REQUESTS.has(situationAi)
  return reader.officer != null && !!reader.officer.agent
}

// ---- the message box ---------------------------------------------------------------------------

/** What a key in the message box does: Enter sends (Ctrl or Cmd with it too), Shift+Enter is a new
 *  line, and nothing while an input method composes (Enter there picks the word). */
export function composeKey(e: { key: string; shiftKey: boolean; altKey: boolean; isComposing?: boolean }): 'send' | null {
  if (e.key !== 'Enter' || e.isComposing || e.shiftKey || e.altKey) return null
  return 'send'
}

/** Whether Send may go, and why not in words where there is something to say (an empty box says
 *  nothing: its placeholder does). The officer's state is the bridge's (`officer` on GET
 *  /api/tasking, officerOf); a bridge that does not say lets it go. */
export function sendState(
  text: string,
  status: 'loading' | 'ok' | 'missing' | 'error',
  reader: Pick<TaskingDoc, 't' | 'officer'>,
  busy: boolean,
): { ok: boolean; why: string | null } {
  if (status === 'missing') return { ok: false, why: 'This bridge has no tasking channel: there is no watch officer to write to.' }
  if (status === 'loading') return { ok: false, why: 'Waiting for the bridge to answer.' }
  if (busy) return { ok: false, why: null }
  const o = reader.officer
  if (officerOf(reader)?.connected === false) {
    return {
      ok: false,
      why: o
        ? `No watch officer is connected (${o.agent} last read ${ago(o.read_t, reader.t)} ago): nobody would read this now. The draft stays here until one connects.`
        : 'No watch officer is connected: nobody would read this now. The draft stays here until one connects.',
    }
  }
  const n = textLength(text)
  if (!n) return { ok: false, why: null }
  if (n > TEXT_MAX) return { ok: false, why: `At most ${TEXT_MAX} characters: ${n - TEXT_MAX} too many.` }
  return { ok: true, why: null }
}

/** The count under the box: characters as the bridge counts them, warned near the cap. */
export function countTone(text: string): 'muted' | 'warn' | 'bad' {
  const n = textLength(text)
  return n > TEXT_MAX ? 'bad' : n >= TEXT_MAX - 50 ? 'warn' : 'muted'
}

/** At most this many vehicles are inserted by callsign; more must be one group. */
export const INSERT_MAX = 8

/** What "insert the selection" writes: the group the selection is exactly ("PICKET (60)": one of
 *  the operator's groups by name, else a fleet swarm by id, as App's `groupLabel`), or up to
 *  INSERT_MAX vehicles by callsign; for more, no text and why. Null with nothing selected. */
export function selectionInsert(group: string | null, uids: readonly number[], nameOf: (uid: number) => string): { text: string } | { text: null; why: string } | null {
  if (!uids.length) return null
  if (group) return { text: `${group} (${uids.length})` }
  if (uids.length <= INSERT_MAX) return { text: [...uids].sort((a, b) => a - b).map(nameOf).join(', ') }
  return { text: null, why: `${uids.length} vehicles are selected and they are not one group: select a group, or at most ${INSERT_MAX} vehicles, to name them here.` }
}

/** `text` with `add` put in at the caret (or over the selected part), a space either side where
 *  the words would otherwise run together; and where the caret goes after it. */
export function insertAt(text: string, start: number, end: number, add: string): { text: string; caret: number } {
  const before = text.slice(0, start)
  const after = text.slice(end)
  const lead = before && !/\s$/.test(before) ? ' ' : ''
  const trail = !after || /^[\s.,;:!?)]/.test(after) ? '' : ' '
  const out = before + lead + add + trail + after
  return { text: out, caret: (before + lead + add + trail).length }
}
