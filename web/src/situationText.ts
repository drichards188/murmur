// The Situation panel's lines in a person's words (SituationPanel.tsx): a scripted operator's
// decision said once and as what it was, and the runner's and the engine's words with times on the
// situation's clock (T+m:ss) and our vehicles by callsign. Display only: the runner's records
// (situation.jsonl) keep their words. Pure: situationText.check.ts.
import { clockText, plainText, situationWhen } from './plainText.ts'

/** One scripted decision as the runner posts it (eval/proxy.py's records). */
export interface ScriptedDecision {
  /** epoch s */
  t: number
  intent_id: number | null
  action: string
  by?: string
  reason?: string
  result?: string
  declaration?: string
  track?: string
  granted?: boolean
  /** rules 30: a loss confirmed */
  callsign?: string | null
  uid?: number
}

export interface ScriptedLine {
  /** "T+3:45", or null before the situation's clock started */
  at: string | null
  text: string
  /** why the script did it, in its own words, without what it read before acting */
  why: string | null
  /** what it read before acting (a declaration's basis, which said "not confirmed by an operator"
   *  then), for a title: said as read before, never as if it still held */
  read: string | null
}

/** A rule declaration's confirmation ends its reason with the declaration's basis as the script
 *  read it before confirming (eval/proxy.py, and the headless operator alike). */
const CONFIRM_RULE = '(confirm_rule_declarations)'

const VERB: Record<string, string> = { approve: 'approved', reject: 'rejected', sign: 'signed' }

/** One scripted decision as a line. `zeroS`: the situation's start (epoch s), or null. */
export function scriptedLine(d: ScriptedDecision, zeroS: number | null, nameOf: (uid: string) => string): ScriptedLine {
  const by = d.by ?? '?'
  const result = d.result ? `: ${d.result}` : ''
  const iid = d.intent_id != null ? `i${d.intent_id}` : 'an order'
  let text: string
  switch (d.action) {
    case 'confirm':
      text = `${by} confirmed ${d.declaration ?? 'the declaration'} on ${d.track ?? '?'}${d.result && d.result !== 'confirmed' ? result : ''}`
      break
    case 'confirm_lost':
      text = `${by} confirmed ${d.callsign ?? (d.uid != null ? nameOf(String(d.uid)) : 'one of ours')} lost${result}`
      break
    case 'sign_failed':
      text = `${by} could not sign ${iid}${result}`
      break
    case 'error':
      text = `${by} met an error on ${iid}`
      break
    default:
      text = VERB[d.action]
        ? `${by} ${VERB[d.action]} ${iid}${d.granted ? ' (it grants every request of the AI)' : ''}${result}`
        : `${by} ${d.action.replace(/_/g, ' ')}${d.intent_id != null ? ` ${iid}` : ''}${result}`
  }
  const when = zeroS == null ? null : situationWhen(zeroS)
  const plain = (s: string) => (when ? plainText(s, { nameOf, when }) : s)
  let why: string | null = d.reason?.trim() || null
  let read: string | null = null
  if (why && d.action === 'confirm') {
    const k = why.indexOf(CONFIRM_RULE)
    if (k >= 0) {
      const rest = why.slice(k + CONFIRM_RULE.length).replace(/^:\s*/, '').trim()
      why = why.slice(0, k + CONFIRM_RULE.length)
      read = rest ? `As ${by} read it before confirming: ${plain(rest)}` : null
    }
  }
  return { at: zeroS == null ? null : `T+${clockText(d.t - zeroS)}`, text, why: why ? plain(why) : null, read }
}

/** The situation's start (epoch s) from its last status: the post's time less the time elapsed. */
export function situationZero(clock: { t: number; elapsed_s: number | null }): number | null {
  return clock.elapsed_s == null ? null : clock.t - clock.elapsed_s
}
