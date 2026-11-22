// Operator tasking for the AI watch officer (GET/POST /api/tasking and
// POST /api/tasking/{id}/deactivate): written requests the live officer reads each wake and
// carries out with the orders it may propose. Posting one approves nothing and decides nothing:
// every order the officer proposes still goes through the engine and the policy and waits for the
// operator where it always waits. The officer says how far it has each item (POST
// /api/tasking/{id}/ack: seen, processed; display only), answers each (`replies`, through its own
// MCP tool: an outcome, a short text, the intents it proposed and the groups it made for it), and
// the bridge says when an agent last read the list (`officer`). This module is the pure part,
// checked by tasking.check.ts; useTasking.ts has the feed and the calls, TaskingPanel.tsx the form.

/** How far the live watch officer has an item, as it acknowledges it to the bridge (display
 *  only): `posted` stored, `seen` an update listing it went to the model, `processed` the turn on
 *  that update finished. Only ever forward; a retired item keeps its last. */
export type Delivery = 'posted' | 'seen' | 'processed'

/** Why an item waits to be given to the model (`waiting`), or how the turn that carried it failed
 *  (`failed`), as the officer said it. */
export interface DeliveryNote {
  /** bridge ms */
  t: number
  /** the officer's update (wake) number */
  wake?: number
  why: string
}

/** What the officer says came of an item: done (every order it needs proposed, or its groups
 *  made), partial, blocked (none of it could go) or declined (not the officer's to do). */
export type Outcome = 'done' | 'partial' | 'blocked' | 'declined'

/** One answer of the officer's to an item. Display only: it decides nothing. */
export interface Reply {
  /** bridge ms */
  t: number
  /** the agent that wrote it */
  agent: string
  status: Outcome
  /** bottom line first, then up to four "- " lines */
  text: string
  /** the intents it proposed for the item (its own) */
  intents: number[]
  /** the groups it had made for the item when it replied */
  groups?: string[]
}

export interface TaskingItem {
  id: string
  text: string
  /** the operator who posted it */
  by: string
  /** bridge ms */
  at_t: number
  /** a deactivated item is kept for the feed but not sent to the officer */
  active: boolean
  /** absent from a bridge older than the officer's acknowledgements */
  delivery?: Delivery
  seen_t?: number
  seen_wake?: number
  processed_t?: number
  processed_wake?: number
  /** the agent that acknowledged it */
  officer?: string
  waiting?: DeliveryNote
  failed?: DeliveryNote
  /** the officer's answers, oldest first; absent: none yet, or a bridge without replies */
  replies?: Reply[]
}

/** The last read of the tasking by an agent (the watch officer reads it every couple of seconds). */
export interface OfficerRead {
  agent: string
  /** bridge ms */
  read_t: number
}

export interface TaskingDoc {
  t: number
  /** newest first */
  items: TaskingItem[]
  /** null: no agent has read it; absent: a bridge that does not say */
  officer?: OfficerRead | null
  /** true: this bridge keeps the officer's replies (absent: an older bridge, whose officer cannot) */
  officer_replies?: boolean
}

/** An item's delivery as the drawer shows it: a label, one tick (stored) or two (given to the
 *  model), a tone, and the times and update number in words for its title. */
export interface DeliveryView {
  label: string
  ticks: 1 | 2
  tone: 'muted' | 'ok' | 'warn'
  title: string
}

export function deliveryOf(i: TaskingItem, nowMs: number): DeliveryView | null {
  if (!i.delivery) return null
  const who = i.officer ? ` (${i.officer})` : ''
  const update = (n: number | undefined) => (n != null ? `update ${n}` : 'an update')
  const given = i.seen_t != null ? `Given to the watch officer${who} in ${update(i.seen_wake)}, ${ago(i.seen_t, nowMs)} ago` : ''
  if (i.delivery === 'processed') {
    const done = i.processed_t != null ? `, ${ago(i.processed_t, nowMs)} ago` : ''
    return {
      label: 'Processed',
      ticks: 2,
      tone: 'ok',
      title: `The watch officer${who} finished ${update(i.processed_wake)}, which carried this request${done}.${given ? ` ${given}.` : ''}`,
    }
  }
  if (i.delivery === 'seen') {
    if (i.failed) {
      return {
        label: 'Seen, turn failed',
        ticks: 2,
        tone: 'warn',
        title: `${given}. The turn on ${update(i.failed.wake)} did not finish (${i.failed.why}), ${ago(i.failed.t, nowMs)} ago: the next update gives it again.`,
      }
    }
    return { label: 'Seen', ticks: 2, tone: 'muted', title: `${given}; it has not finished that turn yet.` }
  }
  const stored = `Stored by the bridge ${ago(i.at_t, nowMs)} ago`
  if (i.waiting) {
    return {
      label: 'Sent, waiting',
      ticks: 1,
      tone: 'warn',
      title: `${stored}. The watch officer has it but has not been given it yet: ${i.waiting.why} (${ago(i.waiting.t, nowMs)} ago).`,
    }
  }
  return { label: 'Sent', ticks: 1, tone: 'muted', title: `${stored}; the watch officer has not been given it yet.` }
}

/** An outcome as its chip shows it. An outcome this console does not know (a newer bridge's) is
 *  shown as it is, muted. */
export function outcomeView(r: Pick<Reply, 'status' | 'agent' | 't'>, nowMs: number): { label: string; tone: 'ok' | 'warn' | 'bad' | 'muted'; title: string } {
  const known: Record<Outcome, { label: string; tone: 'ok' | 'warn' | 'bad' | 'muted'; means: string }> = {
    done: { label: 'Done', tone: 'ok', means: 'every order it needs is proposed, or its groups made' },
    partial: { label: 'Partial', tone: 'warn', means: 'some of it is done' },
    blocked: { label: 'Blocked', tone: 'bad', means: 'none of it could go' },
    declined: { label: 'Declined', tone: 'muted', means: 'not the officer\'s to do' },
  }
  const k = known[r.status as Outcome]
  const when = `${r.agent} replied ${ago(r.t, nowMs)} ago`
  if (!k) return { label: String(r.status), tone: 'muted', title: when }
  return { label: k.label, tone: k.tone, title: `${when}: ${k.means}. Orders it proposed wait in the Inbox where they need you.` }
}

/** The officer's latest answer to an item, or null. */
export function lastReply(i: Pick<TaskingItem, 'replies'>): Reply | null {
  const r = i.replies ?? []
  return r.length ? r[r.length - 1] : null
}

/** An item in force the officer has had (the turn that carried it finished, or failed) and not
 *  answered: the drawer says "No reply yet". Only from a bridge that keeps replies (its answer's
 *  `officer_replies`: an older one's officer cannot reply, so its silence means nothing), and
 *  never for a retired item. */
export function awaitsReply(i: TaskingItem, kept: boolean | undefined): boolean {
  if (kept !== true || !i.active || (i.replies?.length ?? 0) > 0) return false
  return i.delivery === 'processed' || (i.delivery === 'seen' && i.failed != null)
}

/** An officer that has not read the tasking for this long is taken as gone (it reads every 2 s). */
export const OFFICER_STALE_MS = 15_000

/** Whether a watch officer reads the tasking now, from the bridge's answer (both times bridge ms),
 *  or null for a bridge that does not say. */
export function officerOf(doc: Pick<TaskingDoc, 't' | 'officer'>): { connected: boolean; text: string } | null {
  if (doc.officer === undefined) return null
  const o = doc.officer
  if (!o || doc.t - o.read_t > OFFICER_STALE_MS) {
    return {
      connected: false,
      text: o
        ? `No watch officer has read this for ${ago(o.read_t, doc.t)} (${o.agent} was the last): requests wait as Sent until one connects.`
        : 'No watch officer is connected: requests wait as Sent until one connects.',
    }
  }
  return { connected: true, text: `Watch officer ${o.agent} connected: it reads this every few seconds.` }
}

/** How long after a turn finished the drawer still polls quickly for the officer's reply. */
export const REPLY_WAIT_MS = 60_000

/** Whether an item is still on its way to the officer (posted or seen), or its turn finished in
 *  the last minute without an answer yet (`nowMs`, bridge ms), for a quicker poll. */
export function inFlight(items: readonly TaskingItem[], nowMs = 0): boolean {
  return items.some(
    (i) =>
      i.active &&
      (i.delivery === 'posted' ||
        i.delivery === 'seen' ||
        (i.delivery === 'processed' && !(i.replies?.length ?? 0) && i.processed_t != null && nowMs - i.processed_t < REPLY_WAIT_MS)),
  )
}

/** The longest request the bridge takes. */
export const TEXT_MAX = 500

/** How long a request is as the bridge counts it: trimmed, in characters (not UTF-16 units). */
export function textLength(text: string): number {
  return [...text.trim()].length
}

/** Why `text` will not do (the bridge refuses an empty or over-long one), or null. */
export function textProblem(text: string): string | null {
  const n = textLength(text)
  if (!n) return 'Write the request first.'
  if (n > TEXT_MAX) return `A request is at most ${TEXT_MAX} characters (${n}).`
  return null
}

/** The active items, newest first, and the retired ones, newest first. */
export function split(items: readonly TaskingItem[]): { active: TaskingItem[]; retired: TaskingItem[] } {
  const byNew = (a: TaskingItem, b: TaskingItem) => b.at_t - a.at_t
  return { active: items.filter((i) => i.active).sort(byNew), retired: items.filter((i) => !i.active).sort(byNew) }
}

/** The list as it stands once `id` is deactivated, for the optimistic update. */
export function deactivated(items: readonly TaskingItem[], id: string): TaskingItem[] {
  return items.map((i) => (i.id === id ? { ...i, active: false } : i))
}

/** How long ago, in words ("40 s", "5 min", "2 h"), from the bridge clock in ms. */
export function ago(atMs: number, nowMs: number): string {
  const s = Math.max(0, Math.round((nowMs - atMs) / 1000))
  if (s < 90) return `${s} s`
  if (s < 5400) return `${Math.round(s / 60)} min`
  return `${Math.round(s / 3600)} h`
}

/** What the form says it is, shown wherever a request is written or listed: the boundary in words. */
export const BOUNDARY =
  'A request to the AI watch officer: it carries it out with the orders it may propose, where the checks allow, and replies to each. Posting it is not an approval and not a decision: every order the officer proposes is checked by the rules and routed by the policy as always, and those that need you wait in your Inbox. The officer never signs, declares, approves or releases anything, and nor does this.'
