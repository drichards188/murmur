import type { SwarmsFeed } from './swarms'
import { ROLE_LABEL, ROLE_ORDER, type Swarm, type SwarmAlert, swarmColor, swarmTone, taskLine } from './swarmsModel'
import './swarms.css'

/** The Swarms panel (docs/DECISIONS.md, "Swarm units"): every swarm of the fleet from the bridge's
 *  summary (GET /api/swarms, about once a second), grouped by role: how many it has, how many are
 *  navigation-ready, unheard or low on battery, what its members run. A click selects all its
 *  members, so the order form sends one order (one intent) to the whole swarm, and frames it.
 *  Without the summary it says so: it never makes swarms up from anything else. */

interface Props {
  feed: SwarmsFeed
  /** the swarm the selection is exactly, if any */
  selected: string | null
  /** the inbox's swarm alerts, for a mark on the swarms that have one */
  alerts: SwarmAlert[]
  onSelect: (s: Swarm) => void
  onClose: () => void
}

export function SwarmsButton({ feed, open, onClick }: { feed: SwarmsFeed; open: boolean; onClick: () => void }) {
  const swarms = feed.doc?.swarms ?? []
  const trouble = swarms.filter((s) => swarmTone(s) !== 'ok').length
  const title = feed.status === 'missing' ? 'Swarms: no swarm data from the bridge' : `${swarms.length} swarms, ${trouble} with members unheard or low on battery`
  return (
    <button className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} onClick={onClick} title={title} aria-label={title}>
      Swarms
      {trouble > 0 && <span className="ix-badge sw-badge">{trouble}</span>}
    </button>
  )
}

function Row({ s, selected, alerts, onSelect }: { s: Swarm; selected: boolean; alerts: number; onSelect: () => void }) {
  const tone = swarmTone(s)
  return (
    <button className={`sw-row ${tone} ${selected ? 'sel' : ''}`} onClick={onSelect} aria-pressed={selected} title={`Select all ${s.count} of ${s.id} for one order`}>
      <span className="sw-chip" style={{ background: swarmColor(s) }} aria-hidden />
      <span className="sw-main">
        <span className="sw-name">
          <b>{s.id}</b> {s.name}
          {alerts > 0 && <span className="sw-alerts" title="swarm alerts in the Inbox">{alerts} alert{alerts === 1 ? '' : 's'}</span>}
        </span>
        <span className="sw-task">{taskLine(s)}</span>
      </span>
      <span className="sw-nums">
        <span title="navigation-ready of all its members">
          <b>{s.ready}</b>/{s.count} ready
        </span>
        <span className={s.lost_link ? 'bad' : 'dim'} title="members whose link is lost">
          <b>{s.lost_link}</b> lost link
        </span>
        <span className={s.low_battery ? 'warn' : 'dim'} title="members low on battery">
          <b>{s.low_battery}</b> low batt
        </span>
      </span>
    </button>
  )
}

export function SwarmsPanel({ feed, selected, alerts, onSelect, onClose }: Props) {
  const swarms = feed.doc?.swarms ?? []
  const total = swarms.reduce((n, s) => n + s.count, 0)
  const alertsBy = new Map<string, number>()
  for (const a of alerts) alertsBy.set(a.swarm, (alertsBy.get(a.swarm) ?? 0) + 1)
  const roles = [...ROLE_ORDER, ...new Set(swarms.map((s) => s.role).filter((r) => !ROLE_ORDER.includes(r)))]
  return (
    <aside className="ix sw" aria-label="Swarms">
      <div className="ix-top">
        <h2>Swarms</h2>
        <span className="q-sub">{swarms.length ? `${swarms.length} swarms · ${total.toLocaleString()} vehicles` : ''}</span>
        <button className="ghost close" onClick={onClose} aria-label="Close the swarms">
          ×
        </button>
      </div>
      <div className="ix-scroll">
        {feed.status === 'missing' && (
          <p className="empty sw-missing" role="status">
            No swarm data: the bridge does not serve <code>/api/swarms</code>. Swarms need a bridge that summarises them.
          </p>
        )}
        {feed.status === 'error' && (
          <p className="empty sw-missing" role="status">
            No swarm data: {feed.error}
          </p>
        )}
        {feed.status === 'loading' && <p className="empty muted">Asking the bridge for its swarms...</p>}
        {feed.status === 'ok' && swarms.length === 0 && <p className="empty muted">This fleet has no swarms.</p>}
        {feed.status === 'ok' && feed.error && <p className="empty muted">Last summary kept: {feed.error}</p>}
        {roles.map((role) => {
          const group = swarms.filter((s) => s.role === role)
          if (!group.length) return null
          return (
            <section key={role} className="sw-group">
              <h3 className="ix-h">
                {ROLE_LABEL[role] ?? role} <span>{group.length}</span>
                <small>{group.reduce((n, s) => n + s.count, 0).toLocaleString()} vehicles</small>
              </h3>
              {group.map((s) => (
                <Row key={s.id} s={s} selected={selected === s.id} alerts={alertsBy.get(s.id) ?? 0} onSelect={() => onSelect(s)} />
              ))}
            </section>
          )
        })}
        {swarms.length > 0 && (
          <p className="ix-foot">A click selects every member of a swarm: the order form then sends one order, one intent, to all of them.</p>
        )}
      </div>
    </aside>
  )
}
