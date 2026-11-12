import { agoWords, type Blocking, type Pending, stripLine, waited } from './blocking'
import './blocking.css'
import { FLAG_LABEL, type FlagKind } from './ident'

/** at most this many pending lines; the rest counted, the Review one click away */
const PENDING_LINES = 3

/** A pending item in words: "T-0008 declared hostile by rule 2 min ago", "conflicting evidence on
 *  T-0012, T-0013, raised 4 min ago". */
function pendingLine(p: Pending, nowMs: number): string {
  const ago = agoWords(p.t, nowMs)
  if (p.kind === 'declaration') return `${p.tracks[0]} declared ${p.affiliation ?? 'hostile'} by rule ${ago} ago`
  const what = (p.flag && FLAG_LABEL[p.flag as FlagKind]) || 'flagged'
  return `${what} on ${p.tracks.join(', ')}, raised ${ago} ago`
}

/** The calm standing pointer under the top bar (docs/research/saturation-display.md R4): one line
 *  for each rule declaration awaiting confirmation and each open review flag, oldest first, while
 *  it waits. Pending, not urgent: no colour of alarm, no pulse (the urgent strip above keeps that
 *  for an item that alone holds up a nomination). It decides nothing and offers no confirm: Open
 *  takes the person to the item's card in the Inbox's Review, where they read its evidence and
 *  decide with the card's own buttons. In Saturation the reserve's loadout refuses an air track,
 *  so the urgent strip never shows, and this line was missing: the confirmation that matters most
 *  had only a count on the Inbox button. */
export function PendingStrip({ items, now, onOpen }: { items: Pending[]; now: () => number; onOpen: (id: string) => void }) {
  if (!items.length) return null
  const nowMs = now()
  const shown = items.slice(0, PENDING_LINES)
  const more = items.slice(PENDING_LINES)
  return (
    <section className="pend-strip" aria-label="Waiting for your review">
      {shown.map((p, k) => (
        <div className="pend-line" key={p.id} data-review-id={p.id}>
          <span className={`pend-tag${k ? ' quiet' : ''}`}>{k ? '' : 'PENDING REVIEW'}</span>
          <span className="pend-text" title={`${p.id}: ${pendingLine(p, nowMs)}. Waiting for your review: open its card to read the evidence and decide.`}>
            <b>{p.id}</b>
            {pendingLine(p, nowMs)} · waiting for your review
          </span>
          <button className="pend-open" onClick={() => onOpen(p.id)} title="Open its card in the Inbox's Review: you decide there">
            Open {p.id}
          </button>
        </div>
      ))}
      {more.length > 0 && (
        <div className="pend-line">
          <span className="pend-tag quiet" />
          <span className="pend-text" title={more.map((p) => `${p.id}: ${pendingLine(p, nowMs)}`).join('\n')}>
            +{more.length} more waiting for your review
          </span>
          <button className="pend-open" onClick={() => onOpen(more[0].id)} title="Open the next one's card in the Inbox's Review">
            Open {more[0].id}
          </button>
        </div>
      )}
    </section>
  )
}

/** The strip under the top bar while a review item alone holds up a nomination (blocking.ts):
 *  the one waiting longest, what waits, what it holds up, how long, and how many more. It decides
 *  nothing: Open takes the person to the item's card in the Inbox, where they read its evidence
 *  and decide with the card's own buttons and note. */
export function WaitingStrip({ items, now, onOpen }: { items: Blocking[]; now: () => number; onOpen: (id: string) => void }) {
  const b = items[0]
  if (!b) return null
  const more = items.length - 1
  const line = stripLine(b)
  return (
    <section className="wait-strip" aria-label="Waiting on you" data-blocking-id={b.id}>
      <span className="wait-tag">WAITING ON YOU</span>
      {/* announced once per item, not every second */}
      <span className="wait-sr" role="status">
        Waiting on you: {line}
      </span>
      <span className="wait-line" title={`${line}\n${b.unblocks}`} aria-hidden="true">
        <b>{b.id}</b>
        {b.waits_for}. <strong>{b.text}</strong>
      </span>
      <span className="wait-time" title="How long it has held up the nomination (the bridge's reading)">
        {waited(b, now())}
      </span>
      {more > 0 && (
        <span className="wait-more" title={items.slice(1).map(stripLine).join('\n')}>
          +{more} more
        </span>
      )}
      <button className="wait-open" onClick={() => onOpen(b.id)} title="Open its card in the Inbox: you decide there">
        Open {b.id}
      </button>
    </section>
  )
}
