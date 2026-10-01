import { symbolSvg } from './symbols'
import type { Vehicle } from './types'
import { AIR_KINDS, callsign, SEA_KINDS, STALE_MS } from './types'

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
        <span className="vrow-name">{callsign(v)}</span>
        <span className="vrow-mode">
          {v.mode ?? '–'}
          {v.armed ? <em className="armed">ARMED</em> : null}
        </span>
      </span>
      <span className="vrow-kin">
        {v.kind === 'sub' ? (
          <span title="depth">{v.alt_rel == null ? '–' : `${Math.max(0, -v.alt_rel).toFixed(1)} m dp`}</span>
        ) : v.kind && AIR_KINDS.includes(v.kind) ? (
          <span title="height above home">{fmt(v.alt_rel, 0, ' m')}</span>
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
  { title: 'Air', match: (v) => !!v.kind && AIR_KINDS.includes(v.kind) },
  { title: 'Surface', match: (v) => v.kind === 'boat' },
  { title: 'Subsurface', match: (v) => v.kind === 'sub' },
  { title: 'Ground', match: (v) => v.kind === 'tracker' || v.kind === 'unknown' || v.kind == null },
]

export function FleetPanel({ vehicles, selected, onToggle, onSelect }: Props) {
  const ids = (pred: (v: Vehicle) => boolean) => vehicles.filter(pred).map((v) => v.sysid)
  return (
    <aside className="panel fleet">
      <header className="panel-head">
        <h2>Fleet</h2>
        <div className="seg">
          <button onClick={() => onSelect(ids(() => true))}>All</button>
          <button onClick={() => onSelect(ids((v) => !!v.kind && AIR_KINDS.includes(v.kind)))}>Air</button>
          <button onClick={() => onSelect(ids((v) => !!v.kind && SEA_KINDS.includes(v.kind)))}>Sea</button>
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
