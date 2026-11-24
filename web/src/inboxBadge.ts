// The Inbox button's badge (bd murmur-iobd, the cutdown's ledger 1 section 1 and new piece 6): one
// number (needsYou.ts, the same count as the drawer's), red only when an urgent item is among what
// it counts, grey for routine approvals and reviews; and beside it the time left on the item that
// expires soonest, once that is under a few minutes. Urgent: an item that alone holds up a
// nomination (blocking.ts), an engagement or a mine lay waiting for signatures (weapons, two
// signatures), a damage report from one of ours. Display only. Pure (no browser):
// inboxBadge.check.ts runs it under node.

interface PendingLike {
  kind: string
  /** bridge ms, or none */
  expires_t: number | null
}

interface AlertLike {
  id: string
  kind: string
}

interface InboxLike {
  pending: readonly PendingLike[]
  alerts: readonly AlertLike[]
  blocking?: readonly unknown[]
}

/** the kinds of order that need two signatures with engagement authority */
const WEAPON_ORDERS = new Set(['engage', 'lay'])

/** Whether the badge is red: something urgent is among what it counts. `hidden`: alerts
 *  acknowledged or snoozed here (not counted, so not urgent either). */
export function urgentInbox(inbox: InboxLike | null | undefined, hidden: ReadonlySet<string>): boolean {
  if (!inbox) return false
  if ((inbox.blocking?.length ?? 0) > 0) return true
  if (inbox.pending.some((i) => WEAPON_ORDERS.has(i.kind))) return true
  return inbox.alerts.some((a) => a.kind === 'damaged' && !hidden.has(a.id))
}

/** Time left shows once the soonest expiry is within this, s. */
export const LEFT_WITHIN_S = 180

/** The time left (s) on the pending item that expires soonest, when within `withinS`; else null.
 *  One already past its expiry is not counted (the bridge moves it on). */
export function soonestLeftS(pending: readonly PendingLike[], nowMs: number, withinS = LEFT_WITHIN_S): number | null {
  let best: number | null = null
  for (const i of pending) {
    if (i.expires_t == null) continue
    const left = (i.expires_t - nowMs) / 1000
    if (left <= 0 || left > withinS) continue
    if (best == null || left < best) best = left
  }
  return best
}

/** "2:05", "0:09" */
export function leftText(s: number): string {
  const n = Math.max(0, Math.ceil(s))
  return `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`
}
