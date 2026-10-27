// A text as the console shows it: a one-line bold headline, a few short bullets, and the rest
// behind a "More" click (bluf.ts makes the parts). Used for an agent's rationale, alerts, the
// command log, review items, the checks list's summary, and a situation's brief and watch items.
//
// How to use it
//  - <Bluf parts={{ headline, bullets, more }} />: draw parts you built yourself from structured
//    fields (headline one line; 1 to 4 bullets; `more` plain text kept behind More, pre-wrapped).
//  - <Bluf parts={...} detail={node} />: the same, but More shows `node` (paragraphs, a list)
//    instead of `more`; the disclosure shows when either is given.
//  - <Bluf ... className="compact" />: a lighter headline for log rows and trails.
//  - <BlufReason text ctx /> for an engine or bridge string (identify outcomes are taken apart by
//    their fields, any other string splits on "; "), the original under More.
//  - <RationaleText /> (Rationale.tsx) for an agent's rationale: headline, "- " bullets, blank
//    line, detail (bluf.ts `parseRationale`).
//  - <ChecksFold checks>...</ChecksFold>: a checks list folded to one line ("7 checks passed",
//    "1 failed: domain"); the list is its children.
// The component inherits colour and size from its parent and stops click propagation inside More,
// so it sits in clickable cards. Style: bluf.css.
import type { ReactNode } from 'react'
import { type BlufParts, checksSummary, type Outcome, reasonBluf, type ReasonContext } from './bluf'
import './bluf.css'

export function Bluf({ parts, className = '', detail, children }: { parts: BlufParts; className?: string; detail?: ReactNode; children?: ReactNode }) {
  const { headline, bullets, more } = parts
  if (!headline && !bullets.length && !more && !detail) return null
  return (
    <div className={`bluf ${className}`}>
      {headline && <p className="bluf-head">{headline}</p>}
      {bullets.length > 0 && (
        <ul className="bluf-list">
          {bullets.map((b, k) => (
            <li key={k}>{b}</li>
          ))}
        </ul>
      )}
      {children}
      {(more || detail) && (
        <details className="bluf-more" onClick={(e) => e.stopPropagation()}>
          <summary>More</summary>
          <div className={`bluf-detail ${detail ? 'rich' : ''}`}>{detail ?? more}</div>
        </details>
      )}
    </div>
  )
}

/** An engine or bridge reason (bluf.ts `reasonBluf`), the original under More. */
export function BlufReason({ text, ctx, className }: { text: string | null | undefined; ctx?: ReasonContext; className?: string }) {
  return <Bluf parts={reasonBluf(text, ctx)} className={className} />
}

/** A checks list folded to its one line ("7 checks passed", "1 failed: domain"); a click opens
 *  the full list. `note` adds to the line (the engagement card says what changed since the
 *  nomination). */
export function ChecksFold({ checks, note, children }: { checks: { rule: string; outcome: Outcome }[]; note?: string; children: ReactNode }) {
  const sum = checksSummary(checks)
  return (
    <details className={`bluf-checks ${sum.tone}`} onClick={(e) => e.stopPropagation()}>
      <summary>
        <span className="bluf-checks-line">
          {sum.text}
          {note ? `; ${note}` : ''}
        </span>
      </summary>
      {children}
    </details>
  )
}
