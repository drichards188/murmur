import { emuCause } from './linkEmu'
import { symbolSvg } from './symbols'
import type { Vehicle } from './types'
import { callsign, domainOf, isStale, staleMs } from './types'

interface Props {
  vehicles: Vehicle[]
  selected: Set<number>
  onToggle: (uid: number) => void
  /** right click: deselect this vehicle */
  onDeselect: (uid: number) => void
  /** row body click: show this vehicle's details */
  onDetails: (uid: number) => void
  detailsUid: number | null
  onSelect: (uids: number[]) => void
}

function linkClass(v: Vehicle) {
  const since = v.link.since_last_ms ?? Infinity
  const loss = v.link.loss_pct ?? 0
  if (since > staleMs(v)) return 'bad'
  if (since > (v.link.expected_interval_ms ?? 1000) || loss > 10) return 'warn'
  return 'ok'
}

function fmt(n: number | null | undefined, digits = 0, unit = '') {
  return n == null ? '–' : `${n.toFixed(digits)}${unit}`
}

interface RowProps {
  v: Vehicle
  selected: boolean
  open: boolean
  onToggle: Props['onToggle']
  onDeselect: Props['onDeselect']
  onDetails: Props['onDetails']
}

/** Two controls: the left strip selects the vehicle for tasking; the rest opens its details. */
function Row({ v, selected, open, onToggle, onDeselect, onDetails }: RowProps) {
  const stale = isStale(v)
  const lc = linkClass(v)
  return (
    <div
      className={`vrow ${selected ? 'sel' : ''} ${stale ? 'stale' : ''} ${open ? 'open' : ''}`}
      onContextMenu={(e) => {
        e.preventDefault()
        onDeselect(v.uid)
      }}
    >
      <button
        className="vrow-select"
        aria-pressed={selected}
        aria-label={`${selected ? 'Deselect' : 'Select'} ${callsign(v)} for tasking`}
        title={selected ? 'Selected for tasking (click or right-click to deselect)' : 'Select for tasking'}
        onClick={() => onToggle(v.uid)}
      >
        <span className="vrow-box" aria-hidden>
          {selected ? '✓' : ''}
        </span>
      </button>
      <button className="vrow-body" aria-expanded={open} title="Show details" onClick={() => onDetails(v.uid)}>
      <span className="vrow-sym" dangerouslySetInnerHTML={{ __html: symbolSvg(v, { stale, size: 16, labels: false }).svg }} />
      <span className="vrow-main">
        <span className="vrow-name">
          {callsign(v)}
          {v.identity && <span className="vrow-short">{v.identity.short}</span>}
          {v.identity?.armed ? (
            <span className="vrow-weapons" title="Weapons-carrying platform: any task on it needs a human decision">
              WEAPONS
            </span>
          ) : null}
          {v.identity?.sim === 'standin' && (
            <span className="vrow-standin" title="Kinematic stand-in: speaks MAVLink, but not ArduPilot SITL">
              STAND-IN
            </span>
          )}
          {v.nav_source === 'dead_reckoning' && (
            <span className="vrow-standin" title="Dead reckoning: no GNSS fix, position from inertial navigation and DVL, error growing with distance">
              DR
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
          {v.armed ? (
            <em className="motors-on" title="MAVLink arm state: motors enabled. Says nothing about weapons.">
              motors on
            </em>
          ) : null}
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
        <span title="time since last message">{stale || (v.link.since_last_ms ?? 0) >= 1000 ? `${Math.floor((v.link.since_last_ms ?? 0) / 1000)}s` : fmt(v.link.since_last_ms, 0, 'ms')}</span>
        <span title="packet loss, last 10 s">{fmt(v.link.loss_pct, 0, '%')} loss</span>
        <span title="round trip time (TIMESYNC)">{fmt(v.link.rtt_ms, 0, 'ms')} rtt</span>
        {v.link.emu && v.link.emu.state !== 'ok' && (
          <span className={`vrow-emu ${v.link.emu.state}`} title={`Link emulator: ${v.link.emu.state}${v.link.emu.cause ? `, ${emuCause(v.link.emu.cause)}` : ''}`}>
            {v.link.emu.state === 'down' ? 'link down' : 'degraded'}: {emuCause(v.link.emu.cause) || '?'}
          </span>
        )}
      </span>
      </button>
    </div>
  )
}

const GROUPS: { title: string; match: (v: Vehicle) => boolean }[] = [
  { title: 'Air', match: (v) => domainOf(v) === 'air' },
  { title: 'Ground', match: (v) => domainOf(v) === 'ground' },
  { title: 'Water', match: (v) => domainOf(v) === 'surface' },
  { title: 'Subsurface', match: (v) => domainOf(v) === 'subsurface' },
]

export function FleetPanel({ vehicles, selected, onToggle, onDeselect, onDetails, detailsUid, onSelect }: Props) {
  const ids = (pred: (v: Vehicle) => boolean) => vehicles.filter(pred).map((v) => v.uid)
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
                  <Row
                    key={v.uid}
                    v={v}
                    selected={selected.has(v.uid)}
                    open={detailsUid === v.uid}
                    onToggle={onToggle}
                    onDeselect={onDeselect}
                    onDetails={onDetails}
                  />
                ))}
              </div>
            )
          })}
        </div>
      )}
    </aside>
  )
}
