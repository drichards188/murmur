import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, StepUpNeeded, type Check, type Intent } from './decisions'
import type { Declaration } from './ident'
import type { LngLat } from './types'

/** Engagements (Phase E): an abstract effect (a point, a radius, a time) on a track an
 *  operator declared hostile, with one armed platform. The bridge's engine checks it, holds
 *  it for two operators with engagement authority (never its proposer), stores the picture
 *  they approved, holds that picture every second, releases the effect or stops it with its
 *  recorded fallback, and assesses. Times are bridge (engine) milliseconds. */

export type Fallback = 'abort' | 'return' | 'loiter'

export const FALLBACK_LABEL: Record<Fallback, string> = {
  abort: 'Abort: hold where it is',
  return: 'Return home',
  loiter: 'Loiter: orbit where it is',
}

export interface Ellipse {
  center: LngLat
  semi_major_m: number
  semi_minor_m: number
  bearing_deg: number
}

export interface EngagePreview {
  t: number
  window: { from?: number; until: number }
  start: LngLat
  start_t: number
  cruise_mps: number
  path: LngLat[]
  distance_m: number
  track_path: LngLat[]
  track_speed_mps: number
  effect_point: LngLat
  effect_t: number
  effect_radius_m: number
  keep_out_m: number
  track_sigma_m: number
  ellipse: Ellipse
}

export interface TrackFactsSnap {
  position: { value: LngLat; t: number }
  velocity_mps?: [number, number]
  pos_sigma_m: number
  confidence: number
  affiliation: { value: string; t: number }
  identity?: string
  identity_confidence: number
  sources: string[]
  domain: string
  last_seen_t: number
  declared_by?: string
}

export interface EngageSnapshot {
  t: number
  track_id: string
  track: TrackFactsSnap
  preview: EngagePreview
  report_interval_ms?: number
  link_gap_limit_ms: number
  fallback: Fallback
}

export interface PictureVerdict {
  t: number
  holds: boolean
  ready: boolean
  results: Check[]
}

export interface Engagement {
  preview?: EngagePreview
  snapshot?: EngageSnapshot
  watch?: PictureVerdict
  effect?: { t: number; point: LngLat; radius_m: number; planned_t: number }
  stop?: { t: number; cause: { kind: 'operator' | 'picture' | 'system' | 'superseded' | 'revoked'; by?: string | number; track?: string; declaration?: number }; reason: string; fallback?: Fallback | null }
  plan?: { t: number; by: string; preview: EngagePreview; track: TrackIdentity }
  observe?: { covered_by: string[]; track_present: boolean; seen_after_effect_t?: number }
  covered_s: number
  assessment?: { t: number; result: 'persists' | 'ceased' | 'unknown'; evidence: string; covered_s: number }
}

/** The bridge's view of an engagement for the console: signatures, and for a pending one
 *  the checks on the world now and what changed since it was nominated. */
export interface EngageView {
  track: string
  approvers_needed: number
  signed: string[]
  proposer: string | null
  proposer_excluded: boolean
  eligible: string[]
  engage_operators: string[]
  /** each current signature, when, and the effect point it was on */
  signatures?: { by: string; t: number; effect_point: LngLat; effect_t: number; self_accepted?: boolean }[]
  /** signatures dropped with their plan because it stopped holding */
  reset?: { t: number; dropped: string[]; reason: string } | null
  /** rules 6: our vehicles an operator accepted as lost for this engagement (position unknown
   *  since `since`, engine ms; null: never reported): its friendly keep-out leaves them out
   *  while they stay lost since then. Every signer's picture carries them. */
  accepted?: { uid: number | null; engine_uid: string; since: number | null; by: string }[]
  /** rules 13: the lost contacts (tracks the tracker dropped) an operator accepted for this
   *  engagement, each out of view since `since` (engine ms): its keep-out on tracks leaves
   *  them out while they stay lost since then. Every signer's picture carries them. */
  accepted_lost?: { track: string; since: number | null; by: string }[]
  /** how long a picture token is good for, and the most the signatures on one plan may take */
  token_ttl_s?: number
  max_signing_s?: number
  /** the plan the first signature froze: every later signer signs it exactly */
  plan?: { by: string; t: number; effect_point: LngLat; effect_t: number; effect_radius_m: number; track: TrackIdentity } | null
  track_now: {
    position: LngLat
    pos_sigma_m: number
    age_s: number
    affiliation: string
    declared_by: string | null
    /** rules 9: the declaration in force (an operator's or the hostile-act rule's) and its
     *  basis in a sentence */
    declaration?: Declaration | null
    basis?: string | null
    review_flags?: number[]
    identity: string | null
    identity_confidence: number
  } | null
  live?: {
    t: number
    passed: boolean
    checks: Check[]
    preview: EngagePreview | null
    changed: { rule: string; was: Check['outcome'] | null; now: Check['outcome']; detail: string; worse: boolean }[]
    track_drift_m: number | null
  }
}

export interface TrackIdentity {
  id: string
  identity?: string
  affiliation: string
  declared_by?: string
  /** rules 9: how the track was declared (an operator, or the rule and its evidence) */
  declaration?: Declaration
}

/** What the engine shows a signer (`POST /api/intents/{id}/picture`): the plan they would
 *  sign and the track as identified now, bound to their session until `expires_t`. The
 *  signature quotes its `id`. */
export interface PictureToken {
  id: number
  intent: number
  by: string
  t: number
  expires_t: number
  plan: EngagePreview
  frozen: boolean
  track: TrackIdentity
  /** rules 6: the lost vehicles accepted for it when it was shown (engine uid -> who, since
   *  which report): the signature covers them; a token from before a change is refused */
  unknown_accepted?: Record<string, { since: number | null; by: string }>
  /** rules 13: the lost contacts accepted for it when it was shown (track id -> who, since
   *  which fix) */
  lost_accepted?: Record<string, { since: number | null; by: string }>
}

/** Ask for the picture to sign (an operator with engagement authority who may sign it). */
export async function requestPicture(id: number): Promise<PictureToken> {
  return (await post(`/api/intents/${id}/picture`, {})) as PictureToken
}

export interface EngageParams {
  track: string
  effect_radius_m: number
  window?: { from?: number; until: number }
  fallback: Fallback
}

export const isEngage = (i: Intent) => i.kind === 'engage'
export const engageParams = (i: Intent) => i.params as unknown as EngageParams

/** The preview to draw for an engagement: the approval's snapshot once approved, else the
 *  live one (pending), else the nomination's. */
export function previewOf(i: Intent): EngagePreview | null {
  return i.engagement?.snapshot?.preview ?? i.engage?.live?.preview ?? i.engagement?.preview ?? null
}

/** An engagement still in play: approved, on its way, or effected and being assessed. */
export const RUNNING: Intent['state'][] = ['approved', 'dispatched', 'effected']

// ---- the bridge ---------------------------------------------------------------------------

async function post(url: string, body: unknown) {
  const res = await operatorFetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
    body: JSON.stringify(body),
  })
  const b = await res.json().catch(() => ({}))
  if (!res.ok) {
    const msg = b.detail ?? `bridge returned ${res.status}`
    // a step-up the bridge asks for (absent or wrong): the form asks for it
    throw b.step_up ? new StepUpNeeded(msg) : new Error(msg)
  }
  return b
}

export interface EngageOption {
  uid: number
  callsign: string | null
  passed: boolean
  checks: Check[]
  preview: EngagePreview | null
  /** murmur-12e: why a keep-out blocks (or nearly blocks) this option, and what would clear
   *  it; null with `keep_outs_none` saying why there is none (no preview, say) */
  keep_outs?: KeepOutExplanation | null
  keep_outs_none?: string | null
}

// ---- the keep-out explanation (murmur-12e) -------------------------------------------------

/** The bridge's `keep_outs` (`murmur_decide::keepout`): for each track or vehicle of ours that
 *  blocks the keep-out, or nearly does, the region that blocks, the numbers, and what would
 *  clear it. Read-only, from the functions the checks read on the snapshot the dry run
 *  checked: the checks decide, and nothing here relaxes a keep-out. Times are engine ms. */
export interface KeepOutExplanation {
  t: number
  track: string
  platform: string | null
  /** the platform by its callsign */
  platform_name: string | null
  effect_point: LngLat
  effect_t: number
  effect_radius_m: number
  /** every region must stay this far from the effect point: effect radius plus keep-out */
  clearance_m: number
  blocked: boolean
  /** how many block in all (`items` lists at most twelve) */
  blocking: number
  /** the keep-out checks themselves on this snapshot */
  checks: Check[]
  /** blocking first (largest shortfall first), then those that nearly block */
  items: KeepOutItem[]
  /** items not listed */
  omitted: number
  /** for the platform: which effect times (and distances) let every keep-out hold, on the
   *  picture now */
  move_within: MoveWithin | null
  /** the first nomination time at which every keep-out would hold, if everything keeps
   *  behaving as now; else `wait_none` says why not */
  wait: KeepOutWait | null
  wait_none: string | null
  assumptions: string
  note: string
}

export type KeepOutKind = 'velocity' | 'mean_velocity' | 'manoeuvre' | 'young' | 'no_velocity' | 'aircraft' | 'plausible_speed' | 'circling' | 'cruise_reach' | 'lost'

export interface KeepOutRegion {
  kind: KeepOutKind
  center: LngLat
  radius_m: number
  /** its edge from the effect point (negative: it covers the point) */
  edge_m: number
  /** this region alone reaches into the clearance */
  inside: boolean
  /** the check's own words */
  how: string
}

export interface KeepOutWait {
  nominate_t: number
  effect_t: number
  why: string
}

/** Rules 13: a lost contact as an acceptance names it: the track id it was dropped under,
 *  its last fix (engine ms), when the tracker dropped it, its AIS identity if it had one. */
export interface KeepOutLost {
  track: string
  since: number
  dropped_t: number
  ais: string | null
}

export interface KeepOutItem {
  rule: string
  /** rules 13: `lost_contact`, a track the tracker dropped that the keep-out still counts */
  subject: 'track' | 'vehicle' | 'lost_contact'
  id: string
  label: string
  domain: string | null
  /** where it is now */
  now: LngLat | null
  blocks: boolean
  /** the region the check reads (nearest the effect point) */
  region: KeepOutRegion | null
  regions: KeepOutRegion[]
  edge_m: number | null
  /** positive: by how much it fails; negative: by how much it clears */
  short_m: number | null
  altitude: string | null
  clear_by_altitude: string | null
  reason: string
  manoeuvring_now: boolean | null
  /** same picture: every effect from the earliest up to this time leaves it clear */
  fits_until_t: number | null
  /** same picture: the stretches of effect times at which it is clear */
  clear_windows: EffectWindow[]
  wait: KeepOutWait | null
  not_by_waiting: string | null
  other: string | null
  /** rules 13: a lost contact's facts for an acceptance */
  lost?: KeepOutLost | null
}

/** A stretch of effect times (engine ms); for a holding target, the platform's distances
 *  from it that make an effect then. */
export interface EffectWindow {
  from_t: number
  until_t: number
  from_m: number | null
  until_m: number | null
}

export interface MoveWithin {
  possible: boolean
  /** the earliest effect any position allows, and the latest a nomination allows */
  earliest_t: number
  latest_t: number
  /** the stretches of effect times at which every keep-out holds */
  windows: EffectWindow[]
  /** the first window, when it starts at the earliest: within `within_m` of `point` */
  effect_by_t: number | null
  point: LngLat | null
  within_m: number | null
  now_m: number | null
  holding: boolean
  why: string
}

export const KEEP_OUT_KIND_LABEL: Record<KeepOutKind, string> = {
  velocity: 'on its course',
  mean_velocity: 'average course',
  manoeuvre: 'manoeuvre reach',
  young: 'young track reach',
  no_velocity: 'no velocity yet',
  aircraft: 'aircraft reach',
  plausible_speed: 'plausible speed',
  circling: 'circling',
  cruise_reach: 'cruise reach',
  lost: 'lost: cruise reach',
}

/** A distance for the form: metres below a kilometre, else kilometres. */
export function fmtDist(m: number) {
  return Math.abs(m) >= 1000 ? `${(m / 1000).toFixed(2)} km` : `${Math.round(m)} m`
}

/** The nomination form's dry run: which armed platforms could take it now (read-only). */
export async function engageOptions(track: string, radius: number, windowS: number, fallback: Fallback): Promise<EngageOption[]> {
  const b = await post('/api/engage/options', { track, effect_radius_m: radius, window_s: windowS, fallback })
  return b.options ?? []
}

/** Nominate an engagement as the console's operator: it waits in the inbox. Rules 13:
 *  `acceptLost`, the lost contacts the operator accepts for it (each since the last fix the
 *  check names), logged as `engage.accepted` under the operator's session; it takes
 *  engagement authority (the bridge answers 403 otherwise). `proof`: the operator's step-up,
 *  when the policy marks an acceptance consequential. */
export async function nominate(req: {
  track: string
  uid: number
  radius: number
  windowS: number
  fallback: Fallback
  rationale: string
  acceptLost?: KeepOutLost[]
  proof?: { password?: string; passkey?: Record<string, unknown> }
}): Promise<Intent> {
  const accept_lost = acceptLostBody(req.acceptLost ?? [])
  const b = await post('/api/intents', {
    task: 'engage',
    uids: [req.uid],
    track: req.track,
    effect_radius_m: req.radius,
    window_s: req.windowS,
    fallback: req.fallback,
    rationale: req.rationale.trim() || null,
    ...(accept_lost.length ? { accept_lost, ...(req.proof ?? {}) } : {}),
  })
  return b.intent as Intent
}

/** Rules 13: the lost contacts an acceptance names, as the request and its step-up's passkey
 *  challenge both carry them (`{track, since}`, since its last fix in engine ms). */
export function acceptLostBody(list: KeepOutLost[]): { track: string; since: number }[] {
  return list.map((l) => ({ track: l.track, since: l.since }))
}

/** Rules 13: any operator withdraws an acceptance of a lost contact (pending or under way):
 *  the keep-out counts it again, which stops an engagement it now blocks. */
export async function revokeLost(id: number, track: string): Promise<Intent> {
  const b = await post(`/api/intents/${id}/revoke_unknown`, { track })
  return b.intent as Intent
}

/** Rules 6: any operator withdraws an acceptance of a lost vehicle (pending or under way):
 *  the checks count the vehicle again, which stops an engagement it now blocks. */
export async function revokeUnknown(id: number, uid: number): Promise<Intent> {
  const b = await post(`/api/intents/${id}/revoke_unknown`, { uid })
  return b.intent as Intent
}

/** Any operator stops an approved or dispatched engagement; its fallback is commanded. */
export async function abortEngagement(id: number, note: string): Promise<Intent> {
  const b = await post(`/api/intents/${id}/abort`, note.trim() ? { note: note.trim() } : {})
  return b.intent as Intent
}

// ---- geometry -----------------------------------------------------------------------------

/** An ellipse as a closed ring, the major axis along `bearing_deg`. */
export function ellipseRing(e: Ellipse, n = 72): LngLat[] {
  const [lon, lat] = e.center
  const b = (e.bearing_deg * Math.PI) / 180
  const mPerLon = 111_320 * Math.cos((lat * Math.PI) / 180)
  const pts: LngLat[] = []
  for (let k = 0; k <= n; k++) {
    const a = (2 * Math.PI * k) / n
    // along (major) and across (minor), rotated so the major axis points along the bearing
    const along = e.semi_major_m * Math.cos(a)
    const across = e.semi_minor_m * Math.sin(a)
    const east = along * Math.sin(b) + across * Math.cos(b)
    const north = along * Math.cos(b) - across * Math.sin(b)
    pts.push([lon + east / mPerLon, lat + north / 111_320])
  }
  return pts
}

export function fmtRel(ms: number) {
  const s = Math.round(ms / 1000)
  const sign = s < 0 ? '-' : '+'
  const a = Math.abs(s)
  return a < 60 ? `${sign}${a}s` : `${sign}${Math.floor(a / 60)}m${String(a % 60).padStart(2, '0')}s`
}

export const STATE_LABEL: Partial<Record<Intent['state'], string>> = {
  pending_approval: 'awaiting approval',
  approved: 'approved',
  dispatched: 'en route',
  effected: 'effect released, assessing',
  aborted: 'aborted',
  voided: 'voided',
  done: 'assessed',
  rejected: 'rejected',
  expired: 'expired',
}
