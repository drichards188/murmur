// An agent's rationale as a card shows it: its text, and the playbook engine's evidence record
// (everything after the delimiter line, when it is a record naming a play and its version:
// evidence.ts) folded behind a toggle, so the card reads as a sentence and a script or a curious
// operator can still open the facts it was rendered from. Any other rationale shows whole, as
// written.
import { splitRationale } from './evidence'
import './rationale.css'

export function RationaleText({ rationale }: { rationale: string }) {
  const { text, record } = splitRationale(rationale)
  return (
    <>
      {text}
      {record != null && (
        <details className="rationale-evidence" onClick={(e) => e.stopPropagation()}>
          <summary>Evidence record (the facts this was written from)</summary>
          <pre>{JSON.stringify(JSON.parse(record), null, 1)}</pre>
        </details>
      )}
    </>
  )
}
