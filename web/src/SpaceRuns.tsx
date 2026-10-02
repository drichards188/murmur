// Satellite collect and cover runs as the console shows them: the command log card, the
// products list and the coverage watches. Runs come from the bridge's decisions message
// (`space`), times are scenario time (ms), and the scenario clock is a map from the
// bridge's clock (`MURMUR_RATE` scenario seconds per second).
import type { ScenarioMap, SpaceRun } from './decisions'
import { fmtDuration, fmtZ, hasProduct } from './decisions'
import { classColor } from './space'
import './spaceTasking.css'

/** "in 12m30s" (scenario time), with the real wait when the scenario runs faster. */
function until(t: number, scen: number, map: ScenarioMap) {
  const s = (t - scen) / 1000
  if (s <= 0) return null
  const real = map.rate !== 1 ? ` (${fmtDuration(s / map.rate)} real)` : ''
  return `in ${fmtDuration(s)}${real}`
}

const COLLECT_STEPS = ['tasked', 'collected', 'product_ready'] as const
const STEP_LABEL: Record<string, string> = { tasked: 'tasked', collected: 'collected', product_ready: 'product' }

function Steps({ run }: { run: SpaceRun }) {
  const missed = run.state === 'missed'
  const seen = new Set(run.events.map((e) => e.state))
  return (
    <span className={`steps ${missed ? 'failed' : ''}`}>
      {COLLECT_STEPS.map((s) => (
        <span key={s} className={`step ${!missed && seen.has(s) ? 'on' : ''} ${!missed && run.state === s ? 'cur' : ''}`}>
          {missed && s === 'collected' ? 'missed' : STEP_LABEL[s]}
        </span>
      ))}
    </span>
  )
}

/** One collect or cover in the command log. */
export function SpaceRunCard({ run, map, scen, onOpen }: { run: SpaceRun; map: ScenarioMap; scen: number; onOpen?: (intentId: number) => void }) {
  const last = run.events[run.events.length - 1]
  const age = last ? fmtDuration(Math.max(0, (scen - run.events[0].scenario_t) / 1000)) : null
  if (run.kind === 'collect' && run.collect) {
    const c = run.collect
    const toCollect = until(c.collect_time, scen, map)
    const toProduct = c.product_time != null ? until(c.product_time, scen, map) : null
    return (
      <section className={`tcard sr ${run.outcome ?? 'running'}`}>
        <div className="tcard-head">
          <strong>COLLECT</strong>
          <button className="ghost mini sr-link" onClick={() => onOpen?.(run.intent_id)} title="See the intent in Decisions">
            i{run.intent_id}
          </button>
          <span className="tl-swatch" style={{ background: classColor(c.class_id) }} />
          <span className="sr-sat">{c.satellite}</span>
          <span className="muted right">{age} scen.</span>
        </div>
        <div className="op">
          <span className="op-name" title={`${run.resource} (${c.class_id}, ${c.capability})`}>
            {c.class_id} {c.capability.toUpperCase()}
          </span>
          <Steps run={run} />
          <span className="op-detail">
            collect {fmtZ(c.collect_time)}
            {toCollect ? <b className="sr-count"> {toCollect}</b> : null}
            {' · '}product {c.product_time != null ? fmtZ(c.product_time) : 'time unknown'}
            {toProduct && run.state !== 'missed' ? <b className="sr-count"> {toProduct}</b> : null}
            {c.resolution_m != null ? ` · ${c.resolution_m.toFixed(2)} m` : ''}
          </span>
          {last && <span className={`op-detail ${run.outcome === 'failed' ? 'sr-bad' : ''}`}>{last.detail}</span>}
        </div>
        <p className="muted sr-sim">Simulated tasking: no satellite operator is contacted, no imagery is produced.</p>
      </section>
    )
  }
  const cv = run.cover
  if (!cv) return null
  const label = cv.service.type === 'gnss' ? `${cv.service.class.toUpperCase()} GNSS` : `${cv.service.class} SATCOM`
  const inOutage = cv.outages.find(([a, b]) => scen >= a && scen < b)
  const seen = cv.outages.filter(([a]) => scen >= a).length
  const end = until(cv.window.to, scen, map)
  return (
    <section className={`tcard sr ${run.outcome ?? 'running'}`}>
      <div className="tcard-head">
        <strong>COVER</strong>
        <button className="ghost mini sr-link" onClick={() => onOpen?.(run.intent_id)} title="See the intent in Decisions">
          i{run.intent_id}
        </button>
        <span className="tl-swatch" style={{ background: classColor(cv.service.class) }} />
        <span className="sr-sat">{label}</span>
        <span className={`sr-badge ${inOutage ? 'bad' : run.outcome ? '' : 'ok'}`}>{run.outcome ? 'ended' : inOutage ? 'OUTAGE' : scen < cv.window.from ? 'waiting' : 'covered'}</span>
      </div>
      <div className="op">
        <span className="op-name">watch</span>
        <span className="op-detail sr-first">
          {fmtZ(cv.window.from)} to {fmtZ(cv.window.to)}
          {end && !run.outcome ? <b className="sr-count"> ends {end}</b> : null}
        </span>
        <span className="op-detail">
          {cv.service.type === 'gnss'
            ? cv.summary
            : cv.outages.length === 0
              ? 'no outage predicted in the window'
              : `${seen} of ${cv.outages.length} predicted outage${cv.outages.length === 1 ? '' : 's'} so far`}
          {inOutage ? ` · back at ${fmtZ(inOutage[1])}` : ''}
        </span>
        {last && <span className="op-detail">{last.detail}</span>}
      </div>
    </section>
  )
}

/** Simulated products: collects whose "product ready" event fired. */
export function ProductList({ runs, selected, onSelect }: { runs: SpaceRun[]; selected: number | null; onSelect: (r: SpaceRun | null) => void }) {
  const products = runs.filter(hasProduct)
  if (!products.length)
    return <p className="hint pad">No products yet. A tasked collection adds its product here when its "product ready" event fires (simulated: scene outline and metadata, no imagery).</p>
  return (
    <ul className="sr-products pad">
      {products.map((r) => {
        const c = r.collect!
        const ready = r.events.find((e) => e.state === 'product_ready')
        const sel = selected === r.intent_id
        return (
          <li key={r.intent_id}>
            <button className={`opt sr-product ${sel ? 'sel' : ''}`} onClick={() => onSelect(sel ? null : r)} title="Outline the scene on the map">
              <span className="tl-swatch" style={{ background: classColor(c.class_id) }} />
              <span className="opt-name">{c.satellite}</span>
              <span className="opt-num">{c.capability.toUpperCase()}</span>
              <span className="opt-num">{c.resolution_m != null ? `${c.resolution_m.toFixed(2)} m` : 'res ?'}</span>
              <span className="opt-notes">
                collected {fmtZ(c.collect_time)} · ready {fmtZ(c.product_time ?? ready?.scenario_t ?? c.collect_time)} · NORAD {c.norad_id} · i{r.intent_id}
                {c.scene ? '' : ' · scene unknown (area shown)'}
              </span>
            </button>
          </li>
        )
      })}
    </ul>
  )
}
