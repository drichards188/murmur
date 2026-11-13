import { useEffect, useRef, useState } from 'react'
import { stepsUpAct, useAuth } from './auth'
import { type Blocking, holdingFor, waited } from './blocking'
import { fmtDuration, StepUpNeeded, stepsUp, useOperator, useOperators } from './decisions'
import { Bluf, BlufReason } from './Bluf'
import { flagParts } from './bluf'
import { FLASH_MS, focusInView } from './reveal'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import { type Against, type ObservedAct, type Review, type ReviewDeclaration, type ReviewFlag, type ReviewLostClaim, FLAG_LABEL, confirmDeclaration, confirmLostClaim, confirmNodeLost, dismissNodeFlag, resolveFlag, revokeDeclaration, revokeNodeLoss } from './ident'
import { confirmEffects, evidenceLine, regionLines, type ReviewNodeLoss, type ReviewNodeLost } from './nodeLoss'
import './nodeLoss.css'

/** The inbox's Review (rules 9): what the identification asks of a human, none of it stopping
 *  anything in play. A rule declaration is the engine's (an observed hostile act attributed to the
 *  track above the policy's threshold, nothing conflicting): it stands until an operator
 *  revokes it, and any operator confirms it. A flag is what the engine was unsure of (an
 *  attribution below threshold, conflicting evidence, no track, a victim not ours, one
 *  observation where the policy wants two, a track keeping company with a declared hostile):
 *  it never declares anything; an operator confirms the concern or dismisses it, and declares
 *  a track, if they decide to, deliberately in the Tracks drawer (the flag's track buttons open
 *  it there). Every action is logged under the operator's session. A declarer is told when
 *  someone else revoked their declaration. The observed acts list shows what the sensors saw
 *  and what the engine made of each (with rule declaration off, an operator declares from
 *  these).
 *
 *  An item that alone holds up a nomination (the bridge's `blocking`, blocking.ts: every other
 *  check on an armed platform passes) comes first, in the urgent style, saying what it holds up,
 *  for how long and what the person's act does. Its buttons are the same as any item's: the
 *  urgency only points, it never makes the permissive act easier. */

interface Props {
  review: Review | undefined
  acts: ObservedAct[] | undefined
  /** bridge clock, ms */
  now: () => number
  onDone: (text: string, tone: 'ok' | 'bad') => void
  /** fly to and select a track */
  onTrack?: (id: string) => void
  /** the items that alone hold up a nomination (the inbox's `blocking`) */
  blocking?: Blocking[]
  /** bring this item into view and focus it (the strip's or the Situation panel's link) */
  focus?: { id: string; n: number } | null
  /** one of ours by its uid as a person names it (its callsign): the node-loss evidence's sensors */
  nameOf?: (uid: string) => string
}

/** What an item holds up, how long, and what the person's act does: information beside the
 *  card's own buttons, never a button itself. */
function Holding({ b, now }: { b: Blocking; now: () => number }) {
  return (
    <div className="rv-holding" role="note">
      <span className="rv-holding-tag">Waiting on you</span>
      <span className="rv-holding-time" title="How long it has held up the nomination (the bridge's reading)">
        {waited(b, now())}
      </span>
      <p className="rv-holding-text">{b.text}</p>
      <p className="rv-holding-how">{b.unblocks}</p>
    </div>
  )
}

/** A card the strip or the Situation panel points to: in view, focused and marked a moment, on
 *  each new request (`n`). */
function useFocused(focusN: number | undefined) {
  const ref = useRef<HTMLElement | null>(null)
  const [flash, setFlash] = useState(false)
  useEffect(() => {
    if (!focusN) return
    focusInView(ref.current)
    // marked on the next frame (as reveal.ts's useFlash), so a repeat restarts it
    const raf = requestAnimationFrame(() => setFlash(true))
    const t = window.setTimeout(() => setFlash(false), FLASH_MS)
    return () => {
      cancelAnimationFrame(raf)
      window.clearTimeout(t)
    }
  }, [focusN])
  return { ref, flash }
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
function DeclarationItem({
  d,
  now,
  onDone,
  onTrack,
  holding,
  focusN,
}: {
  d: ReviewDeclaration
  now: () => number
  onDone: Props['onDone']
  onTrack?: Props['onTrack']
  holding?: Blocking
  focusN?: number
}) {
  const { ref, flash } = useFocused(focusN)
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
    <article
      ref={ref}
      tabIndex={-1}
      className={`ix-card rv-card rv-decl${holding ? ' rv-urgent' : ''}${flash ? ' rv-flash' : ''}`}
      aria-label={`Rule declaration ${d.id} on ${d.track}${holding ? ', holding up a nomination' : ''}`}
      data-tutorial="review-declaration"
      data-track-id={d.track}
      data-review-id={d.id}
    >
      <header>
        <b>{d.track}</b> <span className="rv-tag">BY RULE</span> <span className="muted">{d.id}, {age(now, d.t)}</span>
        {onTrack && (
          <button className="ghost mini" onClick={() => onTrack(d.track)}>
            show
          </button>
        )}
      </header>
      {holding && <Holding b={holding} now={now} />}
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

function FlagItem({
  f,
  now,
  onDone,
  onTrack,
  holding,
  focusN,
}: {
  f: ReviewFlag
  now: () => number
  onDone: Props['onDone']
  onTrack?: Props['onTrack']
  holding?: Blocking
  focusN?: number
}) {
  const { ref, flash } = useFocused(focusN)
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
    <article
      ref={ref}
      tabIndex={-1}
      className={`ix-card rv-card rv-flag${holding ? ' rv-urgent' : ''}${flash ? ' rv-flash' : ''}`}
      aria-label={`Review flag ${f.id}${holding ? ', holding up a nomination' : ''}`}
      data-review-id={f.id}
    >
      <header>
        <b>{f.id}</b> <span className="rv-tag flag">{FLAG_LABEL[f.flag_kind] ?? f.flag_kind}</span>
        {f.status.status === 'confirmed' && <span className="muted"> confirmed by {f.status.by}</span>}
        <span className="muted"> {age(now, f.t)}</span>
      </header>
      {holding && <Holding b={holding} now={now} />}
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

/** Rules 30 (docs/NODE-LOSS.md section 5, decisions D1 and D2): one of ours presumed lost by
 *  the engine, or silent and flagged by hand. The card shows the evidence with its times, the
 *  silence, where the keep-outs count it now, and what confirming would do; only an operator
 *  with engagement authority confirms it (the bridge refuses anyone else, and every agent), on
 *  exactly the silence shown, saying why where nothing presumes it lost, with the credential
 *  again in a credentials run. A hand flag can be dismissed; a presumption is the engine's and
 *  goes when the vehicle is heard again or is confirmed lost. Confirming is never the easy
 *  act: it opens what it does first. */
function NodeLossItem({ n, now, onDone, focusN, nameOf }: { n: ReviewNodeLoss; now: () => number; onDone: Props['onDone']; focusN?: number; nameOf?: Props['nameOf'] }) {
  const { ref, flash } = useFocused(focusN)
  const [open, setOpen] = useState<'none' | 'confirm' | 'dismiss'>('none')
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  const [operator] = useOperator()
  const auth = useAuth()
  const ops = useOperators()
  // decision D1: only an operator with engagement authority confirms (the bridge enforces it)
  const authority = auth.auth === 'credentials' ? auth.engage === true : ops.engage.includes(operator)
  // removing a friendly-fire protection: the credential again in a credentials run (as an un-spend)
  const stepUp = stepsUp()
  const [step, setStep] = useStepUp()
  const [stepErr, setStepErr] = useState(false)
  const who = n.callsign ?? `vehicle ${n.uid}`
  const ago = (t: number) => `${fmtDuration(Math.max(0, now() / 1000 - t))} ago`
  const noteOk = !n.needs_note || note.trim().length > 0
  const confirm = async () => {
    if (stepUp && !stepUpReady(step)) {
      setStepErr(false)
      return
    }
    setBusy(true)
    setStepErr(false)
    try {
      const proof = stepUp ? await stepUpProof(step, { act: 'node_lost', uid: n.uid, since: n.since }) : {}
      await confirmNodeLost(n.uid, n.since, note, proof)
      onDone(`${who} confirmed lost: out of every keep-out, its orders ended`, 'ok')
      setOpen('none')
    } catch (e) {
      setStepErr(e instanceof StepUpNeeded)
      onDone(`${who}: confirming the loss refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
      setStep({ password: '' })
    }
  }
  const dismiss = async () => {
    setBusy(true)
    try {
      await dismissNodeFlag(n.uid, note)
      onDone(`${who}: the hand flag dismissed`, 'ok')
    } catch (e) {
      onDone(`${who}: dismiss refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
    }
  }
  const where = regionLines(n.region?.parts ?? [])
  return (
    <article
      ref={ref}
      tabIndex={-1}
      className={`ix-card rv-card nl-card${flash ? ' rv-flash' : ''}`}
      aria-label={`${n.state === 'presumed_lost' ? 'Presumed lost' : 'Flagged as possibly lost'}: ${who}`}
      data-review-id={n.id}
    >
      <header>
        <b>{who}</b> <span className={`nl-tag${n.state === 'silent' ? ' silent' : ''}`}>{n.state === 'presumed_lost' ? 'PRESUMED LOST' : 'SILENT, FLAGGED'}</span>
        <span className="muted">
          {' '}
          silent {n.silent_s == null ? 'since it started (no position reported)' : fmtDuration(n.silent_s)}
        </span>
      </header>
      <ul className="nl-lines" aria-label="Evidence">
        {n.evidence.map((e, i) => (
          <li key={i}>{evidenceLine(e, ago(e.t), nameOf)}</li>
        ))}
        {n.since_t != null && <li>Its last report, {ago(n.since_t)}{n.last_position ? ` at ${n.last_position[1].toFixed(5)}, ${n.last_position[0].toFixed(5)}` : ''}.</li>}
      </ul>
      {where.length > 0 && (
        <>
          <p className="muted rv-hint">Until then every engagement keep-out counts it where it could be (drawn on the map):</p>
          <ul className="nl-lines" aria-label="Where the keep-outs count it">
            {where.map((l) => (
              <li key={l}>{l}</li>
            ))}
          </ul>
        </>
      )}
      {n.refused && <p className="eg-alarm">Not confirmable now: {n.refused}.</p>}
      {open === 'none' && (
        <div className="rv-actions">
          {n.actions.includes('confirm') && (
            <button className="ghost mini" disabled={busy} onClick={() => setOpen('confirm')} title="Read what confirming does, then confirm if the evidence holds (an operator with engagement authority)">
              what confirming does, then confirm...
            </button>
          )}
          {n.actions.includes('dismiss') && (
            <button className="ghost mini" disabled={busy} onClick={() => setOpen('dismiss')} title="Take the hand flag off the list (any operator)">
              dismiss the flag...
            </button>
          )}
        </div>
      )}
      {open === 'confirm' && (
        <div className="rv-evidence">
          <ul className="nl-lines" aria-label="What confirming does">
            {confirmEffects(n).map((l, i) => (
              <li key={i}>{l}</li>
            ))}
          </ul>
          {!authority && <p className="eg-alarm">Only an operator with engagement authority confirms one of ours lost: the bridge will refuse you.</p>}
          <div className="rv-actions">
            <input
              className="ix-note"
              value={note}
              maxLength={300}
              onChange={(e) => setNote(e.target.value)}
              placeholder={n.needs_note ? 'Why it is lost: nothing presumes it (required, logged)' : 'Note (optional, logged)'}
              aria-label="Why it is confirmed lost"
              disabled={busy}
            />
            <button className="ghost mini rv-danger" disabled={busy || !noteOk || (stepUp && !stepUpReady(step))} onClick={() => void confirm()}>
              confirm {who} lost
            </button>
            <button className="ghost mini" disabled={busy} onClick={() => setOpen('none')}>
              cancel
            </button>
          </div>
          {stepUp && (
            <div className="tk-stepup">
              <StepUpField
                passwordLabel="Consequential: your password to confirm the loss"
                purpose={`to confirm ${who} lost`}
                ariaLabel="Your password, to confirm this loss"
                value={step}
                onChange={setStep}
                busy={busy}
                bad={stepErr}
                onEnter={() => noteOk && void confirm()}
              />
            </div>
          )}
        </div>
      )}
      {open === 'dismiss' && (
        <div className="rv-actions">
          <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="Why (optional, logged)" aria-label="Note for the dismissal" disabled={busy} />
          <button className="ghost mini" disabled={busy} onClick={() => void dismiss()}>
            dismiss
          </button>
          <button className="ghost mini" disabled={busy} onClick={() => setOpen('none')}>
            cancel
          </button>
        </div>
      )}
    </article>
  )
}

/** Rules 30: one of ours confirmed lost: who confirmed it, when, on what silence, and what it
 *  closed. Any operator revokes it, saying why (it only restricts: it counts in every keep-out
 *  again at once); a report from it revokes it on its own. */
function NodeLostItem({ n, now, onDone, focusN }: { n: ReviewNodeLost; now: () => number; onDone: Props['onDone']; focusN?: number }) {
  const { ref, flash } = useFocused(focusN)
  const [revoking, setRevoking] = useState(false)
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  const who = n.callsign ?? `vehicle ${n.uid}`
  const revoke = async () => {
    setBusy(true)
    try {
      await revokeNodeLoss(n.uid, note)
      onDone(`${who}: the loss revoked: it counts in every keep-out again`, 'ok')
      setRevoking(false)
    } catch (e) {
      onDone(`${who}: revoking the loss refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
    }
  }
  const closed = Object.keys(n.closed)
  return (
    <article ref={ref} tabIndex={-1} className={`ix-card rv-card nl-card nl-settled${flash ? ' rv-flash' : ''}`} aria-label={`Confirmed lost: ${who}`} data-review-id={n.id}>
      <header>
        <b>{who}</b> <span className="nl-tag lost">LOST</span>
        <span className="muted">
          {' '}
          confirmed by {n.by} {fmtDuration(Math.max(0, now() / 1000 - n.t))} ago
        </span>
      </header>
      <p className="muted rv-hint">
        {n.note ? `"${n.note}". ` : ''}Silent {n.since_t == null ? 'since it started' : `since its report ${fmtDuration(Math.max(0, now() / 1000 - n.since_t))} ago`}. Out of every keep-out; it takes no order.
        {closed.length ? ` Its owed lost-link values (${closed.join(', ')}) closed.` : ''}
        {n.from ? ` Carried over from ${n.from}.` : ''}
        {n.not_held ? ` It holds no vehicle now: ${n.not_held}.` : ''} Any report from it revokes the loss.
      </p>
      {!revoking ? (
        <div className="rv-actions">
          <button className="ghost mini rv-danger" disabled={busy} onClick={() => setRevoking(true)} title="Revoke the loss: it counts in every keep-out again at once and takes orders (any operator)">
            revoke...
          </button>
        </div>
      ) : (
        <div className="rv-actions">
          <span className="rv-warn">Revoke the loss of {who}? It counts in every keep-out again at once, and an engagement whose picture left it out stops.</span>
          <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="Why (required, logged)" aria-label="Why the loss is revoked" disabled={busy} />
          <button className="ghost mini rv-danger" disabled={busy || !note.trim()} onClick={() => void revoke()}>
            revoke the loss
          </button>
          <button className="ghost mini" disabled={busy} onClick={() => setRevoking(false)}>
            cancel
          </button>
        </div>
      )}
    </article>
  )
}

function verdictText(a: ObservedAct) {
  const v = a.verdict
  if (v.verdict === 'declare') return `declared ${v.track} hostile by rule (${a.declaration ?? ''}, attribution ${v.attribution.toFixed(2)})`
  if (v.verdict === 'flag') return `flagged ${a.flag ?? ''} (${FLAG_LABEL[v.kind] ?? v.kind}): ${v.why}`
  return `recorded: ${v.why}`
}

export function ReviewSection({ review, acts, now, onDone, onTrack, blocking, focus, nameOf }: Props) {
  const [showActs, setShowActs] = useState(false)
  const [operator] = useOperator()
  const decls = review?.declarations ?? []
  const flags = review?.flags ?? []
  // rules 16: claims on lost contacts
  const claims = review?.lost_claims ?? []
  // rules 30: our vehicles presumed lost or flagged by hand, and those confirmed lost
  const nodeLosses = review?.node_losses ?? []
  const nodesLost = review?.nodes_lost ?? []
  // a declarer's notice: their declaration revoked by someone else
  const notices = (review?.revocations ?? []).filter((r) => r.notify && r.notify === operator).slice(0, 5)
  const n = decls.length + flags.length + claims.length + nodeLosses.length
  const holding = new Set((blocking ?? []).map((b) => b.id))
  const focusN = (id: string) => (focus && focus.id === id ? focus.n : undefined)
  // what holds up a nomination first (declarations, then flags), then the rest as sent
  const urgentFlags = flags.filter((f) => holding.has(f.id))
  const otherFlags = flags.filter((f) => !holding.has(f.id))
  const urgentDecls = decls.filter((d) => holding.has(d.id))
  const otherDecls = decls.filter((d) => !holding.has(d.id))
  return (
    <>
      <h3 className={`ix-h${holding.size ? ' ix-h-urgent' : ''}`}>
        Review <span>{n}</span>
        {holding.size ? (
          <small>
            {holding.size} holding up a nomination: {holding.size === 1 ? 'it is' : 'they are'} first
          </small>
        ) : (
          <small>the engine decided; confirm, dismiss or revoke</small>
        )}
      </h3>
      {notices.map((r) => (
        <p key={`${r.id}-${r.t}`} className="rv-notice" role="status">
          Your declaration {r.id} of {r.track} was revoked by {r.revoked_by} {age(now, r.t)}
          {r.note ? `: ${r.note}` : ''}
        </p>
      ))}
      {n === 0 && <p className="empty">Nothing for review.</p>}
      {/* one keyed list, so an item that starts or stops holding something up moves and keeps
          what its card has open or typed */}
      {[...urgentDecls, ...urgentFlags, ...otherDecls, ...otherFlags].map((x) =>
        x.kind === 'declaration' ? (
          <DeclarationItem key={x.id} d={x} now={now} onDone={onDone} onTrack={onTrack} holding={holdingFor(blocking, x.id)} focusN={focusN(x.id)} />
        ) : (
          <FlagItem key={x.id} f={x} now={now} onDone={onDone} onTrack={onTrack} holding={holdingFor(blocking, x.id)} focusN={focusN(x.id)} />
        ),
      )}
      {claims.map((c) => (
        <LostClaimItem key={c.id} c={c} onDone={onDone} onTrack={onTrack} />
      ))}
      {(nodeLosses.length > 0 || nodesLost.length > 0) && (
        <h4 className="nl-h" aria-label="Node losses">
          Node losses <small className="muted">one of ours silent: only an operator with engagement authority confirms it lost</small>
        </h4>
      )}
      {nodeLosses.map((x) => (
        <NodeLossItem key={x.id} n={x} now={now} onDone={onDone} focusN={focusN(x.id)} nameOf={nameOf} />
      ))}
      {nodesLost.map((x) => (
        <NodeLostItem key={x.id} n={x} now={now} onDone={onDone} focusN={focusN(x.id)} />
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
