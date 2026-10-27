// An agent's rationale as a card shows it: its text, and the playbook engine's evidence record
// (everything after the delimiter line, when the card's proposer is an agent id that runs the
// playbook and the rest is a record naming a play and its version: evidence.ts) folded behind a
// toggle, so the card reads as a sentence and a script or a curious operator can still open the
// facts it was rendered from. The text shows as a BLUF (bluf.ts `parseRationale`): the headline
// the agent was asked to lead with, its bullets, and the rest behind More; an older free-text
// rationale shows its first sentence with the rest behind More. Nothing is dropped.
import { Bluf } from './Bluf'
import { parseRationale } from './bluf'
import { splitRationale } from './evidence'
import './rationale.css'

export function RationaleText({ rationale, source }: { rationale: string; source?: { kind: string; id: string } | null }) {
  const { text, record } = splitRationale(rationale, source)
  return (
    <>
      <Bluf parts={parseRationale(text)} />
      {record != null && (
        <details className="rationale-evidence" onClick={(e) => e.stopPropagation()}>
          <summary>Evidence record (the facts this was written from)</summary>
          <pre>{JSON.stringify(JSON.parse(record), null, 1)}</pre>
        </details>
      )}
    </>
  )
}
