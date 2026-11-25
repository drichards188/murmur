// The decisions layout's column while nothing needs the person (Decisions.tsx): it says so, says
// what to watch for next in a situation, and gives a quiet glance at ours and the contacts with
// the way into their lists, so the main column is never blank. It sits in the Inbox's own list,
// where the Inbox says "Nothing needs you" (decisions.css shows it only then). Display only.
import type { ConsoleFacts } from './parts'
import { contactsLine, type GlanceLine, oursLine, type RefId } from './decisionsModel'

function Line({ line }: { line: GlanceLine }) {
  return line.n != null ? (
    <>
      <b>{line.n}</b> {line.words}
    </>
  ) : (
    <>{line.words}</>
  )
}

export function DecisionsQuiet({ facts, onOpen }: { facts: ConsoleFacts; onOpen: (id: RefId) => void }) {
  // the bridge not answering: nothing can be said about what needs the person
  if (!facts.live)
    return (
      <section className="dc-quiet" aria-label="Not connected">
        <h3 className="dc-quiet-h">Not connected</h3>
        <p className="dc-quiet-sub">What needs you shows here once the bridge answers.</p>
      </section>
    )
  const g = facts.glance
  return (
    <section className="dc-quiet" aria-label="Nothing needs you">
      <div className="dc-quiet-head">
        <h3 className="dc-quiet-h">Nothing needs you</h3>
        <p className="dc-quiet-sub">Approvals, signatures, reviews and alerts arrive here, the most consequential first.</p>
      </div>
      {facts.watch && (
        <div className="dc-quiet-watch">
          <span className="dc-label">Watch for</span>
          <p>{facts.watch}</p>
        </div>
      )}
      <dl className="dc-quiet-glance">
        <dt className="dc-label">Ours</dt>
        <dd>
          <Line line={oursLine(facts.vehicles, facts.trouble?.n ?? 0)} />
        </dd>
        <dd>
          <button className="ghost" onClick={() => onOpen('fleet')} title="Open the Fleet list">
            Fleet
          </button>
        </dd>
        {g && (
          <>
            <dt className="dc-label">Contacts</dt>
            <dd>
              <Line line={contactsLine(g.contacts, g.hostile, g.suspect)} />
            </dd>
            <dd>
              <button className="ghost" onClick={() => onOpen('tracks')} title="Open the Tracks list">
                Tracks
              </button>
            </dd>
          </>
        )}
      </dl>
      <p className="dc-quiet-hint">To give an order, select vehicles on the map, then open Log.</p>
    </section>
  )
}
