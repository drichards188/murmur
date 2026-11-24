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

// ---- the situation's clock, on this console ----------------------------------------------------

/** No new status for this long (s) while the runner starts or runs: it is not reporting. */
export const STALE_S = 10

/** Where the clock was when this console received the runner's last status, by this console's
 *  clock (s): the runner's host clock is never compared with this one's. */
export interface ClockAnchor {
  elapsed: number | null
  at: number
}

export interface SituationTime {
  /** situation time (s) now, never past its duration; null before its clock started */
  elapsed: number | null
  left: number | null
  /** the runner said it runs or starts, and has posted nothing for STALE_S */
  stale: boolean
  running: boolean
  /** seconds since the last status came */
  sincePost: number
}

/** The situation's clock now: the last status's elapsed time, run on by this console's clock
 *  since it came while the runner runs (`nowS`, this console's s). */
export function situationTime(clock: { elapsed_s: number | null; duration_s: number }, phase: string, anchor: ClockAnchor | null, nowS: number): SituationTime {
  const sincePost = anchor ? Math.max(0, nowS - anchor.at) : 0
  const stale = (phase === 'running' || phase === 'starting') && sincePost > STALE_S
  const running = phase === 'running' && !stale
  const base = anchor ? anchor.elapsed : clock.elapsed_s
  const elapsed = base == null ? null : Math.min(clock.duration_s, base + (running ? sincePost : 0))
  const left = elapsed == null ? null : Math.max(0, clock.duration_s - elapsed)
  return { elapsed, left, stale, running, sincePost }
}

/** The runner's phase detail without the command line in it (the menu's End is the way to end
 *  it from the console): "... until you end it (End in the menu, or make down)" says "(End in the
 *  menu)". */
export function withoutCommands(detail: string): string {
  return detail
    .replace(/,? or `?make down`?/g, '')
    .replace(/[:;,]?\s*`?make down`? stops it/g, '')
    .trim()
}

// ---- what happened, in plain words -------------------------------------------------------------

/** One line of the panel's "What happened": the runner's timeline, the scripted operators'
 *  decisions and the AI's proposals, merged by time. */
export interface HappenedLine {
  /** situation time (s), or null before its clock started */
  t_s: number | null
  kind: 'timeline' | 'scripted' | 'ai'
  text: string
  /** the why, for the hover */
  title: string | null
}

interface TimelineEntry {
  t_s: number
  kind: string
  text: string
}

/** One of the AI's orders as the console has it (an intent with the AI as its source). */
export interface Proposal {
  id: number
  kind: string
  /** bridge ms (the wall clock's) */
  created_t: number
  state: string
  uids: readonly string[]
  approval: { by: string } | null
  rationale: string | null
}

const PROPOSAL_STATE: Record<string, string> = {
  pending_approval: 'waiting for a decision',
  rejected: 'rejected',
  expired: 'expired before anyone decided',
  voided: 'voided',
  failed: 'failed',
}

/** What became of an AI proposal, in words: "approved by op-1", "sent by policy", "rejected". */
export function proposalState(p: Pick<Proposal, 'state' | 'approval'>, scripted: ReadonlySet<string>): string {
  const said = PROPOSAL_STATE[p.state]
  if (said) return said
  const by = p.approval ? `approved by ${p.approval.by}${scripted.has(p.approval.by) ? ' (scripted)' : ''}` : 'sent by policy'
  return p.state === 'queued' ? `${by}, held for its link` : by
}

/** The panel's "What happened", the newest first: the runner's timeline (in plain words) and
 *  each of the AI's proposals with what became of it. The scripts' decisions are in the timeline
 *  already, marked scripted (the engagement chain from the audit log, the runner's own approve and
 *  reject lines): merging their records too said each one twice. `zeroS`: the situation's start
 *  (epoch s); `ai`: the AI's label. */
export function whatHappened(
  timeline: readonly TimelineEntry[],
  proposals: readonly Proposal[],
  zeroS: number | null,
  ai: string,
  scripted: ReadonlySet<string>,
  nameOf: (uid: string) => string,
): HappenedLine[] {
  const when = zeroS == null ? null : situationWhen(zeroS)
  const plain = (s: string) => (when ? plainText(s, { nameOf, when }) : s)
  const out: HappenedLine[] = timeline.map((e) => ({ t_s: e.t_s, kind: 'timeline', text: plain(e.text), title: null }))
  for (const p of proposals) {
    const who = p.uids.map(nameOf).join(', ')
    out.push({
      t_s: zeroS == null ? null : p.created_t / 1000 - zeroS,
      kind: 'ai',
      text: `The AI (${ai}) proposed ${p.kind} i${p.id}${who ? ` for ${who}` : ''}: ${proposalState(p, scripted)}`,
      title: p.rationale,
    })
  }
  return out.sort((a, b) => (b.t_s ?? -Infinity) - (a.t_s ?? -Infinity))
}

// ---- the map's first view ----------------------------------------------------------------------

/** A weapon situation opens in top view (the cutdown's D6: keep-outs, distances and the strike are
 *  plan judgements), 3D one key (T) away; the open world as it was. A weapon situation: a situation
 *  runs (its status on GET /api/situation) and its fleet has an armed platform (`identity.armed`). */
export function opensInTopView(situationRuns: boolean, fleet: readonly { identity?: { armed?: boolean | null } | null }[]): boolean {
  return situationRuns && fleet.some((v) => !!v.identity?.armed)
}
