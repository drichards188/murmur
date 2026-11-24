// The Queue drawer's words (ledger 2 section 8): time left in minutes, why an order waits in a
// line, and whether the drawer's button is shown at all (only while something is held or has just
// left the queue). Display only: nothing here decides or is logged. Pure: queueWords.check.ts.

/** An outcome stays listed this long after the order left the queue. */
export const RECENT_MS = 10 * 60_000

/** Time left, in words: "40 s left", "2 min left". */
export function leftWords(s: number): string {
  if (s <= 0) return 'out of time'
  if (s < 90) return `${Math.round(s)} s left`
  if (s < 5400) return `${Math.round(s / 60)} min left`
  return `${Math.round(s / 3600)} h left`
}

/** What changes what may be running, said in one line: an order carried over a restart or sent
 *  before its link was lost asks the vehicle what it holds first. Null when neither. */
export function whyLine(i: { resumed?: unknown; dispatched_before?: boolean }): string | null {
  const parts: string[] = []
  if (i.resumed) parts.push('carried over from before a restart')
  if (i.dispatched_before) parts.push('sent before its link was lost: the vehicle is asked what it holds first')
  return parts.length ? parts.join('; ') : null
}

/** Whether the Queue button shows: an order is held, an outcome is recent, or the drawer is open. */
export function queueShown(held: number, recent: number, open: boolean): boolean {
  return held > 0 || recent > 0 || open
}
