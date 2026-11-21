import { type CSSProperties, type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import type { SwarmsFeed } from './swarms'
import { groupColor, groupCodes, ROLE_LABEL, ROLE_ORDER, type Swarm, type SwarmAlert, swarmTone, taskLine } from './swarmsModel'
import { reformBody, strengthLine, strengthTone, type GroupInForce } from './groupOrders'
import { submitIntent } from './decisions'
import { alertLabel } from './swarms'
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
  /** bring this swarm's card into view (a chip clicked on the map; n changes on every request) */
  focus?: { id: string; n: number } | null
  /** the group whose chip or card is under the pointer: its card marked, its members on the map */
  hovered?: string | null
  onHover?: (id: string | null) => void
  /** the inbox's swarm alerts, for a mark on the swarms that have one */
  alerts: SwarmAlert[]
  onSelect: (s: Swarm) => void
  onClose: () => void
  /** rules 34: whether a vehicle is heard now (its link not stale), for Re-form's members */
  heard?: (uid: string) => boolean
  /** the map's banners (App.tsx), shown under this drawer's header while it is open over the
   *  map's right edge, where they would otherwise sit over its top (bd murmur-76gv) */
  banners?: ReactNode
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

interface Look {
  color: string
  ink: string
  code: string
}

function Row({ s, look, selected, hovered, alerts, onSelect, onHover, rowRef }: { s: Swarm; look: Look; selected: boolean; hovered: boolean; alerts: number; onSelect: () => void; onHover?: (id: string | null) => void; rowRef: (el: HTMLButtonElement | null) => void }) {
  const tone = swarmTone(s)
  return (
    <button
      ref={rowRef}
      className={`sw-row ${tone} ${selected ? 'sel' : ''} ${hovered ? 'hov' : ''}`}
      onClick={onSelect}
      onMouseEnter={() => onHover?.(s.id)}
      onMouseLeave={() => onHover?.(null)}
      aria-pressed={selected}
      title={`Select all ${s.count} of ${s.id} for one order`}
    >
      <span className="sw-chip" style={{ background: look.color }} aria-hidden />
      <span className="sw-main">
        <span className="sw-name">
          <span className="gcode" style={{ background: look.color, color: look.ink } as CSSProperties}>
            {look.code}
          </span>{' '}
          <b>{s.id}</b> {s.name}
          {alerts > 0 && <span className="sw-alerts" title="swarm alerts in the Inbox">{alerts} alert{alerts === 1 ? '' : 's'}</span>}
        </span>
        <span className="sw-task">{taskLine(s)}</span>
        {/* rules 34: each group order in force over its members: its strength against its minimum */}
        {(s.groups ?? []).map((g) => (
          <span key={g.intent} className={`sw-gorder ${strengthTone(g)}`}>
            {strengthLine(g)}
            {g.members_here !== g.strength && <span className="dim"> ({g.members_here} of it here)</span>}
            {g.refused && <span className="warn"> · re-form refused: {g.refused}</span>}
          </span>
        ))}
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

/** Rules 34: re-form a suspended group with one click (docs/SWARMS.md 4.3, bd murmur-ge6t): a new
 *  group order, the same task to the members heard (`reformBody`), submitted as the console's
 *  operator, whose submission is its approval as any of their orders is; the engine checks and
 *  lays it out as any group order. Says what came of it. */
function ReformButton({ g, heard }: { g: GroupInForce; heard?: (uid: string) => boolean }) {
  const [busy, setBusy] = useState(false)
  const [said, setSaid] = useState<string | null>(null)
  const go = async () => {
    setBusy(true)
    setSaid(null)
    try {
      const res = await fetch(`/api/intents/${g.intent}`)
      if (!res.ok) throw new Error(`the bridge returned ${res.status} for i${g.intent}`)
      const body = reformBody((await res.json()).intent, g, heard)
      if (!body) {
        setSaid('No member is heard: nothing to send.')
        return
      }
      const out = await submitIntent(body)
      setSaid(`Ordered anew as i${out.intent.id}: ${out.intent.state.replace('_', ' ')}`)
    } catch (e) {
      setSaid(`Not re-formed: ${(e as Error).message}`)
    } finally {
      setBusy(false)
    }
  }
  return (
    <span className="sw-reform">
      <button className="ghost ix-btn" disabled={busy} onClick={go} title="A new group order: the same task to the members heard, approved by your submission">
        {busy ? 'Re-forming...' : 'Re-form'}
      </button>
      {said && <span className="dim">{said}</span>}
    </span>
  )
}

/** Rules 34: one group order in force, the panel's own line: its strength against its minimum,
 *  what dropped and why, its re-forms, and its group alert (under strength: it re-forms no more;
 *  re-form refused: why), as the Inbox raises it. */
function GroupLine({ g, alerts, heard }: { g: GroupInForce; alerts: SwarmAlert[]; heard?: (uid: string) => boolean }) {
  const dropped = Object.keys(g.dropped ?? {}).length
  const tone = strengthTone({ ...g, dropped })
  const why = new Map<string, number>()
  for (const w of Object.values(g.dropped ?? {})) {
    const k = w.split(':')[0]
    why.set(k, (why.get(k) ?? 0) + 1)
  }
  const mine = alerts.filter((a) => a.id.startsWith(`group:${g.intent}:`))
  return (
    <div className={`sw-gline ${tone}`}>
      <span className="sw-gmain">
        <b>{strengthLine({ ...g, dropped, back: g.back?.length ?? 0 })}</b>
        {g.swarm && <span className="dim"> · {g.swarm}</span>}
      </span>
      {dropped > 0 && (
        <span className="dim">
          dropped: {[...why.entries()].map(([k, n]) => `${n} ${k}`).join(', ')}
        </span>
      )}
      {g.taken > 0 && <span className="dim">{g.taken} taken by a newer order</span>}
      {mine.map((a) => (
        <span key={a.id} className={`sw-galert ${a.severity}`}>
          {alertLabel(a.kind)}
        </span>
      ))}
      {g.suspended && <span className="bad">It re-forms no more: order it anew, or order its members.</span>}
      {g.suspended && <ReformButton g={g} heard={heard} />}
      {g.refused && <span className="warn">Last re-form refused: {g.refused}</span>}
      {!g.reforming && !g.suspended && <span className="dim">does not re-form ({g.kind})</span>}
    </div>
  )
}

export function SwarmsPanel({ feed, selected, focus, hovered = null, onHover, alerts, onSelect, onClose, heard, banners }: Props) {
  const swarms = feed.doc?.swarms ?? []
  // each swarm's colour and code as the map and the fleet list show them (by the summary's order)
  const ids = swarms.map((s) => s.id).join('\n')
  const looks = useMemo(() => {
    const codes = groupCodes(ids ? ids.split('\n') : [])
    return new Map([...codes].map(([id, code], i) => [id, { ...groupColor(i), code }]))
  }, [ids])
  const rows = useRef(new Map<string, HTMLButtonElement>())
  useEffect(() => {
    if (focus) rows.current.get(focus.id)?.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
  }, [focus])
  const groups = feed.doc?.groups ?? []
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
      {banners && <div className="ix-banners">{banners}</div>}
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
        {groups.length > 0 && (
          <section className="sw-group sw-orders" aria-label="Group orders in force">
            <h3 className="ix-h">
              Group orders in force <span>{groups.length}</span>
              <small>{groups.reduce((n, g) => n + g.selection, 0).toLocaleString()} vehicles</small>
            </h3>
            {groups.map((g) => (
              <GroupLine key={g.intent} g={g} alerts={alerts} heard={heard} />
            ))}
          </section>
        )}
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
                <Row
                  key={s.id}
                  s={s}
                  look={looks.get(s.id) ?? { ...groupColor(0), code: s.id }}
                  selected={selected === s.id}
                  hovered={hovered === s.id}
                  alerts={alertsBy.get(s.id) ?? 0}
                  onSelect={() => onSelect(s)}
                  onHover={onHover}
                  rowRef={(el) => {
                    if (el) rows.current.set(s.id, el)
                    else rows.current.delete(s.id)
                  }}
                />
              ))}
            </section>
          )
        })}
        {swarms.length > 0 && (
          <p className="ix-foot">A click selects every member of a swarm: the order form then sends one order to all of them (under group orders, one group order laid out member by member).</p>
        )}
      </div>
    </aside>
  )
}
