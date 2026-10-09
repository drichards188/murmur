import { useEffect, useMemo, useRef, useState } from 'react'
import type { Alert, ApproveExtra, Check, Decisions, Intent } from './decisions'
import {
  ALERT_LABEL,
  fmtDuration,
  getOperator,
  intentAction,
  StepUpNeeded,
  stepsUp,
  useAlertMemory,
  useOperator,
  verdict,
  writeOffRestore,
  writeOffTarget,
  unspendPlatform,
  unspendTarget,
} from './decisions'
import { EngageCard, EngagementsSection } from './EngageCard'
import { RationaleText } from './Rationale'
import { ReviewSection } from './ReviewSection'
import { isEngage } from './engage'
import { distanceM } from './geo'
import { reportView } from './operatorView'
import { scriptedBadge } from './situationIds'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import type { LngLat, Vehicle } from './types'
import { callsign } from './types'
import './inbox.css'

export type InboxTab = 'inbox' | 'decisions'

/** What the operator is looking at: an intent (`i2`) or an alert (`link:3`). */
export type InboxSelection = { kind: 'intent'; id: number } | { kind: 'alert'; id: string } | null

interface Props {
  decisions: Decisions | null
  vehicles: Vehicle[]
  now: () => number
  tab: InboxTab
  setTab: (t: InboxTab) => void
  selection: InboxSelection
  /** fly to and highlight: the vehicles (bridge uids) and the intent whose preview to frame */
  onFocus: (sel: InboxSelection, uids: number[], intent: Intent | null) => void
  onClose: () => void
  /** rules 9: open a track in the tracks drawer (from a review item) */
  onTrack?: (id: string) => void
}

// ---- top bar button -------------------------------------------------------------------------

export function InboxButton({ decisions, open, onClick, hiddenAlerts }: { decisions: Decisions | null; open: boolean; onClick: () => void; hiddenAlerts: Set<string> }) {
  const pending = decisions?.inbox.pending.length ?? 0
  const alerts = (decisions?.inbox.alerts ?? []).filter((a) => !hiddenAlerts.has(a.id))
  const high = alerts.filter((a) => a.severity === 'high').length
  const medium = alerts.length - high
  const review = decisions?.inbox.counts.review ?? 0
  const parts = [`${pending} pending approval${pending === 1 ? '' : 's'}`, `${high} high alert${high === 1 ? '' : 's'}`, `${medium} medium alert${medium === 1 ? '' : 's'}`, `${review} for review (not blocking)`]
  return (
    <button className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} onClick={onClick} title={parts.join(', ')} aria-label={`Inbox: ${parts.join(', ')}`}>
      Inbox
      {pending > 0 && <span className="ix-badge pend">{pending}</span>}
      {high > 0 && <span className="ix-badge high">{high}</span>}
      {medium > 0 && <span className="ix-badge med">{medium}</span>}
      {review > 0 && <span className="ix-badge rev">{review}</span>}
    </button>
  )
}

// ---- helpers ----------------------------------------------------------------------------------

const OUTCOME_ORDER: Record<Check['outcome'], number> = { fail: 0, unverified: 1, not_evaluated: 2, pass: 3 }
const OUTCOME_MARK: Record<Check['outcome'], string> = { pass: 'PASS', fail: 'FAIL', unverified: 'UNVERIFIED', not_evaluated: 'NOT EVALUATED' }

function vehicleOf(vehicles: Vehicle[], uid: string) {
  return vehicles.find((v) => String(v.uid) === uid)
}

function nameOf(vehicles: Vehicle[], uid: string) {
  const v = vehicleOf(vehicles, uid)
  return v ? callsign(v) : `vehicle ${uid}`
}

function ll(p: LngLat) {
  return `${p[1].toFixed(4)}, ${p[0].toFixed(4)}`
}

function km(m: number) {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m)} m`
}

function taskSummary(i: Intent, vehicles: Vehicle[]) {
  const p = i.params
  switch (i.kind) {
    case 'orbit':
      return p.center ? `Orbit, radius ${km(p.radius_m ?? 0)}, centre ${ll(p.center)}` : 'Orbit'
    case 'patrol': {
      const r = p.route ?? []
      let len = 0
      for (let k = 0; k < r.length; k++) len += distanceM(r[k], r[(k + 1) % r.length])
      return `Patrol, ${r.length} waypoint${r.length === 1 ? '' : 's'}, loop ${km(len)}`
    }
    case 'track':
      return `Track ${p.target ? nameOf(vehicles, p.target) : '?'}`
    case 'rtl':
      return 'Return to launch'
    case 'hold':
      return 'Hold position'
    case 'collect': {
      const o = i.space?.chosen
      return o ? `Satellite collection: ${o.label ?? o.satellite} (${o.class_name}, ${o.capability.toUpperCase()})` : 'Satellite collection (no option chosen)'
    }
    case 'cover': {
      const s = p.service as { type?: string; class?: string } | undefined
      return `Coverage: ${s?.type === 'gnss' ? `${(s.class ?? 'gps').toUpperCase()} GNSS` : `${s?.class ?? '?'} SATCOM`} over the area`
    }
    default:
      return i.kind
  }
}

/** Distance from a vehicle to where the task starts, when the task has a place. */
function transitM(i: Intent, v: Vehicle | undefined): number | null {
  if (!v?.position) return null
  const start = i.params.center ?? i.params.route?.[0]
  return start ? distanceM(v.position, start) : null
}

function Countdown({ intent, now }: { intent: Intent; now: () => number }) {
  if (intent.expires_t == null) return null
  const left = (intent.expires_t - now()) / 1000
  const start = intent.history.find((h) => h.state === 'pending_approval')?.t ?? intent.created_t
  const total = Math.max(1, (intent.expires_t - start) / 1000)
  const frac = Math.min(1, Math.max(0, left / total))
  return (
    <span className={`ix-count ${left <= 0 ? 'out' : left < 30 ? 'urgent' : ''}`} title="time left to decide; an intent that expires is not dispatched">
      <b>{left <= 0 ? 'expired' : fmtDuration(left)}</b>
      <span className="ix-count-bar">
        <span style={{ width: `${frac * 100}%` }} />
      </span>
    </span>
  )
}

/** A collect's chosen option, as the submission's checks found it, and what its deadline is. */
function CollectDetails({ intent: i }: { intent: Intent }) {
  const o = i.space?.chosen
  if (!o) return null
  const z = (t: string | number) => new Date(t).toISOString().slice(11, 19) + 'Z'
  const lead = i.space?.tasking_lead_s
  return (
    <dl className="ix-collect">
      <dt>Collect</dt>
      <dd>
        {z(o.collect_time)} · NORAD {o.norad_id}
      </dd>
      <dt>Product</dt>
      <dd>{o.product_time ? z(o.product_time) : 'time unknown'}</dd>
      <dt>Resolution</dt>
      <dd>{o.resolution_m != null ? `${o.resolution_m.toFixed(2)} m` : 'unknown'}</dd>
      <dt>Geometry</dt>
      <dd>
        off-nadir {o.geometry.off_nadir_deg.toFixed(1)}°, incidence {o.geometry.incidence_deg.toFixed(1)}°{o.covers_area === false ? ', partial area' : ''}
      </dd>
      <dt>Deadline</dt>
      <dd>
        {i.expires_t != null ? `${z(i.expires_t)} (bridge clock)` : 'none'}
        {lead != null ? `: the last moment this pass can be tasked (collect minus ${Math.round(lead / 60)} min lead)` : ''}
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
  )
}

// ---- a pending approval -------------------------------------------------------------------------

interface CardProps {
  intent: Intent
  vehicles: Vehicle[]
  now: () => number
  selected: boolean
  onFocus: () => void
  onDone: (text: string, tone: 'ok' | 'bad') => void
  /** engagements in play (pending, approved, under way) on this intent's vehicles: its
   *  dispatch stops them, and the approval must say so */
  engaged?: Intent[]
}

function ApprovalCard({ intent: i, vehicles, now, selected, onFocus, onDone, engaged = [] }: CardProps) {
  const [note, setNote] = useState('')
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
  const unverified = new Set(i.policy?.facts.unverified ?? [])
  const armed = new Set(i.policy?.facts.armed ?? [])
  const reasons = (i.why ?? []).slice(1) // the first line is the policy rule, shown as the tier
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
      if (action === 'reject') onDone(`i${i.id} rejected by ${getOperator()}`, 'ok')
      else if (r.dispatched) onDone(`i${i.id} approved by ${getOperator()}${r.intent.approval?.self_approved ? ' (self-approval)' : ''} and dispatched`, 'ok')
      else onDone(`i${i.id} ${r.intent.state}: ${last?.reason ?? ''}`, r.intent.state === 'voided' ? 'bad' : 'ok')
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      setError((e as Error).message)
      setBusy(false)
    }
  }

  return (
    <article
      className={`ix-card ${selected ? 'sel' : ''}`}
      tabIndex={0}
      onClick={onFocus}
      onKeyDown={(e) => {
        if (e.key === 'Enter' && e.target === e.currentTarget) onFocus()
      }}
    >
      <header className="ix-head">
        <strong className="ix-kind">{i.kind.toUpperCase()}</strong>
        <span className="ix-id">i{i.id}</span>
        {scriptedBadge(i.source.id) ? (
          <span className="ix-src scripted" title={`${i.source.id}: a situation's script, not a person and not the AI`}>
            {scriptedBadge(i.source.id)} {i.source.id}
          </span>
        ) : (
          <span className={`ix-src ${i.source.kind}`} title={i.source.kind === 'agent' ? 'proposed by an AI agent' : 'proposed by a human operator'}>
            {i.source.kind} {i.source.id}
          </span>
        )}
        <Countdown intent={i} now={now} />
      </header>

      {i.rationale && (
        <blockquote className="ix-rationale">
          <span className="ix-label">{i.source.kind === 'agent' ? 'Agent rationale' : 'Rationale'}</span>
          <RationaleText rationale={i.rationale} source={i.source} />
        </blockquote>
      )}

      <div className="ix-task">{taskSummary(i, vehicles)}</div>
      {i.kind === 'collect' && <CollectDetails intent={i} />}
      <ul className="ix-vehicles">
        {i.uids.map((u) => {
          const v = vehicleOf(vehicles, u)
          const age = v?.link.since_last_ms
          const stale = age != null && age > 3000
          const tm = transitM(i, v)
          return (
            <li key={u}>
              <b>{v ? callsign(v) : `vehicle ${u}`}</b>
              {v?.identity && <span className="ix-model">{v.identity.name}</span>}
              {armed.has(u) && <span className="ix-tag warn">armed platform</span>}
              <span className="ix-facts" title="live values now, not the values the checks read">
                {age == null ? 'no telemetry' : <span className={stale ? 'bad' : ''}>position age {age < 1000 ? '<1 s' : `${(age / 1000).toFixed(1)} s`}</span>}
                {v?.battery_pct != null && <span>battery {Math.round(v.battery_pct)}%</span>}
                {v?.mode && <span>{v.mode}</span>}
                {tm != null && <span>{km(tm)} to start</span>}
              </span>
            </li>
          )
        })}
      </ul>

      <section className="ix-why">
        <span className="ix-label">Why this needs a human</span>
        {i.policy ? (
          <p>
            <span className={`ix-tier ${i.policy.tier}`}>{i.policy.tier} approval</span>
            <code>{i.policy.rule}</code> {i.policy.reason}
          </p>
        ) : (
          <p className="muted">No policy decision recorded.</p>
        )}
        {reasons.length > 0 && (
          <ul>
            {reasons.map((r, k) => (
              <li key={k} className={unverified.has(r) ? 'unv' : ''}>
                {r}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="ix-checks">
        <span className="ix-label">
          Checks ({i.checks.length}) {i.checks.some((c) => c.outcome === 'unverified') && <em>some could not be verified: judge them yourself</em>}
        </span>
        {checks.map((c, k) => (
          <div key={k} className={`ix-check ${c.outcome}`}>
            <span className="ix-mark">{OUTCOME_MARK[c.outcome]}</span>
            <span className="ix-text">
              <span className="ix-rule">
                {c.rule}
                {c.uid != null && <i> {nameOf(vehicles, c.uid)}</i>}
              </span>
              <span className="ix-detail">{c.detail}</span>
            </span>
          </div>
        ))}
      </section>

      {selfApproval && (
        <p className="ix-self">
          You ({operator}) proposed this. Approving it is a self-approval, and the decision log records that.
        </p>
      )}
      {engaged.length > 0 && (
        <p className="eg-alarm" role="alert">
          <b>Approving this stops {engaged.map((e) => `engagement i${e.id}`).join(', ')}</b> on the same vehicle (the newest order stands; no fallback is commanded). Your approval records that you accept it.
        </p>
      )}
      <footer className="ix-actions" onClick={(e) => e.stopPropagation()}>
        <input
          className="ix-note"
          placeholder="Note (optional)"
          value={note}
          maxLength={300}
          disabled={busy}
          onChange={(e) => setNote(e.target.value)}
          aria-label="Decision note"
        />
        <button className="ix-reject" disabled={busy || expired} onClick={() => act('reject')}>
          Reject
        </button>
        <button className="ix-approve" disabled={busy || expired || (stepUp && !stepUpReady(step))} onClick={() => act('approve')} title={`as ${operator}`}>
          {engaged.length ? `Approve and stop i${engaged.map((e) => e.id).join(', i')}` : selfApproval ? 'Approve (self)' : 'Approve'}
        </button>
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
        {error && <p className="error ix-error">{error}</p>}
      </footer>
    </article>
  )
}

// ---- an alert -----------------------------------------------------------------------------------

const SNOOZES: [string, number][] = [
  ['5 min', 5 * 60_000],
  ['15 min', 15 * 60_000],
  ['1 h', 3_600_000],
]

function AlertCard({
  alert: a,
  now,
  selected,
  muted,
  mem,
  onFocus,
}: {
  alert: Alert
  now: () => number
  selected: boolean
  muted: string | null
  mem: ReturnType<typeof useAlertMemory>
  onFocus: () => void
}) {
  const [snoozing, setSnoozing] = useState(false)
  // rules 12: a restore that will never be confirmed (the vehicle lost or spent, another one
  // under its uid, an earlier log gone) is written off by an operator, saying why; rules 15: a
  // platform spent by its own effect that survived is un-spent by one with engagement authority
  const target = writeOffTarget(a)
  const spent = unspendTarget(a)
  const [noting, setNoting] = useState<null | 'write_off' | 'unspend'>(null)
  const [note, setNote] = useState('')
  const [writeErr, setWriteErr] = useState<string | null>(null)
  // rules 15: an un-spend makes an armed platform taskable again: a credentials run re-enters
  // the credential (the password, or a passkey made for this un-spend)
  const unspendStepsUp = stepsUp()
  const [step, setStep] = useStepUp()
  const [stepUpError, setStepUpError] = useState(false)
  const [busy, setBusy] = useState(false)
  const submitNote = async () => {
    setWriteErr(null)
    setStepUpError(false)
    setBusy(true)
    try {
      if (noting === 'unspend' && spent) {
        const proof = unspendStepsUp ? await stepUpProof(step, { act: 'unspend', uid: spent.uid }) : {}
        setStep({ password: '' })
        await unspendPlatform(spent.uid, note.trim(), proof)
      } else if (target) {
        await writeOffRestore(target, note.trim())
      }
      setNoting(null)
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      setWriteErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const age = Math.max(0, now() / 1000 - a.since_t)
  return (
    <article className={`ix-alert ${a.severity} ${selected ? 'sel' : ''} ${muted ? 'muted-card' : ''}`} tabIndex={0} onClick={onFocus} onKeyDown={(e) => e.key === 'Enter' && e.target === e.currentTarget && onFocus()}>
      <header className="ix-head">
        <strong className="ix-akind">{ALERT_LABEL[a.kind]}</strong>
        <span className="ix-who">{a.callsign ?? `vehicle ${a.uid}`}</span>
        <span className="ix-age">{fmtDuration(age)} ago</span>
      </header>
      <p className="ix-adetail">{a.kind === 'lost_link' ? `No telemetry for ${fmtDuration(age)}` : a.detail}</p>
      {a.intent_id != null && <p className="ix-link">from intent i{a.intent_id}</p>}
      <footer className="ix-actions" onClick={(e) => e.stopPropagation()}>
        {muted ? (
          <>
            <span className="muted ix-muted">{muted}</span>
            <button className="ghost" onClick={() => mem.wake(a)}>
              Restore
            </button>
          </>
        ) : (noting === 'write_off' && target) || (noting === 'unspend' && spent) ? (
          <>
            <input
              className="ix-note"
              placeholder={noting === 'unspend' ? 'Why: what shows it survived' : 'Why: lost, spent, another vehicle, log gone'}
              value={note}
              onChange={(e) => setNote(e.target.value)}
              aria-label={noting === 'unspend' ? 'Why the platform is un-spent' : 'Why the restore is written off'}
            />
            <button
              className="ghost"
              disabled={busy || !note.trim() || (noting === 'unspend' && unspendStepsUp && !stepUpReady(step))}
              onClick={() => void submitNote()}
            >
              {noting === 'unspend' ? 'Un-spend' : 'Write off'}
            </button>
            <button className="ghost" onClick={() => setNoting(null)}>
              Back
            </button>
            {noting === 'unspend' && unspendStepsUp && (
              <StepUpField
                passwordLabel="Consequential: your password to un-spend"
                purpose="to un-spend this platform"
                ariaLabel="Your password, to un-spend"
                value={step}
                onChange={setStep}
                busy={busy}
                bad={stepUpError}
                onEnter={() => note.trim() && void submitNote()}
              />
            )}
            {writeErr && <span className="muted ix-muted">{writeErr}</span>}
          </>
        ) : snoozing ? (
          <>
            <span className="muted ix-muted">Snooze for</span>
            {SNOOZES.map(([label, ms]) => (
              <button
                key={label}
                className="ghost"
                onClick={() => {
                  mem.snooze(a, ms)
                  setSnoozing(false)
                }}
              >
                {label}
              </button>
            ))}
            <button className="ghost" onClick={() => setSnoozing(false)}>
              Back
            </button>
          </>
        ) : (
          <>
            <button className="ghost" onClick={onFocus}>
              Fly to
            </button>
            <span className="spacer" />
            {target && (
              <button className="ghost" onClick={() => setNoting('write_off')}>
                Write off
              </button>
            )}
            {spent && (
              <button className="ghost" onClick={() => setNoting('unspend')}>
                Un-spend
              </button>
            )}
            <button className="ghost" onClick={() => setSnoozing(true)}>
              Snooze
            </button>
            <button className="ghost" onClick={() => mem.ack(a)}>
              Acknowledge
            </button>
          </>
        )}
      </footer>
    </article>
  )
}

// ---- decisions list -----------------------------------------------------------------------------

function median(xs: number[]) {
  if (!xs.length) return null
  const s = [...xs].sort((a, b) => a - b)
  return s[Math.floor(s.length / 2)]
}

function DecisionRow({ intent: i, vehicles, now }: { intent: Intent; vehicles: Vehicle[]; now: () => number }) {
  const [open, setOpen] = useState(false)
  const v = verdict(i)
  const names = i.uids.length ? i.uids.map((u) => nameOf(vehicles, u)).join(', ') : 'no vehicles'
  const t0 = i.created_t
  const approver = i.approval
  return (
    <li className={`ix-dec ${open ? 'open' : ''}`}>
      <button className="ix-dec-row" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="ix-dec-top">
          <strong>{i.kind.toUpperCase()}</strong>
          <span className="ix-id">i{i.id}</span>
          <span className="ix-dec-names">{names}</span>
          <span className={`ix-state ${i.state}`}>{i.state.replace('_', ' ')}</span>
        </span>
        <span className="ix-dec-sub">
          <span className={`ix-route ${v.route}`}>{v.route === 'auto' ? 'auto' : v.route === 'human' ? 'human' : 'rules'}</span>
          <span>
            from {i.source.kind} {i.source.id}
          </span>
          {approver && (
            <span>
              {approver.by === i.source.id ? '' : 'approved by '}
              {approver.by === i.source.id ? <b className="self">self-approved by {approver.by}</b> : approver.by}
            </span>
          )}
          {v.wait_s != null && <span>decided in {fmtDuration(v.wait_s)}</span>}
          <span className="right">{fmtDuration((now() - t0) / 1000)} ago</span>
        </span>
      </button>
      {open && (
        <div className="ix-dec-body">
          {i.rationale && (
            <div className="ix-rationale">
              <RationaleText rationale={i.rationale} source={i.source} />
            </div>
          )}
          <ol className="ix-trail">
            {i.history.map((h, k) => (
              <li key={k}>
                <span className="ix-t">+{fmtDuration((h.t - t0) / 1000)}</span>
                <b>{h.state.replace('_', ' ')}</b>
                <span>{h.reason}</span>
              </li>
            ))}
          </ol>
          {approver?.note && <p className="ix-note-shown">Note from {approver.by}: {approver.note}</p>}
          {i.checks.some((c) => c.outcome !== 'pass') && (
            <ul className="ix-dec-checks">
              {i.checks
                .filter((c) => c.outcome !== 'pass')
                .map((c, k) => (
                  <li key={k} className={c.outcome}>
                    {OUTCOME_MARK[c.outcome]} {c.rule}: {c.detail}
                  </li>
                ))}
            </ul>
          )}
        </div>
      )}
    </li>
  )
}

function DecisionsList({ intents, vehicles, now }: { intents: Intent[]; vehicles: Vehicle[]; now: () => number }) {
  const stats = useMemo(() => {
    const vs = intents.map(verdict)
    return {
      auto: vs.filter((v) => v.route === 'auto').length,
      human: vs.filter((v) => v.route === 'human').length,
      rules: vs.filter((v) => v.route === 'rules').length,
      approved: vs.filter((v) => v.decided === 'approved').length,
      rejected: vs.filter((v) => v.decided === 'rejected').length,
      expired: vs.filter((v) => v.decided === 'expired').length,
      med: median(vs.flatMap((v) => (v.wait_s != null ? [v.wait_s] : []))),
    }
  }, [intents])
  if (!intents.length) return <p className="empty">No intents yet. Every command sent from this console becomes one, with its checks and decision.</p>
  return (
    <>
      <p className="ix-stats">
        last {intents.length}: <b>{stats.auto}</b> auto, <b>{stats.human}</b> to a human ({stats.approved} approved, {stats.rejected} rejected, {stats.expired} expired), <b>{stats.rules}</b> stopped by rules
        {stats.med != null && (
          <>
            ; median human wait <b>{fmtDuration(stats.med)}</b>
          </>
        )}
      </p>
      <ul className="ix-decs">
        {intents.map((i) => (
          <DecisionRow key={i.id} intent={i} vehicles={vehicles} now={now} />
        ))}
      </ul>
    </>
  )
}

// ---- the panel --------------------------------------------------------------------------------------

interface Notice {
  n: number
  text: string
  tone: 'ok' | 'bad'
}

export function InboxPanel({ decisions, vehicles, now, tab, setTab, selection, onFocus, onClose, onTrack, mem, hidden }: Props & { mem: ReturnType<typeof useAlertMemory>; hidden: Set<string> }) {
  const [notices, setNotices] = useState<Notice[]>([])
  const [showHidden, setShowHidden] = useState(false)
  const pending = decisions?.inbox.pending ?? []
  const alerts = decisions?.inbox.alerts ?? []
  const live = alerts.filter((a) => !hidden.has(a.id))
  const muted = alerts.filter((a) => hidden.has(a.id))

  useEffect(() => {
    if (!notices.length) return
    const id = window.setTimeout(() => setNotices((ns) => ns.slice(1)), 9000)
    return () => window.clearTimeout(id)
  }, [notices])

  const uidsOf = (i: Intent) => i.uids.map(Number).filter((n) => Number.isFinite(n))
  const mutedLabel = (a: Alert) => {
    const until = mem.snoozedUntil(a)
    return until != null ? `snoozed, back in ${fmtDuration(until)}` : 'acknowledged'
  }

  return (
    <aside className="ix" aria-label="Inbox">
      <header className="ix-top">
        <h2>Inbox</h2>
        <div className="seg">
          <button className={tab === 'inbox' ? 'on' : ''} onClick={() => setTab('inbox')}>
            Pending{pending.length + live.length ? ` (${pending.length + live.length})` : ''}
          </button>
          <button className={tab === 'decisions' ? 'on' : ''} onClick={() => setTab('decisions')}>
            Decisions
          </button>
        </div>
        <button className="ghost close" onClick={onClose} aria-label="Close inbox">
          Close
        </button>
      </header>
      {notices.length > 0 && (
        <div className="ix-notices" role="status">
          {notices.map((n) => (
            <p key={n.n} className={n.tone} onClick={() => setNotices((ns) => ns.filter((x) => x !== n))}>
              {n.text}
            </p>
          ))}
        </div>
      )}

      <div className="ix-scroll">
        {tab === 'decisions' ? (
          <DecisionsList intents={decisions?.intents ?? []} vehicles={vehicles} now={now} />
        ) : (
          <>
            <EngagementsSection
              intents={decisions?.intents ?? []}
              vehicles={vehicles}
              now={now}
              selection={selection}
              onFocus={(i) => onFocus({ kind: 'intent', id: i.id }, uidsOf(i), i)}
              onDone={(text, tone) => setNotices((ns) => [...ns, { n: Date.now() + Math.random(), text, tone }])}
            />
            <ReviewSection
              review={decisions?.inbox.review}
              acts={decisions?.inbox.acts}
              now={now}
              onTrack={onTrack}
              onDone={(text, tone) => setNotices((ns) => [...ns, { n: Date.now() + Math.random(), text, tone }])}
            />
            <h3 className="ix-h">
              Approvals <span>{pending.length}</span>
              <small>soonest expiry first</small>
            </h3>
            {pending.length === 0 && <p className="empty">Nothing waiting for a human.</p>}
            {pending.map((i) =>
              isEngage(i) ? (
                <EngageCard
                  key={i.id}
                  intent={i}
                  vehicles={vehicles}
                  now={now}
                  selected={selection?.kind === 'intent' && selection.id === i.id}
                  onFocus={() => onFocus({ kind: 'intent', id: i.id }, uidsOf(i), i)}
                  onDone={(text, tone) => setNotices((ns) => [...ns, { n: Date.now() + Math.random(), text, tone }])}
                />
              ) : (
              <ApprovalCard
                key={i.id}
                intent={i}
                engaged={(decisions?.intents ?? []).filter(
                  (e) => isEngage(e) && e.id !== i.id && ['pending_approval', 'approved', 'dispatched'].includes(e.state) && e.uids.some((u) => i.uids.includes(u)),
                )}
                vehicles={vehicles}
                now={now}
                selected={selection?.kind === 'intent' && selection.id === i.id}
                onFocus={() => onFocus({ kind: 'intent', id: i.id }, uidsOf(i), i)}
                onDone={(text, tone) => setNotices((ns) => [...ns, { n: Date.now() + Math.random(), text, tone }])}
              />
              ),
            )}
            <h3 className="ix-h">
              Alerts <span>{live.length}</span>
              <small>most severe first, then oldest</small>
            </h3>
            {live.length === 0 && <p className="empty">No active alerts.</p>}
            {live.map((a) => (
              <AlertCard key={a.id} alert={a} now={now} selected={selection?.kind === 'alert' && selection.id === a.id} muted={null} mem={mem} onFocus={() => onFocus({ kind: 'alert', id: a.id }, [a.uid], null)} />
            ))}
            {muted.length > 0 && (
              <>
                <button className="ix-toggle" onClick={() => setShowHidden(!showHidden)}>
                  {showHidden ? 'Hide' : 'Show'} {muted.length} acknowledged or snoozed
                </button>
                {showHidden &&
                  muted.map((a) => (
                    <AlertCard key={a.id} alert={a} now={now} selected={false} muted={mutedLabel(a)} mem={mem} onFocus={() => onFocus({ kind: 'alert', id: a.id }, [a.uid], null)} />
                  ))}
              </>
            )}
            <p className="ix-foot">
              Acknowledge and snooze are kept in this browser only. They are not shared with other operators or consoles, and the bridge does not record them.
            </p>
          </>
        )}
      </div>
    </aside>
  )
}
