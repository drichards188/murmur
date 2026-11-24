import { fmtDuration } from './decisions'
import { excerpt, type AwaitRow, type TurnLink } from './officer'
import { Proposer } from './Proposer'
import './dock.css'

/** What waits for a person's approval, at the head of whatever drawer the dock shows but the Inbox
 *  (dock.ts): one line each, soonest expiry first, the ask as the approval card says it, its id,
 *  its proposer as the Inbox labels it (the AI's as the AI's) and the time left. A line only opens
 *  its card in the Inbox: the person decides there, on the card's checks, never here. */
export function Awaiting({ rows, onOpen, max = 3 }: { rows: AwaitRow[]; onOpen: (id: number) => void; max?: number }) {
  if (!rows.length) return null
  const shown = rows.slice(0, max)
  return (
    <section className="aw" aria-label={`Waiting for you: ${rows.length}`}>
      <h3 className="aw-h">
        Waiting for you <span>{rows.length}</span>
      </h3>
      <ul className="aw-list">
        {shown.map((r) => (
          <li key={r.id}>
            <button className={`aw-row${r.engage ? ' engage' : ''}`} onClick={() => onOpen(r.id)}>
              <span className="aw-main">
                <span className="aw-what">{r.ask}</span>
                <span className="ix-id">i{r.id}</span>
                <Proposer source={r.source} />
                <Left s={r.left_s} />
              </span>
            </button>
          </li>
        ))}
      </ul>
      {rows.length > max && (
        <button className="aw-more" onClick={() => onOpen(rows[max].id)}>
          {rows.length - max} more in the Inbox
        </button>
      )}
    </section>
  )
}

function Left({ s }: { s: number | null }) {
  if (s == null) return null
  return (
    <span className={`aw-left${s <= 0 ? ' out' : s < 30 ? ' urgent' : ''}`} title="time left to decide; an order that expires is not dispatched">
      {s <= 0 ? 'expired' : `${fmtDuration(s)} left`}
    </span>
  )
}

/** What the link means, in its title: the officer's own answer naming the order, or only when it
 *  was proposed, never why. */
function turnTitle(l: TurnLink): string {
  if (l.explicit) return `The watch officer's answer to this request names this order as one it proposed for it${l.also ? ` (as do its answers to ${l.also} more)` : ''}.`
  const update = l.wake != null ? `update ${l.wake}` : 'the update'
  return `The watch officer proposed this in ${update}, the turn that first read this request${l.also ? ` (and ${l.also} more)` : ''}. That turn may have read other things too: this says when it was proposed, not why.`
}

/** On an order's card in the Inbox: the request it was proposed for (the officer's answer names it,
 *  or its turn read it), and the way to that message in the officer's drawer. */
export function TurnCause({ link, onShow }: { link: TurnLink; onShow: () => void }) {
  return (
    <p className="ix-turn" title={turnTitle(link)}>
      <span className="ix-label">{link.explicit ? 'Proposed for your request' : `Your request in this turn${link.wake != null ? ` (update ${link.wake})` : ''}`}</span>
      <span className="ix-turn-text">
        “{excerpt(link.item.text, 140)}”{link.also ? ` and ${link.also} more` : ''}
      </span>
      <button
        type="button"
        className="ghost mini"
        onClick={(e) => {
          e.stopPropagation()
          onShow()
        }}
      >
        Show message
      </button>
    </p>
  )
}
