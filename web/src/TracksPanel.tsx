import { useEffect, useRef, useState } from 'react'
import './tracks.css'
import { type Reveal, scrollSpanIntoView, useFlash } from './reveal'
import {
  AFFILIATION_LABEL,
  type Affiliation,
  LEVEL_LABEL,
  SENSOR_LABEL,
  type Track,
  type TracksMessage,
  declareTrack,
  fmtAge,
  isTrackStale,
  kindLabel,
  kindWithLevel,
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
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import { FLAG_LABEL, confirmDeclaration, resolveFlag, revokeDeclaration } from './ident'
import { IdentifyForm } from './IdentifyForm'

/** The tracks section, pinned above the task panel (Phase C2): every contact our sensors hold, ranked hostile, suspect,
 *  unknown first; click a row for what the estimate rests on (sources, confidence, age,
 *  identity and its conflicts) and to declare an affiliation. Everything shown is
 *  perception: the bridge never tells the console what a contact really is. Rules 9: a
 *  declaration may be the engine's (its hostile-act rule, on an observed act); the panel
 *  says so, with its evidence, and any operator confirms or revokes it here or in the
 *  inbox's Review. */

interface Props {
  tracks: TracksMessage | null
  vehicles: Vehicle[]
  /** bridge clock, ms */
  now: () => number
  selected: string | null
  onSelect: (id: string | null) => void
  /** fly the map to a track */
  onFocus: (t: Track) => void
  /** collapsed to its header, so tasking gets the room */
  collapsed: boolean
  onCollapse: () => void
  /** a track just selected from outside the list (map, inbox): scroll to it and mark it */
  reveal?: Reveal | null
  /** open the engagement nomination form for a declared-hostile track (Phase E) */
  onEngage?: (trackId: string) => void
}

/** hot, unknown and other counts of the live tracks, as badges */
export function TrackBadges({ tracks, now }: { tracks: TracksMessage | null; now: () => number }) {
  const list = tracks?.tracks ?? []
  const nowMs = now()
  const live = list.filter((t) => !isTrackStale(t, nowMs, tracks?.stale_after_s))
  const hot = live.filter((t) => t.affiliation === 'hostile' || t.affiliation === 'suspect').length
  const unk = live.filter((t) => t.affiliation === 'unknown').length
  const rest = live.length - hot - unk
  const title = `${list.length} track${list.length === 1 ? '' : 's'}: ${hot} hostile or suspect, ${unk} unknown, ${rest} neutral or friendly`
  return (
    <span className="tk-badges" title={title} aria-label={title}>
      {hot > 0 && <span className="tk-badge hot">{hot}</span>}
      {unk > 0 && <span className="tk-badge unk">{unk}</span>}
      {rest > 0 && <span className="tk-badge n">{rest}</span>}
    </span>
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
          {t.declared &&
            (t.declared.rule && !t.declared.confirmed_by ? (
              <span className="vrow-standin tk-rule" title={t.declared.basis ?? `Declared ${t.declared.affiliation} by rule`}>
                BY RULE
              </span>
            ) : (
              <span className="vrow-standin" title={t.declared.basis ?? `Declared ${t.declared.affiliation} by ${t.declared.by}`}>
                DECLARED
              </span>
            ))}
          {(t.review?.length ?? 0) > 0 && (
            <span className="vrow-standin tk-review" title={t.review?.map((f) => `F-${f.flag}: ${f.evidence}`).join('\n')}>
              REVIEW
            </span>
          )}
        </span>
        <span className="tk-kind">
          <b>{kindLabel(t.identity.kind)}</b>
          {t.identity.kind != null && t.identity.level ? ` (${LEVEL_LABEL[t.identity.level]})` : ''}
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

function Detail({ t, nowMs, staleAfterS, vehicles, onFocus, onEngage }: { t: Track; nowMs: number; staleAfterS: number; vehicles: Vehicle[]; onFocus: () => void; onEngage?: () => void }) {
  const [busy, setBusy] = useState<Affiliation | null>(null)
  const [err, setErr] = useState<string | null>(null)
  // rules 8: when the policy lists declare_hostile, a hostile declaration takes the
  // operator's password again (a step-up); asked for here, sent once, never kept
  const stepUp = stepsUpAct('declare_hostile')
  // rules 9: confirming a rule declaration steps up only where the policy lists it (off by
  // default: the user chose minimal approval)
  const confirmStepUp = stepsUpAct('confirm_declaration')
  const [confirming, setConfirming] = useState(false)
  // revoking takes a second, deliberate click (it stops every engagement in play on the track);
  // so does confirming (the permissive act is never the easier one)
  const [revoking, setRevoking] = useState(false)
  const [confirmOpen, setConfirmOpen] = useState(false)
  const [asking, setAsking] = useState(false)
  // rules 21: the form that sends a vehicle to identify it (rules 22: while the track is live)
  const [identifying, setIdentifying] = useState(false)
  const live = !isTrackStale(t, nowMs, staleAfterS)
  // rules 10: the password again, or the passkey (where the run asks for one)
  const [step, setStep] = useStepUp()
  const [stepUpError, setStepUpError] = useState(false)
  const age = trackAge(t, nowMs)
  const sigma = trackSigma(t, nowMs)
  const who = (uid: number) => {
    const v = vehicles.find((x) => x.uid === uid)
    return v ? callsign(v) : `uid ${uid}`
  }
  const declare = async (a: Affiliation) => {
    if (a === 'hostile' && stepUp && !stepUpReady(step)) {
      setAsking(true)
      return
    }
    setBusy(a)
    setErr(null)
    setStepUpError(false)
    // a hostile declaration on a track with one flag in force answers that flag
    const flag = a === 'hostile' && t.review?.length === 1 ? t.review[0].flag : undefined
    try {
      const proof = a === 'hostile' && stepUp ? await stepUpProof(step, { act: 'declare_hostile', track: t.id }) : {}
      setStep({ password: '' })
      await declareTrack(t.id, a, undefined, proof.password, flag, proof.passkey)
      setAsking(false)
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      if (e instanceof StepUpNeeded) setAsking(true)
      setErr((e as Error).message)
    } finally {
      setBusy(null)
    }
  }
  const act = async (name: string, f: () => Promise<unknown>) => {
    setBusy('unknown')
    setErr(null)
    try {
      await f()
      return true
    } catch (e) {
      if (e instanceof StepUpNeeded) setConfirming(true)
      setErr(`${name}: ${(e as Error).message}`)
      return false
    } finally {
      setBusy(null)
    }
  }
  const confirm = async () => {
    if (confirmStepUp && !stepUpReady(step)) {
      setConfirming(true)
      return
    }
    const ok = await act('confirm', async () => {
      const proof = confirmStepUp ? await stepUpProof(step, { act: 'confirm_declaration', track: t.id }) : {}
      setStep({ password: '' })
      return confirmDeclaration(t.id, proof)
    })
    if (ok) setConfirming(false)
  }
  const unconfirmed = t.declared?.rule && !t.declared.confirmed_by
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
          {kindWithLevel(t.identity)} ({pct(t.identity.confidence)})
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
              {t.declared.basis ? (
                <span className={unconfirmed ? 'warn' : ''}>{t.declared.basis}</span>
              ) : (
                <>
                  {AFFILIATION_LABEL[t.declared.affiliation]} by {t.declared.by}
                </>
              )}
              {t.declared.note ? `: ${t.declared.note}` : ''}
              {unconfirmed && !revoking && (
                <span className="tk-decl-actions">
                  {!confirmOpen ? (
                    <button className="ghost mini" disabled={busy != null} onClick={() => setConfirmOpen(true)} title="Confirm the engine's declaration on its basis above (any operator; logged under your session)">
                      confirm...
                    </button>
                  ) : (
                    <>
                      <span className="rv-warn tk-plain">Confirm {t.id} hostile on the basis above?</span>
                      <button className="ghost mini" disabled={busy != null} onClick={() => void confirm().then(() => setConfirmOpen(false))}>
                        confirm {t.id}
                      </button>
                      <button className="ghost mini" disabled={busy != null} onClick={() => setConfirmOpen(false)}>
                        cancel
                      </button>
                    </>
                  )}
                </span>
              )}
              <span className="tk-decl-actions tk-revoke">
                {!revoking ? (
                  <button className="ghost mini rv-danger" disabled={busy != null} onClick={() => setRevoking(true)} title="Revoke this declaration (any operator): every engagement in play on the track stops first, with its fallback">
                    revoke...
                  </button>
                ) : (
                  <>
                    <span className="rv-warn">Revoke it? Every engagement in play on {t.id} stops first.</span>
                    <button
                      className="ghost mini rv-danger"
                      disabled={busy != null}
                      onClick={() => void act('revoke', () => revokeDeclaration(t.id)).then(() => setRevoking(false))}
                    >
                      revoke {t.id}
                    </button>
                    <button className="ghost mini" disabled={busy != null} onClick={() => setRevoking(false)}>
                      cancel
                    </button>
                  </>
                )}
              </span>
            </dd>
          </>
        )}
        {(t.review?.length ?? 0) > 0 && (
          <>
            <dt>Review</dt>
            <dd>
              {t.review?.map((f) => (
                <div key={f.flag} className="tk-flag">
                  <b>
                    F-{f.flag} {FLAG_LABEL[f.kind] ?? f.kind}
                    {f.status === 'confirmed' ? ' (confirmed)' : ''}
                  </b>
                  : {f.evidence}
                  <span className="tk-decl-actions">
                    {f.status === 'open' && (
                      <button className="ghost mini" disabled={busy != null} onClick={() => void act('confirm flag', () => resolveFlag(f.flag, 'confirmed'))} title="The concern stands (logged under your session); the track is not declared by this">
                        confirm
                      </button>
                    )}
                    <button className="ghost mini" disabled={busy != null} onClick={() => void act('dismiss flag', () => resolveFlag(f.flag, 'dismissed'))} title="Dismiss the concern (logged under your session)">
                      dismiss
                    </button>
                  </span>
                </div>
              ))}
            </dd>
          </>
        )}
      </dl>
      <div className="tk-actions">
        <button className="ghost mini" onClick={onFocus}>
          fly to
        </button>
        {/* rules 22: any live track, an identified one for a second look by our own sensors */}
        {live && (
          <button
            className="ghost mini"
            onClick={() => setIdentifying((v) => !v)}
            title={`Send a vehicle to identify this track: it orbits the track at its camera's standoff, following it, until its own sensors identify it${t.identity.level === 'identify' ? ' again (saying whether that agrees with what it is now)' : ''}, it is lost or out of time, then goes back to what it was doing`}
          >
            identify...
          </button>
        )}
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
        <div className="tk-stepup">
          <StepUpField
            passwordLabel={`Consequential: your password to declare ${t.id} hostile`}
            purpose={`to declare ${t.id} hostile`}
            ariaLabel="Your password, to declare this track hostile"
            value={step}
            onChange={setStep}
            busy={busy != null}
            bad={stepUpError}
            onEnter={() => void declare('hostile')}
          />
          <button className="ghost mini hostile" disabled={!stepUpReady(step) || busy != null} onClick={() => void declare('hostile')}>
            declare hostile
          </button>
        </div>
      )}
      {confirming && confirmStepUp && (
        <div className="tk-stepup">
          <StepUpField
            passwordLabel={`Consequential: your password to confirm the engine's declaration of ${t.id}`}
            purpose={`to confirm the engine's declaration of ${t.id}`}
            ariaLabel="Your password, to confirm this declaration"
            value={step}
            onChange={setStep}
            busy={busy != null}
            onEnter={() => void confirm()}
          />
          <button className="ghost mini" disabled={!stepUpReady(step) || busy != null} onClick={() => void confirm()}>
            confirm
          </button>
        </div>
      )}
      {identifying && live && <IdentifyForm track={t.id} vehicles={vehicles} onClose={() => setIdentifying(false)} />}
      {err && <p className="tk-err">{err}</p>}
    </div>
  )
}

export function TracksPanel({ tracks, vehicles, now, selected, onSelect, onFocus, collapsed, onCollapse, reveal = null, onEngage }: Props) {
  const nowMs = now()
  const staleAfterS = tracks?.stale_after_s ?? 10
  const list = sortTracks(tracks?.tracks ?? [])
  const stale = list.filter((t) => isTrackStale(t, nowMs, staleAfterS)).length
  const box = useRef<HTMLDivElement>(null)
  const flash = useFlash(reveal, (ids) => ids[0] ?? null)
  useEffect(() => {
    if (!reveal || collapsed) return
    const el = box.current
    const item = el?.querySelector(`[data-track-id="${CSS.escape(reveal.ids[0] ?? '')}"]`)
    if (!el || !item) return
    // the row and its expanded detail if they fit, else the row at the top
    const at = el.getBoundingClientRect().top - el.scrollTop
    const top = item.getBoundingClientRect().top - at
    scrollSpanIntoView(el, top, Math.min(item.getBoundingClientRect().height, el.clientHeight))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reveal, collapsed])
  return (
    <aside className={`tk ${collapsed ? 'collapsed' : ''}`} aria-label="Tracks">
      <header className="tk-top">
        <h2>Tracks</h2>
        <TrackBadges tracks={tracks} now={now} />
        <span className="muted">
          {list.length} held{stale ? `, ${stale} stale` : ''}
        </span>
        <button className="ghost mini close" onClick={onCollapse} aria-expanded={!collapsed} aria-label={collapsed ? 'Expand tracks' : 'Collapse tracks'} title={collapsed ? 'Expand tracks' : 'Collapse tracks'}>
          {collapsed ? '▾' : '▴'}
        </button>
      </header>
      {!collapsed && (
      <div className="tk-scroll" ref={box}>
        {list.length === 0 ? (
          <p className="empty">
            No tracks. Contacts appear here as our platforms' sensors detect them (radar, EO/IR, AIS, sonar, ESM per <code>sim/platforms.toml</code>); start a scenario with <code>MURMUR_CONTACTS=sim/scenarios/golden-gate.toml</code>.
          </p>
        ) : (
          <p className="tk-note">Symbols fade and their uncertainty ring grows with time since the last detection. Hostile takes a declaration: an operator&apos;s, or the engine&apos;s rule on a hostile act our sensors observed (marked BY RULE until an operator confirms it). Proximity alone never declares. From our sensors only.</p>
        )}
        {!!tracks?.lost?.length && <p className="tk-note">Red rings on the map show the possible area of a lost hostile contact. They remain in engagement keep-out checks after the contact leaves sensor view.</p>}
        {list.map((t) => (
          <div key={t.id} data-track-id={t.id} className={flash === t.id ? 'tk-flash' : undefined}>
            <Row t={t} nowMs={nowMs} staleAfterS={staleAfterS} sel={selected === t.id} onClick={() => onSelect(selected === t.id ? null : t.id)} />
            {selected === t.id && <Detail t={t} nowMs={nowMs} staleAfterS={staleAfterS} vehicles={vehicles} onFocus={() => onFocus(t)} onEngage={onEngage ? () => onEngage(t.id) : undefined} />}
          </div>
        ))}
        {(tracks?.lost ?? []).length > 0 && (
          <section className="tk-lost" aria-label="Lost contacts">
            <p className="tk-note">
              Red possible-area rings mark lost contacts: tracks the tracker dropped (no detection for 180 s). Every engagement keep-out still counts each, anywhere it could have reached since its
              last fix at its domain's top speed, until an operator confirms a track carrying its AIS as it (the Review list) or an operator with engagement authority
              accepts it for one engagement.
            </p>
            {(tracks?.lost ?? []).map((l) => (
              <p key={l.id} className="tk-lost-row">
                <b>{l.id}</b> {kindLabel(l.identity)}, {l.affiliation.replace(/_/g, " ")}
                {l.ais ? ` (AIS ${l.ais.name})` : ""}: out of view {fmtAge(l.out_of_view_s)}, counted within{" "}
                {Math.round(Math.max(0, ...l.regions.map((r) => r.radius_m)))} m of where it could be now
              </p>
            ))}
          </section>
        )}
      </div>
      )}
    </aside>
  )
}
