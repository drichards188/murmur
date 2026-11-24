// Satellite collect and cover runs as the console shows them: the command log card, the
// products list and the coverage watches. Runs come from the bridge's decisions message
// (`space`), times are scenario time (ms). A running one can be cancelled from its card
// (rules 11): the bridge says what the cancel meant for the pass.
import { useState } from 'react'
import type { SpaceRun } from './decisions'
import { fmtZ, hasProduct, intentAction } from './decisions'
import { classColor } from './space'
import './spaceTasking.css'

const STATE_WORD: Record<string, string> = { tasked: 'tasked', collected: 'collected', product_ready: 'product ready', missed: 'missed', cancelled: 'cancelled' }

/** A collect's state in a word or two (a re-check keeps the state it checked). */
function collectWord(run: SpaceRun): string {
  const s = (run.state === 'rechecked' ? [...run.events].reverse().find((e) => e.state !== 'rechecked')?.state : run.state) ?? 'tasked'
  return STATE_WORD[s] ?? s.replace(/_/g, ' ')
}

/** Cancel a running collect or cover (rules 11): a collect's tasking request is withdrawn
 *  before its pass, a pass already under way cannot be recalled (its product is no longer
 *  awaited); a cover's watch ends. The bridge logs what the cancel meant. */
function CancelButton({ run, enabled }: { run: SpaceRun; enabled: boolean }) {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  if (run.outcome || !enabled) return null
  const cancel = async () => {
    setBusy(true)
    setErr(null)
    try {
      await intentAction(run.intent_id, 'cancel', run.kind === 'collect' ? 'cancelled from the command log' : 'watch ended from the command log')
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <>
      <button
        className="ghost mini sr-cancel"
        disabled={busy}
        onClick={() => void cancel()}
        title={run.kind === 'collect' ? 'Withdraw the tasking request. A pass already under way cannot be recalled; its product is then no longer awaited.' : 'End the coverage watch.'}
      >
        {busy ? 'Cancelling…' : 'Cancel'}
      </button>
      {err && <span className="sr-bad">{err}</span>}
    </>
  )
}

function outcomeLabel(run: SpaceRun) {
  return run.outcome === 'cancelled' ? 'cancelled' : run.outcome === 'failed' ? 'failed' : 'ended'
}

/** One collect or cover in the command log: what and by what, its number, its state in a word,
 *  when it lands (absolute scenario times), its latest event; a collect's resolution folded. */
export function SpaceRunCard({ run, scen, onOpen, cancellable = false }: { run: SpaceRun; scen: number; onOpen?: (intentId: number) => void; cancellable?: boolean }) {
  const last = run.events[run.events.length - 1]
  if (run.kind === 'collect' && run.collect) {
    const c = run.collect
    const word = collectWord(run)
    return (
      <section className={`tcard sr ${run.outcome ?? 'running'}`}>
        <div className="tcard-head">
          <strong>COLLECT</strong>
          <button className="ghost mini sr-link" onClick={() => onOpen?.(run.intent_id)} title="See the intent in Decisions">
            i{run.intent_id}
          </button>
          <span className="sr-sat">{c.satellite}</span>
          <CancelButton run={run} enabled={cancellable} />
        </div>
        <div className="op">
          <b className={`op-word${run.state === 'missed' ? ' bad' : ''}`}>{word}</b>
          <span className="op-detail">
            collect {fmtZ(c.collect_time)} · product {c.product_time != null ? fmtZ(c.product_time) : 'time unknown'}
          </span>
          {last && <span className={`op-detail ${run.outcome === 'failed' ? 'sr-bad' : ''}`}>{last.detail}</span>}
          {c.resolution_m != null && (
            <details className="bluf-more op-detail">
              <summary>More</summary>
              <div className="bluf-detail">
                {c.capability} at {c.resolution_m.toFixed(2)} m
              </div>
            </details>
          )}
        </div>
      </section>
    )
  }
  const cv = run.cover
  if (!cv) return null
  const label = cv.service.type === 'gnss' ? `${cv.service.class.toUpperCase()} GNSS` : `${cv.service.class} SATCOM`
  const inOutage = cv.outages.find(([a, b]) => scen >= a && scen < b)
  const seen = cv.outages.filter(([a]) => scen >= a).length
  return (
    <section className={`tcard sr ${run.outcome ?? 'running'}`}>
      <div className="tcard-head">
        <strong>COVER</strong>
        <button className="ghost mini sr-link" onClick={() => onOpen?.(run.intent_id)} title="See the intent in Decisions">
          i{run.intent_id}
        </button>
        <span className="sr-sat">{label}</span>
        <span className={`sr-badge ${inOutage && !run.outcome ? 'bad' : ''}`}>{run.outcome ? outcomeLabel(run) : inOutage ? 'OUTAGE' : scen < cv.window.from ? 'waiting' : 'covered'}</span>
        <CancelButton run={run} enabled={cancellable} />
      </div>
      <div className="op">
        <span className="op-name">watch</span>
        <span className="op-detail sr-first">
          {fmtZ(cv.window.from)} to {fmtZ(cv.window.to)}
        </span>
        <span className="op-detail">
          {cv.service.type === 'gnss'
            ? cv.summary
            : cv.outages.length === 0
              ? 'no outage predicted anywhere over the area in the window'
              : `${seen} of ${cv.outages.length} predicted outage${cv.outages.length === 1 ? '' : 's'} over the area so far`}
          {inOutage && !run.outcome ? ` · back at ${fmtZ(inOutage[1])}` : ''}
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
