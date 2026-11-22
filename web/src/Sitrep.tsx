import { useEffect, useMemo, useRef, useState } from 'react'
import type { Decisions } from './decisions'
import { agoWords, pendingItems, stripLine, waitingItems } from './blocking'
import { pendingLine } from './pendingLine'
import { STATE_ORDER, STATE_WORDS, type LatestOp, type VehicleState } from './exceptions'
import type { OperatorGroup } from './groups'
import { groupColor, type GroupMarks, type Swarm } from './swarmsModel'
import { ANY, contactSummary, filterNodes, groupRows, headline, nearest, nodeRows, sitrepText, strengthWords, windowOf, worstFirst, type GroupRow, type NodeFilter, type NodeRow } from './sitrep'
import { usePoll } from './space'
import type { Track } from './tracks'
import type { Tasking, Vehicle } from './types'
import { DOMAIN_LABEL } from './types'
import './sitrep.css'

/** The Sitrep: the whole picture, top-down, read-only. A bottom line, then the
 *  groups and swarms by state, then every node (filterable, drilling to its details), the contacts,
 *  and what waits for a person. Assembled from the feeds the console already holds, once a second
 *  (never per frame); long lists are summarised and expanded on demand, and the node list is
 *  virtualised. It sends nothing and decides nothing: every row opens an existing panel or card,
 *  and the only buttons that change anything change what you look at (a selection, a filter). */

interface Props {
  vehicles: Vehicle[]
  taskings: Tasking[]
  swarms: Swarm[]
  /** each swarm's marks (App.tsx, from the same summary): its group order's strength, as the
   *  map's chips and the group strip show it */
  marks: GroupMarks
  groups: OperatorGroup[]
  tracks: Track[]
  decisions: Decisions | null
  damaged: ReadonlySet<number>
  now: () => number
  /** open a vehicle's details / a review item's card / a queued or pending intent / a contact */
  onVehicle: (uid: number) => void
  onWaiting: (id: string) => void
  onIntent: (id: number) => void
  onTrack: (id: string) => void
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
const NEAREST = 5

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
  // one coherent picture a second: the vehicles, the orders, the decisions
  const vehicles = useSnapshot(p.vehicles, 1000)
  const taskings = useSnapshot(p.taskings, 1000)
  const tracks = useSnapshot(p.tracks, 1000)
  const decisions = useSnapshot(p.decisions, 1000)
  const swarms = useSnapshot(p.swarms, 1000)
  const marks = useSnapshot(p.marks, 1000)
  const groups = useSnapshot(p.groups, 1000)
  const damaged = useSnapshot(p.damaged, 1000)
  const situation = usePoll<{ title?: string; name?: string; phase?: string } | null>('/api/situation', 5000).data

  const [filter, setFilter] = useState<NodeFilter>(ANY)
  const [focus, setFocus] = useState<string | null>(null)
  const [allGroups, setAllGroups] = useState(false)
  const [copied, setCopied] = useState(false)
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
    const models = [...new Set(nodes.map((n) => n.model).filter(Boolean))].sort()
    return { rows, nodes, models, by }
  }, [vehicles, taskings, swarms, marks, groups, damaged])

  const inbox = decisions?.inbox
  const blocking = useMemo(() => waitingItems(inbox?.blocking), [inbox?.blocking])
  const pending = useMemo(() => pendingItems(inbox?.review, inbox?.blocking), [inbox?.review, inbox?.blocking])
  const nowMs = p.now()
  const head = headline({
    scenario: situation ? { title: situation.title ?? situation.name, phase: situation.phase } : null,
    vehicles,
    tracks,
    counts: { pending: inbox?.counts.pending, review: inbox?.counts.review, queued: inbox?.counts.queued },
    blocking: blocking.map((b) => ({ text: stripLine(b) })),
    oldest: pending[0] ? `${pending[0].id}: ${pendingLine(pending[0], nowMs)} · waiting for your review` : null,
  })
  const contacts = useMemo(() => contactSummary(tracks), [tracks])
  // the contacts nearest our fleet's centre (the console knows no protected point)
  const near = useMemo(() => {
    const pts = vehicles.flatMap((v) => (v.position ? [v.position] : []))
    if (!pts.length) return []
    const c: [number, number] = [pts.reduce((a, q) => a + q[0], 0) / pts.length, pts.reduce((a, q) => a + q[1], 0) / pts.length]
    return nearest(tracks.filter((t) => t.state === 'active'), c, NEAREST)
  }, [vehicles, tracks])

  const focused = focus ? model.rows.find((r) => r.key === focus) : null
  const filtered = useMemo(() => worstFirst(filterNodes(model.nodes, { ...filter, within: focused ? new Set(focused.members) : null })), [model.nodes, filter, focused])
  const view = windowOf(filtered.length, scroll, 300, ROW_H)
  const shownRows = allGroups ? model.rows : model.rows.slice(0, GROUPS_SHOWN)

  const pick = (key: string) => {
    setFocus((f) => (f === key ? null : key))
    list.current?.scrollTo?.({ top: 0 })
    setScroll(0)
  }
  const when = new Date(nowMs).toISOString().slice(11, 19) + 'Z'
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(sitrepText(head, model.rows, contacts, when))
      setCopied(true)
      window.setTimeout(() => setCopied(false), 2000)
    } catch {
      setCopied(false)
    }
  }

  return (
    <aside className="ix srp" aria-label="Situation report">
      <div className="ix-top">
        <h2>Sitrep</h2>
        <span className="q-sub">read-only · refreshed each second</span>
        <button className="ghost mini" onClick={copy} title="Copy this report as text">
          {copied ? 'Copied' : 'Copy as text'}
        </button>
        <button className="ghost close" onClick={p.onClose} aria-label="Close the situation report">
          ×
        </button>
      </div>
      {p.banners && <div className="ix-banners">{p.banners}</div>}
      <div className="ix-scroll srp-body">
        {/* 1. the bottom line */}
        <section className="srp-head" aria-label="Bottom line">
          {head.scenario && <p className="srp-scn">{head.scenario}</p>}
          <p className="srp-bluf">
            <b className={head.lost ? 'warn' : ''}>
              {head.up} of {head.total}
            </b>{' '}
            vehicles heard
            {head.lost > 0 && <span className="bad"> · {head.lost} lost link</span>} · <b>{head.contacts}</b> contacts
            {(head.hostile > 0 || head.suspect > 0) && (
              <span className={head.hostile ? 'bad' : 'warn'}>
                {' '}
                ({head.hostile} hostile, {head.suspect} suspect)
              </span>
            )}{' '}
            · <b className={head.waiting ? 'warn' : ''}>{head.waiting}</b> waiting for a person
          </p>
          {head.urgent && (
            <button className="srp-urgent" onClick={() => (blocking[0] ? p.onWaiting(blocking[0].id) : pending[0] && p.onWaiting(pending[0].id))} title="Open its card: it is decided there, not here">
              {head.urgent}
            </button>
          )}
        </section>

        {/* 2. by group and swarm */}
        <section aria-label="Groups and swarms">
          <h3 className="ix-h">
            Groups and swarms <span>{model.rows.length}</span>
            <small>worst first; a click lists its nodes below</small>
          </h3>
          {model.rows.length === 0 && <p className="empty muted">This fleet has no swarms and you have made no groups.</p>}
          {shownRows.map((r) => (
            <div key={r.key} className={`srp-group ${r.tone} ${focus === r.key ? 'sel' : ''}`}>
              <button className="srp-gmain" aria-pressed={focus === r.key} onClick={() => pick(r.key)}>
                <span className="srp-chip" style={{ background: r.color ?? '#7d8b9a' }} aria-hidden />
                <span className="srp-gname">
                  {r.name}
                  {r.kind === 'yours' && <span className="og-badge">yours</span>}
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

        {/* 3. nodes */}
        <section aria-label="Nodes">
          <h3 className="ix-h">
            Nodes <span>{filtered.length}</span>
            <small>of {model.nodes.length}{focused ? `, in ${focused.name}` : ''}</small>
          </h3>
          <div className="srp-filters">
            <input type="search" placeholder="name" aria-label="Filter by name" value={filter.text} onChange={(e) => setFilter({ ...filter, text: e.target.value })} />
            <select aria-label="Domain" value={filter.domain} onChange={(e) => setFilter({ ...filter, domain: e.target.value as NodeFilter['domain'] })}>
              <option value="all">any domain</option>
              {(Object.keys(DOMAIN_LABEL) as (keyof typeof DOMAIN_LABEL)[]).map((d) => (
                <option key={d} value={d}>
                  {DOMAIN_LABEL[d]}
                </option>
              ))}
            </select>
            <select aria-label="State" value={filter.state} onChange={(e) => setFilter({ ...filter, state: e.target.value as VehicleState | 'all' })}>
              <option value="all">any state</option>
              {STATE_ORDER.map((s) => (
                <option key={s} value={s}>
                  {STATE_WORDS[s]}
                </option>
              ))}
            </select>
            <select aria-label="Model" value={filter.model} onChange={(e) => setFilter({ ...filter, model: e.target.value })}>
              <option value="all">any model</option>
              {model.models.map((m) => (
                <option key={m} value={m}>
                  {m}
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
            {filtered.length === 0 && <p className="empty muted">No node matches.</p>}
          </div>
        </section>

        {/* 4. contacts */}
        <section aria-label="Contacts">
          <h3 className="ix-h">
            Contacts <span>{contacts.total}</span>
            <small>{contacts.stale} stale · {contacts.declared} declared · {contacts.flagged} flagged</small>
          </h3>
          <p className="srp-chips">
            {Object.entries(contacts.byAffiliation).map(([k, n]) => (
              <span key={k} className={`srp-c aff-${k}`}>
                {n} {k.replace('_', ' ')}
              </span>
            ))}
            {Object.entries(contacts.byDomain).map(([k, n]) => (
              <span key={k} className="srp-c dim">
                {n} {k}
              </span>
            ))}
          </p>
          {near.length > 0 && (
            <>
              <p className="muted srp-sub">Nearest our fleet's centre</p>
              {near.map(({ track: t, km }) => (
                <button key={t.id} className="srp-contact" onClick={() => p.onTrack(t.id)} title="Show it in the Tracks list">
                  <b>{t.id}</b>
                  <span className={`aff-${t.declared?.affiliation ?? t.affiliation}`}>{(t.declared?.affiliation ?? t.affiliation).replace('_', ' ')}</span>
                  <span>{t.identity.kind ?? t.domain}</span>
                  <span className="dim">{km < 10 ? km.toFixed(1) : Math.round(km)} km</span>
                </button>
              ))}
            </>
          )}
        </section>

        {/* 5. what needs a person */}
        <section aria-label="Needs a person">
          <h3 className="ix-h">
            Needs a person <span>{head.waiting}</span>
            <small>decided on their cards, never here</small>
          </h3>
          {blocking.map((b) => (
            <button key={b.id} className="srp-wait urgent" onClick={() => p.onWaiting(b.id)}>
              <b>{b.id}</b> {stripLine(b)}
            </button>
          ))}
          {pending.map((x) => (
            <button key={x.id} className="srp-wait" onClick={() => p.onWaiting(x.id)}>
              <b>{x.id}</b> {pendingLine(x, nowMs)}
            </button>
          ))}
          {(inbox?.pending ?? []).slice(0, 8).map((i) => (
            <button key={`i${i.id}`} className="srp-wait" onClick={() => p.onIntent(i.id)}>
              <b>i{i.id}</b> {i.kind} for {i.uids.length} vehicle{i.uids.length === 1 ? '' : 's'}, {agoWords(i.created_t / 1000, nowMs)} ago{i.why?.[0] ? `: ${i.why[0]}` : ''}
            </button>
          ))}
          {(inbox?.pending?.length ?? 0) > 8 && <p className="muted">+{(inbox?.pending?.length ?? 0) - 8} more in the Inbox</p>}
          {(inbox?.queued?.length ?? 0) > 0 && <p className="muted">{inbox?.queued?.length} order(s) queued for vehicles the link cannot reach now (the Queue)</p>}
          {head.waiting === 0 && blocking.length === 0 && <p className="empty muted">Nothing waits for you.</p>}
        </section>
      </div>
    </aside>
  )
}

function NodeLine({ r, top, onOpen }: { r: NodeRow; top: number; onOpen: (uid: number) => void }) {
  return (
    <button role="row" className={`srp-node ${r.state}`} style={{ top, height: ROW_H }} onClick={() => onOpen(r.uid)} title="Open its details">
      <b className="srp-nname">{r.name}</b>
      <span className="dim">{r.model}</span>
      <span className={`srp-nstate ${r.state === 'lost' || r.state === 'damaged' ? 'bad' : ''}`}>{STATE_WORDS[r.state]}</span>
      <span className={`srp-nlink ${r.link}`} title="link">
        {r.quietS == null ? '–' : r.quietS < 60 ? `${r.quietS}s` : `${Math.round(r.quietS / 60)}m`}
      </span>
      <span className={r.lowBattery ? 'warn' : 'dim'}>{r.battery == null ? '–' : `${Math.round(r.battery)}%`}</span>
      <span className="srp-norder">{r.order}</span>
      <span className="dim srp-ngroups">{r.groups.join(', ')}</span>
    </button>
  )
}
