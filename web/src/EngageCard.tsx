import { useEffect, useRef, useState } from 'react'
import type { ApproveExtra, Check, Intent } from './decisions'
import { fmtDuration, fmtZ, getOperator, intentAction, StepUpNeeded, stepsUp, useOperator, useOperators } from './decisions'
import { reportView } from './operatorView'
import { RationaleText } from './Rationale'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import {
  abortEngagement,
  acceptBehavingFor,
  acceptLostFor,
  behavingKey,
  courseText,
  engageParams,
  FALLBACK_LABEL,
  fmtDist,
  fmtRel,
  isEngage,
  LAPSE_LABEL,
  previewOf,
  requestPicture,
  revokeBehaving,
  revokeLost,
  revokeUnknown,
  RUNNING,
  STATE_LABEL,
  tokenBehavingKey,
  type BehavingInfo,
  type EngagePreview,
  type PictureToken,
} from './engage'
import './engage.css'
import { distanceM } from './geo'
import { secondsToImpact } from './munition'
import { declarationBasis, unconfirmedRule } from './ident'
import { stepsUpAct } from './auth'
import type { Vehicle } from './types'
import { callsign } from './types'

/** Engagement nominations in the inbox (Phase E), kept apart from the generic approval
 *  card: the track as the nomination saw it and as it is now, every check (and what changed
 *  since), the preview, who may sign (two operators with engagement authority, never the
 *  proposer), a countdown to the window, live "picture changed" warnings; and, once
 *  approved, the running engagement with its watch verdict and an Abort any operator may
 *  press. The effect is an abstract event: a point, a radius, a time. */

const OUTCOME_ORDER: Record<Check['outcome'], number> = { fail: 0, unverified: 1, not_evaluated: 2, pass: 3 }
const MARK: Record<Check['outcome'], string> = { pass: 'PASS', fail: 'FAIL', unverified: 'UNVERIFIED', not_evaluated: 'NOT EVALUATED' }

/** Engage checks first (their order is the chain), then the generic vehicle rules. */
const ENGAGE_RULES = [
  'target_declared',
  'target_identity',
  'target_recency',
  'target_uncertainty',
  'engagement_zone',
  'no_strike',
  'keep_out_friendly',
  'keep_out_tracks',
  'platform_armed',
  'platform_free',
  'reach',
  'plan',
  'signing_time',
  'track_deviation',
  'platform_nav',
  'platform_link',
  'engage_window',
  'arrival',
  'effect_time',
  'alignment',
]

function nameOf(vehicles: Vehicle[], uid: string | number) {
  const v = vehicles.find((x) => String(x.uid) === String(uid))
  return v ? callsign(v) : `vehicle ${uid}`
}

/** Rules 6: the lost vehicles an operator accepted for this engagement, shown to every signer
 *  (and while it is under way), each with a button to withdraw it (any operator may). */
function Acceptances({ intent: i, vehicles, now, onDone, signing }: Pick<CardProps, 'intent' | 'vehicles' | 'now' | 'onDone'> & { signing?: boolean }) {
  const [error, setError] = useState<string | null>(null)
  const accepted = i.engage?.accepted ?? []
  // rules 16: lost contacts accepted for it
  const lost = i.engage?.accepted_lost ?? []
  // rules 19: live tracks accepted for it as behaving now, and how each stands on the picture
  const behaving = i.engage?.accepted_behaving ?? []
  const standing = new Map((i.engage?.behaving ?? []).map((b) => [b.track, b]))
  if (!accepted.length && !lost.length && !behaving.length) return null
  const revokeB = async (track: string) => {
    setError(null)
    try {
      await revokeBehaving(i.id, track)
      onDone(`i${i.id}: acceptance of ${track} as behaving now revoked by ${getOperator()}`, 'ok')
    } catch (err) {
      setError((err as Error).message)
    }
  }
  const withdrawLost = async (track: string) => {
    setError(null)
    try {
      await revokeLost(i.id, track)
      onDone(`i${i.id}: acceptance of lost contact ${track} withdrawn by ${getOperator()}`, 'ok')
    } catch (err) {
      setError((err as Error).message)
    }
  }
  const withdraw = async (uid: number) => {
    setError(null)
    try {
      await revokeUnknown(i.id, uid)
      onDone(`i${i.id}: acceptance of ${nameOf(vehicles, uid)} withdrawn by ${getOperator()}`, 'ok')
    } catch (err) {
      setError((err as Error).message)
    }
  }
  return (
    <>
      {accepted.map((a) => (
        <p key={a.engine_uid} className="eg-alarm" role="status">
          Accepted as lost by {a.by}: {a.uid == null ? a.engine_uid : nameOf(vehicles, a.uid)}, position unknown{' '}
          {a.since == null ? '(it never reported one)' : `since ${fmtRel(a.since - now())}`}. The friendly keep-out leaves it out while it stays lost since then
          {signing ? '; your signature covers this acceptance.' : '.'}{' '}
          {a.uid != null && (
            <button type="button" className="ghost mini" onClick={(ev) => { ev.stopPropagation(); void withdraw(a.uid as number) }}>
              Withdraw
            </button>
          )}
        </p>
      ))}
      {behaving.map((a) => {
        const b = standing.get(a.track)
        return (
          <div key={`behaving-${a.track}`} className="eg-alarm eg-behaving" role="status">
            <b>Accepted as behaving now</b> by {a.by} {fmtRel(a.t - now())}: {a.track} ({a.identity ?? 'unclassified'}, {a.affiliation}, {a.domain}),{' '}
            {courseText(a.velocity_mps, a.velocity_sigma_mps)} on its estimate at {fmtZ(a.estimate_t)}. The keep-out counts it along that course, not anywhere it
            could turn or speed up to; the acceptance lapses on its own if it departs from that course beyond its uncertainty or goes out of view
            {signing ? '; your signature covers this acceptance.' : '.'}{' '}
            <button
              type="button"
              className="ghost mini"
              onClick={(ev) => {
                ev.stopPropagation()
                void revokeB(a.track)
              }}
              title="Any operator may revoke it: the keep-out counts the track by all its regions again, which stops an engagement it then blocks"
            >
              Revoke
            </button>
            {b && !b.lapsed && <span className={`eg-behaving-now ${b.clears ? 'clears' : 'short'}`}>Now: {b.reason}</span>}
          </div>
        )
      })}
      {lost.map((a) => (
        <p key={`lost-${a.track}`} className="eg-alarm" role="status">
          Accepted as a lost contact by {a.by}: {a.track}, dropped by the tracker, out of view {a.since == null ? '' : `since ${fmtRel(a.since - now())}`}. The keep-out
          on tracks leaves it out while it stays lost since then (a track an operator confirms as it counts as any track in its place)
          {signing ? '; your signature covers this acceptance.' : '.'}{' '}
          <button type="button" className="ghost mini" onClick={(ev) => { ev.stopPropagation(); void withdrawLost(a.track) }}>
            Withdraw
          </button>
        </p>
      ))}
      {error && <p className="error ix-error">{error}</p>}
    </>
  )
}

/** Rules 16: while the nomination waits with no signature, an operator with engagement
 *  authority may accept a lost contact for it (as the check names it, since its last fix),
 *  with the step-up when the policy lists `accept_unknown`. Logged under their session, shown
 *  to every signer, cleared by a reset. */
function AcceptLostHere({ intent: i, now, onDone }: Pick<CardProps, 'intent' | 'now' | 'onDone'>) {
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [step, setStep] = useStepUp()
  const [bad, setBad] = useState(false)
  const [operator] = useOperator()
  const ops = useOperators()
  const open = i.engage?.lost_open ?? []
  const hasAuthority = (i.engage?.engage_operators ?? ops.engage).includes(operator)
  if (!open.length || !hasAuthority) return null
  const stepUp = stepsUpAct('accept_unknown')
  const accept = async (l: { track: string; since: number }) => {
    setBusy(l.track)
    setError(null)
    setBad(false)
    try {
      const proof = stepUp ? await stepUpProof(step, { act: 'accept_unknown', intent: i.id, accept_lost: [{ track: l.track, since: l.since }] }) : undefined
      await acceptLostFor(i.id, l, proof)
      setStep({ password: '' })
      onDone(`i${i.id}: lost contact ${l.track} accepted for this engagement by ${operator}`, 'ok')
    } catch (err) {
      if (err instanceof StepUpNeeded) setBad(true)
      setError((err as Error).message)
    }
    setBusy(null)
  }
  return (
    <div className="eg-accept-lost-here">
      {open.map((l) => (
        <p key={l.track} className="muted">
          Lost contact {l.track} ({l.identity ?? 'unclassified'}, {l.affiliation}
          {l.ais ? `, AIS ${l.ais}` : ''}), out of view since {fmtRel(l.since - now())}: the keep-out counts it anywhere it could have got to since.{' '}
          <button
            type="button"
            className="ghost mini"
            disabled={busy != null || (stepUp && !stepUpReady(step))}
            onClick={(ev) => {
              ev.stopPropagation()
              void accept(l)
            }}
            title="Accept its unknown position for this engagement: logged under your session, shown to every signer, cleared if the signatures are dropped"
          >
            Accept for this engagement
          </button>
        </p>
      ))}
      {stepUp && (
        <StepUpField
          passwordLabel="Consequential: your password to accept a lost contact"
          purpose="to accept this lost contact for this engagement"
          ariaLabel="password to accept a lost contact"
          value={step}
          onChange={(s) => setStep(s)}
          busy={busy != null}
          bad={bad}
        />
      )}
      {error && <p className="error ix-error">{error}</p>}
    </div>
  )
}

/** Rules 19: the acceptances as behaving now that lapsed on their own, shown prominently to
 *  every signer (and while the engagement is under way): which track, who had accepted it,
 *  when, and the engine's sentence with the numbers. A lapse the picture shows before the
 *  engine logs it (its next step) is shown too. */
function BehavingLapses({ intent: i, now }: Pick<CardProps, 'intent' | 'now'>) {
  const lapsed = i.engage?.lapsed ?? []
  const accepted = new Set((i.engage?.accepted_behaving ?? []).map((a) => a.track))
  // lapsing on the picture now, not yet logged: an acceptance still in force that the picture lapses
  const lapsing = (i.engage?.behaving ?? []).filter((b) => b.lapsed && b.accepted_by && accepted.has(b.track))
  if (!lapsed.length && !lapsing.length) return null
  return (
    <div className="eg-alarm eg-lapse" role="alert">
      <b>Acceptance as behaving now lapsed</b>
      <ul>
        {lapsing.map((b) => (
          <li key={`now-${b.track}`}>
            {b.track} (accepted by {b.accepted_by}) is lapsing now: {b.lapsed}
          </li>
        ))}
        {lapsed.map((l) => (
          <li key={`${l.track}@${l.t}`}>
            {l.track} (accepted by {l.accepted_by}) {LAPSE_LABEL[l.kind] ?? l.kind} {fmtRel(l.t - now())}: {l.detail}
          </li>
        ))}
      </ul>
      <span className="eg-lapse-foot">The keep-out counts it by all its regions again; accepting it again takes a person.</span>
    </div>
  )
}

/** Rules 19: while the nomination waits with no signature, an operator with engagement
 *  authority may accept a live blocker as it is behaving now (its estimate then), with the
 *  step-up when the policy lists `accept_unknown`. Logged under their session, shown to every
 *  signer, cleared by a reset; it lapses on its own. Never offered to anyone without engagement
 *  authority (an agent has no route to it at all). */
function AcceptBehavingHere({ intent: i, onDone }: Pick<CardProps, 'intent' | 'onDone'>) {
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [step, setStep] = useStepUp()
  const [bad, setBad] = useState(false)
  const [operator] = useOperator()
  const ops = useOperators()
  const open = (i.engage?.behaving ?? []).filter((b) => !b.accepted_by)
  const hasAuthority = (i.engage?.engage_operators ?? ops.engage).includes(operator)
  if (!i.engage?.behaving_acceptable_now || !open.length || !hasAuthority) return null
  const stepUp = stepsUpAct('accept_unknown')
  const anyAcceptable = open.some((b) => b.acceptable)
  const accept = async (b: BehavingInfo) => {
    setBusy(b.track)
    setError(null)
    setBad(false)
    try {
      const proof = stepUp ? await stepUpProof(step, { act: 'accept_unknown', intent: i.id, accept_behaving: [{ track: b.track }] }) : undefined
      await acceptBehavingFor(i.id, b.track, proof)
      setStep({ password: '' })
      onDone(`i${i.id}: ${b.track} accepted as behaving now by ${operator}`, 'ok')
    } catch (err) {
      if (err instanceof StepUpNeeded) setBad(true)
      setError((err as Error).message)
    }
    setBusy(null)
  }
  return (
    <div className="eg-accept-behaving-here">
      {open.map((b) => (
        <div key={b.track} className={`ko-behaving ${b.acceptable ? '' : 'not'}`}>
          <span className="eg-behaving-head">
            Live blocker <b>{b.track}</b>
            {b.acceptable ? `, ${courseText(b.velocity_mps, b.velocity_sigma_mps)} on its estimate at ${fmtZ(b.estimate_t)}` : ''}
          </span>
          <div className="ko-behaving-reason">{b.reason}</div>
          {b.acceptable && !b.clears && (
            <div className="ko-behaving-short">
              Accepting it would not clear the keep-out{b.short_m != null && b.short_m > 0 ? `: it would still be short ${fmtDist(b.short_m)}` : ''}.
            </div>
          )}
          {b.acceptable && (
            <button
              type="button"
              className="ghost mini"
              disabled={busy != null || (stepUp && !stepUpReady(step))}
              onClick={(ev) => {
                ev.stopPropagation()
                void accept(b)
              }}
              title="Accept it as it is behaving now, for this engagement only: logged under your session with its estimate now, shown to every signer, cleared if the signatures are dropped; it lapses on its own"
            >
              Accept as behaving now
            </button>
          )}
        </div>
      ))}
      {stepUp && anyAcceptable && (
        <StepUpField
          passwordLabel="Consequential: your password to accept a live track as behaving now"
          purpose="to accept this track as behaving now for this engagement"
          ariaLabel="password to accept a live track as behaving now"
          value={step}
          onChange={(s) => setStep(s)}
          busy={busy != null}
          bad={bad}
        />
      )}
      {error && <p className="error ix-error">{error}</p>}
    </div>
  )
}

function km(m: number) {
  return m >= 1000 ? `${(m / 1000).toFixed(2)} km` : `${Math.round(m)} m`
}

function ll(p: [number, number]) {
  return `${p[1].toFixed(5)}, ${p[0].toFixed(5)}`
}

/** The checks as the bridge last ran them on the world now (`live`), each with what it was at
 *  the nomination; without a live run, the nomination's. An engagement check is matched by
 *  rule alone (its subject changes: a keep-out names the vehicle that fails it), the
 *  generic vehicle rules by rule and vehicle. */
function ChecksList({ checks, live, vehicles }: { checks: Check[]; live?: Check[]; vehicles: Vehicle[] }) {
  const key = (c: Check) => (ENGAGE_RULES.includes(c.rule) ? c.rule : `${c.rule}|${c.uid ?? ''}`)
  const atNomination = new Map(checks.map((c) => [key(c), c]))
  const rank = (c: Check) => {
    const k = ENGAGE_RULES.indexOf(c.rule)
    return (k < 0 ? 100 : k) + OUTCOME_ORDER[c.outcome] * 0.01
  }
  const sorted = [...(live ?? checks)].sort((a, b) => rank(a) - rank(b))
  return (
    <div className="eg-checks">
      {sorted.map((c, k) => {
        const then = live ? atNomination.get(key(c)) : undefined
        const changed = then != null && then.outcome !== c.outcome
        return (
          <div key={k} className={`ix-check ${c.outcome} ${changed ? 'eg-changed' : ''}`}>
            <span className="ix-mark">{MARK[c.outcome]}</span>
            <span className="ix-text">
              <span className="ix-rule">
                {c.rule}
                {c.uid != null && <i> {nameOf(vehicles, c.uid)}</i>}
                {changed && <em className="eg-was"> was {MARK[then.outcome].toLowerCase()} at nomination</em>}
              </span>
              <span className="ix-detail">{c.detail}</span>
            </span>
          </div>
        )
      })}
    </div>
  )
}

function PreviewFacts({ p, now, platform }: { p: EngagePreview; now: () => number; platform: string }) {
  return (
    <dl className="eg-facts">
      <dt>Effect</dt>
      <dd>
        <b>{fmtRel(p.effect_t - now())}</b> at {ll(p.effect_point)}, radius {Math.round(p.effect_radius_m)} m
      </dd>
      <dt>Platform</dt>
      <dd>
        {platform}: {km(p.distance_m)} at {p.cruise_mps.toFixed(0)} m/s, straight line, starts {fmtRel(p.start_t - p.t)} after the decision
      </dd>
      <dt>Track</dt>
      <dd>
        {p.track_speed_mps < 0.3 ? 'holding' : `${p.track_speed_mps.toFixed(1)} m/s`}, 1-sigma {Math.round(p.track_sigma_m)} m now, {Math.round(p.ellipse.semi_major_m)} by {Math.round(p.ellipse.semi_minor_m)} m at the effect time
      </dd>
      <dt>Keep-out</dt>
      <dd>{Math.round(p.keep_out_m)} m around the effect point (no vehicle of ours, no protected track)</dd>
      <dt>Window</dt>
      <dd>closes {fmtRel(p.window.until - now())}</dd>
    </dl>
  )
}

function WindowCountdown({ until, start, now }: { until: number; start: number; now: () => number }) {
  const left = (until - now()) / 1000
  const total = Math.max(1, (until - start) / 1000)
  const frac = Math.min(1, Math.max(0, left / total))
  return (
    <span className={`ix-count ${left <= 0 ? 'out' : left < 60 ? 'urgent' : ''}`} title="time left in the engagement window">
      <b>{left <= 0 ? 'window closed' : `window ${fmtDuration(left)}`}</b>
      <span className="ix-count-bar">
        <span style={{ width: `${frac * 100}%` }} />
      </span>
    </span>
  )
}

interface CardProps {
  intent: Intent
  vehicles: Vehicle[]
  now: () => number
  selected: boolean
  onFocus: () => void
  onDone: (text: string, tone: 'ok' | 'bad') => void
}

/** A pending engagement nomination. */
export function EngageCard({ intent: i, vehicles, now, selected, onFocus, onDone }: CardProps) {
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // rules 8: every engagement signature re-enters the signer's credential (credentials run),
  // so a signature can't come from a stale or shared session; rules 10: by passkey where the
  // run asks for one (its challenge made for this very picture)
  const stepUp = stepsUp()
  const [step, setStep] = useStepUp()
  const [stepUpError, setStepUpError] = useState(false)
  const [operator] = useOperator()
  const ops = useOperators()
  const view = i.engage
  const p = engageParams(i)
  const nomination = i.engagement?.preview ?? null
  const live = view?.live
  const pv = live?.preview ?? nomination
  const platform = nameOf(vehicles, i.uids[0] ?? '?')
  const needed = view?.approvers_needed ?? i.policy?.approvers ?? 1
  const signed = view?.signed ?? []
  const isProposer = i.source.kind === 'human' && i.source.id === operator
  const hasAuthority = (view?.engage_operators ?? ops.engage).includes(operator)
  const alreadySigned = signed.includes(operator)
  const canSign = hasAuthority && !isProposer && !alreadySigned
  const expired = i.expires_t != null && i.expires_t <= now()
  const worse = (live?.changed ?? []).filter((c) => c.worse)
  const drift = live?.track_drift_m ?? null
  // how old the live checks on screen are; the signature itself quotes a picture token the
  // engine issues when asked (look, then sign)
  const ageS = live ? (now() - live.t) / 1000 : null
  const stale = ageS == null || ageS > 10
  const livePasses = live?.passed === true && live.preview != null
  const plan = view?.plan ?? null
  const [shown, setShown] = useState<PictureToken | null>(null)
  const shownLeft = shown ? (shown.expires_t - now()) / 1000 : 0
  // rules 19: an acceptance as behaving now given, revoked or lapsed while a picture is shown
  // makes the signature on it refused (409): drop it and say to look again. Only on a change of
  // the card's acceptances, so a card a moment behind the token never drops a fresh one.
  const behavingNow = behavingKey(view?.accepted_behaving ?? [])
  const [behavingSeen, setBehavingSeen] = useState(behavingNow)
  if (behavingSeen !== behavingNow) {
    setBehavingSeen(behavingNow)
    if (shown && tokenBehavingKey(shown) !== behavingNow) {
      setShown(null)
      setError('The tracks accepted for it as behaving now changed since you looked: look again.')
    }
  }
  const lapses = (view?.lapsed?.length ?? 0) > 0
  const signHint = isProposer
    ? `You (${operator}) proposed this: the proposer may not approve it (two-person rule).`
    : !hasAuthority
      ? stepUp
        ? `${operator} has no engagement authority. Operators who may sign: ${(view?.eligible ?? []).join(', ') || 'none listed'}; each signs at their own console with their own password.`
        : `${operator} has no engagement authority (MURMUR_ENGAGE_OPERATORS). Act as one of: ${(view?.eligible ?? []).join(', ') || 'none listed'}.`
      : alreadySigned
        ? `You (${operator}) already signed; another operator with engagement authority must.`
        : !live
          ? 'No live picture from the bridge yet: wait for it before signing.'
          : !livePasses
            ? 'Some check fails on the world now: it cannot be signed as it stands.'
            : stale
              ? `The checks on screen are ${Math.round(ageS ?? 0)} s old: wait for fresh ones.`
              : plan
                ? `Signing as ${operator}: signature ${signed.length + 1} of ${needed}, on the plan ${plan.by} froze. Look first: the engine shows you exactly what you sign.`
                : `Signing as ${operator}: signature 1 of ${needed}; your signature freezes the plan you are shown. Look first.`
  const canApprove = canSign && livePasses && !stale
  // evaluation instrumentation (operatorView.ts), as on the approval card
  const viewed = useRef({ openedAt: performance.now(), decided: false })
  useEffect(() => {
    const v = viewed.current
    reportView(i, 'open', v.openedAt)
    return () => {
      if (!v.decided) reportView(i, 'close', v.openedAt)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [i.id])

  // look: the engine shows the plan this signature would be on, and the track now
  const look = async () => {
    setBusy(true)
    setError(null)
    try {
      setShown(await requestPicture(i.id))
    } catch (e) {
      setError((e as Error).message)
    }
    setBusy(false)
  }

  const act = async (action: 'approve' | 'reject') => {
    setBusy(true)
    setError(null)
    viewed.current.decided = true
    reportView(i, 'decide', viewed.current.openedAt, action)
    setStepUpError(false)
    try {
      const extra: ApproveExtra = {}
      if (action === 'approve' && shown) extra.token = shown.id
      if (action === 'approve' && stepUp) Object.assign(extra, await stepUpProof(step, { act: 'sign', intent: i.id, token: shown?.id }))
      setShown(null)
      setStep({ password: '' })
      const r = await intentAction(i.id, action, note, extra)
      const last = r.intent.history[r.intent.history.length - 1]
      if (action === 'reject') onDone(`i${i.id} engagement rejected by ${getOperator()}`, 'ok')
      else if (r.intent.state === 'pending_approval') onDone(`i${i.id} signed by ${getOperator()} (${(r.intent.concurrences?.length ?? 0)} of ${needed}); another approver must sign`, 'ok')
      else if (r.dispatched) onDone(`i${i.id} approved by ${getOperator()}: ${platform} sent to the effect point`, 'ok')
      else onDone(`i${i.id} ${r.intent.state}: ${last?.reason ?? ''}`, r.intent.state === 'voided' ? 'bad' : 'ok')
      setBusy(false)
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      setError((e as Error).message)
      setBusy(false)
    }
  }

  return (
    <article
      className={`ix-card eg-card ${selected ? 'sel' : ''} ${worse.length || lapses ? 'eg-warn' : ''}`}
      tabIndex={0}
      onClick={onFocus}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && e.target === e.currentTarget) onFocus()
      }}
    >
      <header className="ix-head">
        <strong className="ix-kind eg-kind">ENGAGE</strong>
        <span className="ix-id">i{i.id}</span>
        <span className={`ix-src ${i.source.kind}`} title={i.source.kind === 'agent' ? 'nominated by an AI agent' : 'nominated by a human operator'}>
          {i.source.kind} {i.source.id}
        </span>
        {p.window && <WindowCountdown until={Math.min(p.window.until, i.expires_t ?? p.window.until)} start={i.created_t} now={now} />}
      </header>
      <p className="eg-abstract">Abstract effect: a point, a radius, a time. Two operators with engagement authority decide; never the proposer.</p>

      {worse.length > 0 && (
        <div className="eg-alarm" role="alert">
          <b>Picture changed since the nomination</b>
          <ul>
            {worse.map((c, k) => (
              <li key={k}>
                {c.rule}: {c.detail}
              </li>
            ))}
          </ul>
        </div>
      )}
      <BehavingLapses intent={i} now={now} />
      {drift != null && drift > 25 && (
        <p className="eg-drift">
          The track is {Math.round(drift)} m from where the nomination's preview put it now.
        </p>
      )}

      {i.rationale && (
        <blockquote className="ix-rationale">
          <span className="ix-label">{i.source.kind === 'agent' ? 'Agent rationale' : 'Rationale'}</span>
          <RationaleText rationale={i.rationale} source={i.source} />
        </blockquote>
      )}

      <section className="eg-sec">
        <span className="ix-label">Target</span>
        <div className="eg-target">
          <b>{p.track}</b>
          {view?.track_now ? (
            <span>
              {view.track_now.affiliation}
              {view.track_now.declaration ? (
                <span className={unconfirmedRule(view.track_now.declaration) ? 'eg-alarm' : ''}> ({declarationBasis(view.track_now.declaration)})</span>
              ) : view.track_now.declared_by ? (
                ` (declared by ${view.track_now.declared_by})`
              ) : (
                ' (not declared)'
              )}
              , {view.track_now.identity ?? 'unclassified'}{' '}
              {Math.round(100 * view.track_now.identity_confidence)}%, last seen {view.track_now.age_s.toFixed(1)} s ago, 1-sigma {Math.round(view.track_now.pos_sigma_m)} m
            </span>
          ) : (
            <span className="bad">no longer in the picture</span>
          )}
        </div>
      </section>

      <section className="eg-sec">
        <span className="ix-label">
          Preview {live?.preview ? `(on the world now, computed ${Math.max(0, Math.round(ageS ?? 0))} s ago)` : '(at nomination)'}
          {live?.preview && stale && <em className="eg-was"> stale</em>}
        </span>
        {pv ? <PreviewFacts p={pv} now={now} platform={platform} /> : <p className="bad">No intercept: see the reach check.</p>}
        <p className="eg-fallback">
          If stopped before the effect (abort, void, link loss): <b>{FALLBACK_LABEL[p.fallback]}</b>
        </p>
      </section>

      <section className="eg-sec">
        <span className="ix-label">
          Checks {live ? (live.passed ? '(all pass now)' : '(some fail now)') : ''}
        </span>
        <ChecksList checks={i.checks} live={live?.checks} vehicles={vehicles} />
      </section>

      <section className="eg-sec eg-sign">
        <span className="ix-label">
          Approvals {signed.length} of {needed}
        </span>
        {view?.reset && (
          <p className="eg-alarm" role="status">
            Signatures of {view.reset.dropped.join(', ')} dropped {fmtRel(view.reset.t - now())}: {view.reset.reason}. They must sign again on the picture as it is now.
          </p>
        )}
        <Acceptances intent={i} vehicles={vehicles} now={now} onDone={onDone} signing />
        {signed.length === 0 && <AcceptLostHere intent={i} now={now} onDone={onDone} />}
        <AcceptBehavingHere intent={i} onDone={onDone} />
        {plan && (
          <p className="eg-plan">
            Plan frozen by {plan.by}&apos;s signature {fmtRel(plan.t - now())}: effect at {ll(plan.effect_point)} {fmtRel(plan.effect_t - now())}, radius {Math.round(plan.effect_radius_m)} m, on {plan.track.id} ({plan.track.identity ?? 'unclassified'}, {declarationBasis(plan.track.declaration, plan.track.declared_by)}). Every signature is on this exact plan; if it stops holding, the signatures are dropped.
          </p>
        )}
        {(view?.signatures ?? []).map((s) => (
          <p key={s.by} className="muted">
            {s.by} signed {fmtRel(s.t - now())} on the effect point {ll(s.effect_point)}
            {s.self_accepted ? ', having also made an acceptance for it (a lost vehicle or contact, or a track as behaving now)' : ''}.
          </p>
        ))}
        <p>
          {signed.length ? `Signed: ${signed.join(', ')}. ` : ''}
          May sign: {(view?.eligible ?? []).join(', ') || 'nobody listed'}
          {view?.proposer ? `; not ${view.proposer} (proposer)` : ''}.
        </p>
        <p className={canApprove ? 'muted' : 'eg-hint'}>{signHint}</p>
      </section>

      <footer className="ix-actions" onClick={(e) => e.stopPropagation()}>
        <input className="ix-note" placeholder="Note (optional)" value={note} maxLength={300} disabled={busy} onChange={(e) => setNote(e.target.value)} aria-label="Decision note" />
        <button className="ix-reject" disabled={busy || expired} onClick={() => act('reject')}>
          Reject
        </button>
        {canSign && stepUp && (
          <StepUpField
            passwordLabel="Your password, to sign"
            purpose="to sign this plan"
            ariaLabel="Your password, to sign this engagement"
            value={step}
            onChange={setStep}
            busy={busy}
            bad={stepUpError}
          />
        )}
        {shown ? (
          <button className="ix-approve eg-approve" disabled={busy || expired || !canApprove || shownLeft <= 0 || (stepUp && !stepUpReady(step))} onClick={() => act('approve')} title={signHint}>
            Sign this plan {signed.length + 1}/{needed}
          </button>
        ) : (
          <button className="ix-approve eg-approve" disabled={busy || expired || !canApprove} onClick={look} title={signHint}>
            Look to sign {signed.length + 1}/{needed}
          </button>
        )}
        {shown && (
          <div className="eg-shown" role="status">
            <b>You would sign</b>: effect at {ll(shown.plan.effect_point)} {fmtRel(shown.plan.effect_t - now())}, radius {Math.round(shown.plan.effect_radius_m)} m, {platform}, fallback {FALLBACK_LABEL[p.fallback]}; {shown.track.id} {shown.track.identity ?? 'unclassified'}, {shown.track.affiliation} {declarationBasis(shown.track.declaration, shown.track.declared_by)}.{' '}
            {shown.frozen ? `The plan ${plan?.by ?? ''} froze.` : 'Your signature freezes this plan.'}{' '}
            {Object.entries(shown.unknown_accepted ?? {}).map(([uid, a]) => (
              <span key={uid} className="eg-alarm">
                It covers {nameOf(vehicles, uid)} accepted as lost by {a.by}
                {a.since == null ? ' (it never reported a position)' : ` (position unknown since ${fmtRel(a.since - now())})`}.{' '}
              </span>
            ))}
            {Object.entries(shown.lost_accepted ?? {}).map(([track, a]) => (
              <span key={`lost-${track}`} className="eg-alarm">
                It covers lost contact {track} accepted by {a.by}
                {a.since == null ? '' : ` (out of view since ${fmtRel(a.since - now())})`}.{' '}
              </span>
            ))}
            {Object.entries(shown.behaving_accepted ?? {}).map(([track, a]) => (
              <span key={`behaving-${track}`} className="eg-alarm">
                It covers {track} accepted as behaving now by {a.by} {fmtRel(a.t - now())} ({courseText(a.facts.velocity_mps ?? null)} on its estimate at{' '}
                {fmtZ(a.facts.position.t)}).{' '}
              </span>
            ))}
            {shownLeft > 0 ? `Valid ${Math.ceil(shownLeft)} s.` : 'Expired: look again.'}{' '}
            <button className="ghost mini" onClick={() => setShown(null)}>
              Cancel
            </button>
          </div>
        )}
        {error && <p className="error ix-error">{error}</p>}
      </footer>
    </article>
  )
}

/** A running (or just finished) engagement: where the platform is, the watch verdict, the
 *  countdown to the predicted effect, Abort; then the effect and the assessment. */
function RunCard({ intent: i, vehicles, now, selected, onFocus, onDone }: CardProps) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const e = i.engagement
  const p = previewOf(i)
  const v = vehicles.find((x) => String(x.uid) === i.uids[0])
  const toPoint = v?.position && p ? distanceM(v.position, p.effect_point) : null
  const watch = e?.watch
  const failing = (watch?.results ?? []).filter((c) => c.outcome !== 'pass')
  const abortable = i.state === 'approved' || i.state === 'dispatched'
  const last = i.history[i.history.length - 1]
  const abort = async () => {
    setBusy(true)
    setError(null)
    try {
      const r = await abortEngagement(i.id, 'operator abort from the console')
      onDone(`i${i.id} aborted by ${getOperator()}; fallback ${e?.snapshot?.fallback ?? ''}`, 'ok')
      void r
    } catch (err) {
      setError((err as Error).message)
    }
    setBusy(false)
  }
  const lapses = (i.engage?.lapsed?.length ?? 0) > 0
  return (
    <article className={`ix-card eg-run ${i.state} ${selected ? 'sel' : ''} ${lapses ? 'eg-warn' : ''}`} tabIndex={0} onClick={onFocus}>
      <header className="ix-head">
        <strong className="ix-kind eg-kind">ENGAGE</strong>
        <span className="ix-id">i{i.id}</span>
        <span className={`eg-state ${i.state}`}>{STATE_LABEL[i.state] ?? i.state}</span>
        {p && abortable && <span className="eg-eta">effect {fmtRel(p.effect_t - now())}</span>}
      </header>
      <div className="eg-target">
        <b>{engageParams(i).track}</b>
        <span>
          {nameOf(vehicles, i.uids[0] ?? '?')}
          {toPoint != null && abortable ? `, ${km(toPoint)} from the effect point` : ''}; approved by{' '}
          {[...(i.concurrences ?? []).map((a) => a.by), i.approval?.by].filter(Boolean).join(' and ')}
        </span>
      </div>
      {abortable && watch && (
        <p className={watch.holds ? 'eg-holds' : 'eg-alarm'}>
          {watch.holds
            ? watch.ready
              ? 'Picture holds; release conditions met.'
              : `Picture holds (checked ${fmtRel(watch.t - now())}); waiting: ${failing.map((c) => c.rule).join(', ') || 'the next step'}`
            : `Picture changed: ${failing.map((c) => `${c.rule} (${c.detail})`).join('; ')}`}
        </p>
      )}
      <BehavingLapses intent={i} now={now} />
      {abortable && <Acceptances intent={i} vehicles={vehicles} now={now} onDone={onDone} />}
      {e?.effect && (
        <p className="eg-effect">
          Effect released {fmtRel(e.effect.t - now())} at {ll(e.effect.point)} (radius {Math.round(e.effect.radius_m)} m, planned {fmtRel(e.effect.planned_t - e.effect.t)} from release)
        </p>
      )}
      {e?.munition &&
        (now() < e.munition.impact_t ? (
          <p className="eg-munition flying">
            <b>{e.munition.name} in flight</b>: impact in {secondsToImpact(e.munition, now())} s
          </p>
        ) : (
          <p className="eg-munition">
            <b>Impact at {fmtZ(e.munition.impact_t)}</b> ({e.munition.name}, {Math.round(e.munition.tof_s)} s from its launch at {fmtZ(e.munition.launch_t)})
          </p>
        ))}
      {i.state === 'effected' && !(e?.munition && now() < e.munition.impact_t) && (
        <p className="muted">
          Assessing: the effect point covered for {e?.covered_s.toFixed(0) ?? 0} s{e?.observe?.covered_by.length ? ` by ${e.observe.covered_by.join(', ')}` : ' (no sensor of ours covers it)'}
        </p>
      )}
      {e?.assessment && (
        <p className={`eg-assess ${e.assessment.result}`}>
          <b>Assessed {e.assessment.result}</b>: {e.assessment.evidence}
        </p>
      )}
      {e?.stop && (
        <p className="eg-stop">
          <b>{i.state === 'aborted' ? 'Aborted' : 'Voided'}</b>: {e.stop.reason}
          {e.stop.cause.kind === 'superseded' ? ` (superseded by i${e.stop.cause.by})` : ''}
          {e.stop.fallback ? `; fallback ${e.stop.fallback} commanded` : ''}
        </p>
      )}
      {!e?.stop && !e?.assessment && !abortable && i.state !== 'effected' && last && <p className="muted">{last.reason}</p>}
      {abortable && (
        <footer className="ix-actions" onClick={(ev) => ev.stopPropagation()}>
          <span className="muted">Any operator may abort until the effect.</span>
          <button className="eg-abort" disabled={busy} onClick={abort}>
            Abort
          </button>
          {error && <p className="error ix-error">{error}</p>}
        </footer>
      )}
    </article>
  )
}

/** The engagements in play (approved, en route, assessing) and the last few that ended. */
export function EngagementsSection({
  intents,
  vehicles,
  now,
  selection,
  onFocus,
  onDone,
}: {
  intents: Intent[]
  vehicles: Vehicle[]
  now: () => number
  selection: { kind: 'intent'; id: number } | { kind: 'alert'; id: string } | null
  onFocus: (i: Intent) => void
  onDone: (text: string, tone: 'ok' | 'bad') => void
}) {
  const engagements = intents.filter(isEngage)
  const running = engagements.filter((i) => RUNNING.includes(i.state))
  const ended = engagements.filter((i) => ['aborted', 'done', 'voided'].includes(i.state) && i.engagement?.snapshot).slice(0, 3)
  if (!running.length && !ended.length) return null
  return (
    <>
      <h3 className="ix-h">
        Engagements <span>{running.length}</span>
        <small>in play, then the last that ended</small>
      </h3>
      {[...running, ...ended].map((i) => (
        <RunCard key={i.id} intent={i} vehicles={vehicles} now={now} selected={selection?.kind === 'intent' && selection.id === i.id} onFocus={() => onFocus(i)} onDone={onDone} />
      ))}
    </>
  )
}
