import { useState } from 'react'
import { ago, BOUNDARY, deliveryOf, officerOf, split, textLength, textProblem, TEXT_MAX, type TaskingItem } from './tasking'
import type { TaskingApi } from './useTasking'
import './tasking.css'

/** Tasking the AI watch officer: type guidance, see what is in force, retire
 *  what no longer applies. Guidance for the officer to weigh, never an order and never a decision
 *  (BOUNDARY says so where it is written), and it reaches the officer only through the bridge: it is
 *  not logged in the audit log and no decision reads it. */

export function TaskingButton({ api, open, onClick }: { api: TaskingApi; open: boolean; onClick: () => void }) {
  if (api.status === 'missing') return null
  const n = api.items.filter((i) => i.active).length
  return (
    <button className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} onClick={onClick} title="Guidance for the AI watch officer (not an order, not a decision)">
      Tasking
      {n > 0 && <span className="ix-badge tko-badge">{n}</span>}
    </button>
  )
}

/** How far the watch officer has an item: one tick sent, two seen, two in colour processed; the
 *  times and the update number in its title. Nothing from a bridge that does not say. */
function DeliveryChip({ item, nowMs }: { item: TaskingItem; nowMs: number }) {
  const d = deliveryOf(item, nowMs)
  if (!d) return null
  return (
    <span className={`tko-dl ${d.tone}`} title={d.title} aria-label={`${d.label}: ${d.title}`}>
      <span className="tko-ticks" aria-hidden="true">
        {d.ticks === 2 ? '✓✓' : '✓'}
      </span>
      {d.label}
    </span>
  )
}

export function TaskingPanel({ api, now, onClose, banners }: { api: TaskingApi; now: () => number; onClose: () => void; banners?: React.ReactNode }) {
  const [text, setText] = useState('')
  const [busy, setBusy] = useState(false)
  const [showRetired, setShowRetired] = useState(false)
  const { active, retired } = split(api.items)
  const problem = text ? textProblem(text) : null
  const nowMs = now()
  const officer = officerOf(api.reader)
  const send = async () => {
    if (textProblem(text)) return
    setBusy(true)
    const item = await api.post(text)
    setBusy(false)
    if (item) setText('')
  }
  return (
    <aside className="ix tko" aria-label="Tasking the AI watch officer">
      <div className="ix-top">
        <h2>Tasking the officer</h2>
        <span className="q-sub">guidance</span>
        <button className="ghost close" onClick={onClose} aria-label="Close tasking">
          ×
        </button>
      </div>
      {banners && <div className="ix-banners">{banners}</div>}
      <div className="ix-scroll">
        <p className="tko-boundary" role="note">
          {BOUNDARY}
        </p>
        {api.status === 'missing' && (
          <p className="empty sw-missing" role="status">
            This bridge has no tasking channel (<code>/api/tasking</code>).
          </p>
        )}
        {api.status !== 'missing' && (
          <form
            className="tko-form"
            onSubmit={(e) => {
              e.preventDefault()
              void send()
            }}
          >
            <label>
              <span>Guidance to the officer</span>
              <textarea
                rows={4}
                value={text}
                placeholder="e.g. Prioritise the ferry lane. Hold your attention on the north pickets. Ignore the sailboats north of the span."
                onChange={(e) => setText(e.target.value)}
                aria-invalid={problem != null}
                aria-describedby="tko-count"
              />
            </label>
            <div className="tko-foot">
              <span id="tko-count" className={`tko-count ${textLength(text) > TEXT_MAX ? 'bad' : ''}`}>
                {textLength(text)}/{TEXT_MAX}
              </span>
              <button type="submit" className="primary" disabled={busy || !text.trim() || problem != null}>
                {busy ? 'Sending...' : 'Send guidance'}
              </button>
            </div>
            {problem && <p className="error">{problem}</p>}
            {api.error && <p className="error">{api.error}</p>}
          </form>
        )}
        <h3 className="ix-h">
          In force <span>{active.length}</span>
          <small>the officer reads these each time it wakes</small>
        </h3>
        {api.status === 'ok' && officer && (
          <p className={`tko-officer ${officer.connected ? 'on' : 'off'}`} role="status">
            {officer.text}
          </p>
        )}
        {api.status === 'ok' && active.length === 0 && <p className="empty muted">No guidance in force: the officer works from its brief alone.</p>}
        {active.map((i) => (
          <div key={i.id} className="tko-item">
            <p>{i.text}</p>
            <div className="tko-meta">
              <span>
                {i.by} · {ago(i.at_t, nowMs)} ago
              </span>
              <DeliveryChip item={i} nowMs={nowMs} />
              <button className="ghost mini" onClick={() => void api.deactivate(i.id)} title="Stop sending this guidance to the officer">
                Retire
              </button>
            </div>
          </div>
        ))}
        {retired.length > 0 && (
          <>
            <button className="ghost mini tko-more" aria-expanded={showRetired} onClick={() => setShowRetired(!showRetired)}>
              {showRetired ? 'Hide' : 'Show'} retired ({retired.length})
            </button>
            {showRetired &&
              retired.map((i) => (
                <div key={i.id} className="tko-item retired">
                  <p>{i.text}</p>
                  <div className="tko-meta">
                    <span>
                      {i.by} · {ago(i.at_t, nowMs)} ago · retired
                    </span>
                    <DeliveryChip item={i} nowMs={nowMs} />
                  </div>
                </div>
              ))}
          </>
        )}
      </div>
    </aside>
  )
}
