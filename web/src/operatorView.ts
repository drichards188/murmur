import type { Intent } from './decisions'
import { getOperator } from './decisions'

/** Display instrumentation for the evaluation (Phase G): when an inbox card is rendered,
 *  removed, or decided, the console tells the bridge what it showed. The bridge appends an
 *  `operator.view` event to the audit log under the operator's session; it decides nothing
 *  and replay ignores it. Posts are best-effort and never block the UI. Kept in its own
 *  module so InboxPanel only needs the two calls below. */

export type ViewEvent = 'open' | 'close' | 'decide'

export interface ViewShown {
  /** rule ids of the checks on the card */
  checks: string[]
  /** the "why this needs a human" lines */
  warnings: string[]
  rationale: boolean
  /** track ids the operator opened while the card was up (none yet: the console does not
   *  link cards to tracks; the proxy operator fills this field) */
  tracks: string[]
}

/** What a card for `i` shows: the checks (every one is rendered), the reasons and whether
 *  a rationale is on it. The first `why` line is the policy rule, shown as the tier. */
export function shownOn(i: Intent): ViewShown {
  return {
    checks: i.checks.map((c) => (c.uid != null ? `${c.rule}:${c.uid}` : c.rule)),
    warnings: (i.why ?? []).slice(1),
    rationale: !!i.rationale,
    tracks: [],
  }
}

/** POST /api/operator/view; failures are swallowed (the log is instrumentation, not a
 *  decision path). */
export function reportView(i: Intent, event: ViewEvent, openedAt: number, action?: 'approve' | 'reject') {
  const body = {
    intent_id: i.id,
    event,
    action,
    shown: shownOn(i),
    open_ms: event === 'open' ? undefined : Math.max(0, Math.round(performance.now() - openedAt)),
    client_t: Date.now(),
  }
  try {
    fetch('/api/operator/view', {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-murmur-operator': getOperator() },
      body: JSON.stringify(body),
      keepalive: true,
    }).catch(() => {})
  } catch {
    // no fetch (tests) or a blocked request: nothing to do
  }
}
