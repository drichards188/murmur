import './hidden.css'

/** One thing the map or a list draws dim, leaves unlabelled or folds: what, how many, and the
 *  switch that shows all of it (or goes back to focus only). */
export interface HiddenRow {
  id: string
  /** "Tracks", "Labels" */
  what: string
  /** "455 of 458 dimmed" while hidden, "all 458 in full" while shown */
  count: string
  /** how it is drawn and how to see one without Show all, for the title */
  how: string
  /** something is hidden now (Show all would change what is drawn) */
  hidden: boolean
  /** Show all is on */
  all: boolean
  onToggle: () => void
}

/** Say what is hidden (docs/research/saturation-display.md R3): a small legend on the map that
 *  counts what is drawn dim, unlabelled or folded, each with Show all, so the person can see that
 *  something is hidden and anything hidden is one action away. Show all is a check, not the way
 *  of working: Focus only goes back. "Nothing hidden" when the view hides nothing. Display only. */
export function HiddenLegend({ rows, drawer }: { rows: HiddenRow[]; drawer: boolean }) {
  const shown = rows.filter((r) => r.hidden || r.all)
  return (
    <section className={`hidden-legend${drawer ? ' beside-drawer' : ''}`} aria-label="What the map and lists hide">
      {shown.length === 0 ? (
        <p className="hl-none" title="Every track, area, estimate and label in view is drawn in full, and no list is folded">
          Nothing hidden
        </p>
      ) : (
        <ul>
          {shown.map((r) => (
            <li key={r.id} className={r.all ? 'all' : ''} title={r.how}>
              <span className="hl-what">{r.what}</span>
              <span className="hl-count">{r.count}</span>
              <button className="hl-toggle" aria-pressed={r.all} onClick={r.onToggle} title={r.all ? `Back to focus only: ${r.how}` : `Show all ${r.what.toLowerCase()} in full`}>
                {r.all ? 'focus only' : 'show all'}
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
