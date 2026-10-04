import type { Authorization, Intent } from './decisions'
import { tierLabel } from './decisions'
import type { Vehicle } from './types'
import { callsign } from './types'

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
  if (outcome.kind === 'authorized') {
    const a = outcome.authorization
    return (
      <div className="tr" role="status">
        <strong>Sent: authorized by you ({tierLabel(a.tier)})</strong>
        <span>
          This task needs a human decision ({a.reason}); sending it was yours, so it was dispatched (intent i{outcome.intentId}).
        </span>
        <div className="row">
          <button className="ghost" onClick={() => onInbox(outcome.intentId)}>
            See i{outcome.intentId} in Decisions
          </button>
          <button className="ghost" onClick={onDismiss}>
            Dismiss
          </button>
        </div>
      </div>
    )
  }
  if (outcome.kind === 'queued') {
    return (
      <div className="tr wait" role="status">
        <strong>Approved, held: a vehicle can't be reached now</strong>
        <span>
          Intent i{outcome.intentId} is queued, not sent into a dead link. It goes when the link can carry it, after its checks run
          again on the picture then; it comes back to the inbox if that picture changed, and expires if the vehicle stays out of
          reach. See it in the Queue.
        </span>
        <span className="muted">{outcome.detail}</span>
        <div className="row">
          <button className="ghost" onClick={onDismiss}>
            Dismiss
          </button>
        </div>
      </div>
    )
  }
  if (outcome.kind === 'wait') {
    return (
      <div className="tr wait" role="status">
        <strong>Not sent yet: waiting for approval</strong>
        <span>
          This task needs a human to approve it (intent i{outcome.intentId}). Nothing has been dispatched to the vehicles.
          {outcome.expires_t != null && ' It expires if nobody decides in time.'}
        </span>
        <div className="row">
          <button className="ghost" onClick={() => onInbox(outcome.intentId)}>
            {stillPending ? `Open i${outcome.intentId} in the Inbox` : `See i${outcome.intentId} in Decisions`}
          </button>
          <button className="ghost" onClick={onDismiss}>
            Dismiss
          </button>
        </div>
        <span className="muted">{stillPending ? 'Still pending.' : 'No longer pending: see its final state in Decisions.'}</span>
      </div>
    )
  }
  const i = outcome.intent
  const failed = i.checks.filter((c) => c.outcome === 'fail')
  const last = i.history[i.history.length - 1]
  return (
    <div className="tr refused" role="alert">
      <strong>Refused by the rules (intent i{i.id}). Nothing was sent.</strong>
      {failed.length > 0 ? (
        <ul>
          {failed.map((c, k) => (
            <li key={k} className="fail">
              <b>{c.rule}</b>
              {c.uid != null && <> ({name(vehicles, c.uid)})</>}: {c.detail}
              {c.fix && <> Suggested radius: {Math.ceil(c.fix.radius_m)} m.</>}
            </li>
          ))}
        </ul>
      ) : (
        <span>{last?.reason ?? outcome.detail}</span>
      )}
      <div className="row">
        <button className="ghost" onClick={onDismiss}>
          Dismiss
        </button>
        <button className="ghost" onClick={() => onInbox(i.id)}>
          See i{i.id} in Decisions
        </button>
      </div>
    </div>
  )
}
