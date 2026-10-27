// A track's declaration where a person acts on it: on its track card (TracksPanel) and at the
// nomination form's first step (EngagePanel). Evidence first (the observed act, whom it was
// against, who saw it, the attribution against the rule's threshold, AIS), then the acts that fit
// its state, each stating its consequence where it is pressed:
//  - a rule declaration nobody confirmed: Confirm and Revoke, side by side and alike, two
//    deliberate clicks each (docs/DECISIONS.md "Confirming is never the easy act in the console");
//  - a declaration in force: Revoke (and, hostile on a track card, Nominate an engagement);
//  - none (track card only): Declare hostile, two clicks and apart from the restrictive marks
//    (suspect, neutral, unknown), which stay one click each.
// The acts are the operator actions the bridge already has (`/api/tracks/{id}/confirm`,
// `/revoke`, `/declare`), with the step-up where the policy lists the act.
import { type KeyboardEvent, useState } from 'react'
import { stepsUpAct } from './auth'
import { StepUpNeeded, useOperator } from './decisions'
import { aisText, declarationLine, declFacts, evidenceBullets } from './engageSteps'
import { confirmDeclaration, revokeDeclaration } from './ident'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import { AFFILIATION_LABEL, type Affiliation, declareTrack, type Track } from './tracks'
import { type Vehicle, vehicleNamer } from './types'
import { focusInView } from './reveal'
import './declaration.css'

type Ask = 'none' | 'confirm' | 'revoke'

/** Escape closes a question without reaching the form's own Escape (which closes the form). */
const escTo = (close: () => void) => (e: KeyboardEvent) => {
  if (e.key === 'Escape') {
    e.stopPropagation()
    close()
  }
}

/** The declaration in force on `t`, its evidence and its acts. `onEngage`: a declared-hostile
 *  track's card offers the nomination form. `where`: the track card, or the nomination form (whose
 *  words say what confirming unblocks there). */
export function DeclarationBlock({
  t,
  vehicles,
  now,
  onEngage,
  where,
  onChanged,
}: {
  t: Track
  vehicles: Vehicle[]
  /** bridge clock, ms */
  now: () => number
  onEngage?: () => void
  where: 'track' | 'nominate'
  /** after a confirmation or revocation the bridge accepted */
  onChanged?: (what: 'confirmed' | 'revoked') => void
}) {
  const [operator] = useOperator()
  const [ask, setAsk] = useState<Ask>('none')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  const [note, setNote] = useState('')
  // rules 9/10: confirming steps up only where the policy lists it (off by default)
  const confirmStepUp = stepsUpAct('confirm_declaration')
  const [step, setStep] = useStepUp()
  const [stepBad, setStepBad] = useState(false)
  const d = t.declared
  const facts = declFacts(t)
  if (!d || !facts) return null
  const unconfirmed = !!d.rule && !d.confirmed_by
  const nameOf = vehicleNamer(vehicles)
  const evidence = d.evidence ? evidenceBullets(d.evidence, nameOf, now()) : []
  const run = async (what: 'confirmed' | 'revoked', f: () => Promise<unknown>) => {
    setBusy(true)
    setErr(null)
    setStepBad(false)
    try {
      await f()
      setAsk('none')
      setNote('')
      onChanged?.(what)
    } catch (e) {
      setStepBad(e instanceof StepUpNeeded)
      setErr(`Not ${what}: ${(e as Error).message}`)
    } finally {
      setStep({ password: '' })
      setBusy(false)
    }
  }
  const confirm = () =>
    void run('confirmed', async () => {
      const proof = confirmStepUp ? await stepUpProof(step, { act: 'confirm_declaration', track: t.id }) : {}
      return confirmDeclaration(t.id, proof)
    })
  const aff = AFFILIATION_LABEL[d.affiliation].toLowerCase()
  const tone = unconfirmed ? 'warn' : d.affiliation === 'hostile' ? 'bad' : ''
  return (
    <section className="dc" aria-label={`Declaration of ${t.id}`}>
      <p className={`dc-head ${tone}`}>
        {unconfirmed ? `Declared ${aff} by the engine's rule: not confirmed by an operator` : declarationLine(facts)}
      </p>
      {(evidence.length > 0 || d.note) && (
        <ul className="dc-evidence">
          {evidence.map((b) => (
            <li key={b}>{b}</li>
          ))}
          {d.rule && <li>{aisText(t)}</li>}
          {d.note && <li>Note: {d.note}</li>}
        </ul>
      )}
      {ask === 'none' && (
        <div className="dc-actions">
          {unconfirmed && (
            <button type="button" className="dc-btn" disabled={busy} onClick={() => setAsk('confirm')}>
              Confirm {aff}…
            </button>
          )}
          {!unconfirmed && d.affiliation === 'hostile' && onEngage && (
            <button type="button" className="dc-btn engage" onClick={onEngage}>
              Nominate an engagement…
            </button>
          )}
          <button type="button" className="dc-btn" disabled={busy} onClick={() => setAsk('revoke')}>
            Revoke…
          </button>
        </div>
      )}
      {ask === 'confirm' && (
        <div className="dc-ask" role="group" aria-label={`Confirm ${t.id} ${aff}`} tabIndex={-1} ref={focusInView} onKeyDown={escTo(() => setAsk('none'))}>
          <p className="dc-ask-head">Confirm {t.id} {aff} on this evidence?</p>
          <ul className="dc-evidence">
            <li>Logged as your act ({operator}); any operator may still revoke it.</li>
            <li>
              {where === 'nominate'
                ? 'The nomination can then go ahead: the policy needs this confirmation.'
                : 'A nomination on it needs this where the policy asks for a confirmation.'}
            </li>
          </ul>
          {confirmStepUp && (
            <StepUpField
              passwordLabel={`Your password, to confirm ${t.id}`}
              purpose={`to confirm the engine's declaration of ${t.id}`}
              ariaLabel="Your password, to confirm this declaration"
              value={step}
              onChange={(s) => {
                setStep(s)
                setStepBad(false)
              }}
              busy={busy}
              bad={stepBad}
              onEnter={confirm}
            />
          )}
          <div className="dc-actions">
            <button type="button" className="dc-btn" disabled={busy || (confirmStepUp && !stepUpReady(step))} onClick={confirm}>
              Confirm {t.id} {aff}
            </button>
            <button type="button" className="dc-btn quiet" disabled={busy} onClick={() => setAsk('none')}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {ask === 'revoke' && (
        <div className="dc-ask" role="group" aria-label={`Revoke the declaration of ${t.id}`} tabIndex={-1} ref={focusInView} onKeyDown={escTo(() => setAsk('none'))}>
          <p className="dc-ask-head">Revoke the declaration of {t.id}?</p>
          <ul className="dc-evidence">
            <li>Every engagement in play on {t.id} stops first, with its fallback.</li>
            {d.rule && <li>The engine's rule does not declare it again; an operator may.</li>}
          </ul>
          <input className="dc-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="Why (optional, logged)" aria-label="Note for the revocation" disabled={busy} />
          <div className="dc-actions">
            <button type="button" className="dc-btn" disabled={busy} onClick={() => void run('revoked', () => revokeDeclaration(t.id, note))}>
              Revoke {t.id}
            </button>
            <button type="button" className="dc-btn quiet" disabled={busy} onClick={() => setAsk('none')}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {err && <p className="dc-err">{err}</p>}
    </section>
  )
}

const MARKS: Affiliation[] = ['suspect', 'neutral', 'unknown']

/** Declaring by hand (track card): hostile on its own, two deliberate clicks with its consequence
 *  stated; the restrictive marks (suspect, neutral, unknown) one click each, apart from it. */
export function DeclareControls({ t }: { t: Track }) {
  const [operator] = useOperator()
  const [asking, setAsking] = useState(false)
  const [busy, setBusy] = useState<Affiliation | null>(null)
  const [err, setErr] = useState<string | null>(null)
  // rules 8: a hostile declaration takes the password again where the policy lists it
  const stepUp = stepsUpAct('declare_hostile')
  const [step, setStep] = useStepUp()
  const [stepBad, setStepBad] = useState(false)
  const current = t.declared?.affiliation
  const declare = async (a: Affiliation) => {
    setBusy(a)
    setErr(null)
    setStepBad(false)
    // a hostile declaration on a track with one review flag in force answers that flag
    const flag = a === 'hostile' && t.review?.length === 1 ? t.review[0].flag : undefined
    try {
      const proof = a === 'hostile' && stepUp ? await stepUpProof(step, { act: 'declare_hostile', track: t.id }) : {}
      await declareTrack(t.id, a, undefined, proof.password, flag, proof.passkey)
      setAsking(false)
    } catch (e) {
      setStepBad(e instanceof StepUpNeeded)
      setErr(`Not declared: ${(e as Error).message}`)
    } finally {
      setStep({ password: '' })
      setBusy(null)
    }
  }
  return (
    <div className="dc-declare">
      <div className="dc-declare-row">
        <span className="dc-lbl">Declare</span>
        {current !== 'hostile' && (
          <button type="button" className="dc-btn hostile" disabled={busy != null || asking} onClick={() => setAsking(true)}>
            Hostile…
          </button>
        )}
        <span className="dc-marks" role="group" aria-label="Mark the track as">
          <span className="dc-lbl soft">or mark</span>
          {MARKS.map((a) => (
            <button
              key={a}
              type="button"
              className={`dc-mark ${current === a ? 'on' : ''}`}
              aria-pressed={current === a}
              disabled={busy != null}
              onClick={() => void declare(a)}
              title={`Declare it ${AFFILIATION_LABEL[a].toLowerCase()} (logged under your session; restricts what may be done to it)`}
            >
              {AFFILIATION_LABEL[a].toLowerCase()}
            </button>
          ))}
        </span>
      </div>
      {asking && (
        <div className="dc-ask" role="group" aria-label={`Declare ${t.id} hostile`} tabIndex={-1} ref={focusInView} onKeyDown={escTo(() => setAsking(false))}>
          <p className="dc-ask-head">Declare {t.id} hostile?</p>
          <ul className="dc-evidence">
            <li>Logged as your act ({operator}); any operator may revoke it.</li>
            <li>It makes {t.id} a target: an engagement may then be nominated on it.</li>
            {(t.affiliation === 'neutral' || t.affiliation === 'assumed_friend') && (
              <li className="warn">
                Against its estimate: {AFFILIATION_LABEL[t.affiliation].toLowerCase()}
                {t.identity.ais ? `, AIS ${t.identity.ais.name}` : ''}.
              </li>
            )}
            {t.declared?.affiliation && <li>It replaces its declaration as {AFFILIATION_LABEL[t.declared.affiliation].toLowerCase()}.</li>}
          </ul>
          {stepUp && (
            <StepUpField
              passwordLabel={`Your password, to declare ${t.id} hostile`}
              purpose={`to declare ${t.id} hostile`}
              ariaLabel="Your password, to declare this track hostile"
              value={step}
              onChange={(s) => {
                setStep(s)
                setStepBad(false)
              }}
              busy={busy != null}
              bad={stepBad}
              onEnter={() => void declare('hostile')}
            />
          )}
          <div className="dc-actions">
            <button type="button" className="dc-btn hostile" disabled={busy != null || (stepUp && !stepUpReady(step))} onClick={() => void declare('hostile')}>
              Declare {t.id} hostile
            </button>
            <button type="button" className="dc-btn quiet" disabled={busy != null} onClick={() => setAsking(false)}>
              Cancel
            </button>
          </div>
        </div>
      )}
      {err && <p className="dc-err">{err}</p>}
    </div>
  )
}
