// Checks for inboxBadge.ts: node web/src/inboxBadge.check.ts, as make check does. The claims (bd
// murmur-iobd, ledger 1 change 4 and new piece 6): the badge is red only for a blocker, a weapon
// order waiting for signatures or a damage report not acknowledged; the time left beside it is the
// soonest expiry, only within a few minutes and never one already past. Prints "inboxBadge: ok".
import { leftText, LEFT_WITHIN_S, soonestLeftS, urgentInbox } from './inboxBadge.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`inboxBadge: ${what}`)
}

const none = new Set<string>()
const orbit = { kind: 'orbit', expires_t: null }

check(!urgentInbox(null, none), 'no inbox: not urgent')
check(!urgentInbox({ pending: [orbit, orbit], alerts: [{ id: 'link:3', kind: 'link_lost' }] }, none), 'routine approvals and alerts: grey')
check(urgentInbox({ pending: [orbit], alerts: [], blocking: [{}] }, none), 'a blocker: red')
check(urgentInbox({ pending: [{ kind: 'engage', expires_t: null }], alerts: [] }, none), 'an engagement waiting for signatures: red')
check(urgentInbox({ pending: [{ kind: 'lay', expires_t: null }], alerts: [] }, none), 'a mine lay waiting for signatures: red')
check(urgentInbox({ pending: [], alerts: [{ id: 'damaged:3', kind: 'damaged' }] }, none), 'a damage report: red')
check(!urgentInbox({ pending: [], alerts: [{ id: 'damaged:3', kind: 'damaged' }] }, new Set(['damaged:3'])), 'one acknowledged here: not counted, not urgent')

const now = 1_000_000
const at = (s: number | null) => ({ kind: 'orbit', expires_t: s == null ? null : now + s * 1000 })
check(soonestLeftS([at(300), at(null)], now) === null, 'nothing within a few minutes: no time shown')
check(soonestLeftS([at(300), at(95), at(40)], now) === 40, 'the soonest')
check(soonestLeftS([at(-2), at(120)], now) === 120, 'one already past is not counted')
check(soonestLeftS([at(LEFT_WITHIN_S)], now) === LEFT_WITHIN_S && soonestLeftS([at(LEFT_WITHIN_S + 1)], now) === null, 'the edge')
check(leftText(125) === '2:05' && leftText(8.2) === '0:09' && leftText(0) === '0:00', 'm:ss, rounded up')

console.log('inboxBadge: ok')
