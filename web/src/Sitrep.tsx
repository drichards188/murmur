import { useEffect, useMemo, useRef, useState } from 'react'
import { STATE_ORDER, STATE_WORDS, type LatestOp, type VehicleState } from './exceptions'
import type { OperatorGroup } from './groups'
import { groupColor, type GroupMarks, type Swarm } from './swarmsModel'
import { ANY, filterNodes, groupRows, nodeRows, strengthWords, windowOf, worstFirst, type GroupRow, type NodeFilter, type NodeRow } from './sitrep'
import type { Tasking, Vehicle } from './types'
import './sitrep.css'

/** The Sitrep, at group scale only: each group's strength against its minimum, and every node with
 *  the filters that find trouble (state, unheard only, low battery), drilling to its details. The
 *  headline is the top bar's glance line; what waits for a person is in the Inbox and the contacts
 *  are in Tracks. Assembled from the feeds the console already holds, once a second (never per
 *  frame); long lists are summarised and expanded on demand, and the node list is virtualised. It
 *  sends nothing and decides nothing: a row opens an existing panel, and the only buttons that
 *  change anything change what you look at (a selection, a filter). */

interface Props {
  vehicles: Vehicle[]
  taskings: Tasking[]
  swarms: Swarm[]
  /** each swarm's marks (App.tsx, from the same summary): its group order's strength, as the
   *  map's chips and the group strip show it */
  marks: GroupMarks
  groups: OperatorGroup[]
  damaged: ReadonlySet<number>
  /** open a vehicle's details */
  onVehicle: (uid: number) => void
  /** select these vehicles (the order form then commands them); a view change, no order */
  onSelect: (uids: number[]) => void
  onClose: () => void
  banners?: React.ReactNode
}

/** The latest of `value`, handed on once every `ms` (and at once the first time): the report is
 *  built from the ~1 Hz picture, not from each 4 Hz update. */
function useSnapshot<T>(value: T, ms: number): T {
  const [snap, setSnap] = useState(value)
  const latest = useRef(value)
  useEffect(() => {
    latest.current = value
  }, [value])
  useEffect(() => {
    const id = window.setInterval(() => setSnap(latest.current), ms)
    return () => window.clearInterval(id)
  }, [ms])
  return snap
}

const ROW_H = 26
const GROUPS_SHOWN = 12

function Counts({ r }: { r: GroupRow }) {
  return (
    <span className="srp-counts">
      {STATE_ORDER.filter((s) => r.counts[s] > 0).map((s) => (
        <span key={s} className={`srp-c ${s === 'lost' || s === 'damaged' ? 'bad' : ''}`} title={STATE_WORDS[s]}>
          {r.counts[s]} {STATE_WORDS[s]}
        </span>
      ))}
      {r.lowBattery > 0 && <span className="srp-c warn">{r.lowBattery} low batt</span>}
      {r.task && <span className="srp-task">{r.task}</span>}
    </span>
  )
}

export function Sitrep(p: Props) {
  // one coherent picture a second: the vehicles, the orders
  const vehicles = useSnapshot(p.vehicles, 1000)
  const taskings = useSnapshot(p.taskings, 1000)
  const swarms = useSnapshot(p.swarms, 1000)
  const marks = useSnapshot(p.marks, 1000)
  const groups = useSnapshot(p.groups, 1000)
  const damaged = useSnapshot(p.damaged, 1000)

  const [filter, setFilter] = useState<NodeFilter>(ANY)
  const [focus, setFocus] = useState<string | null>(null)
  const [allGroups, setAllGroups] = useState(false)
  const [scroll, setScroll] = useState(0)
  const list = useRef<HTMLDivElement>(null)

  const model = useMemo(() => {
    const latest = new Map<number, LatestOp>()
    for (const t of [...taskings].sort((a, b) => a.id - b.id)) for (const o of t.ops) latest.set(o.uid, { state: o.state, task: t.task })
    const ctx = { latest, damaged }
    const by = new Map(vehicles.map((v) => [v.uid, v]))
    const rows = groupRows(swarms, groups, by, ctx, (kind, i) => (kind === 'swarm' ? groupColor(i).color : (groups[i]?.color ?? groupColor(i).color)), (id) => marks.byId.get(id))
    const ofUid = new Map<number, string[]>()
    for (const g of groups) for (const u of g.members) ofUid.set(u, [...(ofUid.get(u) ?? []), g.name])
    for (const s of swarms) for (const u of s.members) ofUid.set(u, [...(ofUid.get(u) ?? []), s.id])
    const nodes = nodeRows(vehicles, ctx, ofUid)
    return { rows, nodes, by }
  }, [vehicles, taskings, swarms, marks, groups, damaged])

  const focused = focus ? model.rows.find((r) => r.key === focus) : null
  const filtered = useMemo(() => worstFirst(filterNodes(model.nodes, { ...filter, within: focused ? new Set(focused.members) : null })), [model.nodes, filter, focused])
  const view = windowOf(filtered.length, scroll, 300, ROW_H)
  const shownRows = allGroups ? model.rows : model.rows.slice(0, GROUPS_SHOWN)

  const pick = (key: string) => {
    setFocus((f) => (f === key ? null : key))
    list.current?.scrollTo?.({ top: 0 })
    setScroll(0)
  }

  return (
    <aside className="ix srp" aria-label="Situation report">
      <div className="ix-top">
        <h2>Sitrep</h2>
        <button className="ghost close" onClick={p.onClose} aria-label="Close the situation report">
          ×
        </button>
      </div>
      {p.banners && <div className="ix-banners">{p.banners}</div>}
      <div className="ix-scroll srp-body">
        {model.rows.length > 0 && (
          <section aria-label="Groups">
            <h3 className="ix-h">
              Groups <span>{model.rows.length}</span>
            </h3>
            {shownRows.map((r) => (
              <div key={r.key} className={`srp-group ${r.tone} ${focus === r.key ? 'sel' : ''}`}>
                <button className="srp-gmain" aria-pressed={focus === r.key} onClick={() => pick(r.key)}>
                  <span className="srp-chip" style={{ background: r.color ?? '#7d8b9a' }} aria-hidden />
                  <span className="srp-gname">
                    {r.name}
                    {r.kind === 'officer' && (
                      <span className="og-badge og-officer" title="Made by the AI watch officer for one of your tasking requests">
                        officer
                      </span>
                    )}
                  </span>
                  <span className="srp-gstr">{strengthWords(r)}</span>
                  <Counts r={r} />
                </button>
                <button className="ghost mini" onClick={() => p.onSelect(r.members.filter((u) => model.by.has(u)))} title="Select its members (the order form then commands them). Sends nothing.">
                  Select
                </button>
              </div>
            ))}
            {model.rows.length > GROUPS_SHOWN && (
              <button className="ghost mini srp-more" onClick={() => setAllGroups(!allGroups)}>
                {allGroups ? 'Show the worst only' : `Show all ${model.rows.length}`}
              </button>
            )}
          </section>
        )}

        <section aria-label="Nodes">
          <h3 className="ix-h">
            Nodes <span>{filtered.length}</span>
            <small>
              of {model.nodes.length}
              {focused ? `, in ${focused.name}` : ''}
            </small>
          </h3>
          <div className="srp-filters">
            <input type="search" placeholder="name" aria-label="Filter by name" value={filter.text} onChange={(e) => setFilter({ ...filter, text: e.target.value })} />
            <select aria-label="State" value={filter.state} onChange={(e) => setFilter({ ...filter, state: e.target.value as VehicleState | 'all' })}>
              <option value="all">any state</option>
              {STATE_ORDER.map((s) => (
                <option key={s} value={s}>
                  {STATE_WORDS[s]}
                </option>
              ))}
            </select>
            <label>
              <input type="checkbox" checked={filter.unheard} onChange={(e) => setFilter({ ...filter, unheard: e.target.checked })} /> unheard only
            </label>
            <label>
              <input type="checkbox" checked={filter.low} onChange={(e) => setFilter({ ...filter, low: e.target.checked })} /> low battery
            </label>
            {(focus || filter !== ANY) && (
              <button
                className="ghost mini"
                onClick={() => {
                  setFilter(ANY)
                  setFocus(null)
                }}
              >
                Clear
              </button>
            )}
          </div>
          <div className="srp-list" ref={list} onScroll={(e) => setScroll(e.currentTarget.scrollTop)} role="table" aria-label="Nodes" aria-rowcount={filtered.length}>
            <div style={{ height: filtered.length * ROW_H, position: 'relative' }}>
              {filtered.slice(view.from, view.to).map((r, k) => (
                <NodeLine key={r.uid} r={r} top={(view.from + k) * ROW_H} onOpen={p.onVehicle} />
              ))}
            </div>
          </div>
        </section>
      </div>
    </aside>
  )
}

function NodeLine({ r, top, onOpen }: { r: NodeRow; top: number; onOpen: (uid: number) => void }) {
  return (
    <button role="row" className={`srp-node ${r.state}`} style={{ top, height: ROW_H }} onClick={() => onOpen(r.uid)} title={r.groups.length ? `Open its details. In ${r.groups.join(', ')}` : 'Open its details'}>
      <b className="srp-nname">{r.name}</b>
      <span className={`srp-nstate ${r.state === 'lost' || r.state === 'damaged' ? 'bad' : ''}`}>{STATE_WORDS[r.state]}</span>
      <span className={r.lowBattery ? 'warn' : 'dim'}>{r.battery == null ? '–' : `${Math.round(r.battery)}%`}</span>
      <span className="srp-norder">{r.order}</span>
    </button>
  )
}
