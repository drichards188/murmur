import { useState } from 'react'
import { stepsUpAct } from './auth'
import { fmtDuration, StepUpNeeded, useOperator } from './decisions'
import { Bluf, BlufReason } from './Bluf'
import { flagParts } from './bluf'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import { type Against, type ObservedAct, type Review, type ReviewDeclaration, type ReviewFlag, type ReviewLostClaim, FLAG_LABEL, confirmDeclaration, confirmLostClaim, resolveFlag, revokeDeclaration } from './ident'

/** The inbox's Review (rules 9): what the identification asks of a human, none of it
 *  blocking. A rule declaration is the engine's (an observed hostile act attributed to the
 *  track above the policy's threshold, nothing conflicting): it stands until an operator
 *  revokes it, and any operator confirms it. A flag is what the engine was unsure of (an
 *  attribution below threshold, conflicting evidence, no track, a victim not ours, one
 *  observation where the policy wants two, a track keeping company with a declared hostile):
 *  it never declares anything; an operator confirms the concern or dismisses it, and declares
 *  a track, if they decide to, deliberately in the Tracks drawer (the flag's track buttons open
 *  it there). Every action is logged under the operator's session. A declarer is told when
 *  someone else revoked their declaration. The observed acts list shows what the sensors saw
 *  and what the engine made of each (with rule declaration off, an operator declares from
 *  these). */

interface Props {
  review: Review | undefined
  acts: ObservedAct[] | undefined
  /** bridge clock, ms */
  now: () => number
  onDone: (text: string, tone: 'ok' | 'bad') => void
  /** fly to and select a track */
  onTrack?: (id: string) => void
}

function age(now: () => number, t: number) {
  return `${fmtDuration(Math.max(0, now() / 1000 - t))} ago`
}

function againstText(a: Against) {
  if (a.kind === 'vehicle') return `our vehicle ${a.uid}`
  if (a.kind === 'track') return `track ${a.track}`
  return 'something we hold no track of'
}

/** A rule declaration: the engine's, standing until revoked. Confirming it is the permissive
 *  act, so it is never the easy one: the card opens its evidence first (the act, whom it was
 *  against, which sensors saw it, the attribution against the threshold), and confirm sits
 *  there, as plain as revoke. Revoking (the brake: it stops every engagement in play on the
 *  track first) takes no more than confirming: open it, then revoke, with an optional note. */
function DeclarationItem({ d, now, onDone, onTrack }: { d: ReviewDeclaration; now: () => number; onDone: Props['onDone']; onTrack?: Props['onTrack'] }) {
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState('')
  // each takes a second, deliberate click: confirm after the evidence, revoke after its warning
  const [open, setOpen] = useState<'none' | 'evidence' | 'revoke'>('none')
  const revoking = open === 'revoke'
  const stepUp = stepsUpAct('confirm_declaration')
  // rules 10: the password again, or the passkey (where the run asks for one)
  const [step, setStep] = useStepUp()
  const [asking, setAsking] = useState(false)
  const run = async (what: string, f: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await f()
      onDone(`${d.track}: ${what}`, 'ok')
      setAsking(false)
      setOpen('none')
    } catch (e) {
      if (e instanceof StepUpNeeded) setAsking(true)
      onDone(`${d.track}: ${what} refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
      setStep({ password: '' })
    }
  }
  const ev = d.declaration.by.kind === 'rule' ? d.declaration.by.evidence : undefined
  const confirm = () => {
    if (stepUp && !stepUpReady(step)) {
      setAsking(true)
      return
    }
    void run('declaration confirmed', async () => {
      const proof = stepUp ? await stepUpProof(step, { act: 'confirm_declaration', track: d.track }) : {}
      return confirmDeclaration(d.track, proof)
    })
  }
  return (
    <article className="ix-card rv-card rv-decl" aria-label={`Rule declaration ${d.id} on ${d.track}`} data-tutorial="review-declaration" data-track-id={d.track}>
      <header>
        <b>{d.track}</b> <span className="rv-tag">BY RULE</span> <span className="muted">{d.id}, {age(now, d.t)}</span>
        {onTrack && (
          <button className="ghost mini" onClick={() => onTrack(d.track)}>
            show
          </button>
        )}
      </header>
      <p className="rv-basis">Declared hostile by rule on act {ev?.act ?? 'unknown'}; it stands until revoked.</p>
      {open === 'none' && (
        <div className="rv-actions">
          <button className="ghost mini" disabled={busy} onClick={() => setOpen('evidence')} title="Read the evidence, then confirm if it holds">
            evidence, then confirm...
          </button>
          <button className="ghost mini rv-danger" disabled={busy} onClick={() => setOpen('revoke')} title="Revoke it: every engagement in play on the track stops first">
            revoke...
          </button>
        </div>
      )}
      {open === 'evidence' && (
        <div className="rv-evidence">
          {ev ? (
            <dl>
              <dt>Act</dt>
              <dd>
                {ev.act} against {againstText(ev.against)}, {age(now, ev.observed_t / 1000)}
              </dd>
              <dt>Seen by</dt>
              <dd>{ev.sensors.join(', ') || 'none named'}</dd>
              <dt>Attribution</dt>
              <dd>
                {ev.attribution.toFixed(2)} to {d.track} (threshold {ev.threshold.toFixed(2)})
                {ev.corroborated_by ? `, corroborated by ${ev.corroborated_by.observer ? `${ev.corroborated_by.observer}'s fix` : `act ${ev.corroborated_by.act}`} at ${ev.corroborated_by.attribution.toFixed(2)}` : ''}
              </dd>
            </dl>
          ) : null}
          <BlufReason className="rv-basis" text={d.basis} />
          <div className="rv-actions">
            <button className="ghost mini" disabled={busy} onClick={confirm} title="Confirm the engine's declaration (logged under your session)">
              confirm {d.track} hostile
            </button>
            <button className="ghost mini rv-danger" disabled={busy} onClick={() => setOpen('revoke')}>
              revoke...
            </button>
            <button className="ghost mini" disabled={busy} onClick={() => setOpen('none')}>
              close
            </button>
          </div>
        </div>
      )}
      {revoking && (
        <div className="rv-revoke">
          <span className="rv-warn">Revoke {d.id}? Every engagement in play on {d.track} stops first.</span>
          <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="why (optional, logged)" aria-label="Note for the revocation" disabled={busy} />
          <button className="ghost mini rv-danger" disabled={busy} onClick={() => void run('declaration revoked', () => revokeDeclaration(d.track, note))}>
            revoke {d.track}
          </button>
          <button className="ghost mini" disabled={busy} onClick={() => setOpen('none')}>
            cancel
          </button>
        </div>
      )}
      {asking && stepUp && (
        <div className="tk-stepup">
          <StepUpField
            passwordLabel="Consequential: your password to confirm"
            purpose={`to confirm the engine's declaration of ${d.track}`}
            ariaLabel="Your password, to confirm this declaration"
            value={step}
            onChange={setStep}
            busy={busy}
            onEnter={confirm}
          />
          <button className="ghost mini" disabled={!stepUpReady(step) || busy} onClick={confirm}>
            confirm
          </button>
        </div>
      )}
    </article>
  )
}

/** Rules 16: a claim on a lost contact: the evidence (names and ship types compared, how far
 *  it is against how far the lost contact could have got, what our sensors say of its kind),
 *  what keeps it from being confirmed, and a deliberate Confirm when nothing does. */
function LostClaimItem({ c, onDone, onTrack }: { c: ReviewLostClaim; onDone: Props['onDone']; onTrack?: Props['onTrack'] }) {
  const [busy, setBusy] = useState(false)
  const [sure, setSure] = useState(false)
  const confirm = async () => {
    setBusy(true)
    try {
      await confirmLostClaim(c.lost, c.track)
      onDone(`${c.lost} confirmed as ${c.track}: retired from the keep-outs while ${c.track} stands`, 'ok')
    } catch (e) {
      onDone(`${c.lost} as ${c.track} refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
      setSure(false)
    }
  }
  return (
    <article className="ix-card rv-card rv-claim" aria-label={`Claim of ${c.track} to be lost contact ${c.lost}`}>
      <header>
        <b>{c.track}</b> <span className="rv-tag flag">claims to be lost contact {c.lost}</span> <span className="muted">MMSI {c.mmsi}</span>
      </header>
      <BlufReason className="rv-basis" text={c.evidence} />
      {c.blockers.length > 0 && <p className="eg-alarm">Not confirmable: {c.blockers.join('; ')}.</p>}
      <p className="muted rv-hint">
        An AIS identity is what a contact says it is: one may broadcast another&apos;s. Until confirmed, {c.lost} counts in every keep-out beside {c.track}; confirmed, it
        is retired while {c.track} stands (void if {c.track} turns suspect, is declared or is dropped).{' '}
        <button className="ghost mini" onClick={() => onTrack?.(c.track)} disabled={!onTrack}>
          {c.track}
        </button>
      </p>
      {c.actions.includes('confirm') && (
        <div className="rv-actions">
          {!sure ? (
            <button className="ghost mini" disabled={busy} onClick={() => setSure(true)} title="Look at the evidence first">
              confirm…
            </button>
          ) : (
            <>
              <span className="muted">{c.track} is {c.lost}: logged under your session.</span>
              <button className="ghost mini" disabled={busy} onClick={() => void confirm()}>
                confirm
              </button>
              <button className="ghost mini" disabled={busy} onClick={() => setSure(false)}>
                cancel
              </button>
            </>
          )}
        </div>
      )}
    </article>
  )
}

function FlagItem({ f, now, onDone, onTrack }: { f: ReviewFlag; now: () => number; onDone: Props['onDone']; onTrack?: Props['onTrack'] }) {
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState('')
  const run = async (what: string, fn: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await fn()
      onDone(`F-${f.flag}: ${what}`, 'ok')
    } catch (e) {
      onDone(`F-${f.flag}: ${what} refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
    }
  }
  return (
    <article className="ix-card rv-card rv-flag" aria-label={`Review flag ${f.id}`}>
      <header>
        <b>{f.id}</b> <span className="rv-tag flag">{FLAG_LABEL[f.flag_kind] ?? f.flag_kind}</span>
        {f.status.status === 'confirmed' && <span className="muted"> confirmed by {f.status.by}</span>}
        <span className="muted"> {age(now, f.t)}</span>
      </header>
      <Bluf className="rv-basis" parts={flagParts(f.flag_kind, f.evidence)} />
      <p className="muted rv-hint">
        {f.tracks.length ? (
          <>
            Open to look, or to declare deliberately in the Tracks drawer:{' '}
            {f.tracks.map((t) => (
              <button key={t} className="ghost mini" onClick={() => onTrack?.(t)} disabled={!onTrack}>
                {t}
              </button>
            ))}
          </>
        ) : (
          'No track named.'
        )}
        {f.raises_suspect ? ' Shown suspect (weak evidence: never a declaration on its own).' : ' The flag declares nothing.'}
      </p>
      <div className="rv-actions">
        <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="note (optional, logged)" aria-label="Note for this resolution" disabled={busy} />
        {f.actions.includes('confirm') && (
          <button className="ghost mini" disabled={busy} onClick={() => void run('confirmed', () => resolveFlag(f.flag, 'confirmed', note))} title="The concern stands (the track is not declared by this)">
            confirm
          </button>
        )}
        {f.actions.includes('dismiss') && (
          <button className="ghost mini" disabled={busy} onClick={() => void run('dismissed', () => resolveFlag(f.flag, 'dismissed', note))} title="Dismiss the concern">
            dismiss
          </button>
        )}
      </div>
    </article>
  )
}

function verdictText(a: ObservedAct) {
  const v = a.verdict
  if (v.verdict === 'declare') return `declared ${v.track} hostile by rule (${a.declaration ?? ''}, attribution ${v.attribution.toFixed(2)})`
  if (v.verdict === 'flag') return `flagged ${a.flag ?? ''} (${FLAG_LABEL[v.kind] ?? v.kind}): ${v.why}`
  return `recorded: ${v.why}`
}

export function ReviewSection({ review, acts, now, onDone, onTrack }: Props) {
  const [showActs, setShowActs] = useState(false)
  const [operator] = useOperator()
  const decls = review?.declarations ?? []
  const flags = review?.flags ?? []
  // rules 16: claims on lost contacts
  const claims = review?.lost_claims ?? []
  // a declarer's notice: their declaration revoked by someone else
  const notices = (review?.revocations ?? []).filter((r) => r.notify && r.notify === operator).slice(0, 5)
  const n = decls.length + flags.length + claims.length
  return (
    <>
      <h3 className="ix-h">
        Review <span>{n}</span>
        <small>not blocking: the engine decided; confirm, dismiss or revoke</small>
      </h3>
      {notices.map((r) => (
        <p key={`${r.id}-${r.t}`} className="rv-notice" role="status">
          Your declaration {r.id} of {r.track} was revoked by {r.revoked_by} {age(now, r.t)}
          {r.note ? `: ${r.note}` : ''}
        </p>
      ))}
      {n === 0 && <p className="empty">Nothing for review.</p>}
      {decls.map((d) => (
        <DeclarationItem key={d.id} d={d} now={now} onDone={onDone} onTrack={onTrack} />
      ))}
      {flags.map((f) => (
        <FlagItem key={f.id} f={f} now={now} onDone={onDone} onTrack={onTrack} />
      ))}
      {claims.map((c) => (
        <LostClaimItem key={c.id} c={c} onDone={onDone} onTrack={onTrack} />
      ))}
      {(acts?.length ?? 0) > 0 && (
        <>
          <button className="ix-toggle" onClick={() => setShowActs(!showActs)}>
            {showActs ? 'Hide' : 'Show'} {acts?.length} observed act{acts?.length === 1 ? '' : 's'}
          </button>
          {showActs && (
            <ul className="rv-acts">
              {acts?.map((a) => (
                <li key={a.act.act}>
                  <b>{a.act.act}</b> seen by {a.act.sensors.join(', ')} (1-sigma {Math.round(a.act.sigma_m)} m{a.act.fixes?.length ? `; ${a.act.fixes.length} more observer${a.act.fixes.length === 1 ? '' : 's'}` : ''})
                  {a.candidates.length ? `; candidates ${a.candidates.map((c) => `${c.track} ${c.confidence.toFixed(2)}`).join(', ')}` : '; no track near'}: {verdictText(a)}
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </>
  )
}
