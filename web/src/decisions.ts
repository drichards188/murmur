import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'
import type { Engagement, EngageView } from './engage'
import type { TaskOption } from './space'
import type { LngLat } from './types'

/** The decision layer as the bridge serves it: GET /api/inbox, GET /api/intents and the
 *  WebSocket `{"type":"decisions"}` message. Intent times are wall-clock milliseconds on the
 *  bridge's clock; alert `since_t` is wall-clock seconds. */

export type IntentState =
  | 'proposed'
  | 'checked'
  | 'auto_approved'
  | 'pending_approval'
  | 'approved'
  | 'rejected'
  | 'expired'
  | 'voided'
  | 'dispatched'
  | 'effected' // engage: the abstract effect was released; the assessment follows
  | 'aborted' // engage: stopped after dispatch, before the effect; the fallback was commanded
  | 'queued' // rules 5: approved, held for a vehicle the link can't carry it to now
  | 'done'
  | 'failed'

export type CheckOutcome = 'pass' | 'fail' | 'unverified' | 'not_evaluated'

export interface Check {
  rule: string
  uid: string | null
  outcome: CheckOutcome
  detail: string
  fix?: { fix: 'widen_radius'; radius_m: number }
}

export interface PolicyDecision {
  tier: 'auto' | 'human'
  rule: string
  reason: string
  facts: {
    kind: string
    source: 'human' | 'agent'
    armed: string[]
    leaves_approved_area: boolean
    area_detail: string
    unverified: string[]
  }
  /** distinct approvers the tier needs (two-person: 2); absent means 1 */
  approvers?: number
  /** the proposer may not approve it (always for an engagement) */
  proposer_excluded?: boolean
}

export interface Approval {
  by: string
  t: number
  self_approved: boolean
  /** the approval was the submission itself (an operator's own task in a human tier) */
  at_submission?: boolean
  note: string | null
}

/** A human-tier task the operator authorized by sending it (POST /api/tasks, 200). */
export interface Authorization {
  by: string
  at_submission: true
  tier: string
  reason: string
  why: string[]
}

/** "armed-platform" -> "armed platform", for "authorized by you (armed platform)". */
export function tierLabel(tier: string) {
  return tier.replace(/[()]/g, '').replace(/-/g, ' ')
}

export type IntentParams = {
  center?: LngLat
  radius_m?: number
  route?: LngLat[]
  target?: string
  [k: string]: unknown
}

export interface Intent {
  id: number
  created_t: number
  source: { kind: 'human' | 'agent'; id: string }
  kind: string
  params: IntentParams
  uids: string[]
  constraints: { reserve_pct?: number; duration_s?: number }
  rationale: string | null
  state: IntentState
  checks: Check[]
  policy: PolicyDecision | null
  approval: Approval | null
  expires_t: number | null
  history: { state: IntentState; t: number; reason: string }[]
  tasking_id?: number
  /** inbox only: why a human is needed, as plain lines */
  why?: string[]
  /** collect and cover: the space answer the last check read, cut down */
  space?: IntentSpace
  /** signatures before the last one, when the tier needs more than one approver */
  concurrences?: Approval[]
  /** engage: preview, approval snapshot, watch verdict, effect, stop, assessment */
  engagement?: Engagement
  /** engage: signatures and, while pending, the live picture (the bridge's view) */
  engage?: EngageView
  /** rules 5: held for an unreachable vehicle (or once was): since, until, why, and the last
   *  re-check's verdict when its vehicles could be reached again */
  queue?: QueueInfo
  /** rules 5: carried over from the previous run after a bridge restart */
  resumed?: { from: { log: string; head: string; seq: number; was: IntentState }; ops?: Record<string, string> }
  /** queued intents in the inbox message: each vehicle's reach on the world now */
  reach_now?: Reach[]
}

/** Whether one vehicle can be reached for an order now, and why. */
export interface Reach {
  uid: string
  reachable: boolean
  detail: string
}

export interface QueueInfo {
  t: number
  expires_t: number
  reach: Reach[]
  recheck?: { t: number; verdict: 'dispatch' | 'inbox' | 'void'; changes: string[] }
}

export interface IntentSpace {
  time_map: { shift_days: number }
  segment: string
  /** collect: the first ranked options, and the chosen one as the last check found it */
  options?: TaskOption[]
  options_total?: number
  exclusions_total?: number
  chosen?: TaskOption
  tasking_lead_s?: number
  /** cover */
  links?: { class_id: string; coverage_fraction: number; outages: { start: string; end: string }[]; longest_outage_s: number }
  gnss?: { class_id: string; visible_min: number; visible_max: number; pdop_max: number | null }
}

export type Severity = 'high' | 'medium'

export interface Alert {
  id: string // link:<uid> or op:<op id>; stable while the condition lasts
  kind: 'lost_link' | 'not_moving' | 'left_task' | 'task_failed' | 'ack_timeout'
  severity: Severity
  uid: number
  callsign: string | null
  detail: string
  since_t: number
  op_id?: number
  intent_id?: number
}

export interface Inbox {
  pending: Intent[]
  alerts: Alert[]
  /** rules 5: approved orders held for vehicles the link can't carry them to now */
  queued?: Intent[]
  counts: { pending: number; alerts: number; queued?: number }
}

export interface Decisions {
  inbox: Inbox
  intents: Intent[] // newest first, the latest 50
  /** satellite collect and cover runs (absent from bridges without the space executor) */
  space?: SpaceRuns
  /** the operators the bridge accepts, and those with engagement authority */
  operators?: { operators: string[]; engage: string[] }
}

// ---- satellite tasking: collect and cover runs ----------------------------------------------

/** Scenario time from the bridge clock: scenario_origin + (t - engine_origin) x rate (ms). */
export interface ScenarioMap {
  engine_origin: number
  scenario_origin: number
  rate: number
}

export function scenarioAt(m: ScenarioMap, bridgeMs: number) {
  return m.scenario_origin + Math.floor((bridgeMs - m.engine_origin) * m.rate)
}

/** One reported state of a run (also an op.state in the audit log). t is bridge time,
 *  scenario_t scenario time, both ms. */
export interface SpaceRunEvent {
  t: number
  scenario_t: number
  state: string
  detail: string
}

export interface CollectRunInfo {
  satellite: string
  norad_id: number
  class_id: string
  capability: string
  collect_time: number
  product_time: number | null
  resolution_m: number | null
  scene: LngLat[] | null
  area: LngLat[]
}

export interface CoverRunInfo {
  service: { type: 'satcom' | 'gnss'; class: string }
  window: { from: number; to: number }
  area: LngLat[]
  outages: [number, number][]
  summary: string
}

/** A dispatched collect (tasked, collected, product_ready, or missed) or cover (watching,
 *  outage, restored, fewest_in_view, ended). Simulated: no satellite operator behind it. */
export interface SpaceRun {
  intent_id: number
  kind: 'collect' | 'cover'
  resource: string
  state: string | null
  outcome: 'done' | 'failed' | null
  outcome_detail: string | null
  events: SpaceRunEvent[]
  collect?: CollectRunInfo
  cover?: CoverRunInfo
}

export interface SpaceRuns {
  clock: ScenarioMap
  loaded: boolean
  runs: SpaceRun[] // newest first
}

/** HH:MM:SS UTC. */
export function fmtZ(ms: number) {
  return new Date(ms).toISOString().slice(11, 19) + 'Z'
}

/** Scenario now from the bridge clock, or null before the bridge has sent its map. */
export function scenarioNow(space: SpaceRuns | undefined, now: () => number) {
  return space ? scenarioAt(space.clock, now()) : null
}

export const hasProduct = (r: SpaceRun) => r.kind === 'collect' && r.events.some((e) => e.state === 'product_ready')

/** POST /api/intents as the console's operator. 201 with the intent; a refused or waiting
 *  intent is still a 201 (its state says so). Throws an Error with the bridge's reason. */
export async function submitIntent(body: Record<string, unknown>): Promise<{ intent: Intent; dispatched: boolean; space: SpaceRun | null }> {
  const res = await fetch('/api/intents', {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-murmur-operator': getOperator() },
    body: JSON.stringify(body),
  })
  const b = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(b.detail ?? `bridge returned ${res.status}`)
  return b
}

export const ALERT_LABEL: Record<Alert['kind'], string> = {
  lost_link: 'LOST LINK',
  not_moving: 'NOT MOVING',
  left_task: 'LEFT TASK',
  task_failed: 'TASK FAILED',
  ack_timeout: 'NO ACK',
}

// ---- operator (placeholder authentication) ------------------------------------------------

const OPERATORS_ENV = (import.meta.env.VITE_MURMUR_OPERATORS as string | undefined) ?? 'op-1'
/** Must match the bridge's MURMUR_OPERATORS. The bridge does not publish its list (API gap),
 *  so the console takes it from VITE_MURMUR_OPERATORS at build or dev-server start. */
export const OPERATORS = OPERATORS_ENV.split(',')
  .map((s) => s.trim())
  .filter(Boolean)

/** The bridge's own list once it sends one (the decisions message carries it); until then
 *  VITE_MURMUR_OPERATORS. */
let bridgeOperators: { operators: string[]; engage: string[] } | null = null
const bridgeOperatorListeners = new Set<() => void>()

export function setBridgeOperators(ops: { operators: string[]; engage: string[] } | undefined) {
  if (!ops) return
  const same = bridgeOperators && bridgeOperators.operators.join() === ops.operators.join() && bridgeOperators.engage.join() === ops.engage.join()
  if (same) return
  bridgeOperators = ops
  bridgeOperatorListeners.forEach((l) => l())
}

/** Every operator the console may act as, and who of them has engagement authority. */
export function useOperators(): { operators: string[]; engage: string[] } {
  return useSyncExternalStore(
    (cb) => {
      bridgeOperatorListeners.add(cb)
      return () => bridgeOperatorListeners.delete(cb)
    },
    () => bridgeOperators ?? FALLBACK_OPERATORS,
  )
}

const FALLBACK_OPERATORS = { operators: OPERATORS, engage: [] as string[] }

const OPERATOR_KEY = 'murmur.operator'
let operator = (() => {
  try {
    const saved = localStorage.getItem(OPERATOR_KEY)
    // checked against the bridge's list when it arrives (setOperator keeps any listed id)
    if (saved) return saved
  } catch {
    // blocked storage: the first listed operator
  }
  return OPERATORS[0] ?? 'op-1'
})()
const operatorListeners = new Set<() => void>()

export function getOperator() {
  return operator
}

export function setOperator(id: string) {
  operator = id
  try {
    localStorage.setItem(OPERATOR_KEY, id)
  } catch {
    // not persisted; still applies for this session
  }
  operatorListeners.forEach((l) => l())
}

export function useOperator(): [string, (id: string) => void] {
  const op = useSyncExternalStore(
    (cb) => {
      operatorListeners.add(cb)
      return () => operatorListeners.delete(cb)
    },
    () => operator,
  )
  return [op, setOperator]
}

// ---- actions ------------------------------------------------------------------------------

export interface ActionResult {
  intent: Intent
  dispatched: boolean
}

/** What an approval adds beyond the note: an engagement's signature quotes the picture token
 *  its signer was shown; an approval whose dispatch stops an engagement in play says so. */
export interface ApproveExtra {
  token?: number
  acknowledge_supersede?: boolean
}

/** approve | reject | cancel. Throws an Error with the bridge's reason. */
export async function intentAction(id: number, action: 'approve' | 'reject' | 'cancel', note: string, extra?: ApproveExtra): Promise<ActionResult> {
  const body: { note?: string } & ApproveExtra = { ...(extra ?? {}) }
  if (note.trim()) body.note = note.trim()
  const res = await fetch(`/api/intents/${id}/${action}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-murmur-operator': getOperator() },
    body: JSON.stringify(body),
  })
  const reply = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(reply.detail ?? `bridge returned ${res.status}`)
  return reply as ActionResult
}

// ---- time ---------------------------------------------------------------------------------

/** Seconds since the bridge's wall clock, for expiry countdowns: the browser's clock can
 *  differ from the bridge's, so the offset is taken from the bridge's own message times. */
export function useBridgeClock(bridgeT: number | undefined) {
  const offset = useRef(0)
  const [, tick] = useState(0)
  useEffect(() => {
    if (bridgeT != null) offset.current = bridgeT * 1000 - Date.now()
  }, [bridgeT])
  useEffect(() => {
    const id = window.setInterval(() => tick((n) => n + 1), 500)
    return () => window.clearInterval(id)
  }, [])
  return useCallback(() => Date.now() + offset.current, [])
}

export function fmtDuration(s: number) {
  const n = Math.max(0, Math.round(s))
  if (n < 60) return `${n}s`
  if (n < 3600) return `${Math.floor(n / 60)}m${String(n % 60).padStart(2, '0')}s`
  return `${Math.floor(n / 3600)}h${String(Math.floor((n % 3600) / 60)).padStart(2, '0')}m`
}

/** What happened to an intent and how fast, for the Decisions list. */
export interface Verdict {
  route: 'auto' | 'human' | 'rules'
  decided: IntentState | null // approved | rejected | expired | voided, when a human was asked
  wait_s: number | null // pending_approval to the decision
}

const HUMAN_ENDS: IntentState[] = ['approved', 'rejected', 'expired', 'voided']

export function verdict(i: Intent): Verdict {
  const pend = i.history.find((h) => h.state === 'pending_approval')
  if (pend) {
    const end = i.history.find((h) => h.t >= pend.t && HUMAN_ENDS.includes(h.state))
    return { route: 'human', decided: end?.state ?? null, wait_s: end ? (end.t - pend.t) / 1000 : null }
  }
  if (i.history.some((h) => h.state === 'auto_approved')) return { route: 'auto', decided: null, wait_s: null }
  return { route: 'rules', decided: null, wait_s: null }
}

// ---- alert acknowledge and snooze (client-side) ---------------------------------------------

/** Acknowledged and snoozed alerts live only in this browser's localStorage: they are not
 *  shared between operators or consoles and the bridge doesn't know about them. An
 *  acknowledgement holds for one occurrence (the alert id plus when it started); the same
 *  condition starting again later is a new alert. */
interface AlertMemory {
  acked: Record<string, number> // alert id -> since_t acknowledged
  snoozed: Record<string, number> // alert id -> browser time (ms) when the snooze ends
}

const ALERT_KEY = 'murmur.alerts'

function loadMemory(): AlertMemory {
  try {
    const raw = localStorage.getItem(ALERT_KEY)
    if (raw) {
      const m = JSON.parse(raw)
      return { acked: m.acked ?? {}, snoozed: m.snoozed ?? {} }
    }
  } catch {
    // blocked or corrupt storage: start empty
  }
  return { acked: {}, snoozed: {} }
}

export function useAlertMemory(live: Alert[]) {
  const [mem, setMem] = useState<AlertMemory>(loadMemory)
  const save = (m: AlertMemory) => {
    setMem(m)
    try {
      localStorage.setItem(ALERT_KEY, JSON.stringify(m))
    } catch {
      // not persisted; still applies for this session
    }
  }
  // forget entries for conditions that have cleared, so storage stays small
  useEffect(() => {
    const ids = new Set(live.map((a) => a.id))
    const now = Date.now()
    const keep = (rec: Record<string, number>, expired: (v: number) => boolean) =>
      Object.fromEntries(Object.entries(rec).filter(([id, v]) => ids.has(id) && !expired(v)))
    const acked = keep(mem.acked, () => false)
    const snoozed = keep(mem.snoozed, (v) => v < now - 60_000)
    if (Object.keys(acked).length !== Object.keys(mem.acked).length || Object.keys(snoozed).length !== Object.keys(mem.snoozed).length) {
      save({ acked, snoozed })
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [live])
  return {
    isAcked: (a: Alert) => mem.acked[a.id] === a.since_t,
    /** seconds of snooze left, or null */
    snoozedUntil: (a: Alert) => {
      const u = mem.snoozed[a.id]
      return u != null && u > Date.now() ? (u - Date.now()) / 1000 : null
    },
    ack: (a: Alert) => save({ ...mem, acked: { ...mem.acked, [a.id]: a.since_t } }),
    snooze: (a: Alert, ms: number) => save({ ...mem, snoozed: { ...mem.snoozed, [a.id]: Date.now() + ms } }),
    wake: (a: Alert) => {
      const { [a.id]: _s, ...snoozed } = mem.snoozed
      const { [a.id]: _a, ...acked } = mem.acked
      save({ acked, snoozed })
    },
  }
}

/** Alert ids that are acknowledged or snoozed here (for the badge and the panel). */
export function useHiddenAlerts(decisions: Decisions | null) {
  const mem = useAlertMemory(decisions?.inbox.alerts ?? [])
  const hidden = new Set<string>()
  for (const a of decisions?.inbox.alerts ?? []) if (mem.isAcked(a) || mem.snoozedUntil(a) != null) hidden.add(a.id)
  return { mem, hidden }
}
