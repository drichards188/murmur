import { useEffect, useMemo, useRef, useState } from 'react'
import { ALT_PRESETS, CLASSES, GRID_BUDGET, gridRequest, gridUrl, SAT_GRID_BUDGET, satcomClass, STATE_RGB, type CoverageGrid, type GridTarget } from './coverage'
import type { LngLat, Vehicle } from './types'
import { callsign } from './types'
import './coverage.css'

/** Asks the bridge for the grid of the view (coverage.ts gridRequest), again when the view
 *  settles somewhere the last grid does not cover or the question changes (after a short pause,
 *  so a pan or a zoom asks once), while terrain is still being answered (a few times), and every
 *  minute where satellites decide it (they move). The previous grid stays drawn until the next
 *  arrives; a newer question cancels an older one in flight. `budget`: the most cells to ask for
 *  (fewer where satellites decide it). */
function useCoverageGrid(target: GridTarget | null, alt: number | null, above: boolean, view: { center: LngLat; bounds: [number, number, number, number] } | null, budget: number) {
  const [grid, setGrid] = useState<CoverageGrid | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [again, setAgain] = useState(0)
  const url = useMemo(() => {
    if (!target || !view) return null
    return gridUrl(target, alt, above, gridRequest(view.bounds, view.center, 250, budget))
  }, [target, alt, above, view, budget])
  const tries = useRef<{ url: string | null; unknown: number }>({ url: null, unknown: 0 })
  useEffect(() => {
    if (!url) return
    const ctl = new AbortController()
    const timer = window.setTimeout(async () => {
      setLoading(true)
      try {
        const res = await fetch(url, { signal: ctl.signal })
        const body = await res.json().catch(() => ({}))
        if (!res.ok) throw new Error(body.detail ?? `bridge returned ${res.status}`)
        setGrid(body as CoverageGrid)
        setError(null)
      } catch (e) {
        if (!ctl.signal.aborted) setError((e as Error).message)
      } finally {
        if (!ctl.signal.aborted) setLoading(false)
      }
    }, 450)
    return () => {
      ctl.abort()
      window.clearTimeout(timer)
    }
  }, [url, again])
  // terrain not answered yet: ask again shortly (each answer warms the bridge's cache), a few
  // times; satellites: every minute
  useEffect(() => {
    if (!grid || !url) return
    if (tries.current.url !== url) tries.current = { url, unknown: 0 }
    let wait: number | null = null
    if (grid.counts.unknown > 0 && tries.current.unknown < 5) {
      tries.current.unknown++
      wait = 3000
    } else if (grid.sources.satcom) wait = 60_000
    if (wait == null) return
    const t = window.setTimeout(() => setAgain((n) => n + 1), wait)
    return () => window.clearTimeout(t)
  }, [grid, url])
  return { grid: url ? grid : null, error, loading }
}

const fmtAlt = (alt: number, above: boolean) => (alt < 0 && !above ? `${-alt} m deep` : `${alt} m ${above ? 'above surface' : 'MSL'}`)

/** The coverage layer's card: what is predicted (a link class, or the one selected vehicle's own
 *  link at its mission altitude), at what altitude, its legend and what it rests on. Off by
 *  default (the topbar's Links button). Hands the grid up for the map to draw. */
export function CoverageControl({
  view,
  vehicle,
  onGrid,
  onClose,
}: {
  view: { center: LngLat; bounds: [number, number, number, number] } | null
  /** the one vehicle selected, if exactly one is: offered as "its own link" */
  vehicle: Vehicle | null
  onGrid: (g: CoverageGrid | null) => void
  onClose: () => void
}) {
  const [cls, setCls] = useState('los')
  const [own, setOwn] = useState(false)
  // null: the vehicle's own mission altitude (its own link only)
  const [alt, setAlt] = useState<number | null>(150)
  const [above, setAbove] = useState(false)
  const [altText, setAltText] = useState('150')
  const ownUid = own ? (vehicle?.uid ?? null) : null
  const useOwn = ownUid != null
  const target = useMemo<GridTarget>(() => (ownUid != null ? { uid: ownUid } : { class: cls }), [ownUid, cls])
  const askAlt = useOwn ? alt : (alt ?? 0)
  const sat = satcomClass(useOwn ? vehicle?.identity?.link : cls)
  const { grid, error, loading } = useCoverageGrid(target, askAlt, above, view, sat ? SAT_GRID_BUDGET : GRID_BUDGET)

  useEffect(() => onGrid(grid), [grid, onGrid])
  useEffect(() => () => onGrid(null), [onGrid])

  const pickAlt = (a: number | null, ab: boolean) => {
    setAlt(a)
    setAbove(ab)
    setAltText(a == null ? '' : String(a))
  }
  const pickClass = (v: string) => {
    if (v === '@own') {
      setOwn(true)
      pickAlt(null, false)
      return
    }
    setOwn(false)
    setCls(v)
    const c = CLASSES.find((x) => x.id === v)
    if (c) pickAlt(c.alt, c.above)
  }
  const shownAlt = grid ? (grid.alt_ref === 'surface' ? `${grid.alt_m} m above surface` : fmtAlt(grid.alt_m, false)) : null
  const total = grid ? grid.nlat * grid.nlon : 0
  return (
    <section className="cov" aria-label="Predicted link coverage">
      <header className="cov-head">
        <h3>Link coverage</h3>
        <span className="cov-pred" title="The link emulator's geometry, not what the links do now">
          predicted
        </span>
        {loading && <span className="cov-busy">updating…</span>}
        <button className="ghost mini close" onClick={onClose} aria-label="Hide link coverage">
          ✕
        </button>
      </header>
      <label className="cov-row">
        <span>Link</span>
        <select value={useOwn ? '@own' : cls} onChange={(e) => pickClass(e.target.value)}>
          {vehicle && <option value="@own">{callsign(vehicle)}'s own link</option>}
          {CLASSES.map((c) => (
            <option key={c.id} value={c.id}>
              {c.label}
            </option>
          ))}
        </select>
      </label>
      <div className="cov-row">
        <span>Altitude</span>
        <input
          type="number"
          step={5}
          value={altText}
          placeholder={useOwn ? 'its mission altitude' : 'm'}
          aria-label="Altitude, metres"
          onChange={(e) => {
            setAltText(e.target.value)
            const v = e.target.value.trim() === '' ? null : Number(e.target.value)
            if (v == null) {
              if (useOwn) setAlt(null)
            } else if (Number.isFinite(v)) setAlt(v)
          }}
        />
        <label className="cov-above" title="Metres above the ground or the water there, not above sea level">
          <input type="checkbox" checked={above} disabled={alt == null} onChange={(e) => setAbove(e.target.checked)} /> above surface
        </label>
      </div>
      <div className="cov-presets" role="group" aria-label="Altitude presets">
        {useOwn && (
          <button className={alt == null ? 'on' : ''} title="The altitude its orders fly at (the task layer's mission altitude)" onClick={() => pickAlt(null, false)}>
            mission
          </button>
        )}
        {ALT_PRESETS.map((p) => (
          <button key={p.label} className={alt === p.alt && above === p.above ? 'on' : ''} title={p.title} onClick={() => pickAlt(p.alt, p.above)}>
            {p.label}
          </button>
        ))}
      </div>
      <div className="cov-legend">
        {(['up', 'degraded', 'down'] as const).map((s) => (
          <span key={s}>
            <i style={{ background: `rgb(${STATE_RGB[s].join(',')})`, opacity: s === 'up' ? 0.45 : 0.85 }} />
            {s}
            {grid && <b>{grid.counts[s === 'degraded' ? 'degraded' : s]}</b>}
          </span>
        ))}
        {grid && grid.counts.unknown > 0 && (
          <span title="Terrain not answered yet: counted open, as the emulator does until its answer comes">
            <i style={{ background: `rgb(${STATE_RGB.unknown.join(',')})` }} />
            not yet<b>{grid.counts.unknown}</b>
          </span>
        )}
      </div>
      {error && <p className="cov-error">{error}</p>}
      {grid && (
        <p className="cov-meta">
          {grid.callsign ? `${grid.callsign}: ` : ''}
          {grid.profile} at {shownAlt} · {Math.round(grid.res_m)} m cells ({total.toLocaleString()})
          {grid.failsafe_s != null && ` · failsafe ${grid.failsafe_s} s`}
          <br />
          terrain: {grid.sources.terrain}
          {grid.sources.satcom && (
            <>
              <br />
              satellites at {grid.at.slice(11, 16)}Z
            </>
          )}
        </p>
      )}
      <p className="cov-note">
        Predicted from the link emulator's own geometry: range from the ground station, terrain, satellites in view, depth. Its random
        loss and scripted events (jammers, cuts) are not in it. Hover the map for why.
      </p>
    </section>
  )
}
