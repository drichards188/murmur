import { type ReactNode, useEffect, useRef, useState } from 'react'
import './tracks.css'
import { type Reveal, scrollSpanIntoView, useFlash } from './reveal'
import {
  AFFILIATION_LABEL,
  SENSOR_LABEL,
  type Track,
  type TracksMessage,
  type LostContact,
  type Affiliation,
  type IdentityLevel,
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
import { callsign, vehicleNamer } from './types'
import type { Intent } from './decisions'
import { trackEngagement, type TrackEngagement } from './trackEngagement'
import { FLAG_LABEL, resolveFlag } from './ident'
import { IdentifyForm } from './IdentifyForm'
import { DeclarationBlock, DeclareControls } from './Declaration'
import type { Blocking } from './blocking'
import { holdingLine, leadLine, positionWords, reasonWords } from './trackCard'
import { Lazy } from './Lazy'
import { agoWhen, plainText } from './plainText'
import { foldTracks } from './exceptions'
import { FOCUS_WORDS, type FocusReason } from './focus'

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
  /** the run's latest intents (newest first): an engagement on a track in play, or its outcome,
   *  said on the track's card (trackEngagement.ts); and a way to open its card in the Inbox */
  intents?: readonly Intent[]
  onOpenEngagement?: (id: number) => void
  /** the review items that alone hold up a nomination (blocking.ts): said on their track's card */
  blocking?: Blocking[]
  /** open flags a later declaration by a person settled (needsYou.ts `settledFlags`): a quiet line
   *  on the card saying so, the flag's answers still there */
  settled?: ReadonlyMap<string, string>
  /** what is in focus (focus.ts) and why: listed first, the rest folded (R6) */
  focus?: ReadonlyMap<string, FocusReason>
  /** the picture is crowded: the list by exception; else every track, as before */
  crowded?: boolean
}

/** The live hostile or suspect tracks, counted: shown only while the list is folded to its header,
 *  standing in for the rows it hides. */
function HotCount({ tracks, now }: { tracks: TracksMessage | null; now: () => number }) {
  const nowMs = now()
  const hot = (tracks?.tracks ?? []).filter((t) => (t.affiliation === 'hostile' || t.affiliation === 'suspect') && !isTrackStale(t, nowMs, tracks?.stale_after_s)).length
  if (!hot) return null
  const title = `${hot} hostile or suspect`
  return (
    <span className="tk-badge hot" title={title} aria-label={title}>
      {hot}
    </span>
  )
}

/** An identity level in a word, after the kind: "(identified)". */
const LEVEL_WORD: Record<IdentityLevel, string> = { identify: 'identified', recognise: 'recognised', detect: 'detected' }

/** The affiliation as the list shows it: hostile and suspect in their colour, the rest plain words
 *  (a ferry's green NEUTRAL said nothing a person acts on). */
function Aff({ a }: { a: Affiliation }) {
  return a === 'hostile' || a === 'suspect' ? <span className={`tk-aff ${a}`}>{AFFILIATION_LABEL[a]}</span> : <span className="tk-aff-plain">{AFFILIATION_LABEL[a].toLowerCase()}</span>
}

/** rows to a chunk, and a row's height in px before any has been drawn */
const CHUNK = 25
const ROW_EST = 56

function pct(x: number) {
  return `${Math.round(100 * x)}%`
}

/** One contact (R6): what it is in words, how specific, its AIS name; then its number, its
 *  affiliation, BY RULE while the engine's declaration waits for a person, and at scale why it is
 *  listed first; its age only once it is old. */
function Row({ t, nowMs, staleAfterS, sel, onClick, why }: { t: Track; nowMs: number; staleAfterS: number; sel: boolean; onClick: () => void; why?: string }) {
  const stale = isTrackStale(t, nowMs, staleAfterS)
  return (
    <button className={`tk-row ${t.affiliation} ${sel ? 'sel' : ''} ${stale ? 'stale' : ''}`} onClick={onClick} aria-expanded={sel} title="Show what this track rests on">
      <span className="tk-sym" dangerouslySetInnerHTML={{ __html: trackSymbolSvg(t, { stale, size: 18, labels: false }).svg }} />
      <span className="tk-main">
        <span className="tk-kind">
          <b>{kindLabel(t.identity.kind)}</b>
          {t.identity.kind != null && t.identity.level ? ` (${LEVEL_WORD[t.identity.level]})` : ''}
          {t.identity.ais ? ` · AIS ${t.identity.ais.name}` : ''}
        </span>
        <span className="tk-name">
          <span className="tk-id">{t.id}</span>
          <Aff a={t.affiliation} />
          {t.declared?.rule && !t.declared.confirmed_by && (
            <span className="vrow-standin tk-rule" title={t.declared.basis ?? `Declared ${t.declared.affiliation} by rule`}>
              BY RULE
            </span>
          )}
          {why && (
            <span className="tk-why-tag" title="Why it is listed first and drawn in full on the map">
              {why}
            </span>
          )}
        </span>
      </span>
      {stale && (
        <span className="tk-side" title="time since the last detection">
          {fmtAge(trackAge(t, nowMs))} ago
        </span>
      )}
    </button>
  )
}

/** A track's card (ledger 1 section 5): one line saying what it is, who says so, how fresh and where
 *  it heads; then its declaration and evidence with their acts (Declaration.tsx: confirm or revoke
 *  the engine's declaration, nominate on a declared hostile), or, undeclared, what deciding it lets
 *  go on and why it is shown so; its review flags, each with its two answers; what it is and how
 *  sure; the evidence in a fold; the acts that fit (fly to, identify, declare by hand). */
function Detail({ t, nowMs, now, staleAfterS, vehicles, onFocus, onEngage, holding, engagement, onOpenEngagement, settled }: { t: Track; nowMs: number; now: () => number; staleAfterS: number; vehicles: Vehicle[]; onFocus: () => void; onEngage?: () => void; holding?: Blocking; engagement?: TrackEngagement | null; onOpenEngagement?: (id: number) => void; settled?: ReadonlyMap<string, string> }) {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  // rules 21: the form that sends a vehicle to identify it (rules 22: while the track is live)
  const [identifying, setIdentifying] = useState(false)
  const live = !isTrackStale(t, nowMs, staleAfterS)
  const who = (uid: number) => {
    const v = vehicles.find((x) => x.uid === uid)
    return v ? callsign(v) : `vehicle ${uid}`
  }
  const act = async (name: string, f: () => Promise<unknown>) => {
    setBusy(true)
    setErr(null)
    try {
      await f()
    } catch (e) {
      setErr(`${name}: ${(e as Error).message}`)
    } finally {
      setBusy(false)
    }
  }
  const threat = t.affiliation === 'hostile' || t.affiliation === 'suspect'
  return (
    <div className="tk-detail">
      <p className={`tk-status ${threat ? 'bad' : ''}`}>{leadLine(t, nowMs / 1000, kindLabel, who)}</p>
      {t.declared ? (
        <DeclarationBlock t={t} vehicles={vehicles} now={now} onEngage={onEngage} where="track" holding={holding} engagement={engagement} onOpenEngagement={onOpenEngagement} />
      ) : (
        <>
          {/* a flag on an act against ours, nothing declared: what the track's declaration would let go on */}
          {holding && (
            <p className="dc-holding" role="note">
              {holdingLine(holding)}
            </p>
          )}
          <p className={`tk-reason ${threat ? 'bad' : ''}`}>
            {AFFILIATION_LABEL[t.affiliation]} by estimate ({pct(t.affiliation_confidence)}): {reasonWords(t.affiliation_reason, kindLabel)}
          </p>
        </>
      )}
      {(t.review?.length ?? 0) > 0 && (
        <div className="tk-flags">
          {t.review?.map((f) => {
            const done = settled?.get(`F-${f.flag}`)
            return (
              <div key={f.flag} className={`tk-flag${done ? ' settled' : ''}`}>
                <b>
                  Review flag: {FLAG_LABEL[f.kind] ?? f.kind}
                  {f.status === 'confirmed' ? ' (the concern stands)' : ''}
                </b>{' '}
                {plainText(f.evidence, { nameOf: vehicleNamer(vehicles), when: agoWhen(nowMs) })}
                {done && <span className="tk-settled"> Settled since: {done}.</span>}
                <div className="dc-actions">
                  {f.status === 'open' && (
                    <button type="button" className="dc-btn" disabled={busy} onClick={() => void act('confirm flag', () => resolveFlag(f.flag, 'confirmed'))} title="Logged under your session; the track is not declared by this">
                      The concern stands
                    </button>
                  )}
                  <button type="button" className="dc-btn" disabled={busy} onClick={() => void act('dismiss flag', () => resolveFlag(f.flag, 'dismissed'))} title="Logged under your session">
                    Dismiss the concern
                  </button>
                </div>
              </div>
            )
          })}
        </div>
      )}
      <dl className="tk-why">
        <dt>Identity</dt>
        <dd>
          {kindWithLevel(t.identity)} ({pct(t.identity.confidence)})
        </dd>
        <dt>AIS</dt>
        <dd>{t.identity.ais ? `${t.identity.ais.name}, ${t.identity.ais.ship_type.replace(/_/g, ' ')}` : 'none received'}</dd>
        {t.identity.conflict && (
          <>
            <dt>Conflict</dt>
            <dd className="warn">{t.identity.conflict}</dd>
          </>
        )}
        <dt>Position</dt>
        <dd>{positionWords(trackSigma(t, nowMs))}</dd>
      </dl>
      <details className="tk-evidence">
        <summary>Evidence</summary>
        <dl>
          <dt>Seen by</dt>
          <dd>
            {/* one line a vehicle of ours, its sensors and detections */}
            {[...new Set(t.sources.map((s) => s.uid))].map((uid) => (
              <span className="tk-src" key={uid}>
                <b>{who(uid)}</b>{' '}
                {t.sources
                  .filter((s) => s.uid === uid)
                  .map((s) => (
                    <span key={s.sensor} title={`${s.hits} detections, last ${fmtAge(Math.max(0, nowMs / 1000 - s.last_t))} ago`}>
                      {SENSOR_LABEL[s.sensor]} ×{s.hits}
                    </span>
                  ))
                  .reduce<ReactNode[]>((acc, el, k) => (k ? [...acc, ' · ', el] : [el]), [])}
              </span>
            ))}
          </dd>
          {t.identity.supported_by.length > 0 && (
            <>
              <dt>Identity by</dt>
              <dd>{t.identity.supported_by.map((s) => SENSOR_LABEL[s]).join(', ')}</dd>
            </>
          )}
          {t.identity.ais && (
            <>
              <dt>MMSI</dt>
              <dd>{t.identity.ais.mmsi}</dd>
            </>
          )}
          <dt>Detections</dt>
          <dd>
            {t.hits} since {fmtAge(nowMs / 1000 - t.first_seen_t)} ago
          </dd>
          <dt>Confidence</dt>
          <dd>track {pct(trackConfidence(t, nowMs))} (decays while unseen)</dd>
          <dt>Uncertainty</dt>
          <dd>
            1 sigma {trackSigma(t, nowMs).toFixed(0)} m now ({t.pos_sigma_m.toFixed(0)} m at the last fix, growing {t.sigma_growth_mps.toFixed(1)} m/s)
          </dd>
        </dl>
      </details>
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
      </div>
      {identifying && live && <IdentifyForm track={t.id} vehicles={vehicles} onClose={() => setIdentifying(false)} />}
      <DeclareControls t={t} />
      {err && <p className="tk-err">{err}</p>}
    </div>
  )
}

export function TracksPanel({ tracks, vehicles, now, selected, onSelect, onFocus, collapsed, onCollapse, reveal = null, onEngage, intents, onOpenEngagement, blocking, settled, focus, crowded = false }: Props) {
  const nowMs = now()
  const staleAfterS = tracks?.stale_after_s ?? 10
  const list = sortTracks(tracks?.tracks ?? [])
  // R6: what is in focus first (with why), the rest folded by affiliation and kind; few tracks as before
  const reason = (id: string) => {
    const r = focus?.get(id)
    return r && r !== 'hovered' ? r : id === selected ? 'selected' : null
  }
  const listing = foldTracks(list, (t) => reason(t.id) != null, crowded)
  const [openFolds, setOpenFolds] = useState<ReadonlySet<string>>(() => new Set())
  const toggleFold = (key: string) =>
    setOpenFolds((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })
  const foldOpen = (key: string) => openFolds.has(key)
  const lost = tracks?.lost ?? []
  const lostPinned = crowded ? lost.filter((l) => reason(l.id) != null) : lost
  const lostFolded = crowded ? lost.filter((l) => reason(l.id) == null) : []
  const lostOpen = foldOpen('lost')
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
  /** Rows in chunks, each built only near the view (Lazy): a Lazy a row was most of what the list
   *  cost to render with hundreds of tracks. In focus: each says why it is listed first. */
  const rows = (ts: Track[], first: boolean) =>
    Array.from({ length: Math.ceil(ts.length / CHUNK) }, (_, i) => ts.slice(i * CHUNK, (i + 1) * CHUNK)).map((chunk) => (
      <Lazy key={chunk[0].id} className="tk-chunk" estimate={chunk.length * (ROW_EST + 4)} force={first || chunk.some((t) => t.id === selected || t.id === flash || t.id === reveal?.ids[0])}>
        {() =>
          chunk.map((t) => {
            const why = first ? reason(t.id) : null
            return (
              <div key={t.id} data-track-id={t.id} data-tutorial="track-row" className={flash === t.id ? 'tk-flash' : undefined}>
                <Row t={t} nowMs={nowMs} staleAfterS={staleAfterS} sel={selected === t.id} onClick={() => onSelect(selected === t.id ? null : t.id)} why={crowded && why ? FOCUS_WORDS[why] : undefined} />
                {selected === t.id && <Detail t={t} nowMs={nowMs} now={now} staleAfterS={staleAfterS} vehicles={vehicles} onFocus={() => onFocus(t)} onEngage={onEngage ? () => onEngage(t.id) : undefined} engagement={trackEngagement(intents, t.id, vehicleNamer(vehicles))} onOpenEngagement={onOpenEngagement} holding={(blocking ?? []).find((b) => b.track === t.id)} settled={settled} />}
              </div>
            )
          })
        }
      </Lazy>
    ))
  /** a dropped track every keep-out still counts: what it was, then how long lost */
  const lostRow = (l: LostContact) => (
    <p key={l.id} className="tk-lost-row" title={`Counted within ${Math.round(Math.max(0, ...l.regions.map((r) => r.radius_m)))} m of where it could be now`}>
      <b>{kindLabel(l.identity)}</b>, {l.affiliation.replace(/_/g, ' ')}
      {l.ais ? ` (AIS ${l.ais.name})` : ''} <span className="tk-id">{l.id}</span>: lost {fmtAge(l.out_of_view_s)}; still in keep-outs
    </p>
  )
  /** "130 unknown · kept in every keep-out" */
  const lostSummary = (ls: LostContact[]) => {
    const by = new Map<string, number>()
    for (const l of ls) by.set(l.affiliation, (by.get(l.affiliation) ?? 0) + 1)
    return `${[...by].map(([a, n]) => `${n} ${a.replace(/_/g, ' ')}`).join(' · ')} · each still in every keep-out`
  }
  return (
    <aside className={`tk ${collapsed ? 'collapsed' : ''}`} aria-label="Tracks" data-tutorial="tracks">
      <header className="tk-top">
        <h2>Tracks</h2>
        {collapsed && <HotCount tracks={tracks} now={now} />}
        <button className="ghost mini close" onClick={onCollapse} aria-expanded={!collapsed} aria-label={collapsed ? 'Expand tracks' : 'Collapse tracks'} title={collapsed ? 'Expand tracks' : 'Collapse tracks'}>
          {collapsed ? '▾' : '▴'}
        </button>
      </header>
      {!collapsed && (
      <div className="tk-scroll" ref={box}>
        {list.length === 0 && lost.length === 0 && <p className="empty">No contacts yet</p>}
        {listing.pinned.length > 0 && (
          <h3 className="tk-sec" title="Selected, hostile or suspect, declared, flagged, a review item's, an engagement's target or blocker: drawn in full on the map">
            In focus <span>{listing.pinned.length}</span>
          </h3>
        )}
        {rows(listing.pinned, true)}
        {listing.folds.map((f) => (
          <div key={f.key} className="tk-fold-wrap">
            <button className={`tk-fold ${f.affiliation}`} aria-expanded={foldOpen(f.key)} onClick={() => toggleFold(f.key)} title={foldOpen(f.key) ? 'Fold these tracks back into one row' : `List these ${f.tracks.length} tracks one by one`}>
              <span className="tk-fold-n">{f.tracks.length}</span>
              <span className="tk-fold-main">
                <span>
                  <b>{kindLabel(f.kind)}</b> {f.affiliation in AFFILIATION_LABEL ? <Aff a={f.affiliation as Affiliation} /> : f.affiliation}
                </span>
              </span>
              <span className="tk-fold-caret" aria-hidden>
                {foldOpen(f.key) ? '▾' : '▸'}
              </span>
            </button>
            {foldOpen(f.key) && rows(f.tracks, false)}
          </div>
        ))}
        {rows(listing.loose, false)}
        {lost.length > 0 && (
          <section className="tk-lost" aria-label="Lost contacts">
            {lostPinned.map(lostRow)}
            {lostFolded.length > 0 && (
              <button className="tk-fold lost" aria-expanded={lostOpen} onClick={() => toggleFold('lost')} title={lostOpen ? 'Fold the lost contacts back into one row' : 'List every lost contact out of focus'}>
                <span className="tk-fold-n">{lostFolded.length}</span>
                <span className="tk-fold-main">
                  <span>
                    <b>lost contacts</b> out of focus
                  </span>
                  <span className="tk-fold-levels">{lostSummary(lostFolded)}</span>
                </span>
                <span className="tk-fold-caret" aria-hidden>
                  {lostOpen ? '▾' : '▸'}
                </span>
              </button>
            )}
            {lostOpen && lostFolded.map(lostRow)}
          </section>
        )}
      </div>
      )}
    </aside>
  )
}
