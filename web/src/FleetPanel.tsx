import { memo, useEffect, useMemo, useRef, useState } from 'react'
import { type GroupContext, groupView, type GroupView, latestOps, STATE_ORDER, STATE_WORDS } from './exceptions'
import { emuCause } from './linkEmu'
import { type Reveal, scrollSpanIntoView, useFlash } from './reveal'
import { symbolSvg } from './symbols'
import { type Swarm, swarmColor, swarmGroups } from './swarmsModel'
import './nodeLoss.css'
import './swarms.css'
import type { Tasking, Vehicle } from './types'
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
  /** the orders' ops (a member on its order's active op is on station) */
  taskings?: Tasking[]
  /** vehicles with an inbox `damaged` alert, and with an alert of their own (not folded into
   *  their group's): exceptions, listed on their own */
  damaged?: ReadonlySet<number>
  alerted?: ReadonlySet<number>
  /** rules 30: our vehicles an operator confirmed lost (nodeLoss.ts), by uid: tagged LOST, not EST */
  lost?: ReadonlyMap<number, unknown>
  /** the map legend's Show all for lists: every group open */
  expandAll?: boolean
  /** how many vehicles the folded groups do not list (the map legend counts them) */
  onFolded?: (n: { vehicles: number; groups: number }) => void
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
  /** rules 30: confirmed lost by an operator */
  lost?: boolean
}

/** Two controls: the left strip selects the vehicle for tasking; the rest opens its details. */
const Row = memo(function Row({ v, selected, open, flash, onToggle, onDeselect, onDetails, reach, lost = false }: RowProps) {
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
          {stale && lost ? (
            <span className="vrow-standin vrow-lost" title="Confirmed lost by an operator: out of every engagement keep-out, it takes no order (Review)">
              LOST
            </span>
          ) : (
            stale &&
            v.estimate && (
              <span className="vrow-standin" title={`Silent: the map shows where it probably is (${v.estimate.detail})`}>
                EST
              </span>
            )
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
/** a group's row (name, counts by state) and the line under a folded group's listed members */
export const GROUP_H = 54
export const MORE_H = 28
const OVERSCAN_PX = 400

type Item =
  | { kind: 'head'; title: string; count: number; top: number; color?: string }
  | { kind: 'group'; id: string; title: string; color: string; members: Vehicle[]; view: GroupView<Vehicle>; expanded: boolean; top: number }
  | { kind: 'row'; v: Vehicle; top: number }
  | { kind: 'more'; id: string; text: string; top: number }

const HEIGHT: Record<Item['kind'], number> = { head: HEAD_H, group: GROUP_H, row: ROW_H, more: MORE_H }

/** The rows and headers, top to bottom. A fleet with groups (swarmGroups) by exception
 *  (docs/research/saturation-display.md R6): one row per group with its members counted by state
 *  (on station, moving, returning, idle, lost link, damaged), and under it only its exceptions
 *  (details open, just revealed, damaged, lost link, an alert of its own, selected; at most
 *  GROUP_SHOWN) unless the group is open, then every member, exceptions first. A fleet without
 *  groups by domain, every vehicle, as before. */
function layout(vehicles: Vehicle[], swarms: Props['swarms'], ctx: GroupContext, open: ReadonlySet<string>, expandAll: boolean): { items: Item[]; height: number } {
  const items: Item[] = []
  let top = 0
  const bySwarm = swarmGroups(vehicles, swarms)
  if (bySwarm) {
    const colors = new Map((swarms ?? []).map((s) => [s.id, swarmColor(s)]))
    for (const g of bySwarm) {
      if (!g.vehicles.length) continue
      const id = g.id ?? 'none'
      const expanded = expandAll || open.has(id)
      const view = groupView(g.vehicles, ctx, expanded)
      items.push({ kind: 'group', id, title: g.title, color: g.id ? (colors.get(g.id) ?? '#7d8b9a') : '#7d8b9a', members: g.vehicles, view, expanded, top })
      top += GROUP_H
      for (const v of view.shown) {
        items.push({ kind: 'row', v, top })
        top += ROW_H
      }
      // under listed members, the line that opens the rest (the group's row itself opens it too)
      if (view.folded > 0 && view.shown.length > 0) {
        const need = view.foldedExceptions ? `, ${view.foldedExceptions} more to look at` : ''
        items.push({ kind: 'more', id, text: `+${view.folded} more${need} · show all ${g.vehicles.length}`, top })
        top += MORE_H
      }
    }
    return { items, height: top }
  }
  for (const { title, match } of GROUPS) {
    const group = vehicles.filter(match)
    if (!group.length) continue
    items.push({ kind: 'head', title, count: group.length, top })
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
    if (it.top + HEIGHT[it.kind] <= y) lo = mid + 1
    else hi = mid
  }
  return lo
}

const NONE: ReadonlySet<number> = new Set()

/** A group's row: its select box (every member, for one order), its name and size, its members
 *  counted by state (lost link and damaged in the alert colour), and a click to open or fold it. */
function GroupRow({ it, selected, onSelect, onOpen }: { it: Extract<Item, { kind: 'group' }>; selected: Set<number>; onSelect: (uids: number[]) => void; onOpen: () => void }) {
  const { view, members } = it
  const all = view.selected === members.length
  const some = view.selected > 0 && !all
  return (
    <div className={`vgroup${all ? ' sel' : ''}`}>
      <button
        className="vrow-select"
        aria-pressed={all}
        aria-label={`${all ? 'Deselect' : 'Select'} every member of ${it.title}`}
        title={all ? 'Every member selected (click to deselect them)' : 'Select every member, for one order to the group'}
        onClick={() => onSelect(all ? [...selected].filter((u) => !members.some((v) => v.uid === u)) : members.map((v) => v.uid))}
      >
        <span className="vrow-box" aria-hidden>
          {all ? '✓' : some ? '–' : ''}
        </span>
      </button>
      <button className="vgroup-body" aria-expanded={it.expanded} onClick={onOpen} title={it.expanded ? 'Fold: list only its exceptions' : 'Open: list every member, exceptions first'}>
        <span className="vgroup-name">
          <i style={{ background: it.color }} aria-hidden />
          <span className="vgroup-title">{it.title}</span>
          <b>{members.length}</b>
          <span className="vgroup-caret" aria-hidden>
            {it.expanded ? '▾' : '▸'}
          </span>
        </span>
        <span className="vgroup-counts">
          {STATE_ORDER.filter((s) => view.counts[s] > 0).map((s) => (
            <span key={s} className={`vg-${s}`}>
              {view.counts[s]} {STATE_WORDS[s]}
            </span>
          ))}
          {view.selected > 0 && <span className="vg-sel">{view.selected} selected</span>}
        </span>
      </button>
    </div>
  )
}

export function FleetPanel({ vehicles, selected, onToggle, onDeselect, onDetails, detailsUid, onSelect, reveal = null, reachBadges, swarms, taskings, damaged = NONE, alerted = NONE, lost, expandAll = false, onFolded }: Props) {
  const ids = (pred: (v: Vehicle) => boolean) => vehicles.filter(pred).map((v) => v.uid)
  const list = useRef<HTMLDivElement>(null)
  const [scroll, setScroll] = useState({ top: 0, height: 800 })
  // the groups the person opened (a view setting of this console)
  const [open, setOpen] = useState<ReadonlySet<string>>(() => new Set())
  const toggleGroup = (id: string) =>
    setOpen((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  const opStates = useMemo(() => latestOps(taskings ?? []), [taskings])
  const revealUid = reveal?.ids.length ? Number(reveal.ids[0]) : null
  const { items, height } = useMemo(
    () => layout(vehicles, swarms, { latest: opStates, damaged, alerted, selected, open: detailsUid, flash: revealUid }, open, expandAll),
    [vehicles, swarms, opStates, damaged, alerted, selected, detailsUid, revealUid, open, expandAll],
  )
  // what the folded groups do not list, for the map legend
  let foldedVehicles = 0
  let foldedGroups = 0
  for (const it of items) {
    if (it.kind !== 'group' || it.view.folded === 0) continue
    foldedVehicles += it.view.folded
    foldedGroups++
  }
  const foldedRef = useRef(onFolded)
  foldedRef.current = onFolded
  useEffect(() => {
    foldedRef.current?.({ vehicles: foldedVehicles, groups: foldedGroups })
  }, [foldedVehicles, foldedGroups])
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
              ) : it.kind === 'group' ? (
                <div key={`g:${it.id}`} className="vlist-group" style={{ top: it.top }}>
                  <GroupRow it={it} selected={selected} onSelect={onSelect} onOpen={() => toggleGroup(it.id)} />
                </div>
              ) : it.kind === 'more' ? (
                <button key={`m:${it.id}`} className="vlist-more" style={{ top: it.top }} onClick={() => toggleGroup(it.id)} disabled={expandAll}>
                  {it.text}
                </button>
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
                    lost={lost?.has(it.v.uid)}
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
