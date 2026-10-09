// The playbook engine's evidence record rides in its rationale after a delimiter line
// (agents/playbook/rationale.py): the text a person reads, then the record as JSON.
export const EVIDENCE_DELIMITER = '--- evidence record (playbook, JSON) ---'
const SEP = `\n\n${EVIDENCE_DELIMITER}\n`

export function splitRationale(rationale: string): { text: string; record: string | null } {
  const i = rationale.indexOf(SEP)
  if (i < 0) return { text: rationale, record: null }
  return { text: rationale.slice(0, i), record: rationale.slice(i + SEP.length) }
}
