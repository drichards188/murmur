import { useEffect, useRef, useState } from 'react'
import { useAuth } from './auth'
import type { Blocking } from './blocking'
import { fmtDuration, StepUpNeeded, stepsUp, useOperator, useOperators } from './decisions'
import { Bluf, BlufReason } from './Bluf'
import { flagParts } from './bluf'
import { DeclarationBlock } from './Declaration'
import { FLASH_MS, focusInView } from './reveal'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import {
  type ObservedAct,
  type Review,
  type ReviewDeclaration,
  type ReviewFlag,
  type ReviewLostClaim,
  FLAG_LABEL,
  confirmLostClaim,
  confirmNodeLost,
  dismissNodeFlag,
  resolveFlag,
  revokeNodeLoss,
} from './ident'
import { confirmEffects, evidenceLine, regionLines, type ReviewNodeLoss, type ReviewNodeLost } from './nodeLoss'
import { agoWhen, plainText, sensorList } from './plainText'
import { holdingLine } from './trackCard'
import type { Track } from './tracks'
import type { Vehicle } from './types'
import './nodeLoss.css'

/** The inbox's review (rules 9): what the identification asks of a human, none of it stopping
 *  anything in play. A rule declaration is the engine's (an observed hostile act attributed to the
 *  track above the policy's threshold, nothing conflicting): it stands until an operator
 *  revokes it, and any operator confirms it. A flag is what the engine was unsure of: it never
 *  declares anything; an operator confirms the concern or dismisses it. Every action is logged
 *  under the operator's session. A declarer is told when someone else revoked their declaration.
 *
 *  Each item is one card (ledger 2 section 4): the ask first, the evidence in words in view, what
 *  each answer does, two buttons of equal weight naming their outcomes, a note and the credential
 *  behind the click. The Inbox sorts them with everything else (inboxOrder.ts); `reviewEntries`
 *  hands it each card with its rank. An item that alone holds up a nomination (the bridge's
 *  `blocking`, blocking.ts) ranks first and says in one line what confirming lets go on; its
 *  buttons are the same as any item's: the urgency only points, it never makes the permissive act
 *  easier. */

export interface Props {
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
  /** one of ours by its uid as a person names it (its callsign): the evidence's sensors and victims */
  nameOf?: (uid: string) => string
  /** open flags a later declaration settled (needsYou.ts `settledFlags`): quiet rows in For information */
  settled?: ReadonlyMap<string, string>
  /** the track picture, for a declaration's AIS line and its kind in words */
  tracks?: readonly Track[]
  vehicles?: Vehicle[]
}

/** A uid as is, where no callsign lookup was given. */
const byUid = (uid: string) => `vehicle ${uid}`

/** The engine's evidence text with callsigns, sensor names and times in words (plainText.ts). */
function plain(text: string, now: () => number, nameOf: (uid: string) => string = byUid) {
  return plainText(text, { nameOf, when: agoWhen(now()) })
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

/** The note a decision carries, behind a click (an optional one; a required one is always shown). */
function NoteField({ note, setNote, busy, label, placeholder, required }: { note: string; setNote: (s: string) => void; busy: boolean; label: string; placeholder: string; required?: boolean }) {
  const [open, setOpen] = useState(false)
  if (!open && !required) {
    return (
      <button type="button" className="ghost mini ap-addnote" onClick={() => setOpen(true)}>
        Add a note
      </button>
    )
  }
  return <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder={placeholder} aria-label={label} disabled={busy} autoFocus={!required} />
}

/** A declaration as the track card's component reads it, from the review item when the track has
 *  left the picture (or no longer carries it). */
function trackOf(d: ReviewDeclaration, track: Track | undefined): Track {
  if (track?.declared && track.declared.id === d.declaration.id) return track
  const decl = d.declaration
  const ev = decl.by.kind === 'rule' ? decl.by.evidence : undefined
  return {
    ...(track ?? {}),
    id: d.track,
    identity: track?.identity ?? { kind: null, confidence: 0, ais: null, conflict: null, supported_by: [] },
    declared: {
      affiliation: decl.affiliation,
      by: decl.by.kind === 'rule' ? `rule:${decl.by.rule}` : decl.by.operator,
      t: decl.t,
      note: decl.by.kind === 'operator' ? (decl.by.note ?? null) : null,
      id: decl.id,
      rule: decl.by.kind === 'rule',
      evidence: ev,
      confirmed_by: decl.confirmed?.by,
      basis: d.basis,
    },
  } as unknown as Track
}

/** The observed acts (what the sensors saw, what the engine made of each) that produced this
 *  item, behind a fold on its card: evidence, not a standing list. */
function ActsFold({ acts, now, nameOf }: { acts: ObservedAct[]; now: () => number; nameOf?: Props['nameOf'] }) {
  if (!acts.length) return null
  return (
    <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
      <summary>
        The sensors&apos; reading{acts.length === 1 ? '' : `s (${acts.length})`}
      </summary>
      <ul className="rv-acts">
        {acts.map((a) => (
          <li key={a.act.act}>
            seen by {sensorList(a.act.sensors, nameOf ?? byUid)} (1-sigma {Math.round(a.act.sigma_m)} m{a.act.fixes?.length ? `; ${a.act.fixes.length} more observer${a.act.fixes.length === 1 ? '' : 's'}` : ''})
            {a.candidates.length ? `; candidates ${a.candidates.map((c) => `${c.track} ${c.confidence.toFixed(2)}`).join(', ')}` : '; no track near'}: {verdictText(a, (t) => plain(t, now, nameOf))}
          </li>
        ))}
      </ul>
    </details>
  )
}

/** A rule declaration: the engine's, standing until revoked. It is the one declaration component
 *  the track card shows too (Declaration.tsx, decision D1): the bottom line, the evidence in view,
 *  one line on what each answer does, and Confirm hostile and Revoke of equal weight, each a
 *  second deliberate click with its consequence stated. */
export function DeclarationItem({ d, now, onDone, onTrack, holding, focusN, nameOf, track, vehicles, acts }: { d: ReviewDeclaration; now: () => number; onDone: Props['onDone']; onTrack?: Props['onTrack']; holding?: Blocking; focusN?: number; nameOf?: Props['nameOf']; track?: Track; vehicles: Vehicle[]; acts: ObservedAct[] }) {
  const { ref, flash } = useFocused(focusN)
  const t = trackOf(d, track)
  const ev = d.declaration.by.kind === 'rule' ? d.declaration.by.evidence : undefined
  const mine = acts.filter((a) => a.act.act === ev?.act || a.declaration === d.id)
  const kind = track?.identity.kind ? track.identity.kind.replace(/_/g, ' ') : null
  return (
    <article
      ref={ref}
      tabIndex={-1}
      className={`ix-card rv-card rv-decl${holding ? ' rv-urgent' : ''}${flash ? ' rv-flash' : ''}`}
      aria-label={`The engine declared ${d.track} hostile${holding ? ', holding up a nomination' : ''}`}
      data-tutorial="review-declaration"
      data-track-id={d.track}
      data-review-id={d.id}
    >
      <header>
        <b>{kind ? `${kind} ${d.track}` : d.track}</b> <span className="rv-tag">BY RULE</span>
        {onTrack && (
          <button className="ghost mini" onClick={() => onTrack(d.track)}>
            Show on map
          </button>
        )}
      </header>
      <DeclarationBlock t={t} vehicles={vehicles} now={now} where="inbox" holding={holding} onChanged={(what) => onDone(`${d.track}: declaration ${what}`, 'ok')} />
      <ActsFold acts={mine} now={now} nameOf={nameOf} />
    </article>
  )
}

/** Rules 16: a claim on a lost contact: the evidence (names and ship types compared, how far
 *  it is against how far the lost contact could have got, what our sensors say of its kind),
 *  what keeps it from being confirmed, what confirming does and what leaving it does, and a
 *  deliberate Confirm (its only act) when nothing keeps it. */
export function LostClaimItem({ c, onDone, onTrack }: { c: ReviewLostClaim; onDone: Props['onDone']; onTrack?: Props['onTrack'] }) {
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
    <article className="ix-card rv-card rv-claim" aria-label={`${c.track} says it is lost contact ${c.lost}`}>
      <header>
        <b>
          {c.track} says it is lost contact {c.lost}
        </b>
        {onTrack && (
          <button className="ghost mini" onClick={() => onTrack(c.track)}>
            Show {c.track}
          </button>
        )}
      </header>
      <BlufReason className="rv-basis" text={c.evidence} />
      {c.blockers.length > 0 && <p className="eg-alarm">Not confirmable: {c.blockers.join('; ')}.</p>}
      <p className="rv-means">
        Confirming: {c.lost} leaves the keep-outs while {c.track} stands (void if {c.track} turns suspect, is declared or is dropped). Left alone, {c.lost} keeps counting in every keep-out beside {c.track}.
      </p>
      <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
        <summary>Its AIS</summary>
        <p>MMSI {c.mmsi}. An AIS identity is what a contact says it is: one may broadcast another&apos;s.</p>
      </details>
      {c.actions.includes('confirm') && (
        <div className="rv-actions">
          {!sure ? (
            <button className="ap-btn" disabled={busy} onClick={() => setSure(true)}>
              Confirm…
            </button>
          ) : (
            <>
              <span className="muted">
                {c.track} is {c.lost}: logged under your session.
              </span>
              <button className="ap-btn" disabled={busy} onClick={() => void confirm()}>
                Confirm {c.track} is {c.lost}
              </button>
              <button className="ghost mini" disabled={busy} onClick={() => setSure(false)}>
                Cancel
              </button>
            </>
          )}
        </div>
      )}
    </article>
  )
}

/** What a flag's answers do, in one clause each. */
function flagMeans(f: ReviewFlag): string {
  return `${f.raises_suspect ? 'The track stays shown suspect on weak evidence, never declared. ' : 'The flag declares nothing. '}The concern stands: it stays on the list as confirmed. Dismiss: it comes off the list.`
}

export function FlagItem({ f, now, onDone, onTrack, holding, focusN, nameOf, acts }: { f: ReviewFlag; now: () => number; onDone: Props['onDone']; onTrack?: Props['onTrack']; holding?: Blocking; focusN?: number; nameOf?: Props['nameOf']; acts: ObservedAct[] }) {
  const { ref, flash } = useFocused(focusN)
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState('')
  const run = async (what: string, fn: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await fn()
      onDone(`${f.tracks.join(', ') || 'The flag'}: ${what}`, 'ok')
    } catch (e) {
      onDone(`${f.tracks.join(', ') || 'The flag'}: ${what} refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
    }
  }
  const mine = acts.filter((a) => a.flag != null && [f.id, String(f.flag), `F-${f.flag}`].includes(a.flag))
  return (
    <article
      ref={ref}
      tabIndex={-1}
      className={`ix-card rv-card rv-flag${holding ? ' rv-urgent' : ''}${flash ? ' rv-flash' : ''}`}
      aria-label={`Review flag, ${FLAG_LABEL[f.flag_kind] ?? f.flag_kind}${holding ? ', holding up a nomination' : ''}`}
      data-review-id={f.id}
    >
      <header>
        <span className="rv-tag flag">{FLAG_LABEL[f.flag_kind] ?? f.flag_kind}</span>
        {f.status.status === 'confirmed' && <span className="muted"> confirmed by {f.status.by}</span>}
      </header>
      <Bluf className="rv-basis" parts={flagParts(f.flag_kind, plain(f.evidence, now, nameOf))} />
      {holding && <p className="rv-means">{holdingLine(holding)}</p>}
      {f.tracks.length > 0 && (
        <p className="rv-tracks">
          {f.tracks.map((t) => (
            <button key={t} className="ghost mini" onClick={() => onTrack?.(t)} disabled={!onTrack}>
              Open {t}
            </button>
          ))}
        </p>
      )}
      <p className="rv-means">{flagMeans(f)}</p>
      <ActsFold acts={mine} now={now} nameOf={nameOf} />
      <div className="rv-actions">
        <NoteField note={note} setNote={setNote} busy={busy} label="Note for this resolution" placeholder="note (optional, logged)" />
        {f.actions.includes('confirm') && (
          <button className="ap-btn" disabled={busy} onClick={() => void run('the concern stands', () => resolveFlag(f.flag, 'confirmed', note))}>
            The concern stands
          </button>
        )}
        {f.actions.includes('dismiss') && (
          <button className="ap-btn" disabled={busy} onClick={() => void run('dismissed', () => resolveFlag(f.flag, 'dismissed', note))}>
            Dismiss
          </button>
        )}
      </div>
    </article>
  )
}

/** Rules 30 (docs/NODE-LOSS.md section 5, decisions D1 and D2): one of ours presumed lost by
 *  the engine, or silent and flagged by hand. The ask first ("HUNTER 1 has been silent 47 s: is it
 *  lost?"), the evidence, what confirming does (in view: it is the decision), then Confirm lost and,
 *  for a hand flag, Dismiss. Only an operator with engagement authority confirms it (the bridge
 *  refuses anyone else, and every agent), on exactly the silence shown, saying why where nothing
 *  presumes it lost, with the credential again in a credentials run. A presumption is the engine's
 *  and goes when the vehicle is heard again or is confirmed lost. Confirming is a second click: it
 *  asks for the note and the credential first. */
export function NodeLossItem({ n, now, onDone, focusN, nameOf }: { n: ReviewNodeLoss; now: () => number; onDone: Props['onDone']; focusN?: number; nameOf?: Props['nameOf'] }) {
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
  const since = (t: number) => `${fmtDuration(Math.max(0, now() / 1000 - t))} ago`
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
      </header>
      <p className="rv-basis">
        <b>
          {who} has been silent {n.silent_s == null ? 'since it started (no position reported)' : fmtDuration(n.silent_s)}: is it lost?
        </b>
      </p>
      {n.evidence.length > 0 && (
        <ul className="nl-lines" aria-label="Evidence">
          {n.evidence.map((e, i) => (
            <li key={i}>{evidenceLine(e, since(e.t), nameOf)}</li>
          ))}
        </ul>
      )}
      {n.since_t != null && <p className="rv-means">Its last report was {since(n.since_t)}.</p>}
      <ul className="nl-lines" aria-label="What confirming does">
        {confirmEffects(n).map((l, i) => (
          <li key={i}>{l}</li>
        ))}
      </ul>
      {!authority && n.actions.includes('confirm') && <p className="eg-alarm">Only an operator with engagement authority confirms one of ours lost: the bridge will refuse you.</p>}
      {n.refused && <p className="eg-alarm">Not confirmable now: {n.refused}.</p>}
      {(where.length > 0 || n.last_position) && (
        <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
          <summary>Where it was last, and where the keep-outs count it</summary>
          {n.last_position && <p className="ap-where">Last position {n.last_position[1].toFixed(5)}, {n.last_position[0].toFixed(5)}</p>}
          {where.length > 0 && (
            <ul className="nl-lines" aria-label="Where the keep-outs count it">
              {where.map((l) => (
                <li key={l}>{l}</li>
              ))}
            </ul>
          )}
        </details>
      )}
      {open === 'none' && (
        <div className="rv-actions">
          {n.actions.includes('confirm') && (
            <button className="ap-btn" disabled={busy} onClick={() => setOpen('confirm')} title="An operator with engagement authority confirms it">
              Confirm lost
            </button>
          )}
          {n.actions.includes('dismiss') && (
            <button className="ap-btn" disabled={busy} onClick={() => setOpen('dismiss')} title="Take the hand flag off the list (any operator)">
              Dismiss
            </button>
          )}
        </div>
      )}
      {open === 'confirm' && (
        <div className="rv-actions">
          <NoteField
            note={note}
            setNote={setNote}
            busy={busy}
            label="Why it is confirmed lost"
            placeholder={n.needs_note ? 'Why it is lost: nothing presumes it (required, logged)' : 'Note (optional, logged)'}
            required={n.needs_note}
          />
          <button className="ap-btn" disabled={busy || !noteOk || (stepUp && !stepUpReady(step))} onClick={() => void confirm()}>
            Confirm {who} lost
          </button>
          <button className="ghost mini" disabled={busy} onClick={() => setOpen('none')}>
            Cancel
          </button>
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
          <button className="ap-btn" disabled={busy} onClick={() => void dismiss()}>
            Dismiss the flag
          </button>
          <button className="ghost mini" disabled={busy} onClick={() => setOpen('none')}>
            Cancel
          </button>
        </div>
      )}
    </article>
  )
}

/** Rules 30: one of ours confirmed lost, a settled state with a way back: what is true now, what
 *  would bring it back (any report from it, or an operator's revoke, saying why), and the engine's
 *  records behind a fold. Any operator revokes it (it only restricts: it counts in every keep-out
 *  again at once). */
export function NodeLostItem({ n, now, onDone, focusN }: { n: ReviewNodeLost; now: () => number; onDone: Props['onDone']; focusN?: number }) {
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
      <p className="rv-means">
        {n.note ? `"${n.note}". ` : ''}Out of every keep-out; it takes no order. Any report from it revokes the loss.
      </p>
      {(closed.length > 0 || n.from || n.not_held || n.since_t != null) && (
        <details className="ap-fold" onClick={(e) => e.stopPropagation()}>
          <summary>What it closed</summary>
          <p className="ap-where">
            Silent {n.since_t == null ? 'since it started' : `since its report ${fmtDuration(Math.max(0, now() / 1000 - n.since_t))} ago`}.
            {closed.length ? ` Its owed lost-link values (${closed.join(', ')}) closed.` : ''}
            {n.from ? ` Carried over from ${n.from}.` : ''}
            {n.not_held ? ` It holds no vehicle now: ${n.not_held}.` : ''}
          </p>
        </details>
      )}
      {!revoking ? (
        <div className="rv-actions">
          <button className="ap-btn" disabled={busy} onClick={() => setRevoking(true)} title="It counts in every keep-out again at once and takes orders (any operator)">
            Revoke the loss
          </button>
        </div>
      ) : (
        <div className="rv-actions">
          <span className="rv-warn">Revoke the loss of {who}? It counts in every keep-out again at once, and an engagement whose picture left it out stops.</span>
          <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="Why (required, logged)" aria-label="Why the loss is revoked" disabled={busy} />
          <button className="ap-btn" disabled={busy || !note.trim()} onClick={() => void revoke()}>
            Revoke the loss of {who}
          </button>
          <button className="ghost mini" disabled={busy} onClick={() => setRevoking(false)}>
            Cancel
          </button>
        </div>
      )}
    </article>
  )
}

function verdictText(a: ObservedAct, why: (text: string) => string) {
  const v = a.verdict
  if (v.verdict === 'declare') return `declared ${v.track} hostile by rule (attribution ${v.attribution.toFixed(2)})`
  if (v.verdict === 'flag') return `flagged it (${FLAG_LABEL[v.kind] ?? v.kind}): ${why(v.why)}`
  return `recorded: ${why(v.why)}`
}

/** An open flag a later declaration by a person settled (needsYou.ts `settledFlags`): one quiet row
 *  saying what settled it, its own card a click away (or at once when a link points to it), where
 *  the person may still confirm or dismiss it. Nothing here resolves a flag: it stays open in the
 *  engine until a person does. */
export function SettledFlagRow({ f, why, now, onDone, onTrack, focus, nameOf, acts }: { f: ReviewFlag; why: string; now: () => number; onDone: Props['onDone']; onTrack?: Props['onTrack']; focus?: Props['focus']; nameOf?: Props['nameOf']; acts: ObservedAct[] }) {
  const [open, setOpen] = useState(false)
  const [seen, setSeen] = useState<number | null>(null)
  if (focus && focus.id === f.id && focus.n !== seen) {
    setSeen(focus.n)
    if (!open) setOpen(true)
  }
  return (
    <div className="rv-settled-item">
      <p className="rv-settled">
        {FLAG_LABEL[f.flag_kind] ?? f.flag_kind} on {f.tracks.join(', ')}: settled since, {why}.{' '}
        <button className="ghost mini" aria-expanded={open} onClick={() => setOpen((v) => !v)} title="Its card: you may still confirm the concern or dismiss it">
          {open ? 'Fold' : 'Open'}
        </button>
      </p>
      {open && <FlagItem f={f} now={now} onDone={onDone} onTrack={onTrack} focusN={focus?.id === f.id ? focus.n : undefined} nameOf={nameOf} acts={acts} />}
    </div>
  )
}
