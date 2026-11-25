import { useEffect, useState, type ReactNode } from 'react'
import { Bluf } from './Bluf'
import { parseRationale } from './bluf'
import { getOperator, type Intent } from './decisions'
import { groupsFor, lookOf, type OperatorGroup } from './groups'
import { proposedIn } from './officer'
import { awaitsReply, BOUNDARY, deliveryOf, lastReply, officerOf, outcomeView, split, type Reply, type TaskingItem } from './tasking'
import type { TaskingApi } from './useTasking'
import './tasking.css'
import { OffButton } from './OffButton'

/** The AI watch officer's drawer: what the operator asked of it, what it made of each request (its
 *  reply, the intents it proposed and the groups it made), and retiring what no longer applies. A
 *  request is carried out with the orders the officer may propose, never an approval and never a
 *  decision (BOUNDARY says so where it is written); it reaches the officer only through the bridge,
 *  is not logged in the audit log, and no decision reads it or the officer's reply. */

/** `off`: why this run has no officer to ask (the button stays, greyed, saying so). */
export function TaskingButton({ api, open, onClick, off = null }: { api: TaskingApi; open: boolean; onClick: () => void; off?: string | null }) {
  if (off && !open) return <OffButton label="Officer" off={off} />
  const n = api.items.filter((i) => i.active).length
  const officer = officerOf(api.reader)
  return (
    <button
      className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`}
      onClick={onClick}
      aria-pressed={open}
      aria-label={`Watch officer${officer ? (officer.connected ? ', connected' : ', not connected') : ''}: ${n} message${n === 1 ? '' : 's'} in force`}
      title={`The watch officer: requests to it (not an approval, not a decision)${officer ? `. ${officer.text}` : ''}`}
    >
      {officer && !officer.connected && <span className="tko-dot off" aria-hidden="true" />}
      Officer
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

interface Links {
  groups: readonly OperatorGroup[]
  /** open an intent's card (in the Inbox while it waits) */
  onIntent?: (id: number) => void
  /** select a group's members */
  onGroup?: (uids: number[]) => void
}

/** An intent as a link to its card. */
function IntentLink({ id, onIntent, title }: { id: number; onIntent?: (id: number) => void; title: string }) {
  return (
    <button className="ghost mini tko-link" onClick={() => onIntent?.(id)} disabled={!onIntent} title={title}>
      i{id}
    </button>
  )
}

/** One of the officer's answers: its outcome as a chip, its text bottom line first, the intents it
 *  proposed (each opens its card) and the groups it made (each selects its members). */
function ReplyView({ r, nowMs, groups, onIntent, onGroup, compact = false, turn = [] }: Links & { r: Reply; nowMs: number; compact?: boolean; turn?: readonly Intent[] }) {
  const o = outcomeView(r, nowMs)
  const made = (r.groups ?? []).map((id) => ({ id, g: groups.find((x) => x.id === id) }))
  // one list of the orders it proposed: those its answer names, then those of the turn that read the request
  const orders = [...r.intents, ...turn.map((i) => i.id).filter((id) => !r.intents.includes(id))]
  return (
    <div className={`tko-reply ${o.tone} ${compact ? 'compact' : ''}`}>
      <div className="tko-rhead">
        <span className={`tko-out ${o.tone}`} title={o.title}>
          {o.label}
        </span>
      </div>
      <Bluf parts={parseRationale(r.text)} className="compact" />
      {(orders.length > 0 || made.length > 0) && (
        <div className="tko-links">
          {orders.length > 0 && <span className="tko-turn-label">orders proposed</span>}
          {orders.map((id) => (
            <IntentLink key={id} id={id} onIntent={onIntent} title={`Order i${id}: open its card (in the Inbox while it waits for you)`} />
          ))}
          {made.map(({ id, g }) =>
            g ? (
              <button key={id} className="ghost mini tko-link tko-glink" onClick={() => onGroup?.(g.members)} disabled={!onGroup} title={`${g.name}: select its ${g.members.length} members (sends nothing)`}>
                <span className="tko-gchip" style={{ background: lookOf(g, groups.indexOf(g)).color }} aria-hidden />
                {g.name} · {g.members.length}
              </button>
            ) : (
              <span key={id} className="tko-link gone" title="The group was deleted since">
                {id}
              </span>
            ),
          )}
        </div>
      )}
    </div>
  )
}

/** What the officer made of an item: its latest answer (earlier ones a click away), or that it has
 *  had the item and not answered yet; and, where no answer names its orders, the orders proposed
 *  in the turn that first read it (officer.ts `proposedIn`: by time, not by cause). */
function Answer({ item, nowMs, kept, intents, ...links }: Links & { item: TaskingItem; nowMs: number; kept: boolean | undefined; intents: readonly Intent[] }) {
  const [all, setAll] = useState(false)
  const last = lastReply(item)
  const made = groupsFor(links.groups, item.id)
  const named = (item.replies ?? []).some((r) => r.intents.length > 0)
  const inTurn = named ? [] : proposedIn(item, intents)
  const turn =
    inTurn.length > 0 ? (
      <div className="tko-links tko-turn">
        <span className="tko-turn-label">orders proposed</span>
        {inTurn.map((i) => (
          <IntentLink key={i.id} id={i.id} onIntent={links.onIntent} title={`Order i${i.id}: open its card`} />
        ))}
      </div>
    ) : null
  if (!last) {
    return (
      <>
        {awaitsReply(item, kept) && (
          <p className="tko-noreply" role="status" title="The officer has had this request (the turn that carried it ended) and has not replied. Its next update lists it again as not answered.">
            No reply yet{made.length ? ` · it made ${made.length} group${made.length === 1 ? '' : 's'} for it` : ''}
          </p>
        )}
        {turn}
      </>
    )
  }
  const earlier = (item.replies ?? []).slice(0, -1)
  return (
    <>
      <ReplyView r={last} nowMs={nowMs} {...links} turn={inTurn} />
      {earlier.length > 0 && (
        <>
          <button className="ghost mini tko-more" aria-expanded={all} onClick={() => setAll(!all)}>
            {all ? 'Hide' : 'Show'} {earlier.length} earlier repl{earlier.length === 1 ? 'y' : 'ies'}
          </button>
          {all &&
            earlier
              .slice()
              .reverse()
              .map((r, k) => <ReplyView key={k} r={r} nowMs={nowMs} {...links} compact />)}
        </>
      )}
    </>
  )
}

interface PanelProps extends Partial<Links> {
  api: TaskingApi
  now: () => number
  onClose: () => void
  banners?: ReactNode
  /** the message box (OfficerCompose.tsx), pinned between the head and the list so it stays in
   *  reach however long the list grows */
  compose?: ReactNode
  /** every intent of the run, for the orders proposed in a request's turn */
  intents?: readonly Intent[]
}

const SEEN_KEY = 'murmur.officer-note-seen'

/** The first visit shows what a request is (BOUNDARY); after that it is behind the "?". */
function useFirstVisit(): [boolean, (v: boolean) => void] {
  const [show, setShow] = useState(() => {
    try {
      return window.localStorage.getItem(SEEN_KEY) == null
    } catch {
      return false
    }
  })
  useEffect(() => {
    try {
      window.localStorage.setItem(SEEN_KEY, '1')
    } catch {
      /* a private window: the note is behind the ? every time */
    }
  }, [])
  return [show, setShow]
}

/** A request's meta line: its number as a grey chip, who only when it is not me, the time it was
 *  posted as a clock time. */
function Meta({ item, me }: { item: TaskingItem; me: string | null }) {
  return (
    <>
      <span className="ix-id" title={`request ${item.id}`}>
        {item.id}
      </span>
      {item.by !== me && <span> {item.by}</span>}
      <span> {new Date(item.at_t).toISOString().slice(11, 16)}Z</span>
    </>
  )
}

/** The drawer's body. */
export function TaskingPanel({ api, now, onClose, banners, compose, groups = [], intents = [], onIntent, onGroup }: PanelProps) {
  const [showRetired, setShowRetired] = useState(false)
  const [note, setNote] = useFirstVisit()
  const { active, retired } = split(api.items)
  const nowMs = now()
  const officer = officerOf(api.reader)
  const kept = api.reader.officer_replies
  const links: Links = { groups, onIntent, onGroup }
  const me = getOperator()
  return (
    <aside className="ix tko" aria-label="The AI watch officer">
      <div className="ix-top">
        <h2>Watch officer</h2>
        <button className="ghost mini tko-why" aria-expanded={note} onClick={() => setNote(!note)} title="What a request is">
          ?
        </button>
        <button className="ghost close" onClick={onClose} aria-label="Close the watch officer">
          ×
        </button>
      </div>
      {banners && <div className="ix-banners">{banners}</div>}
      {compose}
      <div className="ix-scroll">
        {note && (
          <p className="tko-boundary" role="note">
            {BOUNDARY}
          </p>
        )}
        {api.status === 'missing' && (
          <p className="empty sw-missing" role="status">
            This bridge has no tasking channel.
          </p>
        )}
        {api.status === 'ok' && officer && !officer.connected && (
          <p className="tko-officer off" role="status">
            {officer.text}
          </p>
        )}
        {active.map((i) => (
          <div key={i.id} className="tko-item" data-tasking-id={i.id}>
            <p>{i.text}</p>
            <div className="tko-meta">
              <span>
                <Meta item={i} me={me} />
              </span>
              <DeliveryChip item={i} nowMs={nowMs} />
              <button className="ghost mini" onClick={() => void api.deactivate(i.id)} title="Stop sending this request to the officer">
                Retire
              </button>
            </div>
            <Answer item={i} nowMs={nowMs} kept={kept} intents={intents} {...links} />
          </div>
        ))}
        {retired.length > 0 && (
          <>
            <button className="ghost mini tko-more" aria-expanded={showRetired} onClick={() => setShowRetired(!showRetired)}>
              {showRetired ? 'Hide' : 'Show'} retired ({retired.length})
            </button>
            {showRetired &&
              retired.map((i) => (
                <div key={i.id} className="tko-item retired" data-tasking-id={i.id}>
                  <p>{i.text}</p>
                  <div className="tko-meta">
                    <span>
                      <Meta item={i} me={me} /> · retired
                    </span>
                    <DeliveryChip item={i} nowMs={nowMs} />
                  </div>
                  <Answer item={i} nowMs={nowMs} kept={kept} intents={intents} {...links} />
                </div>
              ))}
          </>
        )}
      </div>
    </aside>
  )
}
