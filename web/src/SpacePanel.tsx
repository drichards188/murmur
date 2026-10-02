import { Fragment, useEffect, useMemo, useState, type ReactNode } from 'react'
import type { AccessWindow, Catalog, Gnss, Links, Passes, SatClassInfo, TaskingResult, TaskOption, TaskQuery } from './space'
import { classColor, dayLabel, hhmm, offsetLabel, productTime, requestOptions, usePoll, useScenarioNow } from './space'
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

export function SpacePanel({ catalog, catalogError, mapBounds, box, drawing, onDrawBox, onClearBox, onSelect, onClose }: Props) {
  const [spanH, setSpanH] = useState(6)
  const [panH, setPanH] = useState(0)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [item, setItem] = useState<Item | null>(null)
  const [tab, setTab] = useState<'details' | 'request'>('request')

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

  const select = (it: Item | null) => {
    setItem(it)
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
              {rows}
              {satRows.length > 0 && <div className="tl-group">SATCOM at the area</div>}
              {satRows}
              {gnssRow && <div className="tl-group">GNSS</div>}
              {gnssRow}
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
            <button className={tab === 'details' ? 'on' : ''} onClick={() => setTab('details')}>
              Pass details
            </button>
            <button className={tab === 'request' ? 'on' : ''} onClick={() => setTab('request')}>
              Request collection
            </button>
          </div>
          {tab === 'details' ? (
            <Details item={item} classes={classes} off={off} onClear={() => select(null)} />
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
        onPick(null)
      })
      .catch((e: Error) => setError(e.message))
      .finally(() => setBusy(false))
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
          <span
            className="submit-wrap"
            title="Submitting a collection for approval arrives with the decision pipeline (Phase C). Nothing has been requested."
          >
            <button className="primary" disabled>
              Submit for approval
            </button>
            <span className="muted small">arrives with the decision pipeline</span>
          </span>
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
