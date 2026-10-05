import { useState } from 'react'
import { stepsUpAct } from './auth'
import { fmtDuration, StepUpNeeded, useOperator } from './decisions'
import { type ObservedAct, type Review, type ReviewDeclaration, type ReviewFlag, FLAG_LABEL, confirmDeclaration, resolveFlag, revokeDeclaration } from './ident'

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

function DeclarationItem({ d, now, onDone, onTrack }: { d: ReviewDeclaration; now: () => number; onDone: Props['onDone']; onTrack?: Props['onTrack'] }) {
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState('')
  // revoking takes a second, deliberate click (it stops every engagement in play on the track)
  const [revoking, setRevoking] = useState(false)
  const stepUp = stepsUpAct('confirm_declaration')
  const [password, setPassword] = useState('')
  const [asking, setAsking] = useState(false)
  const run = async (what: string, f: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await f()
      onDone(`${d.track}: ${what}`, 'ok')
      setAsking(false)
      setRevoking(false)
    } catch (e) {
      if (e instanceof StepUpNeeded) setAsking(true)
      onDone(`${d.track}: ${what} refused: ${(e as Error).message}`, 'bad')
    } finally {
      setBusy(false)
      setPassword('')
    }
  }
  const confirm = () => {
    if (stepUp && !password) {
      setAsking(true)
      return
    }
    void run('declaration confirmed', () => confirmDeclaration(d.track, stepUp ? password : undefined))
  }
  return (
    <article className="ix-card rv-card rv-decl" aria-label={`Rule declaration ${d.id} on ${d.track}`}>
      <header>
        <b>{d.track}</b> <span className="rv-tag">BY RULE</span> <span className="muted">{d.id}, {age(now, d.t)}</span>
        {onTrack && (
          <button className="ghost mini" onClick={() => onTrack(d.track)}>
            show
          </button>
        )}
      </header>
      <p className="rv-basis">{d.basis}</p>
      <div className="rv-confirm">
        <button className="rv-ok" disabled={busy} onClick={confirm} title="Confirm the engine's declaration (logged under your session)">
          Confirm {d.track} hostile
        </button>
      </div>
      <div className="rv-revoke">
        {!revoking ? (
          <button className="ghost mini rv-danger" disabled={busy} onClick={() => setRevoking(true)} title="Revoke it: every engagement in play on the track stops first">
            revoke...
          </button>
        ) : (
          <>
            <span className="rv-warn">Revoke {d.id}? Every engagement in play on {d.track} stops first.</span>
            <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="why (optional, logged)" aria-label="Note for the revocation" disabled={busy} />
            <button className="ghost mini rv-danger" disabled={busy} onClick={() => void run('declaration revoked', () => revokeDeclaration(d.track, note))}>
              revoke {d.track}
            </button>
            <button className="ghost mini" disabled={busy} onClick={() => setRevoking(false)}>
              cancel
            </button>
          </>
        )}
      </div>
      {asking && stepUp && (
        <label className="ix-stepup">
          Consequential: your password to confirm
          <input type="password" autoComplete="current-password" value={password} disabled={busy} onChange={(e) => setPassword(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && password && confirm()} aria-label="Your password, to confirm this declaration" autoFocus />
          <button className="ghost mini" disabled={!password || busy} onClick={confirm}>
            confirm
          </button>
        </label>
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
      <p className="rv-basis">{f.evidence}</p>
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
  // a declarer's notice: their declaration revoked by someone else
  const notices = (review?.revocations ?? []).filter((r) => r.notify && r.notify === operator).slice(0, 5)
  const n = decls.length + flags.length
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
