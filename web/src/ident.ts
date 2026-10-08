import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, StepUpNeeded } from './decisions'

/** Rule-based identification with human review (rules 9). The engine declares a track
 *  hostile by rule when one of our sensors observed a hostile act against ours (or a track
 *  we protect) and attributed it to that track at the policy's threshold or more, nothing
 *  conflicting; what it is unsure of it flags for review instead. Neither blocks anything:
 *  any operator confirms or revokes a rule declaration, confirms or dismisses a flag, or
 *  declares the track themselves. Revoking a declaration stops every engagement in play on
 *  the track first. Agents can do none of this. */

export type Against = { kind: 'vehicle'; uid: string } | { kind: 'track'; track: string } | { kind: 'unknown' }

export interface RuleEvidence {
  act: string
  against: Against
  /** engine ms */
  observed_t: number
  /** `<uid>:<sensor>` */
  sensors: string[]
  attribution: number
  threshold: number
  /** the second observation that agreed (corroboration) */
  corroborated_by?: { act: string; observer?: string; attribution: number }
}

export type Declarer =
  | { kind: 'operator'; operator: string; session: number; note?: string; flag?: number }
  | { kind: 'rule'; rule: string; evidence: RuleEvidence }

/** A declaration as the engine's ledger holds it (and as plans and pictures carry it). */
export interface Declaration {
  id: number
  track: string
  affiliation: string
  t: number
  by: Declarer
  confirmed?: { by: string; session: number; t: number } | null
}

export type FlagKind = 'attribution' | 'conflict' | 'untracked' | 'victim' | 'proximity' | 'uncorroborated'

export const FLAG_LABEL: Record<FlagKind, string> = {
  attribution: 'attribution below threshold',
  conflict: 'conflicting evidence',
  untracked: 'author not tracked',
  victim: 'victim not ours',
  proximity: 'with a declared hostile',
  uncorroborated: 'one observation, two wanted',
}

export interface FlagStatus {
  status: 'open' | 'confirmed' | 'dismissed' | 'declared'
  by?: string
  t?: number
  track?: string
}

export interface ReviewDeclaration {
  kind: 'declaration'
  id: string
  track: string
  t: number
  affiliation: string
  declared_by: string
  basis: string
  declaration: Declaration
  actions: ('confirm' | 'revoke')[]
}

export interface ReviewFlag {
  kind: 'flag'
  id: string
  flag: number
  flag_kind: FlagKind
  tracks: string[]
  related: string | null
  act: string | null
  t: number
  evidence: string
  status: FlagStatus
  raises_suspect: boolean
  actions: ('confirm' | 'dismiss' | 'declare')[]
}

/** A declaration revoked, and whom to tell (its declarer, when someone else revoked it). */
export interface ReviewRevocation {
  id: string
  track: string
  declared_by: string
  revoked_by: string
  t: number
  note: string | null
  notify: string | null
}

/** Rules 16: a live track carrying a lost contact's AIS identity: a claim to be it, which any
 *  operator may confirm (logged under their session) only while nothing contradicts it
 *  (`blockers` empty: then `actions` has `confirm`). Until then the lost contact keeps
 *  counting in every keep-out. */
export interface ReviewLostClaim {
  kind: 'lost_claim'
  id: string
  lost: string
  track: string
  mmsi: string
  distance_m: number
  reach_m: number
  evidence: string
  blockers: string[]
  actions: 'confirm'[]
}

export interface Review {
  declarations: ReviewDeclaration[]
  flags: ReviewFlag[]
  resolved: ReviewFlag[]
  confirmed: { id: string; track: string; basis: string }[]
  revocations?: ReviewRevocation[]
  /** rules 16: claims on lost contacts, and the lost contacts confirmed as a live track */
  lost_claims?: ReviewLostClaim[]
  lost_confirmed?: { lost: string; track: string; by: string; t: number }[]
  counts: { declarations: number; flags: number; lost_claims?: number }
}

export interface Candidate {
  track: string
  distance_m: number
  sigma_m: number
  confidence: number
}

/** An act our sensors observed, and what the engine made of it. */
export interface ObservedAct {
  act: {
    act: string
    against: Against
    observed_t: number
    sensors: string[]
    location: [number, number]
    sigma_m: number
    /** further observers' fixes of it */
    fixes?: { observer: string; sensors: string[]; location: [number, number]; sigma_m: number }[]
    /** the channel it came in on (sim:contacts) */
    channel?: string
  }
  candidates: Candidate[]
  verdict: { verdict: 'declare'; track: string; attribution: number } | { verdict: 'flag'; kind: FlagKind; tracks: string[]; why: string } | { verdict: 'recorded'; why: string }
  declaration: string | null
  flag: string | null
}

/** Declared by the engine's rule and not confirmed by an operator. */
export function unconfirmedRule(d: Declaration | null | undefined) {
  return d?.by.kind === 'rule' && !d.confirmed
}

function against(a: Against) {
  return a.kind === 'vehicle' ? `our vehicle ${a.uid}` : a.kind === 'track' ? `protected track ${a.track}` : 'something we hold no track of'
}

/** The basis of a declaration in a sentence: "declared hostile by op-1", or "by rule
 *  hostile_act on act A-1 (against our vehicle 3, seen by 3:eo_ir, attribution 0.97 >= 0.90),
 *  not confirmed by an operator". `declaredBy`: the label when the declaration itself is not
 *  carried (a log from before rules 9). */
export function declarationBasis(d: Declaration | null | undefined, declaredBy?: string | null): string {
  if (!d) return declaredBy ? `declared by ${declaredBy}` : 'not declared'
  if (d.by.kind === 'operator') return `declared ${d.affiliation} by ${d.by.operator}${d.by.note ? ` (${d.by.note})` : ''}`
  const e = d.by.evidence
  const conf = d.confirmed ? `confirmed by ${d.confirmed.by}` : 'not confirmed by an operator'
  const c = e.corroborated_by
  const corr = c ? (c.observer ? `, corroborated by ${c.observer}'s fix at ${c.attribution.toFixed(2)}` : `, corroborated by act ${c.act} at ${c.attribution.toFixed(2)}`) : ''
  return `declared ${d.affiliation} by rule ${d.by.rule} on act ${e.act} (against ${against(e.against)}, seen by ${e.sensors.join(', ')}, attribution ${e.attribution.toFixed(2)} >= ${e.threshold.toFixed(2)}${corr}), ${conf}`
}

async function post(path: string, body: Record<string, unknown>) {
  const res = await operatorFetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
    body: JSON.stringify(body),
  })
  const reply = await res.json().catch(() => ({}))
  if (!res.ok) {
    const msg = reply.detail ?? `bridge returned ${res.status}`
    throw reply.step_up ? new StepUpNeeded(msg) : new Error(msg)
  }
  return reply
}

/** Confirm the rule declaration in force on a track (any operator; logged under their
 *  session). `password`, or (rules 10) a passkey's assertion made for this confirmation: when
 *  the policy lists `confirm_declaration` as a step-up. */
export function confirmDeclaration(track: string, proof: { password?: string; passkey?: Record<string, unknown> } = {}) {
  return post(`/api/tracks/${encodeURIComponent(track)}/confirm`, proof)
}

/** Revoke the declaration in force on a track (any operator): the engagements in play on it
 *  stop first, with their fallbacks. */
export function revokeDeclaration(track: string, note?: string) {
  return post(`/api/tracks/${encodeURIComponent(track)}/revoke`, { note: note?.trim() || null })
}

/** Rules 16: confirm that live track `byTrack` is lost contact `lost` again (its claim, an AIS
 *  identity; any operator, logged under their session). Refused while anything contradicts it. */
export function confirmLostClaim(lost: string, byTrack: string) {
  return post(`/api/review/lost/${encodeURIComponent(lost)}/confirm`, { by_track: byTrack })
}

/** Confirm (the concern stands) or dismiss a review flag. */
export function resolveFlag(flag: number, resolution: 'confirmed' | 'dismissed', note?: string) {
  return post(`/api/review/${flag}/resolve`, { resolution, note: note?.trim() || null })
}
