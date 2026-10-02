import { symbolSvg } from './symbols'
import type { Vehicle } from './types'
import { callsign, domainOf, STALE_MS } from './types'

interface Props {
  vehicles: Vehicle[]
  selected: Set<number>
  onToggle: (sysid: number) => void
  onSelect: (sysids: number[]) => void
}

function linkClass(v: Vehicle) {
  const since = v.link.since_last_ms ?? Infinity
  const loss = v.link.loss_pct ?? 0
  if (since > STALE_MS) return 'bad'
  if (since > 1000 || loss > 10) return 'warn'
  return 'ok'
}

function fmt(n: number | null | undefined, digits = 0, unit = '') {
  return n == null ? '–' : `${n.toFixed(digits)}${unit}`
}

function Row({ v, selected, onToggle }: { v: Vehicle; selected: boolean; onToggle: Props['onToggle'] }) {
  const stale = (v.link.since_last_ms ?? 0) > STALE_MS
  const lc = linkClass(v)
  return (
    <button
      className={`vrow ${selected ? 'sel' : ''} ${stale ? 'stale' : ''}`}
      onClick={() => onToggle(v.sysid)}
    >
      <span className="vrow-check" aria-hidden>{selected ? '■' : '□'}</span>
      <span className="vrow-sym" dangerouslySetInnerHTML={{ __html: symbolSvg(v, { stale, size: 16, labels: false }).svg }} />
      <span className="vrow-main">
        <span className="vrow-name">
          {callsign(v)}
          {v.identity && <span className="vrow-short">{v.identity.short}</span>}
          {v.identity?.sim === 'standin' && (
            <span className="vrow-standin" title="Kinematic stand-in: speaks MAVLink, but not ArduPilot SITL">
              STAND-IN
            </span>
          )}
        </span>
        {v.identity && (
          <span className="vrow-model" title={`${v.identity.name} · ${v.identity.role}`}>
            {v.identity.name} · {v.identity.role}
          </span>
        )}
        <span className="vrow-mode">
          {v.mode ?? '–'}
          {v.armed ? <em className="armed">ARMED</em> : null}
        </span>
      </span>
      <span className="vrow-kin">
        {domainOf(v) === 'subsurface' ? (
          <span title="depth">{v.alt_msl == null ? '–' : `${Math.max(0, -v.alt_msl).toFixed(1)} m dp`}</span>
        ) : domainOf(v) === 'air' ? (
          <span title="altitude above mean sea level">{fmt(v.alt_msl, 0, ' m')}</span>
        ) : (
          <span>&nbsp;</span>
        )}
        <span>{fmt(v.groundspeed, 1, ' m/s')}</span>
      </span>
      <span className={`vrow-link ${lc}`}>
        <span title="time since last message">{stale ? `${Math.floor((v.link.since_last_ms ?? 0) / 1000)}s` : fmt(v.link.since_last_ms, 0, 'ms')}</span>
        <span title="packet loss, last 10 s">{fmt(v.link.loss_pct, 0, '%')} loss</span>
        <span title="round trip time (TIMESYNC)">{fmt(v.link.rtt_ms, 0, 'ms')} rtt</span>
      </span>
    </button>
  )
}

const GROUPS: { title: string; match: (v: Vehicle) => boolean }[] = [
  { title: 'Air', match: (v) => domainOf(v) === 'air' },
  { title: 'Ground', match: (v) => domainOf(v) === 'ground' },
  { title: 'Surface', match: (v) => domainOf(v) === 'surface' },
  { title: 'Subsurface', match: (v) => domainOf(v) === 'subsurface' },
]

export function FleetPanel({ vehicles, selected, onToggle, onSelect }: Props) {
  const ids = (pred: (v: Vehicle) => boolean) => vehicles.filter(pred).map((v) => v.sysid)
  return (
    <aside className="panel fleet">
      <header className="panel-head">
        <h2>Fleet</h2>
        <div className="seg">
          <button onClick={() => onSelect(ids(() => true))}>All</button>
          <button onClick={() => onSelect(ids((v) => domainOf(v) === 'air'))}>Air</button>
          <button onClick={() => onSelect(ids((v) => domainOf(v) === 'ground'))}>Gnd</button>
          <button onClick={() => onSelect(ids((v) => ['surface', 'subsurface'].includes(domainOf(v))))}>Sea</button>
          <button onClick={() => onSelect([])}>None</button>
        </div>
      </header>
      {vehicles.length === 0 ? (
        <p className="empty">
          No vehicles yet. Start the swarm with <code>make sim</code>; vehicles appear here as their first heartbeat
          arrives.
        </p>
      ) : (
        <div className="vlist">
          {GROUPS.map(({ title, match }) => {
            const group = vehicles.filter(match)
            if (!group.length) return null
            return (
              <div key={title}>
                <h3>
                  {title} <span>{group.length}</span>
                </h3>
                {group.map((v) => (
                  <Row key={v.sysid} v={v} selected={selected.has(v.sysid)} onToggle={onToggle} />
                ))}
              </div>
            )
          })}
        </div>
      )}
    </aside>
  )
}
