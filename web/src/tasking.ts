// Operator tasking for the AI watch officer (GET/POST /api/tasking and
// POST /api/tasking/{id}/deactivate): free-text guidance the live officer reads each wake and
// weighs. Guidance and priority only: it is never an order and never a decision, and the officer
// still proposes everything through the engine and the policy. The officer says how far it has
// each item (POST /api/tasking/{id}/ack: seen, processed; display only), and the bridge says when an
// agent last read the list (`officer`). This module is the pure part, checked by tasking.check.ts;
// useTasking.ts has the feed and the calls, TaskingPanel.tsx the form.

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
      title: `The watch officer${who} finished ${update(i.processed_wake)}, which carried this guidance${done}.${given ? ` ${given}.` : ''}`,
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
        ? `No watch officer has read this for ${ago(o.read_t, doc.t)} (${o.agent} was the last): guidance waits as Sent until one connects.`
        : 'No watch officer is connected: guidance waits as Sent until one connects.',
    }
  }
  return { connected: true, text: `Watch officer ${o.agent} connected: it reads this every few seconds.` }
}

/** Whether an item is still on its way to the officer (posted or seen), for a quicker poll. */
export function inFlight(items: readonly TaskingItem[]): boolean {
  return items.some((i) => i.active && (i.delivery === 'posted' || i.delivery === 'seen'))
}

/** The longest guidance the bridge takes. */
export const TEXT_MAX = 500

/** How long guidance is as the bridge counts it: trimmed, in characters (not UTF-16 units). */
export function textLength(text: string): number {
  return [...text.trim()].length
}

/** Why `text` will not do (the bridge refuses an empty or over-long one), or null. */
export function textProblem(text: string): string | null {
  const n = textLength(text)
  if (!n) return 'Write the guidance first.'
  if (n > TEXT_MAX) return `Guidance is at most ${TEXT_MAX} characters (${n}).`
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

/** What the form says it is, shown wherever guidance is written or listed: the boundary in words. */
export const BOUNDARY =
  'Guidance for the AI watch officer: what to watch and what to put first. It is not an order and not a decision. The officer still proposes orders, which the rules and the policy check as always; it never signs, declares, approves or releases anything, and nor does this.'
