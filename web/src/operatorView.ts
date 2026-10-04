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
  /** track ids the operator opened (selected on the map or in the track list) while this
   *  card was the selected one in the inbox, since it first rendered, in the order first
   *  opened */
  tracks: string[]
}

/** The inbox card the operator has selected (an intent id), or null: a track opened now is
 *  attributed to that card only, not to every card that happens to be up. */
let focused: number | null = null

export function noteFocusedIntent(id: number | null) {
  focused = id
}

/** Track openings in this console, newest last (bounded): `noteTrackViewed` is called by
 *  the app whenever the operator selects a track, with the card selected at that moment. */
const opened: { id: string; at: number; intent: number | null }[] = []
const MAX_OPENED = 256

export function noteTrackViewed(id: string) {
  opened.push({ id, at: performance.now(), intent: focused })
  if (opened.length > MAX_OPENED) opened.splice(0, opened.length - MAX_OPENED)
}

/** Distinct track ids opened at or after `since` (performance.now() ms) while card
 *  `intent` was the selected one, first-opened order, at most 64 (the bridge's bound on a
 *  view's lists). */
export function tracksViewedSince(since: number, intent: number): string[] {
  const out: string[] = []
  for (const o of opened) {
    if (o.at >= since && o.intent === intent && !out.includes(o.id)) out.push(o.id)
    if (out.length >= 64) break
  }
  return out
}

/** What a card for `i` shows: the checks (every one is rendered), the reasons and whether
 *  a rationale is on it, and the tracks the operator opened while it was the selected card,
 *  since it first rendered (`openedAt`; none on the `open` event). The first `why` line is the policy rule, shown
 *  as the tier. */
export function shownOn(i: Intent, openedAt?: number): ViewShown {
  return {
    checks: i.checks.map((c) => (c.uid != null ? `${c.rule}:${c.uid}` : c.rule)),
    warnings: (i.why ?? []).slice(1),
    rationale: !!i.rationale,
    tracks: openedAt == null ? [] : tracksViewedSince(openedAt, i.id),
  }
}

/** POST /api/operator/view; failures are swallowed (the log is instrumentation, not a
 *  decision path). `open_ms` is the time from the card's first render (`openedAt`) to this
 *  event, on `close` and `decide` only (eval/METRICS.md, "Views"). */
export function reportView(i: Intent, event: ViewEvent, openedAt: number, action?: 'approve' | 'reject') {
  const body = {
    intent_id: i.id,
    event,
    action,
    shown: shownOn(i, event === 'open' ? undefined : openedAt),
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
