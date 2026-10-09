// The playbook engine's evidence record rides in its rationale after a delimiter line
// (agents/playbook/rationale.py): the text a person reads, then the record as JSON.
export const EVIDENCE_DELIMITER = '--- evidence record (playbook, JSON) ---'
const SEP = `\n\n${EVIDENCE_DELIMITER}\n`
// a play and its version, as the record names them: "isr.investigate@1"
const PLAY = /^[a-z][a-z0-9_]*\.[a-z0-9_.]+@\d+$/
// the agent ids the console knows run the playbook engine (agents/playbook_agent.py)
export const PLAYBOOK_AGENTS: readonly string[] = ['agent-playbook']

// The text and the record, folded only when the card was proposed by an agent id the console
// knows runs the playbook, and what follows the delimiter is a JSON object naming a play and its
// version. Anyone can type the delimiter line into a rationale; another proposer's text after it,
// a model agent's included, shows whole, as written, never hidden behind the toggle.
export function splitRationale(
  rationale: string,
  source?: { kind: string; id: string } | null,
): { text: string; record: string | null } {
  const whole = { text: rationale, record: null }
  if (!source || source.kind !== 'agent' || !PLAYBOOK_AGENTS.includes(source.id)) return whole
  const i = rationale.indexOf(SEP)
  if (i < 0) return whole
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
  return whole
}
