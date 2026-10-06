import { Fragment, useEffect, useMemo, useState, type ReactNode } from 'react'
import type { Intent, SpaceRun, SpaceRuns } from './decisions'
import { fmtZ, hasProduct, scenarioAt, submitIntent } from './decisions'
import type { AccessWindow, AreaInfo, Catalog, Gnss, Links, Passes, SatClassInfo, TaskingResult, TaskOption, TaskQuery } from './space'
import { classColor, dayLabel, hhmm, offsetLabel, productTime, requestOptions, usePoll, useScenarioNow } from './space'
import { ProductList, SpaceRunCard } from './SpaceRuns'
import type { LngLat } from './types'

/** What the panel highlights on the map and globe. */
export interface SpaceSelection {
  scene: { ring: LngLat[]; label: string } | null
  norad: number | null
}

interface Props {
  catalog: Catalog | null
  catalogError: string | null
  /** current theater map view, w,s,e,n */
  mapBounds: [number, number, number, number] | null
  box: LngLat[] | null
  drawing: boolean
  onDrawBox: () => void
  onClearBox: () => void
  onSelect: (s: SpaceSelection) => void
  onClose: () => void
  /** tasked collects and cover watches (the bridge's space runs) and the bridge clock */
  space?: SpaceRuns
  bridgeNow: () => number
  /** open an intent in the inbox or Decisions */
  onIntent?: (id: number) => void
}

type Item = { kind: 'pass'; w: AccessWindow } | { kind: 'option'; o: TaskOption }

const SPANS = [3, 6, 12, 24]
const GAP_MIN_S = 20 * 60
const GROUPS: { cap: 'optical' | 'sar'; title: string }[] = [
  { cap: 'optical', title: 'Optical' },
  { cap: 'sar', title: 'SAR' },
]
const REASONS: Record<string, string> = {
  night: 'night',
  off_nadir: 'off-nadir',
  incidence: 'incidence',
  too_soon: 'too soon (lead time)',
  product_too_late: 'product too late',
  resolution_too_coarse: 'resolution too coarse',
  not_taskable: 'not taskable',
}

const TEN_MIN = 600_000

/** Merge [start, end] intervals (ms) and return the uncovered parts of [from, to]. */
function gaps(intervals: [number, number][], from: number, to: number) {
  const sorted = [...intervals].sort((a, b) => a[0] - b[0])
  const out: [number, number][] = []
  let cursor = from
  for (const [s, e] of sorted) {
    if (s > cursor) out.push([cursor, Math.min(s, to)])
    cursor = Math.max(cursor, e)
    if (cursor >= to) break
  }
  if (cursor < to) out.push([cursor, to])
  return out.filter(([s, e]) => e - s >= GAP_MIN_S * 1000)
}

function deg(x: number) {
  return `${x.toFixed(1)}°`
}

type Tab = 'details' | 'request' | 'coverage' | 'products'

export function SpacePanel({ catalog, catalogError, mapBounds, box, drawing, onDrawBox, onClearBox, onSelect, onClose, space, bridgeNow, onIntent }: Props) {
  const [spanH, setSpanH] = useState(6)
  const [panH, setPanH] = useState(0)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [item, setItem] = useState<Item | null>(null)
  const [tab, setTab] = useState<Tab>('request')
  const [runSel, setRunSel] = useState<number | null>(null)
  const runs = space?.runs ?? []

  const area = catalog?.area ?? null
  const off = area?.utc_offset_h ?? 0
  const now = useScenarioNow(catalog)
  // the window starts at the last 10-minute mark (stable URLs, so the bridge's cache hits)
  const base = now == null ? null : Math.floor(now / TEN_MIN) * TEN_MIN + panH * 3_600_000
  const from = base == null ? null : new Date(base).toISOString()
  const to = base == null ? null : new Date(base + spanH * 3_600_000).toISOString()
  const q = from && to ? `from=${from}&to=${to}` : null
  const passes = usePoll<Passes>(q && `/api/space/passes?${q}`, 60_000)
  const links = usePoll<Links>(q && `/api/space/links?${q}&windows=0&step=60`, 120_000)
  const gnss = usePoll<Gnss>(q && `/api/space/gnss?${q}&step=${Math.max(300, (spanH * 3600) / 24)}`, 120_000)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && !drawing && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose, drawing])

  const classes = useMemo(() => new Map((catalog?.classes ?? []).map((c) => [c.id, c])), [catalog])
  const p = passes.data
  const t0 = p ? Date.parse(p.from) : (base ?? 0)
  const t1 = p ? Date.parse(p.to) : t0 + spanH * 3_600_000
  const pct = (t: number) => `${(((t - t0) / (t1 - t0)) * 100).toFixed(3)}%`
  const span = (a: number, b: number) => ({
    left: pct(Math.max(a, t0)),
    width: `${((Math.max(0, Math.min(b, t1) - Math.max(a, t0)) / (t1 - t0)) * 100).toFixed(3)}%`,
  })

  /** a tasked collect or a product: outline its scene (or the area) on the map */
  const selectRun = (r: SpaceRun | null) => {
    setItem(null)
    setRunSel(r?.intent_id ?? null)
    const c = r?.collect
    if (!r || !c) return onSelect({ scene: null, norad: null })
    const ring = c.scene ?? c.area
    onSelect({ scene: { ring, label: `i${r.intent_id} ${c.satellite} ${hhmm(c.collect_time, off)}${c.scene ? '' : ' (area)'}` }, norad: c.norad_id })
  }

  const select = (it: Item | null) => {
    setItem(it)
    setRunSel(null)
    if (!it) return onSelect({ scene: null, norad: null })
    if (it.kind === 'pass') {
      setTab('details')
      const w = it.w
      // no swath figure (e.g. Legion): outline the target area instead, labelled as such
      const ring = w.scene ?? area?.ring ?? null
      const label = `${w.label ?? w.satellite} ${hhmm(w.best_time, off)}${w.scene ? '' : ' (swath unknown: area shown)'}`
      onSelect({ scene: ring ? { ring, label } : null, norad: w.norad_id })
    } else {
      const o = it.o
      onSelect({ scene: o.scene ? { ring: o.scene, label: `#${o.rank} ${o.label ?? o.satellite} ${hhmm(o.collect_time, off)}` } : null, norad: o.norad_id })
    }
  }

  // hour ticks in local time
  const ticks: number[] = []
  if (p) {
    const offMs = off * 3_600_000
    const step = spanH > 12 ? 3 : 1
    let t = Math.ceil((t0 + offMs) / (step * 3_600_000)) * step * 3_600_000 - offMs
    for (; t <= t1; t += step * 3_600_000) ticks.push(t)
  }

  // sun shading: night (below 0 deg) and too dark for optical (below 15 deg, the class minimum)
  const shade: { a: number; b: number; cls: string; title: string }[] = []
  if (p) {
    for (let i = 0; i < p.sun.length; i++) {
      const [t, el] = p.sun[i]
      const next = p.sun[i + 1]?.[0] ?? t1
      const cls = el < 0 ? 'night' : el < 15 ? 'dusk' : ''
      if (!cls) continue
      const last = shade[shade.length - 1]
      if (last && last.cls === cls && last.b === t) last.b = next
      else shade.push({ a: t, b: next, cls, title: cls === 'night' ? 'night: no optical collection' : 'sun under 15°: too dark for optical' })
    }
  }

  const usable = (p?.windows ?? []).filter((w) => w.daylight_ok !== false)
  const imagingGaps = p ? gaps(usable.map((w) => [Date.parse(w.start), Date.parse(w.end)]), Math.max(t0, now ?? t0), t1) : []
  const selectedOption = item?.kind === 'option' ? item.o : null
  const selectedPass = item?.kind === 'pass' ? item.w : null

  const bar = (w: AccessWindow, key: string) => {
    const s = Date.parse(w.start)
    const e = Date.parse(w.end)
    const night = w.daylight_ok === false
    const sel = selectedPass === w
    return (
      <button
        key={key}
        className={`tl-bar ${night ? 'night' : ''} ${w.taskable ? '' : 'untaskable'} ${sel ? 'sel' : ''}`}
        style={{ ...span(s, e), background: night ? undefined : classColor(w.class_id) }}
        title={`${w.label ?? w.satellite} ${hhmm(s, off, true)} to ${hhmm(e, off, true)}${night ? ' (dark: no optical)' : ''}${w.taskable ? '' : ' (routine, not taskable)'}`}
        onClick={() => select(sel ? null : { kind: 'pass', w })}
      />
    )
  }

  const optionMarker = (cid: string) =>
    selectedOption && selectedOption.class_id === cid ? (
      <>
        {selectedOption.product_time && (
          <span className="tl-product" style={span(Date.parse(selectedOption.collect_time), Date.parse(selectedOption.product_time))} />
        )}
        <span className="tl-option" style={{ left: pct(Date.parse(selectedOption.collect_time)) }} title={`option #${selectedOption.rank}`} />
      </>
    ) : null

  const rows: ReactNode[] = []
  for (const g of GROUPS) {
    const ids = (catalog?.classes ?? []).filter((c) => c.capability === g.cap).map((c) => c.id)
    rows.push(
      <div className="tl-group" key={`g-${g.cap}`}>
        {g.title}
        {g.cap === 'optical' && <span className="muted"> daylight only; shaded where the sun is too low</span>}
      </div>,
    )
    for (const id of ids) {
      const cls = classes.get(id)
      const ws = (p?.windows ?? []).filter((w) => w.class_id === id)
      const open = expanded.has(id)
      const sats = [...new Set(ws.map((w) => w.norad_id))]
      rows.push(
        <Fragment key={id}>
          <button
            className="tl-label"
            onClick={() =>
              setExpanded((s) => {
                const n = new Set(s)
                if (!n.delete(id)) n.add(id)
                return n
              })
            }
            title={cls?.role}
          >
            <span className="tl-caret">{sats.length ? (open ? '▾' : '▸') : ' '}</span>
            <span className="tl-swatch" style={{ background: classColor(id) }} />
            {cls?.name.replace(/ .*$/, '') ?? id}
            <span className="tl-count">{ws.filter((w) => w.daylight_ok !== false).length}</span>
          </button>
          <div className="tl-track">
            {g.cap === 'optical' && shade.map((s, i) => <span key={i} className={`tl-shade ${s.cls}`} style={span(s.a, s.b)} />)}
            {ws.map((w, i) => bar(w, `${id}-${i}`))}
            {optionMarker(id)}
          </div>
        </Fragment>,
      )
      if (open)
        for (const n of sats) {
          const mine = ws.filter((w) => w.norad_id === n)
          rows.push(
            <Fragment key={`${id}-${n}`}>
              <div className="tl-label sat" title={mine[0].satellite}>
                {mine[0].label ?? mine[0].satellite}
              </div>
              <div className="tl-track sat">
                {g.cap === 'optical' && shade.map((s, i) => <span key={i} className={`tl-shade ${s.cls}`} style={span(s.a, s.b)} />)}
                {mine.map((w, i) => bar(w, `${n}-${i}`))}
              </div>
            </Fragment>,
          )
        }
    }
  }

  // SATCOM and GNSS at the area's centre
  const satRows = (links.data?.classes ?? []).map((c) => (
    <Fragment key={`l-${c.class_id}`}>
      <div className="tl-label" title={`mask ${c.mask_deg}°, visible ${c.visible_min} to ${c.visible_max}`}>
        <span className="tl-caret"> </span>
        <span className="tl-swatch" style={{ background: classColor(c.class_id) }} />
        {c.name.replace(/ .*$/, '')}
        <span className="tl-count">{Math.round(c.coverage_fraction * 100)}%</span>
      </div>
      <div className="tl-track">
        <span className="tl-cover" style={{ ...span(t0, t1), background: classColor(c.class_id) }} />
        {c.outages.map((o, i) => (
          <span
            key={i}
            className="tl-outage"
            style={span(Date.parse(o.start), Date.parse(o.end))}
            title={`no ${c.class_id} satellite above the mask ${hhmm(o.start, off)} to ${hhmm(o.end, off)}`}
          />
        ))}
      </div>
    </Fragment>
  ))
  const g = gnss.data
  const gnssRow = g && (
    <Fragment key="gnss">
      <div className="tl-label" title="GPS satellites in view and PDOP at the area's centre (geometry only: no jamming or masking)">
        <span className="tl-caret"> </span>
        <span className="tl-swatch" style={{ background: classColor('gps') }} />
        GPS
        <span className="tl-count">{g.pdop_max != null ? `PDOP ≤ ${g.pdop_max.toFixed(1)}` : ''}</span>
      </div>
      <div className="tl-track">
        {g.samples.map((s, i) => {
          const a = Date.parse(s.t)
          const b = g.samples[i + 1] ? Date.parse(g.samples[i + 1].t) : t1
          const poor = s.visible < 4 || (s.dop?.pdop ?? 99) > 6
          return (
            <span
              key={i}
              className={`tl-gnss ${poor ? 'poor' : ''}`}
              style={span(a, b)}
              title={`${hhmm(a, off)}: ${s.visible} in view, PDOP ${s.dop?.pdop.toFixed(2) ?? 'n/a'}`}
            >
              {s.visible}
            </span>
          )
        })}
      </div>
    </Fragment>
  )

  // tasked collects: a diamond at the collect, a bar to the product; filled once collected
  const scen = space ? scenarioAt(space.clock, bridgeNow()) : now
  const collects = runs.filter((r) => r.kind === 'collect' && r.collect)
  const taskedRow = collects.length > 0 && (
    <Fragment key="tasked">
      <div className="tl-label tasked" title="tasked collections (simulated): diamond at the collect, bar to the product">
        <span className="tl-caret"> </span>Tasked collections
        <span className="tl-count">{collects.filter((r) => !r.outcome).length} open</span>
      </div>
      <div className="tl-track tasked">
        {collects.map((r) => {
          const c = r.collect!
          // only what falls inside the view
          if (c.collect_time < t0 || c.collect_time > t1) return null
          const ready = r.state === 'product_ready'
          return (
            <Fragment key={r.intent_id}>
              {c.product_time != null && r.state !== 'missed' && (
                <span className={`tl-tproduct ${ready ? 'ready' : ''}`} style={span(c.collect_time, c.product_time)} />
              )}
              <button
                className={`tl-tasked ${r.state ?? ''} ${runSel === r.intent_id ? 'sel' : ''}`}
                style={{ left: pct(c.collect_time) }}
                title={`i${r.intent_id} ${c.satellite}: ${r.state ?? 'tasked'}, collect ${hhmm(c.collect_time, off, true)}${c.product_time != null ? `, product ${hhmm(c.product_time, off)}` : ''}`}
                onClick={() => selectRun(runSel === r.intent_id ? null : r)}
              />
            </Fragment>
          )
        })}
      </div>
    </Fragment>
  )
  const watchRows = runs
    .filter((r) => r.kind === 'cover' && r.cover)
    .map((r) => {
      const cv = r.cover!
      const name = cv.service.type === 'gnss' ? cv.service.class.toUpperCase() : cv.service.class
      return (
        <Fragment key={`w-${r.intent_id}`}>
          <div className="tl-label" title={`i${r.intent_id}: ${cv.summary}`}>
            <span className="tl-caret"> </span>
            <span className="tl-swatch" style={{ background: classColor(cv.service.class) }} />
            i{r.intent_id} {name}
            <span className="tl-count">{r.outcome === 'cancelled' ? 'cancelled' : r.outcome ? 'ended' : r.state}</span>
          </div>
          <div className="tl-track">
            <span className="tl-watch" style={span(cv.window.from, cv.window.to)} />
            {cv.outages.map(([a, b], i) => (
              <span
                key={i}
                className={`tl-watch-out ${scen != null && scen >= a ? 'seen' : ''}`}
                style={span(a, b)}
                title={`predicted outage ${hhmm(a, off, true)} to ${hhmm(b, off, true)}`}
              />
            ))}
          </div>
        </Fragment>
      )
    })
  const products = runs.filter(hasProduct).length

  const shifted = catalog?.time_shifted
  const error = catalogError ?? passes.error

  return (
    <section className="space" aria-label="Space timeline">
      <header className="space-head">
        <h2>Space</h2>
        {area && (
          <span className="muted" title={area.title}>
            {area.title}
          </span>
        )}
        {shifted && (
          <span className="shifted" title={catalog?.time_note}>
            TIME-SHIFTED {catalog!.time_map.shift_days > 0 ? '+' : ''}
            {catalog!.time_map.shift_days} d
          </span>
        )}
        <span className="spacer" />
        {now != null && (
          <span className="scen-clock" title={`scenario clock (${catalog?.scenario.source}, rate ${catalog?.scenario.rate})`}>
            {dayLabel(now, off)} {hhmm(now, off, true)} <span className="muted">{offsetLabel(off)}</span>
          </span>
        )}
        <div className="seg">
          <button onClick={() => setPanH((h) => h - spanH / 2)} title="earlier">
            ◀
          </button>
          <button className={panH === 0 ? 'on' : ''} onClick={() => setPanH(0)} title="from now">
            now
          </button>
          <button onClick={() => setPanH((h) => h + spanH / 2)} title="later">
            ▶
          </button>
        </div>
        <div className="seg">
          {SPANS.map((s) => (
            <button key={s} className={spanH === s ? 'on' : ''} onClick={() => setSpanH(s)}>
              {s}h
            </button>
          ))}
        </div>
        <button className="ghost" onClick={onClose} aria-label="Close space panel">
          ✕
        </button>
      </header>
      {error && <p className="error space-error">{error}</p>}
      <div className="space-body">
        <div className="timeline">
          {!p && !error && <p className="empty">Computing passes…</p>}
          {p && (
            <div className="tl-grid">
              <div className="tl-label axis">{offsetLabel(off)}</div>
              <div className="tl-track axis">
                {ticks.map((t) => (
                  <span key={t} className="tl-tick" style={{ left: pct(t) }}>
                    {hhmm(t, off)}
                  </span>
                ))}
              </div>
              <div className="tl-label strong" title="union of usable imaging windows (optical in daylight, SAR any time)">
                <span className="tl-caret"> </span>Imaging access
                <span className="tl-count">{usable.length}</span>
              </div>
              <div className="tl-track summary">
                {usable.map((w, i) => (
                  <span key={i} className="tl-any" style={span(Date.parse(w.start), Date.parse(w.end))} />
                ))}
                {imagingGaps.map(([a, b]) => (
                  <span key={a} className="tl-gap" style={span(a, b)} title={`no imaging access ${hhmm(a, off)} to ${hhmm(b, off)}`}>
                    {(b - a) / (t1 - t0) > 0.12 ? `no imaging access ${hhmm(a, off)} to ${hhmm(b, off)}` : ''}
                  </span>
                ))}
              </div>
              {taskedRow}
              {rows}
              {satRows.length > 0 && <div className="tl-group">SATCOM at the area</div>}
              {satRows}
              {gnssRow && <div className="tl-group">GNSS</div>}
              {gnssRow}
              {watchRows.length > 0 && <div className="tl-group">Coverage watches</div>}
              {watchRows}
              {now != null && now >= t0 && now <= t1 && <span className="tl-now" style={{ left: `calc(170px + (100% - 170px) * ${(now - t0) / (t1 - t0)})` }} />}
            </div>
          )}
          {p && (
            <p className="tl-foot muted">
              {p.windows.length} windows · {p.classes.reduce((a, c) => a + c.night_excluded, 0)} optical in darkness (hatched) · computed in {p.compute_ms} ms ·
              times {offsetLabel(off)}
            </p>
          )}
        </div>
        <aside className="space-side">
          <div className="seg wide">
            <button className={tab === 'details' ? 'on' : ''} onClick={() => setTab('details')} title="the pass or option selected on the timeline">
              Pass
            </button>
            <button className={tab === 'request' ? 'on' : ''} onClick={() => setTab('request')} title="find collection options and task one">
              Collect
            </button>
            <button className={tab === 'coverage' ? 'on' : ''} onClick={() => setTab('coverage')} title="request SATCOM or GNSS coverage over an area">
              Coverage
            </button>
            <button className={tab === 'products' ? 'on' : ''} onClick={() => setTab('products')} title="products of tasked collections (simulated)">
              Products{products ? ` (${products})` : ''}
            </button>
          </div>
          {tab === 'details' ? (
            <Details item={item} classes={classes} off={off} onClear={() => select(null)} />
          ) : tab === 'products' ? (
            <ProductList runs={runs} selected={runSel} onSelect={selectRun} />
          ) : tab === 'coverage' ? (
            <CoverageForm
              catalog={catalog}
              mapBounds={mapBounds}
              box={box}
              drawing={drawing}
              onDrawBox={onDrawBox}
              onClearBox={onClearBox}
              now={now}
              space={space}
              scen={scen}
              onIntent={onIntent}
            />
          ) : (
            <RequestForm
              off={off}
              now={now}
              mapBounds={mapBounds}
              box={box}
              drawing={drawing}
              onDrawBox={onDrawBox}
              onClearBox={onClearBox}
              selected={selectedOption}
              onPick={(o) => select(o ? { kind: 'option', o } : null)}
              onIntent={onIntent}
              spaceLoaded={space?.loaded ?? false}
            />
          )}
        </aside>
      </div>
    </section>
  )
}

function Details({ item, classes, off, onClear }: { item: Item | null; classes: Map<string, SatClassInfo>; off: number; onClear: () => void }) {
  if (!item) return <p className="hint pad">Click a bar on the timeline for the pass details; its scene is outlined on the map.</p>
  if (item.kind === 'option') return <OptionCard o={item.o} off={off} onClear={onClear} />
  const w = item.w
  const cls = classes.get(w.class_id)
  const product = productTime(w, cls)
  const guessed = cls?.guessed.some((g) => g.startsWith('timing') || g.includes('latency') || g.includes('processing'))
  return (
    <div className="detail pad">
      <div className="detail-head">
        <span className="tl-swatch" style={{ background: classColor(w.class_id) }} />
        <b>{w.label ?? w.satellite}</b>
        <span className="muted">{cls?.name}</span>
        <button className="ghost" onClick={onClear}>
          clear
        </button>
      </div>
      <dl>
        <dt>Satellite</dt>
        <dd>
          {w.satellite} <span className="muted">NORAD {w.norad_id}</span>
        </dd>
        <dt>Window</dt>
        <dd>
          {hhmm(w.start, off, true)} to {hhmm(w.end, off, true)} ({Math.round(w.duration_s)} s){w.clipped ? ', clipped by the view' : ''}
        </dd>
        <dt>Collect</dt>
        <dd>{hhmm(w.best_time, off, true)} (best geometry)</dd>
        <dt>Off-nadir</dt>
        <dd>{deg(w.best.off_nadir_deg)}</dd>
        <dt>Incidence</dt>
        <dd>{deg(w.best.incidence_deg)}</dd>
        <dt>Elevation</dt>
        <dd>
          {deg(w.best.elevation_deg)} · range {Math.round(w.best.range_km)} km
        </dd>
        <dt>Resolution</dt>
        <dd>{w.resolution_m != null ? `${w.resolution_m.toFixed(2)} m` : 'unknown'}</dd>
        <dt>Sun</dt>
        <dd>
          {deg(w.sun_elevation_deg)}
          {w.daylight_ok === false ? ' (too dark for optical)' : ''}
        </dd>
        <dt>Product</dt>
        <dd>
          {product ? `~${hhmm(product, off)}` : 'unknown'}
          {product && guessed ? <span className="muted"> (downlink and processing are estimates)</span> : null}
        </dd>
        <dt>Tasking</dt>
        <dd>{w.taskable ? 'taskable' : 'routine collect, not taskable'}</dd>
      </dl>
      {!w.scene && <p className="hint">No swath figure for this satellite: the map outlines the target area instead.</p>}
    </div>
  )
}

function OptionCard({ o, off, onClear }: { o: TaskOption; off: number; onClear?: () => void }) {
  return (
    <div className="detail pad">
      <div className="detail-head">
        <span className="tl-swatch" style={{ background: classColor(o.class_id) }} />
        <b>
          #{o.rank} {o.label ?? o.satellite}
        </b>
        <span className="muted">{o.class_name}</span>
        {onClear && (
          <button className="ghost" onClick={onClear}>
            clear
          </button>
        )}
      </div>
      <dl>
        <dt>Collect</dt>
        <dd>{hhmm(o.collect_time, off, true)}</dd>
        <dt>Product</dt>
        <dd>{o.product_time ? `~${hhmm(o.product_time, off)}` : 'unknown'}</dd>
        <dt>Geometry</dt>
        <dd>
          off-nadir {deg(o.geometry.off_nadir_deg)} · incidence {deg(o.geometry.incidence_deg)}
        </dd>
        <dt>Resolution</dt>
        <dd>{o.resolution_m != null ? `${o.resolution_m.toFixed(2)} m` : 'unknown'}</dd>
        <dt>Notes</dt>
        <dd>{o.notes.join('; ') || 'none'}</dd>
      </dl>
    </div>
  )
}

const DEADLINES = [2, 4, 6, 12, 24]

function RequestForm({
  off,
  now,
  mapBounds,
  box,
  drawing,
  onDrawBox,
  onClearBox,
  selected,
  onPick,
  onIntent,
  spaceLoaded,
}: {
  off: number
  now: number | null
  mapBounds: [number, number, number, number] | null
  box: LngLat[] | null
  drawing: boolean
  onDrawBox: () => void
  onClearBox: () => void
  selected: TaskOption | null
  onPick: (o: TaskOption | null) => void
  onIntent?: (id: number) => void
  /** the decision layer has the space segment (else a collect fails closed) */
  spaceLoaded: boolean
}) {
  const [where, setWhere] = useState<'view' | 'box' | 'scenario'>('view')
  const [caps, setCaps] = useState({ optical: true, sar: true })
  const [hours, setHours] = useState(6)
  const [rank, setRank] = useState<TaskQuery['rank']>('earliest_product')
  const [maxRes, setMaxRes] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<TaskingResult | null>(null)
  const [showExcl, setShowExcl] = useState(false)
  const [tasking, setTasking] = useState(false)
  const [tasked, setTasked] = useState<{ intent: Intent; dispatched: boolean } | null>(null)

  const bbox = (ring: LngLat[]): [number, number, number, number] => [
    Math.min(...ring.map((p) => p[0])),
    Math.min(...ring.map((p) => p[1])),
    Math.max(...ring.map((p) => p[0])),
    Math.max(...ring.map((p) => p[1])),
  ]
  const area: TaskQuery['area'] | null =
    where === 'scenario' ? 'scenario' : where === 'box' ? (box ? { bbox: bbox(box) } : null) : mapBounds ? { bbox: mapBounds } : null
  const capList = (['optical', 'sar'] as const).filter((c) => caps[c])
  const deadline = now == null ? null : now + hours * 3_600_000

  const submit = () => {
    if (!area || !capList.length || deadline == null) return
    setBusy(true)
    setError(null)
    requestOptions({
      capabilities: capList,
      area,
      deadline: new Date(deadline).toISOString(),
      rank,
      max_resolution_m: maxRes ? Number(maxRes) : undefined,
    })
      .then((r) => {
        setResult(r)
        setTasked(null)
        onPick(null)
      })
      .catch((e: Error) => setError(e.message))
      .finally(() => setBusy(false))
  }

  /** Task the selected option: a collect intent with the same request, committed to it. The
   *  operator's own collect is authorized by sending it (DECISIONS.md: the operator's own
   *  command is the human decision); an agent's would wait in the inbox. */
  const taskIt = () => {
    if (!result || !selected || !result.area.area) return
    setTasking(true)
    setError(null)
    submitIntent({
      task: 'collect',
      area: result.area.area,
      window: { from: result.request.from, to: result.request.to },
      capability: capList.length === 2 ? 'any' : capList[0],
      ...(maxRes ? { min_resolution_m: Number(maxRes) } : {}),
      ...(deadline != null ? { product_deadline: new Date(deadline).toISOString() } : {}),
      chosen: { norad_id: selected.norad_id, collect_time: selected.collect_time },
    })
      .then((r) => setTasked({ intent: r.intent, dispatched: r.dispatched }))
      .catch((e: Error) => setError(e.message))
      .finally(() => setTasking(false))
  }

  const km = (b: [number, number, number, number]) => {
    const w = (b[2] - b[0]) * 111.32 * Math.cos((((b[1] + b[3]) / 2) * Math.PI) / 180)
    const h = (b[3] - b[1]) * 111.32
    return `${w.toFixed(1)} × ${h.toFixed(1)} km`
  }
  const summary = new Map<string, string[]>()
  for (const s of result?.exclusion_summary ?? []) {
    const l = summary.get(s.class_id) ?? []
    l.push(`${REASONS[s.reason] ?? s.reason} ${s.passes}`)
    summary.set(s.class_id, l)
  }

  return (
    <div className="reqform pad">
      <fieldset>
        <legend>Area</legend>
        <label>
          <input type="radio" checked={where === 'view'} onChange={() => setWhere('view')} /> Map view
          {mapBounds && <span className="muted"> {km(mapBounds)}</span>}
        </label>
        <label>
          <input type="radio" checked={where === 'box'} onChange={() => setWhere('box')} /> Drawn box
          {box && <span className="muted"> {km(bbox(box))}</span>}
          <button
            className="ghost mini"
            onClick={() => {
              setWhere('box')
              onDrawBox()
            }}
          >
            {drawing ? 'click two corners…' : box ? 'redraw' : 'draw'}
          </button>
          {box && (
            <button className="ghost mini" onClick={onClearBox}>
              clear
            </button>
          )}
        </label>
        <label>
          <input type="radio" checked={where === 'scenario'} onChange={() => setWhere('scenario')} /> Scenario area
        </label>
      </fieldset>
      <fieldset className="inline">
        <legend>Capability</legend>
        <label>
          <input type="checkbox" checked={caps.optical} onChange={(e) => setCaps({ ...caps, optical: e.target.checked })} /> Optical
        </label>
        <label>
          <input type="checkbox" checked={caps.sar} onChange={(e) => setCaps({ ...caps, sar: e.target.checked })} /> SAR
        </label>
      </fieldset>
      <div className="reqrow">
        <label>
          Product by
          <select value={hours} onChange={(e) => setHours(Number(e.target.value))}>
            {DEADLINES.map((h) => (
              <option key={h} value={h}>
                +{h} h{now != null ? ` (${hhmm(now + h * 3_600_000, off)})` : ''}
              </option>
            ))}
          </select>
        </label>
        <label>
          Rank
          <select value={rank} onChange={(e) => setRank(e.target.value as TaskQuery['rank'])}>
            <option value="earliest_product">earliest product</option>
            <option value="best_resolution">best resolution</option>
          </select>
        </label>
        <label title="leave empty for any resolution">
          Max res. m
          <input type="number" min={0.1} step={0.1} value={maxRes} placeholder="any" onChange={(e) => setMaxRes(e.target.value)} />
        </label>
      </div>
      <button className="primary" disabled={!area || !capList.length || busy || now == null} onClick={submit}>
        {busy ? 'Computing…' : 'Find options'}
      </button>
      {error && <p className="error">{error}</p>}
      {result && (
        <div className="options">
          <p className="muted small">
            {result.options.length} feasible · {result.exclusions_total} passes excluded · cloud model: {result.cloud_model} · {result.compute_ms} ms
          </p>
          {result.options.length === 0 && <p className="hint">No feasible collection before the deadline. See the exclusions below.</p>}
          <ol className="optlist">
            {result.options.slice(0, 25).map((o) => (
              <li key={`${o.norad_id}-${o.collect_time}`}>
                <button className={`opt ${selected === o ? 'sel' : ''} ${o.taskable ? '' : 'untaskable'}`} onClick={() => onPick(selected === o ? null : o)}>
                  <span className="opt-rank">{o.rank}</span>
                  <span className="tl-swatch" style={{ background: classColor(o.class_id) }} />
                  <span className="opt-name">{o.label ?? o.satellite}</span>
                  <span className="opt-num">{hhmm(o.collect_time, off)}</span>
                  <span className="opt-num">{o.resolution_m != null ? `${o.resolution_m.toFixed(2)} m` : '?'}</span>
                  <span className="opt-num">→ {o.product_time ? hhmm(o.product_time, off) : '?'}</span>
                  <span className="opt-notes">
                    {o.capability} · off-nadir {o.geometry.off_nadir_deg.toFixed(0)}°{o.covers_area === false ? ' · partial area' : ''}
                    {o.taskable ? '' : ' · routine, not taskable'}
                  </span>
                </button>
              </li>
            ))}
          </ol>
          {selected && <OptionCard o={selected} off={off} />}
          <button
            className="primary"
            disabled={!selected || !selected.taskable || tasking || !spaceLoaded}
            onClick={taskIt}
            title={
              !spaceLoaded
                ? 'The decision layer has no space segment yet'
                : selected && !selected.taskable
                  ? 'A routine collect: this sensor cannot be tasked'
                  : 'Sends a collect intent. Your own command is the human decision: it is authorized as you send it, after the checks pass.'
            }
          >
            {tasking ? 'Tasking…' : selected ? `Task collection: #${selected.rank} ${selected.label ?? selected.satellite}` : 'Task collection (pick an option)'}
          </button>
          {tasked && <TaskedResult intent={tasked.intent} dispatched={tasked.dispatched} onIntent={onIntent} />}
          <div className="excl">
            <button className="ghost mini" onClick={() => setShowExcl((s) => !s)}>
              {showExcl ? '▾' : '▸'} Why not the others ({result.exclusions_total})
            </button>
            <ul>
              {[...summary].map(([cid, reasons]) => (
                <li key={cid}>
                  <span className="tl-swatch" style={{ background: classColor(cid) }} /> <b>{cid}</b> {reasons.join(', ')}
                </li>
              ))}
              {result.classes_without_passes.length > 0 && <li className="muted">no passes at all: {result.classes_without_passes.join(', ')}</li>}
            </ul>
            {showExcl && (
              <ul className="excl-list">
                {result.exclusions.slice(0, 60).map((e, i) => (
                  <li key={i}>
                    <span className="opt-num">{hhmm(e.pass_time, off)}</span> {e.satellite}: {e.detail}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

/** What came of a collect or cover the operator sent. */
function TaskedResult({ intent: i, dispatched, onIntent }: { intent: Intent; dispatched: boolean; onIntent?: (id: number) => void }) {
  const last = i.history[i.history.length - 1]
  const failed = i.checks.filter((c) => c.outcome === 'fail')
  const tone = i.state === 'pending_approval' ? 'wait' : dispatched && i.state !== 'failed' ? '' : 'bad'
  const what = i.kind === 'collect' ? 'collection' : 'coverage watch'
  return (
    <div className={`sr-result ${tone}`} role="status">
      {i.state === 'pending_approval' ? (
        <strong>
          Waiting for approval (i{i.id}){i.expires_t ? `: expires ${fmtZ(i.expires_t)} bridge time` : ''}
        </strong>
      ) : dispatched ? (
        <strong>
          {i.kind === 'collect' ? 'Tasked' : 'Watching'}:{' '}
          {i.approval?.at_submission ? `authorized by you (${i.policy?.rule ?? 'policy'})` : i.state === 'dispatched' ? 'auto-approved by policy' : i.state} (i{i.id})
        </strong>
      ) : (
        <strong>
          Refused by the rules (i{i.id}): no {what} was requested.
        </strong>
      )}
      {failed.length > 0 ? (
        <ul>
          {failed.map((c, k) => (
            <li key={k}>
              <b>{c.rule}</b>: {c.detail}
            </li>
          ))}
        </ul>
      ) : (
        <span className="muted sr-why">{last?.reason}</span>
      )}
      {dispatched && <span className="muted sr-why">Follow it in the command log{i.kind === 'collect' ? '; its scene is outlined on the map until it is done' : ''}.</span>}
      <button className="ghost mini" onClick={() => onIntent?.(i.id)}>
        See i{i.id} in {i.state === 'pending_approval' ? 'the Inbox' : 'Decisions'}
      </button>
    </div>
  )
}

const COVER_HOURS = [1, 2, 6, 12, 24]

/** Ask for SATCOM coverage (one class) or GNSS visibility over an area for a window: a cover
 *  intent. Its check passes when the service holds well enough (the minimum level); then it
 *  runs as a watch that reports the predicted outages as they come. */
function CoverageForm({
  catalog,
  mapBounds,
  box,
  drawing,
  onDrawBox,
  onClearBox,
  now,
  space,
  scen,
  onIntent,
}: {
  catalog: Catalog | null
  mapBounds: [number, number, number, number] | null
  box: LngLat[] | null
  drawing: boolean
  onDrawBox: () => void
  onClearBox: () => void
  now: number | null
  space?: SpaceRuns
  scen: number | null
  onIntent?: (id: number) => void
}) {
  const services = useMemo(
    () => (catalog?.classes ?? []).filter((c) => c.capability === 'satcom' || c.capability === 'gnss').map((c) => ({ id: c.id, cap: c.capability })),
    [catalog],
  )
  const [service, setService] = useState('starlink')
  const [where, setWhere] = useState<'view' | 'box' | 'scenario'>('scenario')
  const [hours, setHours] = useState(2)
  const [level, setLevel] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [sent, setSent] = useState<{ intent: Intent; dispatched: boolean } | null>(null)
  const svc = services.find((s) => s.id === service) ?? services[0]
  const gnss = svc?.cap === 'gnss'
  const bbox = (ring: LngLat[]): [number, number, number, number] => [
    Math.min(...ring.map((p) => p[0])),
    Math.min(...ring.map((p) => p[1])),
    Math.max(...ring.map((p) => p[0])),
    Math.max(...ring.map((p) => p[1])),
  ]
  const scenarioArea: AreaInfo | null = catalog?.area ?? null
  const area =
    where === 'scenario' ? (scenarioArea?.area ?? null) : where === 'box' ? (box ? { bbox: bbox(box) } : null) : mapBounds ? { bbox: mapBounds } : null

  const send = () => {
    if (!svc || !area || now == null) return
    setBusy(true)
    setError(null)
    const from = Math.floor(now / 60_000) * 60_000
    const min = level.trim() === '' ? undefined : gnss ? Number(level) : Number(level) / 100
    submitIntent({
      task: 'cover',
      area,
      window: { from, to: from + hours * 3_600_000 },
      service: gnss ? { type: 'gnss', class: svc.id } : { type: 'satcom', class: svc.id },
      ...(min != null && Number.isFinite(min) ? { min_level: min } : {}),
    })
      .then((r) => setSent({ intent: r.intent, dispatched: r.dispatched }))
      .catch((e: Error) => setError(e.message))
      .finally(() => setBusy(false))
  }

  const watches = (space?.runs ?? []).filter((r) => r.kind === 'cover')
  return (
    <div className="reqform pad">
      <div className="reqrow">
        <label>
          Service
          <select value={svc?.id ?? ''} onChange={(e) => setService(e.target.value)}>
            {services.map((s) => (
              <option key={s.id} value={s.id}>
                {s.cap === 'gnss' ? `${s.id.toUpperCase()} (GNSS)` : `${s.id} (SATCOM)`}
              </option>
            ))}
          </select>
        </label>
        <label>
          For
          <select value={hours} onChange={(e) => setHours(Number(e.target.value))}>
            {COVER_HOURS.map((h) => (
              <option key={h} value={h}>
                {h} h
              </option>
            ))}
          </select>
        </label>
        <label title={gnss ? 'satellites in view at every sample (default 4, a 3D fix)' : 'percent of the window covered (default 99%)'}>
          {gnss ? 'Min sats' : 'Min %'}
          <input type="number" min={0} max={gnss ? 40 : 100} value={level} placeholder={gnss ? '4' : '99'} onChange={(e) => setLevel(e.target.value)} />
        </label>
      </div>
      <fieldset>
        <legend>Area (service at its centre)</legend>
        <label>
          <input type="radio" checked={where === 'scenario'} onChange={() => setWhere('scenario')} /> Scenario area
        </label>
        <label>
          <input type="radio" checked={where === 'view'} onChange={() => setWhere('view')} /> Map view
        </label>
        <label>
          <input type="radio" checked={where === 'box'} onChange={() => setWhere('box')} /> Drawn box
          <button
            className="ghost mini"
            onClick={() => {
              setWhere('box')
              onDrawBox()
            }}
          >
            {drawing ? 'click two corners…' : box ? 'redraw' : 'draw'}
          </button>
          {box && (
            <button className="ghost mini" onClick={onClearBox}>
              clear
            </button>
          )}
        </label>
      </fieldset>
      <button className="primary" disabled={!svc || !area || busy || now == null || !(space?.loaded ?? false)} onClick={send}>
        {busy ? 'Computing…' : 'Request coverage'}
      </button>
      {error && <p className="error">{error}</p>}
      {sent && <TaskedResult intent={sent.intent} dispatched={sent.dispatched} onIntent={onIntent} />}
      {space && scen != null && watches.length > 0 && (
        <div className="sr-watches">
          {watches.map((r) => (
            <SpaceRunCard key={r.intent_id} run={r} map={space.clock} scen={scen} onOpen={onIntent} cancellable={space.cancellable ?? false} />
          ))}
        </div>
      )}
    </div>
  )
}
