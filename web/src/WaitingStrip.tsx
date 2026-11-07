import { type Blocking, stripLine, waited } from './blocking'
import './blocking.css'

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
