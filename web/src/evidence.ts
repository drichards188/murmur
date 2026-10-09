// The playbook engine's evidence record rides in its rationale after a delimiter line
// (agents/playbook/rationale.py): the text a person reads, then the record as JSON.
export const EVIDENCE_DELIMITER = '--- evidence record (playbook, JSON) ---'
const SEP = `\n\n${EVIDENCE_DELIMITER}\n`
// a play and its version, as the record names them: "isr.investigate@1"
const PLAY = /^[a-z][a-z0-9_]*\.[a-z0-9_.]+@\d+$/

// The text and the record, folded only when what follows the delimiter is a JSON object naming
// a play and its version: anyone can type the delimiter line into a rationale, and what follows
// it then shows whole, as written, never hidden behind the toggle.
export function splitRationale(rationale: string): { text: string; record: string | null } {
  const i = rationale.indexOf(SEP)
  if (i < 0) return { text: rationale, record: null }
  const body = rationale.slice(i + SEP.length)
  try {
    const rec: unknown = JSON.parse(body)
    if (rec !== null && typeof rec === 'object' && !Array.isArray(rec)) {
      const play = (rec as { play?: unknown }).play
      if (typeof play === 'string' && PLAY.test(play)) return { text: rationale.slice(0, i), record: body }
    }
  } catch {
    // not a record: shown whole
  }
  return { text: rationale, record: null }
}
