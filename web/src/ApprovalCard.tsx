import { type ReactNode, useEffect, useRef, useState } from 'react'
import type { ApproveExtra, Check, Intent } from './decisions'
import { fmtDuration, getOperator, intentAction, StepUpNeeded, stepsUp, useOperator } from './decisions'
import { ChecksFold } from './Bluf'
import { RefusalFold } from './RefusalFold'
import { RationaleText } from './Rationale'
import { StepUpField } from './StepUp'
import { Proposer } from './Proposer'
import { agoWhen, plainText } from './plainText'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import { reportView } from './operatorView'
import { checkTitle } from './engageSteps'
import { askSentence, DONT_SEND, NOTHING_SENT, placeLine, sendLabel } from './approvalText'
import { askFleet, nameOf, vehicleOf } from './approvalFleet'
import { afterText, callouts, holdsText, nowRows, reserveLine } from './approvalModel'
import { counts, foldByRule, foldTone, memberClasses, reforms } from './groupOrders'
import { distanceM } from './geo'
import type { Vehicle } from './types'

const OUTCOME_ORDER: Record<Check['outcome'], number> = { fail: 0, unverified: 1, not_evaluated: 2, pass: 3 }
const OUTCOME_MARK: Record<Check['outcome'], string> = { pass: 'PASS', fail: 'FAIL', unverified: 'UNVERIFIED', not_evaluated: 'NOT EVALUATED' }

function km(m: number) {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m)} m`
}

/** The time left to decide, labelled: an order that expires is not dispatched. */
function Countdown({ intent, now }: { intent: Intent; now: () => number }) {
  if (intent.expires_t == null) return null
  const left = (intent.expires_t - now()) / 1000
  const start = intent.history.find((h) => h.state === 'pending_approval')?.t ?? intent.created_t
  const total = Math.max(1, (intent.expires_t - start) / 1000)
  const frac = Math.min(1, Math.max(0, left / total))
  return (
    <span className={`ix-count ${left <= 0 ? 'out' : left < 30 ? 'urgent' : ''}`} title="an order that expires is not dispatched">
      <b>{left <= 0 ? 'expired' : `${fmtDuration(left)} to decide`}</b>
      <span className="ix-count-bar">
        <span style={{ width: `${frac * 100}%` }} />
      </span>
    </span>
  )
}

/** A collect's chosen option as the submission's checks found it: the times, resolution and
 *  deadline a person approves on; the satellite's number, the geometry, notes and the others'
 *  count are folded. */
function CollectDetails({ intent: i }: { intent: Intent }) {
  const o = i.space?.chosen
  if (!o) return null
  const z = (t: string | number) => new Date(t).toISOString().slice(11, 19) + 'Z'
  const lead = i.space?.tasking_lead_s
  return (
    <>
      <dl className="ix-collect">
        <dt>Collect</dt>
        <dd>{z(o.collect_time)}</dd>
        <dt>Product</dt>
        <dd>{o.product_time ? z(o.product_time) : 'time unknown'}</dd>
        <dt>Resolution</dt>
        <dd>{o.resolution_m != null ? `${o.resolution_m.toFixed(2)} m` : 'unknown'}</dd>
        <dt>Deadline</dt>
        <dd>
          {i.expires_t != null ? z(i.expires_t) : 'none'}
          {lead != null ? `: the last moment this pass can be tasked (collect minus ${Math.round(lead / 60)} min lead)` : ''}
        </dd>
      </dl>
      <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
        <summary>The satellite and its geometry</summary>
        <dl className="ix-collect">
          <dt>Satellite</dt>
          <dd>NORAD {o.norad_id}</dd>
          <dt>Geometry</dt>
          <dd>
            off-nadir {o.geometry.off_nadir_deg.toFixed(1)}°, incidence {o.geometry.incidence_deg.toFixed(1)}°{o.covers_area === false ? ', partial area' : ''}
          </dd>
          {o.notes.length > 0 && (
            <>
              <dt>Notes</dt>
              <dd>{o.notes.join('; ')}</dd>
            </>
          )}
          {(i.space?.options_total ?? 0) > 1 && (
            <>
              <dt>Others</dt>
              <dd>
                {(i.space?.options_total ?? 1) - 1} other feasible option{(i.space?.options_total ?? 2) === 2 ? '' : 's'}, {i.space?.exclusions_total ?? 0} passes excluded
              </dd>
            </>
          )}
        </dl>
      </details>
    </>
  )
}

export interface CardProps {
  intent: Intent
  vehicles: Vehicle[]
  now: () => number
  selected: boolean
  onFocus: () => void
  onDone: (text: string, tone: 'ok' | 'bad') => void
  /** engagements in play (pending, approved, under way) on this intent's vehicles: its
   *  dispatch stops them, and the approval must say so */
  engaged?: Intent[]
  /** what the watch officer was asked in the turn that proposed it (App.tsx), with the reasons */
  cause?: ReactNode
  /** what a vehicle is doing now, in the words of its order (orderWords.ts) */
  doing: (uid: string) => string
}

/** A pending approval, in murmur-i9zw's order: who asks and the time to decide, the ask as one
 *  sentence, Now / After / Holds, the catch, the proposer's reasons word for word, two equal
 *  buttons naming their outcomes and what leaving it does. The checks, the coordinates and the
 *  note are folded behind a click; nothing the person approves on is removed. The policy's route
 *  is said once, in the section header (InboxPanel.tsx). */
export function ApprovalCard({ intent: i, vehicles, now, selected, onFocus, onDone, engaged = [], cause, doing }: CardProps) {
  const [note, setNote] = useState('')
  const [noting, setNoting] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // rules 8: a tier the policy marks consequential takes the approver's credential again
  // (rules 10: by passkey where the run asks for one)
  const stepUp = stepsUp() && i.policy?.step_up === true
  const [step, setStep] = useStepUp()
  const [stepUpError, setStepUpError] = useState(false)
  const [operator] = useOperator()
  const selfApproval = i.source.kind === 'human' && i.source.id === operator
  const expired = i.expires_t != null && i.expires_t <= now()
  const checks = [...i.checks].sort((a, b) => OUTCOME_ORDER[a.outcome] - OUTCOME_ORDER[b.outcome])
  // evaluation instrumentation (operatorView.ts): the card opened, closed or was decided
  const view = useRef({ openedAt: performance.now(), decided: false })
  useEffect(() => {
    const v = view.current
    reportView(i, 'open', v.openedAt)
    return () => {
      if (!v.decided) reportView(i, 'close', v.openedAt)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [i.id])

  const act = async (action: 'approve' | 'reject') => {
    setBusy(true)
    setError(null)
    view.current.decided = true
    reportView(i, 'decide', view.current.openedAt, action)
    setStepUpError(false)
    try {
      const extra: ApproveExtra = {}
      if (action === 'approve' && engaged.length) extra.acknowledge_supersede = true
      if (action === 'approve' && stepUp) Object.assign(extra, await stepUpProof(step, { act: 'sign', intent: i.id }))
      setStep({ password: '' })
      const r = await intentAction(i.id, action, note, extra)
      const last = r.intent.history[r.intent.history.length - 1]
      if (action === 'reject') onDone(`i${i.id} not sent: rejected by ${getOperator()}`, 'ok')
      else if (r.dispatched) onDone(`i${i.id} approved by ${getOperator()}${r.intent.approval?.self_approved ? ' (self-approval)' : ''} and sent`, 'ok')
      else onDone(`i${i.id} ${r.intent.state}: ${last?.reason ?? ''}`, r.intent.state === 'voided' ? 'bad' : 'ok')
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      setError((e as Error).message)
      setBusy(false)
    }
  }

  const fleet = askFleet(vehicles)
  const name = (u: string) => nameOf(vehicles, u)
  const plain = (t: string) => plainText(t, { nameOf: name, when: agoWhen(now()) })
  const group = i.group != null
  const lines = group ? memberClasses(i.uids, i.checks) : []
  const c = group ? counts(lines, i.group, i.kind) : null
  const folds = group ? foldByRule(i.checks) : []
  const reserve = reserveLine(i.checks)
  const hazards = callouts({
    facts: i.policy?.facts,
    extra: (i.why ?? []).slice(1),
    selfApproval,
    engaged: engaged.map((e) => e.id),
    name,
  })
  // what it will do: the sentence per kind, and how far each of the first few has to go
  const after = afterText(i.kind)
  const starts = i.uids.length <= 3 ? i.uids.map((u) => ({ u, d: startM(i, vehicleOf(vehicles, u)) })).filter((x): x is { u: string; d: number } => x.d != null) : []
  const holds = holdsText({ kind: i.kind, group, min: c?.min ?? null, reform: i.group?.reform ?? reforms(i.kind) })
  const now_ = nowRows(i.uids, doing, name)
  const where = placeLine(i)
  const failingRules = folds.filter((f) => foldTone(f) !== 'ok').map((f) => f.rule)
  const checkLine = group
    ? { text: failingRules.length ? `${failingRules.length} of ${folds.length} checks need a look: ${failingRules.map((r) => checkTitle(r).toLowerCase()).join(', ')}` : `Every check passes for all ${c?.n}`, tone: folds.some((f) => foldTone(f) === 'bad') ? ('bad' as const) : failingRules.length ? ('warn' as const) : ('ok' as const) }
    : undefined

  return (
    <article
      className={`ix-card ap ${selected ? 'sel' : ''}`}
      data-intent-id={i.id}
      tabIndex={0}
      onClick={onFocus}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && e.target === e.currentTarget) onFocus()
      }}
    >
      <header className="ix-head">
        <Proposer source={i.source} />
        <span className="ix-id">i{i.id}</span>
        <Countdown intent={i} now={now} />
      </header>

      <p className="ap-ask">{askSentence({ kind: i.kind, params: i.params as Record<string, unknown>, uids: i.uids, group: i.group }, fleet)}</p>
      {i.kind === 'collect' && <CollectDetails intent={i} />}

      <dl className="ap-rows">
        <dt>Now</dt>
        <dd>{now_.length ? now_.join('; ') : 'no vehicle named'}</dd>
        {(after || starts.length > 0) && (
          <>
            <dt>After</dt>
            <dd>
              {after}
              {starts.length > 0 && ` ${starts.map((s) => `${name(s.u)} is ${km(s.d)} from the start.`).join(' ')}`}
              {group && c && c.sent < c.n && ` ${c.sent} of ${c.n} would go now${c.waiting ? `; ${c.waiting} more if heard in time` : ''}${c.excluded ? `; ${c.excluded} are left out` : ''}.`}
            </dd>
          </>
        )}
        {holds && (
          <>
            <dt>Holds</dt>
            <dd>{holds}</dd>
          </>
        )}
      </dl>

      <section className="ap-catch">
        <span className="ix-label">The catch</span>
        {hazards.length > 0 && (
          <ul className="ap-callout" role="note">
            {hazards.map((h, k) => (
              <li key={k}>{h}</li>
            ))}
          </ul>
        )}
        {reserve && <p className={`ap-reserve${reserve.close ? ' close' : ''}`}>{reserve.text}</p>}
        {group && <RefusalFold checks={i.checks} uids={i.uids} vehicles={vehicles} />}
        <ChecksFold checks={i.checks} line={checkLine}>
          {group
            ? folds.map((f) => (
                <div key={f.rule} className={`ix-check ${foldTone(f) === 'ok' ? 'pass' : foldTone(f) === 'bad' ? 'fail' : 'unverified'}`}>
                  <span className="ix-mark">{foldTone(f) === 'ok' ? 'PASS' : foldTone(f) === 'bad' ? 'FAIL' : 'SEE'}</span>
                  <span className="ix-text">
                    <span className="ix-rule">{checkTitle(f.rule)}</span>
                    <span className="ix-detail">
                      {f.group ? plain(f.group.detail) : `${f.pass} pass${f.waiting ? `, ${f.waiting} waiting` : ''}${f.excluded ? `, ${f.excluded} excluded` : ''}`}
                    </span>
                    {f.reasons.map((r, k) => (
                      <span key={k} className="ix-detail">
                        {r.class ?? r.outcome.replace('_', ' ')} x{r.uids.length}: {plain(r.example)} ({r.uids.length > 8 ? `${r.uids.slice(0, 8).map(name).join(', ')} and ${r.uids.length - 8} more` : r.uids.map(name).join(', ')})
                      </span>
                    ))}
                  </span>
                </div>
              ))
            : checks.map((k, n) => (
                <div key={n} className={`ix-check ${k.outcome}`}>
                  <span className="ix-mark">{OUTCOME_MARK[k.outcome]}</span>
                  <span className="ix-text">
                    <span className="ix-rule">
                      {checkTitle(k.rule)}
                      {k.uid != null && <i> {name(k.uid)}</i>}
                    </span>
                    <span className="ix-detail">{plain(k.detail)}</span>
                  </span>
                </div>
              ))}
        </ChecksFold>
        {group && (
          <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
            <summary>
              {c?.n} members ({c?.sent} would be sent{c?.waiting ? `, ${c.waiting} waiting` : ''}
              {c?.excluded ? `, ${c.excluded} left out` : ''})
            </summary>
            <p className="ix-group-names">{i.uids.map(name).join(', ')}</p>
          </details>
        )}
      </section>

      {i.rationale && (
        <blockquote className="ix-rationale">
          <span className="ix-label">{i.source.kind === 'agent' ? `Their reasons (AI ${i.source.id}), word for word` : `Their reasons (${i.source.id}), word for word`}</span>
          {cause}
          <RationaleText rationale={i.rationale} source={i.source} />
        </blockquote>
      )}
      {!i.rationale && cause}

      {where && (
        <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
          <summary>Where</summary>
          <p className="ap-where">{where}</p>
        </details>
      )}

      <footer className="ap-actions" onClick={(e) => e.stopPropagation()}>
        {noting ? (
          <input className="ix-note" placeholder="Note (optional)" value={note} maxLength={300} disabled={busy} onChange={(e) => setNote(e.target.value)} aria-label="Decision note" autoFocus />
        ) : (
          <button type="button" className="ghost mini ap-addnote" onClick={() => setNoting(true)}>
            Add a note
          </button>
        )}
        <div className="ap-decide">
          <button className="ap-btn" disabled={busy || expired} onClick={() => act('reject')}>
            {DONT_SEND}
          </button>
          <button className="ap-btn" disabled={busy || expired || (stepUp && !stepUpReady(step))} onClick={() => act('approve')}>
            {engaged.length ? `${sendLabel(i.kind)} and stop engagement i${engaged.map((e) => e.id).join(', i')}` : sendLabel(i.kind)}
          </button>
        </div>
        {stepUp && (
          <StepUpField
            passwordLabel="Consequential: your password to approve"
            purpose="to sign this approval"
            ariaLabel="Your password, to approve"
            value={step}
            onChange={setStep}
            busy={busy}
            bad={stepUpError}
            onEnter={() => !expired && void act('approve')}
          />
        )}
        <p className="ap-leave">{NOTHING_SENT}</p>
        {error && <p className="error ix-error">{error}</p>}
      </footer>
    </article>
  )
}

/** Distance from a vehicle to where the task starts, when the task has a place. */
function startM(i: Intent, v: Vehicle | undefined): number | null {
  if (!v?.position) return null
  const start = i.params.center ?? i.params.route?.[0]
  return start ? distanceM(v.position, start) : null
}
