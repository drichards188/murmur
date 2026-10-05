import { useEffect, useState } from 'react'
import './tracks.css'
import {
  AFFILIATION_LABEL,
  type Affiliation,
  SENSOR_LABEL,
  type Track,
  type TracksMessage,
  declareTrack,
  fmtAge,
  isTrackStale,
  kindLabel,
  sortTracks,
  trackAge,
  trackConfidence,
  trackSigma,
  trackSymbolSvg,
} from './tracks'
import type { Vehicle } from './types'
import { callsign } from './types'
import { stepsUpAct } from './auth'
import { StepUpNeeded } from './decisions'

/** The tracks drawer (Phase C2): every contact our sensors hold, ranked hostile, suspect,
 *  unknown first; click a row for what the estimate rests on (sources, confidence, age,
 *  identity and its conflicts) and to declare an affiliation. Everything shown is
 *  perception: the bridge never tells the console what a contact really is. */

interface Props {
  tracks: TracksMessage | null
  vehicles: Vehicle[]
  /** bridge clock, ms */
  now: () => number
  selected: string | null
  onSelect: (id: string | null) => void
  /** fly the map to a track */
  onFocus: (t: Track) => void
  onClose: () => void
  /** open the engagement nomination form for a declared-hostile track (Phase E) */
  onEngage?: (trackId: string) => void
}

export function TracksButton({ tracks, open, now, onClick }: { tracks: TracksMessage | null; open: boolean; now: () => number; onClick: () => void }) {
  const list = tracks?.tracks ?? []
  const nowMs = now()
  const live = list.filter((t) => !isTrackStale(t, nowMs, tracks?.stale_after_s))
  const hot = live.filter((t) => t.affiliation === 'hostile' || t.affiliation === 'suspect').length
  const unk = live.filter((t) => t.affiliation === 'unknown').length
  const rest = live.length - hot - unk
  const title = `${list.length} track${list.length === 1 ? '' : 's'}: ${hot} hostile or suspect, ${unk} unknown, ${rest} neutral or friendly`
  return (
    <button className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} onClick={onClick} title={title} aria-label={`Tracks: ${title}`}>
      Tracks
      {hot > 0 && <span className="tk-badge hot">{hot}</span>}
      {unk > 0 && <span className="tk-badge unk">{unk}</span>}
      {rest > 0 && <span className="tk-badge n">{rest}</span>}
    </button>
  )
}

function pct(x: number) {
  return `${Math.round(100 * x)}%`
}

function Row({ t, nowMs, staleAfterS, sel, onClick }: { t: Track; nowMs: number; staleAfterS: number; sel: boolean; onClick: () => void }) {
  const stale = isTrackStale(t, nowMs, staleAfterS)
  const age = trackAge(t, nowMs)
  const conf = trackConfidence(t, nowMs)
  return (
    <button className={`tk-row ${t.affiliation} ${sel ? 'sel' : ''} ${stale ? 'stale' : ''}`} onClick={onClick} aria-expanded={sel} title="Show what this track rests on">
      <span className="tk-sym" dangerouslySetInnerHTML={{ __html: trackSymbolSvg(t, { stale, size: 18, labels: false }).svg }} />
      <span className="tk-main">
        <span className="tk-name">
          {t.id}
          <span className={`tk-aff ${t.affiliation}`}>{AFFILIATION_LABEL[t.affiliation]}</span>
          {t.declared && <span className="vrow-standin" title={`Declared ${t.declared.affiliation} by ${t.declared.by}`}>DECLARED</span>}
        </span>
        <span className="tk-kind">
          <b>{kindLabel(t.identity.kind)}</b>
          {t.identity.ais ? ` · AIS ${t.identity.ais.name}` : ''}
          {t.identity.conflict ? ' · conflict' : ''}
          {` · ${t.sources.map((s) => SENSOR_LABEL[s.sensor]).filter((v, i, a) => a.indexOf(v) === i).join(', ')}`}
        </span>
      </span>
      <span className="tk-side">
        <span className={`age ${stale ? 'old' : ''}`} title="time since the last detection">
          {stale ? `${fmtAge(age)} ago` : 'live'}
        </span>
        <span className="tk-conf" title={`track confidence ${pct(conf)}`}>
          <i style={{ width: pct(conf) }} />
        </span>
        <span className="tk-conf id" title={`identity confidence ${pct(t.identity.confidence)}`}>
          <i style={{ width: pct(t.identity.confidence) }} />
        </span>
      </span>
    </button>
  )
}

const DECLARABLE: Affiliation[] = ['hostile', 'suspect', 'neutral', 'unknown']

function Detail({ t, nowMs, vehicles, onFocus, onEngage }: { t: Track; nowMs: number; vehicles: Vehicle[]; onFocus: () => void; onEngage?: () => void }) {
  const [busy, setBusy] = useState<Affiliation | null>(null)
  const [err, setErr] = useState<string | null>(null)
  // rules 7: when the policy lists declare_hostile, a hostile declaration takes the
  // operator's password again (a step-up); asked for here, sent once, never kept
  const stepUp = stepsUpAct('declare_hostile')
  const [asking, setAsking] = useState(false)
  const [password, setPassword] = useState('')
  const [stepUpError, setStepUpError] = useState(false)
  const age = trackAge(t, nowMs)
  const sigma = trackSigma(t, nowMs)
  const who = (uid: number) => {
    const v = vehicles.find((x) => x.uid === uid)
    return v ? callsign(v) : `uid ${uid}`
  }
  const declare = async (a: Affiliation) => {
    if (a === 'hostile' && stepUp && !password) {
      setAsking(true)
      return
    }
    setBusy(a)
    setErr(null)
    setStepUpError(false)
    const pw = a === 'hostile' && stepUp ? password : undefined
    setPassword('')
    try {
      await declareTrack(t.id, a, undefined, pw)
      setAsking(false)
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      if (e instanceof StepUpNeeded) setAsking(true)
      setErr((e as Error).message)
    } finally {
      setBusy(null)
    }
  }
  return (
    <div className="tk-detail">
      <dl>
        <dt>Affiliation</dt>
        <dd>
          {AFFILIATION_LABEL[t.affiliation]} ({pct(t.affiliation_confidence)})
          <br />
          <span className={t.affiliation === 'suspect' || t.affiliation === 'hostile' ? 'bad' : ''}>{t.affiliation_reason}</span>
        </dd>
        <dt>Identity</dt>
        <dd>
          {kindLabel(t.identity.kind)} ({pct(t.identity.confidence)})
          {t.identity.supported_by.length ? ` by ${t.identity.supported_by.map((s) => SENSOR_LABEL[s]).join(', ')}` : ''}
          {t.identity.ais && (
            <>
              <br />
              AIS {t.identity.ais.name} · MMSI {t.identity.ais.mmsi} · {t.identity.ais.ship_type}
            </>
          )}
          {t.identity.conflict && (
            <>
              <br />
              <span className="warn">{t.identity.conflict}</span>
            </>
          )}
        </dd>
        <dt>Last seen</dt>
        <dd>
          {fmtAge(age)} ago · {t.hits} detection{t.hits === 1 ? '' : 's'} since {fmtAge(nowMs / 1000 - t.first_seen_t)} ago
        </dd>
        <dt>Confidence</dt>
        <dd>
          track {pct(trackConfidence(t, nowMs))} (decays while unseen)
        </dd>
        <dt>Uncertainty</dt>
        <dd>
          1 sigma {sigma.toFixed(0)} m now ({t.pos_sigma_m.toFixed(0)} m at the last fix, growing {t.sigma_growth_mps.toFixed(1)} m/s)
        </dd>
        <dt>Motion</dt>
        <dd>
          {t.course_deg != null ? `${t.course_deg.toFixed(0)}° at ` : ''}
          {t.speed_mps.toFixed(1)} m/s
          {t.alt_m != null && t.domain !== 'surface' ? ` · ${t.domain === 'subsurface' ? `${Math.max(0, -t.alt_m).toFixed(0)} m depth` : `${t.alt_m.toFixed(0)} m`}` : ''}
        </dd>
        <dt>Sources</dt>
        <dd>
          {t.sources.map((s) => (
            <span className="tk-src" key={`${s.uid}-${s.sensor}`} title={`${s.hits} detections, last ${fmtAge(Math.max(0, nowMs / 1000 - s.last_t))} ago`}>
              {who(s.uid)} {SENSOR_LABEL[s.sensor]} ×{s.hits}
            </span>
          ))}
        </dd>
        {t.declared && (
          <>
            <dt>Declared</dt>
            <dd>
              {AFFILIATION_LABEL[t.declared.affiliation]} by {t.declared.by}
              {t.declared.note ? `: ${t.declared.note}` : ''}
            </dd>
          </>
        )}
      </dl>
      <div className="tk-actions">
        <button className="ghost mini" onClick={onFocus}>
          fly to
        </button>
        {onEngage && t.affiliation === 'hostile' && t.declared && (
          <button className="ghost mini eg-engage-btn" onClick={onEngage} title="Nominate an engagement on this declared-hostile track (two operators with engagement authority decide)">
            engage...
          </button>
        )}
        <span className="lbl">declare</span>
        {DECLARABLE.map((a) => (
          <button key={a} className={`ghost mini ${a} ${t.declared?.affiliation === a ? 'on' : ''}`} disabled={busy != null} onClick={() => declare(a)} title={`Declare this track ${AFFILIATION_LABEL[a].toLowerCase()} (an operator action, logged)`}>
            {AFFILIATION_LABEL[a].toLowerCase()}
          </button>
        ))}
      </div>
      {asking && (
        <label className={`ix-stepup ${stepUpError ? 'bad' : ''}`}>
          Consequential: your password to declare {t.id} hostile
          <input
            type="password"
            autoComplete="current-password"
            value={password}
            disabled={busy != null}
            onChange={(e) => setPassword(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && password && busy == null && void declare('hostile')}
            aria-label="Your password, to declare this track hostile"
            autoFocus
          />
          <button className="ghost mini hostile" disabled={!password || busy != null} onClick={() => void declare('hostile')}>
            declare hostile
          </button>
        </label>
      )}
      {err && <p className="tk-err">{err}</p>}
    </div>
  )
}

export function TracksPanel({ tracks, vehicles, now, selected, onSelect, onFocus, onClose, onEngage }: Props) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])
  const nowMs = now()
  const staleAfterS = tracks?.stale_after_s ?? 10
  const list = sortTracks(tracks?.tracks ?? [])
  const stale = list.filter((t) => isTrackStale(t, nowMs, staleAfterS)).length
  return (
    <aside className="tk" aria-label="Tracks">
      <header className="tk-top">
        <h2>Tracks</h2>
        <span className="muted">
          {list.length} held{stale ? `, ${stale} stale` : ''} · from our sensors only
        </span>
        <button className="ghost mini close" onClick={onClose} aria-label="Close tracks">
          ✕
        </button>
      </header>
      <div className="tk-scroll">
        {list.length === 0 ? (
          <p className="empty">
            No tracks. Contacts appear here as our platforms' sensors detect them (radar, EO/IR, AIS, sonar, ESM per <code>sim/platforms.toml</code>); start a scenario with <code>MURMUR_CONTACTS=sim/scenarios/golden-gate.toml</code>.
          </p>
        ) : (
          <p className="tk-note">Symbols fade and their uncertainty ring grows with time since the last detection. Hostile is never inferred: it takes a declaration.</p>
        )}
        {list.map((t) => (
          <div key={t.id}>
            <Row t={t} nowMs={nowMs} staleAfterS={staleAfterS} sel={selected === t.id} onClick={() => onSelect(selected === t.id ? null : t.id)} />
            {selected === t.id && <Detail t={t} nowMs={nowMs} vehicles={vehicles} onFocus={() => onFocus(t)} onEngage={onEngage ? () => onEngage(t.id) : undefined} />}
          </div>
        ))}
      </div>
    </aside>
  )
}
