import { useEffect, useMemo, useState } from 'react'
import { stepsUpAct } from './auth'
import type { Check } from './decisions'
import { StepUpNeeded, useOperator } from './decisions'
import { acceptLostBody, engageOptions, FALLBACK_LABEL, fmtDist, fmtRel, KEEP_OUT_KIND_LABEL, nominate, type EngageOption, type EngagePreview, type Fallback, type KeepOutExplanation, type KeepOutLost } from './engage'
import './engage.css'
import { KeepOutExplain } from './KeepOutExplain'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import type { TracksMessage } from './tracks'
import { kindLabel } from './tracks'
import type { Vehicle } from './types'
import { callsign } from './types'

/** The nomination form (Phase E): pair a declared-hostile track with one armed platform for
 *  an abstract effect (a point, a radius, a time). The platform list comes from the bridge's
 *  dry run (`POST /api/engage/options`: every check and the preview on the world now, nothing
 *  created), listed by uid: no ranking, no recommendation, nothing preselected; the operator
 *  picks. Nominating puts it in the inbox, where two operators with engagement authority
 *  (never the proposer) decide it. */

interface Props {
  tracks: TracksMessage | null
  vehicles: Vehicle[]
  now: () => number
  /** what opened the form: a track (from the tracks drawer) or a platform (an armed vehicle) */
  initial: { track?: string; uid?: number }
  /** the selected option's preview, to draw on the map */
  onPreview: (p: EngagePreview | null) => void
  /** murmur-12e: the selected option's keep-out explanation, to draw its regions on the map */
  onKeepOuts: (ex: KeepOutExplanation | null) => void
  onNominated: (intentId: number) => void
  onClose: () => void
}

const FALLBACKS: Fallback[] = ['return', 'loiter', 'abort']

function firstFail(checks: Check[]) {
  return checks.find((c) => c.outcome !== 'pass')
}

/** One line for an option a keep-out blocks: the worst item and where the platform would
 *  let every keep-out hold, on the picture now. */
function keepOutLine(o: EngageOption) {
  const ex = o.keep_outs
  const worst = ex?.items.find((i) => i.blocks)
  if (!ex?.blocked || !worst) return null
  const kind = worst.region ? KEEP_OUT_KIND_LABEL[worst.region.kind] : 'cannot be placed'
  const short = worst.short_m != null ? `, short ${fmtDist(worst.short_m)}` : ''
  const mw = ex.move_within
  const band = mw?.windows.find((w) => w.from_m != null && w.until_m != null)
  const where =
    mw?.within_m != null
      ? `; every keep-out holds within ${fmtDist(mw.within_m)} (on the picture now)`
      : band && band.from_m != null && band.until_m != null
        ? `; every keep-out holds ${fmtDist(band.from_m)} to ${fmtDist(band.until_m)} out (on the picture now)`
        : ''
  return `keep-out: ${worst.label} ${kind}${short}${where}`
}

export function EngagePanel({ tracks, vehicles, now, initial, onPreview, onKeepOuts, onNominated, onClose }: Props) {
  const [operator] = useOperator()
  const hostile = useMemo(() => (tracks?.tracks ?? []).filter((t) => t.affiliation === 'hostile' && t.declared), [tracks])
  const [track, setTrack] = useState<string>(initial.track ?? hostile[0]?.id ?? '')
  const [radius, setRadius] = useState(75)
  const [windowS, setWindowS] = useState(600)
  const [fallback, setFallback] = useState<Fallback>('return')
  const [options, setOptions] = useState<EngageOption[] | null>(null)
  const [uid, setUid] = useState<number | null>(initial.uid ?? null)
  const [rationale, setRationale] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // why the bridge refused the nomination itself (kept until the next try: the dry run's
  // refresh clears only its own error)
  const [refused, setRefused] = useState<string | null>(null)
  const [tick, setTick] = useState(0)
  // rules 16: the lost contacts the operator accepts with the nomination (by track id)
  const [acceptLost, setAcceptLost] = useState<Record<string, KeepOutLost>>({})
  const onAcceptLost = (l: KeepOutLost, on: boolean) =>
    setAcceptLost((cur) => {
      const next = { ...cur }
      if (on) next[l.track] = l
      else delete next[l.track]
      return next
    })
  // what was accepted goes with the target and the platform it was shown for: a new choice
  // starts with none ticked
  useEffect(() => setAcceptLost({}), [track, uid])
  // rules 8 and 10: in a credentials run whose policy lists accept_unknown, accepting a lost
  // contact takes the operator's step-up (the password again, or their passkey), as accepting a
  // lost vehicle of ours does; the bridge refuses the nomination without it
  const accepting = Object.keys(acceptLost).length > 0
  const stepUp = accepting && stepsUpAct('accept_unknown')
  const [step, setStep] = useStepUp()
  const [stepBad, setStepBad] = useState(false)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  // the dry run, again when the parameters change and every few seconds (the picture moves)
  useEffect(() => {
    if (!track) {
      setOptions(null)
      return
    }
    let gone = false
    const id = window.setTimeout(() => {
      engageOptions(track, radius, windowS, fallback)
        .then((o) => {
          if (gone) return
          setOptions(o)
          setError(null)
        })
        .catch((e) => !gone && setError((e as Error).message))
    }, 250)
    const again = window.setTimeout(() => setTick((n) => n + 1), 4000)
    return () => {
      gone = true
      window.clearTimeout(id)
      window.clearTimeout(again)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [track, radius, windowS, fallback, tick])

  const chosen = options?.find((o) => o.uid === uid) ?? null
  useEffect(() => {
    onPreview(chosen?.preview ?? null)
  }, [chosen, onPreview])
  useEffect(() => () => onPreview(null), [onPreview])
  useEffect(() => {
    onKeepOuts(chosen?.keep_outs ?? null)
  }, [chosen, onKeepOuts])
  useEffect(() => () => onKeepOuts(null), [onKeepOuts])

  const t = (tracks?.tracks ?? []).find((x) => x.id === track)
  const reachable = (options ?? []).filter((o) => o.passed)
  const nameOf = (u: number) => {
    const v = vehicles.find((x) => x.uid === u)
    return v ? `${callsign(v)} (${v.identity?.short ?? v.identity?.platform ?? ''})` : `vehicle ${u}`
  }

  const submit = async () => {
    if (!track || uid == null) return
    if (stepUp && !stepUpReady(step)) {
      setStepBad(true)
      return
    }
    setBusy(true)
    setRefused(null)
    try {
      const list = Object.values(acceptLost)
      const proof = stepUp ? await stepUpProof(step, { act: 'accept_unknown', intent: null, accept_lost: acceptLostBody(list) }) : undefined
      const i = await nominate({ track, uid, radius, windowS, fallback, rationale, acceptLost: list, proof })
      setStepBad(false)
      onNominated(i.id)
    } catch (e) {
      setStepBad(e instanceof StepUpNeeded)
      setRefused((e as Error).message)
    }
    setStep({ password: '' })
    setBusy(false)
  }

  return (
    <aside className="eg-panel" aria-label="Nominate an engagement">
      <header className="eg-top">
        <h2>Nominate an engagement</h2>
        <button className="ghost mini close" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>
      <p className="eg-abstract">
        An abstract effect: a point, a radius, a time. As {operator} you nominate; two operators with engagement authority decide, never you. Every check runs again at each signature, and the picture they approve is held until the effect.
      </p>
      <div className="eg-form">
        <label>
          Track
          <select value={track} onChange={(e) => setTrack(e.target.value)}>
            {!hostile.some((h) => h.id === track) && <option value={track}>{track || 'choose a declared-hostile track'}</option>}
            {hostile.map((h) => (
              <option key={h.id} value={h.id}>
                {h.id} {kindLabel(h.identity.kind)} (declared hostile by{' '}
                {h.declared?.rule ? (h.declared.confirmed_by ? `rule, confirmed by ${h.declared.confirmed_by}` : 'rule, not confirmed') : h.declared?.by})
              </option>
            ))}
          </select>
        </label>
        {t && !(t.affiliation === 'hostile' && t.declared) && (
          <p className="eg-hint">
            {t.id} is {t.affiliation} by estimate. The tracker never infers hostile: an operator declares it (Tracks drawer), or the engine does by rule on a hostile act our sensors observed, before it can be engaged.
          </p>
        )}
        {!hostile.length && <p className="eg-hint">No track is declared hostile. Declare one in the Tracks drawer first.</p>}
        <div className="eg-row">
          <label>
            Effect radius (m)
            <input type="number" min={10} max={250} step={5} value={radius} onChange={(e) => setRadius(Math.max(1, Number(e.target.value) || 0))} />
          </label>
          <label>
            Window (s)
            <input type="number" min={30} max={3600} step={30} value={windowS} onChange={(e) => setWindowS(Math.max(30, Number(e.target.value) || 0))} />
          </label>
        </div>
        <fieldset className="eg-fallbacks">
          <legend>If it stops before the effect (abort, void, link loss)</legend>
          {FALLBACKS.map((f) => (
            <label key={f}>
              <input type="radio" name="fallback" checked={fallback === f} onChange={() => setFallback(f)} />
              {FALLBACK_LABEL[f]}
            </label>
          ))}
        </fieldset>
      </div>

      <section className="eg-options">
        <span className="ix-label">
          Armed platforms {options ? `(${reachable.length} of ${options.length} pass every check now; listed by uid, not ranked)` : '(checking...)'}
        </span>
        {uid == null && options && options.length > 0 && <p className="eg-hint">Pick a platform: nothing is chosen for you.</p>}
        {(options ?? []).map((o) => {
          const f = firstFail(o.checks)
          const ko = keepOutLine(o)
          return (
            <button key={o.uid} className={`eg-opt ${o.passed ? 'ok' : 'bad'} ${uid === o.uid ? 'on' : ''}`} onClick={() => setUid(o.uid)}>
              <b>{nameOf(o.uid)}</b>
              {o.passed && o.preview ? (
                <span>
                  effect {fmtRel(o.preview.effect_t - now())}, {(o.preview.distance_m / 1000).toFixed(2)} km at {o.preview.cruise_mps.toFixed(0)} m/s
                </span>
              ) : (
                <span>{f ? `${f.rule}: ${f.detail}` : 'some check did not pass'}</span>
              )}
              {ko && <span className="ko-sum">{ko}</span>}
            </button>
          )
        })}
        {options && !options.length && <p className="eg-hint">No armed platform in the fleet.</p>}
      </section>

      {chosen && (
        <section className="eg-sec">
          <span className="ix-label">Checks for {nameOf(chosen.uid)} now</span>
          <div className="eg-checks">
            {chosen.checks
              .filter((c) => c.outcome !== 'pass' || ['target_declared', 'keep_out_tracks', 'keep_out_friendly', 'no_strike', 'engagement_zone', 'platform_free', 'reach'].includes(c.rule))
              .map((c, k) => (
                <div key={k} className={`ix-check ${c.outcome}`}>
                  <span className="ix-mark">{c.outcome.toUpperCase()}</span>
                  <span className="ix-text">
                    <span className="ix-rule">{c.rule}</span>
                    <span className="ix-detail">{c.detail}</span>
                  </span>
                </div>
              ))}
          </div>
        </section>
      )}
      {chosen?.keep_outs && (
        <KeepOutExplain ex={chosen.keep_outs} name={nameOf(chosen.uid)} now={now} acceptLost={acceptLost} onAcceptLost={onAcceptLost} />
      )}
      {Object.keys(acceptLost).length > 0 && (
        <p className="eg-alarm" role="status">
          You accept {Object.keys(acceptLost).join(', ')} as lost for this engagement: logged under your session with the nomination, shown to every signer, and
          the checks leave {Object.keys(acceptLost).length > 1 ? 'them' : 'it'} out while out of view since that fix (the dry run above does not).
        </p>
      )}
      {stepUp && (
        <div className="tk-stepup eg-stepup">
          <StepUpField
            passwordLabel={`Consequential: your password to accept ${Object.keys(acceptLost).join(', ')} as lost`}
            purpose={`to accept ${Object.keys(acceptLost).join(', ')} as lost for this engagement`}
            ariaLabel="Your password, to accept the lost contacts"
            value={step}
            onChange={(s) => {
              setStep(s)
              setStepBad(false)
              setRefused(null)
            }}
            busy={busy}
            bad={stepBad}
            onEnter={() => void submit()}
          />
        </div>
      )}

      <label className="eg-rationale">
        Rationale (optional for an operator; shown to the approvers)
        <textarea rows={2} value={rationale} maxLength={500} onChange={(e) => setRationale(e.target.value)} />
      </label>
      <footer className="ix-actions">
        <button className="ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="eg-nominate" disabled={busy || !track || uid == null || (stepUp && !stepUpReady(step))} onClick={submit} title="Creates a nomination in the inbox; nothing moves until two approvers sign">
          Nominate
        </button>
      </footer>
      {refused && <p className="error ix-error">Not nominated: {refused}</p>}
      {error && <p className="error ix-error">{error}</p>}
    </aside>
  )
}
