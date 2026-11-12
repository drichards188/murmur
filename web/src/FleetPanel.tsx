import { memo, useEffect, useMemo, useRef, useState } from 'react'
import { emuCause } from './linkEmu'
import { type Reveal, scrollSpanIntoView, useFlash } from './reveal'
import { symbolSvg } from './symbols'
import { type Swarm, swarmColor, swarmGroups } from './swarmsModel'
import './swarms.css'
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
  /** vehicles just selected from outside the list: scroll to the first and mark it */
  reveal?: Reveal | null
  /** bd murmur-4tq: vehicles whose sensor reach is under half its clear day's ("EO 18% · fog") */
  reachBadges?: Map<number, { text: string; title: string }>
  /** the bridge's swarm summary (swarmsModel.ts), for the groups' order, names and colours: a fleet
   *  with swarms is listed swarm by swarm */
  swarms?: Pick<Swarm, 'id' | 'name' | 'color' | 'role'>[]
}

function linkClass(v: Vehicle) {
  // a lite snapshot says lost or not instead of when the last packet came
  const since = v.link.since_last_ms ?? (v.link.stale == null || v.link.stale ? Infinity : 0)
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
  flash: boolean
  onToggle: Props['onToggle']
  onDeselect: Props['onDeselect']
  onDetails: Props['onDetails']
  reach?: { text: string; title: string }
}

/** Two controls: the left strip selects the vehicle for tasking; the rest opens its details. */
const Row = memo(function Row({ v, selected, open, flash, onToggle, onDeselect, onDetails, reach }: RowProps) {
  const stale = isStale(v)
  const lc = linkClass(v)
  return (
    <div
      className={`vrow ${selected ? 'sel' : ''} ${stale ? 'stale' : ''} ${open ? 'open' : ''} ${flash ? 'flash' : ''}`}
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
            <span className="vrow-standin" title="Dead reckoning: no GNSS fix, position from inertial navigation and DVL (sub) or airspeed (fixed-wing), error growing with distance">
              DR
            </span>
          )}
          {stale && v.estimate && (
            <span className="vrow-standin" title={`Silent: the map shows where it probably is (${v.estimate.detail})`}>
              EST
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
          {reach && (
            <span className="vrow-standin vrow-reach" title={reach.title}>
              {reach.text}
            </span>
          )}
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
        <span title="time since last message">
          {stale || (v.link.since_last_ms ?? 0) >= 1000
            ? `${Math.floor((v.link.since_last_ms ?? 0) / 1000)}s`
            : v.link.since_last_ms == null && v.link.stale === false
              ? 'live' // a lite snapshot (out of view): the link is heard from, no exact age
              : fmt(v.link.since_last_ms, 0, 'ms')}
        </span>
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
})

const GROUPS: { title: string; match: (v: Vehicle) => boolean }[] = [
  { title: 'Air', match: (v) => domainOf(v) === 'air' },
  { title: 'Ground', match: (v) => domainOf(v) === 'ground' },
  { title: 'Water', match: (v) => domainOf(v) === 'surface' },
  { title: 'Subsurface', match: (v) => domainOf(v) === 'subsurface' },
]

// Windowed list (murmur-ica): every row and group header has a fixed height (index.css keeps
// them in step), so only the rows in view, plus a margin, are in the DOM, whatever the fleet size.
export const ROW_H = 84
export const HEAD_H = 32
const OVERSCAN_PX = 400

type Item = { kind: 'head'; title: string; count: number; top: number; color?: string } | { kind: 'row'; v: Vehicle; top: number }

/** The rows and headers, top to bottom: by swarm when the fleet has swarms (swarmGroups), else by
 *  domain. */
function layout(vehicles: Vehicle[], swarms: Props['swarms']): { items: Item[]; height: number } {
  const items: Item[] = []
  let top = 0
  const bySwarm = swarmGroups(vehicles, swarms)
  const colors = new Map((swarms ?? []).map((s) => [s.id, swarmColor(s)]))
  const groups = bySwarm
    ? bySwarm.map((g) => ({ title: g.title, group: g.vehicles, color: g.id ? (colors.get(g.id) ?? '#7d8b9a') : undefined }))
    : GROUPS.map(({ title, match }) => ({ title, group: vehicles.filter(match), color: undefined }))
  for (const { title, group, color } of groups) {
    if (!group.length) continue
    items.push({ kind: 'head', title, count: group.length, top, color })
    top += HEAD_H
    for (const v of group) {
      items.push({ kind: 'row', v, top })
      top += ROW_H
    }
  }
  return { items, height: top }
}

/** first item whose bottom is below y (items are sorted by top) */
function firstBelow(items: Item[], y: number) {
  let lo = 0
  let hi = items.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    const it = items[mid]
    if (it.top + (it.kind === 'head' ? HEAD_H : ROW_H) <= y) lo = mid + 1
    else hi = mid
  }
  return lo
}

export function FleetPanel({ vehicles, selected, onToggle, onDeselect, onDetails, detailsUid, onSelect, reveal = null, reachBadges, swarms }: Props) {
  const ids = (pred: (v: Vehicle) => boolean) => vehicles.filter(pred).map((v) => v.uid)
  const list = useRef<HTMLDivElement>(null)
  const [scroll, setScroll] = useState({ top: 0, height: 800 })
  const { items, height } = useMemo(() => layout(vehicles, swarms), [vehicles, swarms])
  // the list element exists only once there are vehicles
  const hasList = vehicles.length > 0

  // the first revealed vehicle in list order; scroll just enough to show it
  const itemsRef = useRef(items)
  itemsRef.current = items
  const firstRevealed = (ids: string[]) => {
    const want = new Set(ids)
    const it = itemsRef.current.find((i) => i.kind === 'row' && want.has(String(i.v.uid)))
    return it && it.kind === 'row' ? String(it.v.uid) : null
  }
  const flash = useFlash(reveal, firstRevealed)
  // the rows take these, the same functions every render, so that a row whose vehicle did not
  // change is not rendered again; they call the latest callbacks (murmur-4xq3)
  const latest = useRef({ onToggle, onDeselect, onDetails })
  latest.current = { onToggle, onDeselect, onDetails }
  const act = useMemo(
    () => ({
      onToggle: (uid: number) => latest.current.onToggle(uid),
      onDeselect: (uid: number) => latest.current.onDeselect(uid),
      onDetails: (uid: number) => latest.current.onDetails(uid),
    }),
    [],
  )
  useEffect(() => {
    const el = list.current
    if (!el || !reveal) return
    const id = firstRevealed(reveal.ids)
    const it = itemsRef.current.find((i) => i.kind === 'row' && String(i.v.uid) === id)
    if (it) scrollSpanIntoView(el, it.top, ROW_H)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reveal])

  useEffect(() => {
    const el = list.current
    if (!el) return
    let raf = 0
    const read = () => {
      raf = 0
      setScroll((s) => (s.top === el.scrollTop && s.height === el.clientHeight ? s : { top: el.scrollTop, height: el.clientHeight }))
    }
    const onScroll = () => {
      if (!raf) raf = requestAnimationFrame(read)
    }
    read()
    el.addEventListener('scroll', onScroll, { passive: true })
    const ro = new ResizeObserver(onScroll)
    ro.observe(el)
    return () => {
      el.removeEventListener('scroll', onScroll)
      ro.disconnect()
      cancelAnimationFrame(raf)
    }
  }, [hasList])

  const from = firstBelow(items, scroll.top - OVERSCAN_PX)
  const shown: Item[] = []
  for (let i = from; i < items.length && items[i].top < scroll.top + scroll.height + OVERSCAN_PX; i++) shown.push(items[i])

  return (
    <aside className="panel fleet" data-tutorial="fleet">
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
        <div className="vlist" ref={list}>
          <div className="vlist-inner" style={{ height }}>
            {shown.map((it) =>
              it.kind === 'head' ? (
                <h3 key={`h:${it.title}`} className={`vlist-head${it.color ? ' sw-head' : ''}`} style={{ top: it.top }} title={it.title}>
                  {it.color && <i style={{ background: it.color }} aria-hidden />}
                  {it.title} <span>{it.count}</span>
                </h3>
              ) : (
                <div key={it.v.uid} className="vlist-item" style={{ top: it.top }}>
                  <Row
                    v={it.v}
                    selected={selected.has(it.v.uid)}
                    open={detailsUid === it.v.uid}
                    flash={flash === String(it.v.uid)}
                    onToggle={act.onToggle}
                    onDeselect={act.onDeselect}
                    onDetails={act.onDetails}
                    reach={reachBadges?.get(it.v.uid)}
                  />
                </div>
              ),
            )}
          </div>
        </div>
      )}
    </aside>
  )
}
