// Operator tasking for the AI watch officer (GET/POST /api/tasking and
// POST /api/tasking/{id}/deactivate): free-text guidance the live officer reads each wake and
// weighs. Guidance and priority only: it is never an order and never a decision, and the officer
// still proposes everything through the engine and the policy. This module is the pure part,
// checked by tasking.check.ts; useTasking.ts has the feed and the calls, TaskingPanel.tsx the form.

export interface TaskingItem {
  id: string
  text: string
  /** the operator who posted it */
  by: string
  /** bridge ms */
  at_t: number
  /** a deactivated item is kept for the feed but not sent to the officer */
  active: boolean
}

export interface TaskingDoc {
  t: number
  /** newest first */
  items: TaskingItem[]
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
