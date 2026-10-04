import { useEffect, useMemo, useState } from 'react'
import type { Check } from './decisions'
import { useOperator } from './decisions'
import { engageOptions, FALLBACK_LABEL, fmtRel, nominate, type EngageOption, type EngagePreview, type Fallback } from './engage'
import './engage.css'
import type { TracksMessage } from './tracks'
import { kindLabel } from './tracks'
import type { Vehicle } from './types'
import { callsign } from './types'

/** The nomination form (Phase E): pair a declared-hostile track with one armed platform for
 *  an abstract effect (a point, a radius, a time). The platform list comes from the bridge's
 *  dry run (`POST /api/engage/options`: every check and the preview on the world now, nothing
 *  created), reachable platforms first. Nominating puts it in the inbox, where two operators
 *  with engagement authority (never the proposer) decide it. */

interface Props {
  tracks: TracksMessage | null
  vehicles: Vehicle[]
  now: () => number
  /** what opened the form: a track (from the tracks drawer) or a platform (an armed vehicle) */
  initial: { track?: string; uid?: number }
  /** the selected option's preview, to draw on the map */
  onPreview: (p: EngagePreview | null) => void
  onNominated: (intentId: number) => void
  onClose: () => void
}

const FALLBACKS: Fallback[] = ['return', 'loiter', 'abort']

function firstFail(checks: Check[]) {
  return checks.find((c) => c.outcome !== 'pass')
}

export function EngagePanel({ tracks, vehicles, now, initial, onPreview, onNominated, onClose }: Props) {
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
  const [showAll, setShowAll] = useState(false)
  const [tick, setTick] = useState(0)

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
          if (uid == null) {
            const first = o.find((x) => x.passed)
            if (first) setUid(first.uid)
          }
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

  const t = (tracks?.tracks ?? []).find((x) => x.id === track)
  const reachable = (options ?? []).filter((o) => o.passed)
  const others = (options ?? []).filter((o) => !o.passed)
  const nameOf = (u: number) => {
    const v = vehicles.find((x) => x.uid === u)
    return v ? `${callsign(v)} (${v.identity?.short ?? v.identity?.platform ?? ''})` : `vehicle ${u}`
  }

  const submit = async () => {
    if (!track || uid == null) return
    setBusy(true)
    setError(null)
    try {
      const i = await nominate({ track, uid, radius, windowS, fallback, rationale })
      onNominated(i.id)
    } catch (e) {
      setError((e as Error).message)
    }
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
                {h.id} {kindLabel(h.identity.kind)} (declared hostile by {h.declared?.by})
              </option>
            ))}
          </select>
        </label>
        {t && !(t.affiliation === 'hostile' && t.declared) && (
          <p className="eg-hint">
            {t.id} is {t.affiliation} by estimate. Hostile is never inferred: an operator must declare it (Tracks drawer) before it can be engaged.
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
          Armed platforms {options ? `(${reachable.length} can take it now)` : '(checking...)'}
        </span>
        {reachable.map((o) => (
          <button key={o.uid} className={`eg-opt ok ${uid === o.uid ? 'on' : ''}`} onClick={() => setUid(o.uid)}>
            <b>{nameOf(o.uid)}</b>
            {o.preview && (
              <span>
                effect {fmtRel(o.preview.effect_t - now())}, {(o.preview.distance_m / 1000).toFixed(2)} km at {o.preview.cruise_mps.toFixed(0)} m/s
              </span>
            )}
          </button>
        ))}
        {others.length > 0 && (
          <button className="ix-toggle" onClick={() => setShowAll(!showAll)}>
            {showAll ? 'Hide' : 'Show'} {others.length} that can't
          </button>
        )}
        {showAll &&
          others.map((o) => {
            const f = firstFail(o.checks)
            return (
              <button key={o.uid} className={`eg-opt bad ${uid === o.uid ? 'on' : ''}`} onClick={() => setUid(o.uid)}>
                <b>{nameOf(o.uid)}</b>
                <span>{f ? `${f.rule}: ${f.detail}` : 'some check did not pass'}</span>
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
              .filter((c) => c.outcome !== 'pass' || ['target_declared', 'keep_out_tracks', 'keep_out_friendly', 'no_strike', 'engagement_zone', 'reach'].includes(c.rule))
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

      <label className="eg-rationale">
        Rationale (optional for an operator; shown to the approvers)
        <textarea rows={2} value={rationale} maxLength={500} onChange={(e) => setRationale(e.target.value)} />
      </label>
      <footer className="ix-actions">
        <button className="ghost" onClick={onClose}>
          Cancel
        </button>
        <button className="eg-nominate" disabled={busy || !track || uid == null} onClick={submit} title="Creates a nomination in the inbox; nothing moves until two approvers sign">
          Nominate
        </button>
      </footer>
      {error && <p className="error ix-error">{error}</p>}
    </aside>
  )
}
