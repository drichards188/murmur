import type { Authorization, Intent } from './decisions'
import { tierLabel } from './decisions'
import { ruleName } from './commandModel'
import type { Vehicle } from './types'
import { callsign } from './types'
import { RefusalFold } from './RefusalFold'

/** What the task panel shows after sending: sent on the operator's own authority (200, a
 *  human tier), waiting for approval (202), or refused (422). */
export type TaskOutcome =
  | { kind: 'authorized'; intentId: number; authorization: Authorization }
  | { kind: 'wait'; intentId: number; detail: string; expires_t: number | null }
  /** rules 5: approved, held for a vehicle the link can't carry it to now (202, queued) */
  | { kind: 'queued'; intentId: number; detail: string }
  | { kind: 'refused'; intent: Intent; detail: string }

function name(vehicles: Vehicle[], uid: string | null) {
  if (uid == null) return null
  const v = vehicles.find((x) => String(x.uid) === uid)
  return v ? callsign(v) : `vehicle ${uid}`
}

/** One line for each outcome (R12, change 27), its why in a clause, and where to follow it; the
 *  order's number only on the buttons that open it. */
export function TaskResult({
  outcome,
  vehicles,
  stillPending,
  onInbox,
  onDismiss,
}: {
  outcome: TaskOutcome
  vehicles: Vehicle[]
  /** the intent is still in the inbox */
  stillPending: boolean
  onInbox: (intentId: number) => void
  onDismiss: () => void
}) {
  const dismiss = (
    <button className="ghost" onClick={onDismiss}>
      Dismiss
    </button>
  )
  if (outcome.kind === 'authorized') {
    const a = outcome.authorization
    return (
      <div className="tr" role="status">
        <strong>Sent on your authority ({tierLabel(a.tier)}): {a.reason}.</strong>
        <div className="row">
          <button className="ghost" onClick={() => onInbox(outcome.intentId)}>
            See i{outcome.intentId} in Decisions
          </button>
          {dismiss}
        </div>
      </div>
    )
  }
  if (outcome.kind === 'queued') {
    return (
      <div className="tr wait" role="status">
        <strong>Approved, held until the link can carry it{outcome.detail ? `: ${outcome.detail}` : ''}.</strong>
        <div className="row">{dismiss}</div>
      </div>
    )
  }
  if (outcome.kind === 'wait') {
    return (
      <div className="tr wait" role="status">
        <strong>{stillPending ? `Not sent: waiting for approval in the Inbox${outcome.expires_t != null ? ', until it expires' : ''}.` : 'No longer waiting: see how it ended in Decisions.'}</strong>
        <div className="row">
          <button className="ghost" onClick={() => onInbox(outcome.intentId)}>
            {stillPending ? `Open i${outcome.intentId} in the Inbox` : `See i${outcome.intentId} in Decisions`}
          </button>
          {dismiss}
        </div>
      </div>
    )
  }
  const i = outcome.intent
  const failed = i.checks.filter((c) => c.outcome === 'fail')
  const last = i.history[i.history.length - 1]
  return (
    <div className="tr refused" role="alert">
      <strong>Refused by the rules: nothing was sent.</strong>
      {i.uids.length > 1 && i.checks.some((c) => c.uid == null) ? (
        <RefusalFold checks={i.checks} uids={i.uids} vehicles={vehicles} />
      ) : failed.length > 0 ? (
        <ul>
          {failed.map((c, k) => (
            <li key={k} className="fail">
              <b>{ruleName(c.rule)}</b>
              {c.uid != null && <> ({name(vehicles, c.uid)})</>}: {c.detail}
              {c.fix && <> Suggested radius: {Math.ceil(c.fix.radius_m)} m.</>}
            </li>
          ))}
        </ul>
      ) : (
        <span>{last?.reason ?? outcome.detail}</span>
      )}
      <div className="row">
        {dismiss}
        <button className="ghost" onClick={() => onInbox(i.id)}>
          See i{i.id} in Decisions
        </button>
      </div>
    </div>
  )
}
