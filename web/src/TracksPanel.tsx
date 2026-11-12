import { type ReactNode, useEffect, useRef, useState } from 'react'
import './tracks.css'
import { type Reveal, scrollSpanIntoView, useFlash } from './reveal'
import {
  AFFILIATION_LABEL,
  LEVEL_LABEL,
  SENSOR_LABEL,
  type Track,
  type TracksMessage,
  type LostContact,
  type Affiliation,
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
import { FLAG_LABEL, resolveFlag } from './ident'
import { IdentifyForm } from './IdentifyForm'
import { DeclarationBlock, DeclareControls } from './Declaration'
import { type Blocking, waited } from './blocking'
import { Lazy } from './Lazy'
import { foldTracks, type TrackFold } from './exceptions'
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
  /** the review items that alone hold up a nomination (blocking.ts): said on their track's card */
  blocking?: Blocking[]
  /** tracks the person pinned to be drawn in full on the map (focus.ts), and the toggle */
  pinned?: ReadonlySet<string>
  onPin?: (id: string) => void
  /** what is in focus (focus.ts) and why: listed first, the rest folded (R6) */
  focus?: ReadonlyMap<string, FocusReason>
  /** the picture is crowded: the list by exception; else every track, as before */
  crowded?: boolean
  /** the map legend's Show all for lists: every fold open */
  expandAll?: boolean
  /** how many tracks and lost contacts the folds hold (the map legend counts them) */
  onFolded?: (n: { tracks: number; folds: number }) => void
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

/** rows to a chunk, and a row's height in px before any has been drawn */
const CHUNK = 25
const ROW_EST = 56

function pct(x: number) {
  return `${Math.round(100 * x)}%`
}

function Row({ t, nowMs, staleAfterS, sel, onClick, why }: { t: Track; nowMs: number; staleAfterS: number; sel: boolean; onClick: () => void; why?: string }) {
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
          {why && (
            <span className="tk-why-tag" title="Why it is listed first and drawn in full on the map">
              {why}
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

/** A track's card: what it is and why it is shown so (its declaration and the evidence first, then
 *  the estimate's), then the acts that fit (Declaration.tsx: confirm or revoke the engine's
 *  declaration, nominate on a declared hostile, declare by hand), then the track's numbers. */
function Detail({ t, nowMs, now, staleAfterS, vehicles, onFocus, onEngage, holding, pinned, onPin }: { t: Track; nowMs: number; now: () => number; staleAfterS: number; vehicles: Vehicle[]; onFocus: () => void; onEngage?: () => void; holding?: Blocking; pinned?: boolean; onPin?: () => void }) {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  // rules 21: the form that sends a vehicle to identify it (rules 22: while the track is live)
  const [identifying, setIdentifying] = useState(false)
  const live = !isTrackStale(t, nowMs, staleAfterS)
  const age = trackAge(t, nowMs)
  const sigma = trackSigma(t, nowMs)
  const who = (uid: number) => {
    const v = vehicles.find((x) => x.uid === uid)
    return v ? callsign(v) : `uid ${uid}`
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
  const aff = AFFILIATION_LABEL[t.affiliation].toLowerCase()
  return (
    <div className="tk-detail">
      {t.declared ? (
        <DeclarationBlock t={t} vehicles={vehicles} now={now} onEngage={onEngage} where="track" holding={holding} />
      ) : (
        <p className={`tk-status ${t.affiliation === 'suspect' ? 'bad' : ''}`}>
          Not declared: {aff} by estimate ({pct(t.affiliation_confidence)})
        </p>
      )}
      {/* a flag on an act against ours, nothing declared: what the track's declaration would let go on */}
      {!t.declared && holding && (
        <p className="dc-holding" role="note">
          <b>Waiting on you {waited(holding, nowMs)}</b>
          {holding.id}: {holding.waits_for}. {holding.text} {holding.unblocks}
        </p>
      )}
      <dl className="tk-why">
        {/* declared, the reason is the declaration's basis, which the block above words */}
        {!t.declared && (
          <>
            <dt>Estimate</dt>
            <dd className={t.affiliation === 'suspect' || t.affiliation === 'hostile' ? 'bad' : ''}>{t.affiliation_reason.replace(/_/g, ' ')}</dd>
          </>
        )}
        <dt>Identity</dt>
        <dd>
          {kindWithLevel(t.identity)} ({pct(t.identity.confidence)})
          {t.identity.supported_by.length ? `, by ${t.identity.supported_by.map((s) => SENSOR_LABEL[s]).join(', ')}` : ''}
        </dd>
        <dt>AIS</dt>
        <dd>{t.identity.ais ? `${t.identity.ais.name}, MMSI ${t.identity.ais.mmsi}, ${t.identity.ais.ship_type}` : 'none received'}</dd>
        {t.identity.conflict && (
          <>
            <dt>Conflict</dt>
            <dd className="warn">{t.identity.conflict}</dd>
          </>
        )}
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
      </dl>
      {(t.review?.length ?? 0) > 0 && (
        <div className="tk-flags">
          {t.review?.map((f) => (
            <div key={f.flag} className="tk-flag">
              <b>
                Review F-{f.flag}: {FLAG_LABEL[f.kind] ?? f.kind}
                {f.status === 'confirmed' ? ' (confirmed)' : ''}
              </b>{' '}
              {f.evidence}
              <span className="tk-decl-actions">
                {f.status === 'open' && (
                  <button className="ghost mini" disabled={busy} onClick={() => void act('confirm flag', () => resolveFlag(f.flag, 'confirmed'))} title="The concern stands (logged under your session); the track is not declared by this">
                    confirm
                  </button>
                )}
                <button className="ghost mini" disabled={busy} onClick={() => void act('dismiss flag', () => resolveFlag(f.flag, 'dismissed'))} title="Dismiss the concern (logged under your session)">
                  dismiss
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
      <div className="tk-actions">
        <button className="ghost mini" onClick={onFocus}>
          fly to
        </button>
        {onPin && (
          <button
            className={`ghost mini${pinned ? ' on' : ''}`}
            aria-pressed={!!pinned}
            onClick={onPin}
            title={pinned ? 'Stop drawing this track in full on the map' : 'Draw this track in full on the map (its ring filled, its column, its label first) while it is pinned: a view setting of this console only'}
          >
            {pinned ? 'unpin' : 'pin on map'}
          </button>
        )}
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
      <dl className="tk-facts">
        <dt>Last seen</dt>
        <dd>
          {fmtAge(age)} ago · {t.hits} detection{t.hits === 1 ? '' : 's'} since {fmtAge(nowMs / 1000 - t.first_seen_t)} ago
        </dd>
        <dt>Confidence</dt>
        <dd>track {pct(trackConfidence(t, nowMs))} (decays while unseen)</dd>
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
      </dl>
      {err && <p className="tk-err">{err}</p>}
    </div>
  )
}

/** "identified 372 · recognised 39": a fold's identity levels, the most specific first */
function levelLine(f: TrackFold<Track>) {
  const order: [string, string][] = [['identify', 'identified'], ['recognise', 'recognised'], ['detect', 'detected'], ['', 'no level']]
  return order.flatMap(([k, w]) => (f.levels[k] ? [`${w} ${f.levels[k]}`] : [])).join(' · ')
}

export function TracksPanel({ tracks, vehicles, now, selected, onSelect, onFocus, collapsed, onCollapse, reveal = null, onEngage, blocking, pinned, onPin, focus, crowded = false, expandAll = false, onFolded }: Props) {
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
  const foldOpen = (key: string) => expandAll || openFolds.has(key)
  // the help paragraphs: behind a "?" while the list is by exception, else as before
  const [help, setHelp] = useState(false)
  const showHelp = !crowded || help
  const lost = tracks?.lost ?? []
  const lostPinned = crowded ? lost.filter((l) => reason(l.id) != null) : lost
  const lostFolded = crowded ? lost.filter((l) => reason(l.id) == null) : []
  const lostOpen = foldOpen('lost')
  const foldedTracks = listing.folds.reduce((n, f) => n + (foldOpen(f.key) ? 0 : f.tracks.length), 0) + (lostOpen ? 0 : lostFolded.length)
  const foldedRows = listing.folds.filter((f) => !foldOpen(f.key)).length + (lostFolded.length && !lostOpen ? 1 : 0)
  const foldedRef = useRef(onFolded)
  foldedRef.current = onFolded
  useEffect(() => {
    foldedRef.current?.({ tracks: foldedTracks, folds: foldedRows })
  }, [foldedTracks, foldedRows])
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
                {selected === t.id && <Detail t={t} nowMs={nowMs} now={now} staleAfterS={staleAfterS} vehicles={vehicles} onFocus={() => onFocus(t)} onEngage={onEngage ? () => onEngage(t.id) : undefined} holding={(blocking ?? []).find((b) => b.track === t.id)} pinned={pinned?.has(t.id)} onPin={onPin ? () => onPin(t.id) : undefined} />}
              </div>
            )
          })
        }
      </Lazy>
    ))
  const lostRow = (l: LostContact) => (
    <p key={l.id} className="tk-lost-row">
      <b>{l.id}</b> {kindLabel(l.identity)}, {l.affiliation.replace(/_/g, ' ')}
      {l.ais ? ` (AIS ${l.ais.name})` : ''}: out of view {fmtAge(l.out_of_view_s)}, counted within {Math.round(Math.max(0, ...l.regions.map((r) => r.radius_m)))} m of where it could be now
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
        <TrackBadges tracks={tracks} now={now} />
        <span className="muted">
          {list.length} held{stale ? `, ${stale} stale` : ''}
        </span>
        {crowded && (
          <button className="ghost mini tk-help" aria-pressed={help} onClick={() => setHelp((h) => !h)} title="What the symbols, rings and BY RULE mean">
            ?
          </button>
        )}
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
          showHelp && <p className="tk-note">Symbols fade and their uncertainty ring grows with time since the last detection. Hostile takes a declaration: an operator&apos;s, or the engine&apos;s rule on a hostile act our sensors observed (marked BY RULE until an operator confirms it). Proximity alone never declares. From our sensors only.{crowded ? ' Listed first: what is in focus, and why; the rest folded by affiliation and kind (a count, not a grouping by place), each fold one click from its tracks.' : ''}</p>
        )}
        {showHelp && !!tracks?.lost?.length && <p className="tk-note">Red rings on the map show the possible area of a lost hostile contact. They remain in engagement keep-out checks after the contact leaves sensor view.</p>}
        {listing.pinned.length > 0 && (
          <h3 className="tk-sec" title="Selected, pinned, hostile or suspect, declared, flagged, a review item's, an engagement's target or blocker: drawn in full on the map">
            In focus <span>{listing.pinned.length}</span>
          </h3>
        )}
        {rows(listing.pinned, true)}
        {listing.folds.map((f) => (
          <div key={f.key} className="tk-fold-wrap">
            <button className={`tk-fold ${f.affiliation}`} aria-expanded={foldOpen(f.key)} onClick={() => toggleFold(f.key)} disabled={expandAll} title={foldOpen(f.key) ? 'Fold these tracks back into one row' : `List these ${f.tracks.length} tracks one by one`}>
              <span className="tk-fold-n">{f.tracks.length}</span>
              <span className="tk-fold-main">
                <span>
                  <span className={`tk-aff ${f.affiliation}`}>{AFFILIATION_LABEL[f.affiliation as Affiliation] ?? f.affiliation}</span> <b>{kindLabel(f.kind)}</b>
                </span>
                <span className="tk-fold-levels">{levelLine(f)}</span>
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
            {showHelp && (
              <p className="tk-note">
                Red possible-area rings mark lost contacts: tracks the tracker dropped (no detection for 180 s). Every engagement keep-out still counts each, anywhere it could have reached since its
                last fix at its domain's top speed, until an operator confirms a track carrying its AIS as it (the Review list) or an operator with engagement authority
                accepts it for one engagement.
              </p>
            )}
            {lostPinned.map(lostRow)}
            {lostFolded.length > 0 && (
              <button className="tk-fold lost" aria-expanded={lostOpen} onClick={() => toggleFold('lost')} disabled={expandAll} title={lostOpen ? 'Fold the lost contacts back into one row' : 'List every lost contact out of focus'}>
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
