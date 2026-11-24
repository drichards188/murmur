import { type Blocking, stripWords } from './blocking'
import './blocking.css'

/** The strip under the top bar while a review item alone holds up a nomination (blocking.ts), in
 *  plain words: what waits for the person (the one waiting longest), what their delay holds up,
 *  and how many more. Its tag pulses once when the item arrives, then holds. It decides nothing:
 *  Open takes the person to the item's card in the Inbox, where they read its evidence and decide
 *  with the card's own buttons and note. `victim`: whom an item's act was against, by name. */
export function WaitingStrip({ items, onOpen, victim }: { items: Blocking[]; onOpen: (id: string) => void; victim: (b: Blocking) => string | null }) {
  const b = items[0]
  if (!b) return null
  const more = items.length - 1
  const w = stripWords(b, victim(b))
  return (
    <section key={b.id} className="wait-strip" aria-label="Waiting on you" data-blocking-id={b.id}>
      <span className="wait-tag">WAITING ON YOU</span>
      {/* announced once per item, not every second */}
      <span className="wait-sr" role="status">
        Waiting on you: {w.ask} {w.holds}
      </span>
      <span className="wait-line" title={b.unblocks} aria-hidden="true">
        {w.ask} <strong>{w.holds}</strong>
      </span>
      {more > 0 && (
        <span className="wait-more" title={items.slice(1).map((x) => { const y = stripWords(x, victim(x)); return `${y.ask} ${y.holds}` }).join('\n')}>
          +{more} more
        </span>
      )}
      <button className="wait-open" onClick={() => onOpen(b.id)} title="Open its card in the Inbox: you decide there">
        Open
      </button>
    </section>
  )
}
