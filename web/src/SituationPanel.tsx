import { useEffect, useMemo, useState } from 'react'
import type { Decisions } from './decisions'
import { Bluf } from './Bluf'
import { watchParts } from './bluf'
import { setScriptedBadges } from './situationIds'
import { clockText } from './plainText'
import { situationZero, type SituationTime, whatHappened, withoutCommands } from './situationText'
import './situation.css'

/** The Situation panel: shown only while a situation runner (`make situation`,
 *  `sim/situations/`) has posted its status to the bridge (`GET /api/situation`, read once for the
 *  console by useSituation.ts). Display only: the status is the runner's, never a decision input
 *  and not in the audit log. It opens to its chip (the run's title and the newest thing to watch:
 *  one line at the glance); a click opens the panel: what this run is, who decides, the brief,
 *  what to watch for, and one closed fold of what happened. The situation's time is the top bar's
 *  clock. With `reveal` off (a blinded run) the bridge strips the director's items. */

interface Entry {
  t_s: number
  kind: string
  text: string
  director?: boolean
}

/** A watch item: `headline` and `bullets` (a few words, 1 to 3 lines) when the file wrote them, `text` whole. */
export interface WatchItem {
  at_s: number
  text: string
  director?: boolean
  headline?: string
  bullets?: string[]
}

export interface SituationStatus {
  name: string
  title: string
  brief: string
  /** the file's glance at the brief (`[summary]`), when it wrote one */
  summary?: { headline: string; bullets: string[] }
  director?: string
  reveal: boolean
  modes: {
    operator: 'live' | 'autoplay'
    ai: 'claude' | 'codex' | 'playbook' | 'none'
    ai_label: string
    cosigner: 'you' | 'scripted' | 'scripted-pair'
    console_operator: string
    autoplay_operator?: string | null
    scripted_operators: string[]
    /** set when what the AI did is no evidence about it (a revealed run with a model AI) */
    evidence_note?: string | null
    /** the console's guided tutorial runs over this situation (Tutorial.tsx) */
    tutorial?: boolean
  }
  phase: string
  phase_detail: string
  clock: { t: number; elapsed_s: number | null; duration_s: number; remaining_s: number | null }
  orders: { id: string; task: string; vehicles?: string[]; by?: string; routed?: string; intent_id?: number; state?: string; error?: string }[]
  timeline: { done: Entry[] }
  watch: WatchItem[]
  agent: {
    kind: string
    label: string
    id: string | null
    status: string
  }
  autoplay: null | {
    operators: string[]
    mode: 'autoplay' | 'cosigner' | 'signers' | 'setup'
    /** an intent's approval, rejection or signature; or (`declaration`, `track`) a rule
     * declaration's confirmation; `granted`: an approval by the setting that grants every AI request */
    decisions: {
      t: number
      intent_id: number | null
      action: string
      by?: string
      reason?: string
      result?: string
      declaration?: string
      track?: string
      granted?: boolean
    }[]
  }
  run: { id: string; audit_log: string; situation_log: string }
  /** what went wrong in the runner itself */
  problems?: string[]
  /** every scripted id of the run and its badge (the Inbox shows it on their cards) */
  scripted?: Record<string, string>
}

/** a watch item this new (situation s) is marked new */
const FRESH_S = 60

export function SituationPanel({
  s,
  time,
  decisions,
  nameOf,
}: {
  s: SituationStatus
  time: SituationTime
  decisions: Decisions | null
  /** one of ours by its uid (its callsign), for the engine's words the panel shows */
  nameOf?: (uid: string) => string
}) {
  // closed to its chip at the start (one line at the glance; the panel a click away)
  const [open, setOpen] = useState(false)
  // the Inbox badges a scripted id's card (kept outside React: a few ids, display only)
  const badges = s.scripted
  useEffect(() => setScriptedBadges(badges), [badges])
  const { elapsed, stale, running, sincePost } = time
  const watch = [...s.watch].reverse()
  const latestWatch = watch[0]
  const fresh = (at: number) => elapsed != null && elapsed - at < FRESH_S
  const name = nameOf ?? ((uid: string) => `vehicle ${uid}`)
  const zero = situationZero(s.clock)
  // the AI's orders (intents whose source is the run's AI), for what happened
  const agentId = s.agent.id
  const intents = decisions?.intents
  const proposals = useMemo(() => (agentId ? (intents ?? []).filter((i) => i.source.kind === 'agent' && i.source.id === agentId) : []), [agentId, intents])
  const happened = useMemo(
    () => whatHappened(s.timeline.done, proposals, zero, s.modes.ai_label, new Set(s.modes.scripted_operators), name),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [s, proposals, zero],
  )

  if (!open) {
    const head = latestWatch ? watchParts(latestWatch).headline : null
    return (
      <button className="sit-chip" onClick={() => setOpen(true)} title="Show the situation panel: what this run is, your job, what to watch for">
        SITUATION <b>{s.title}</b>
        {head && <span className="sit-chip-next">{head}</span>}
        {latestWatch && fresh(latestWatch.at_s) && <i className="sit-dot" aria-label="new item to watch for" />}
      </button>
    )
  }

  const earlier = watch.slice(1)
  return (
    <aside className="sit" aria-label="Situation">
      <div className="sit-top">
        <span className="sit-label">Situation</span>
        <h2>{s.title}</h2>
        <button className="ghost close" onClick={() => setOpen(false)} aria-label="Collapse the situation panel" title="Collapse">
          –
        </button>
      </div>
      <div className="sit-chips">
        {s.modes.operator === 'live' ? (
          <span className="sit-mode you" title={`You decide at this console, as ${s.modes.console_operator}`}>
            You decide
          </span>
        ) : (
          <span className="sit-mode scripted" title={`A scripted operator model (${s.modes.autoplay_operator ?? 'scripted'}) decides: not a person, never evidence`}>
            Scripts decide (autoplay)
          </span>
        )}
        <span className={`sit-mode ai ${s.modes.ai}`}>AI: {s.modes.ai_label}</span>
        {s.modes.operator === 'live' && s.modes.cosigner === 'scripted' && (
          <span className="sit-mode scripted" title="A scripted operator adds an engagement's second signature, never the first">
            cosigner: scripted
          </span>
        )}
        {s.modes.operator === 'live' && s.modes.cosigner === 'scripted-pair' && (
          <span className="sit-mode scripted" title="op-2 and op-3, scripts, sign a nomination you made: a proposer never signs their own">
            signers: op-2, op-3 (scripted)
          </span>
        )}
      </div>
      {/* the phase only when the run is not simply running; the time is the top bar's */}
      {(stale || !running) && (
        <div className="sit-clock">
          <span className={`sit-phase ${stale ? 'failed' : s.phase}`}>{stale ? 'not reporting' : s.phase}</span>
          {!stale && s.phase_detail && <small>{withoutCommands(s.phase_detail)}</small>}
        </div>
      )}
      {stale && (
        <p className="sit-warn">
          Runner not reporting: no status for {Math.round(sincePost)} s. The stack may still be running; this is its last status.
        </p>
      )}
      {s.modes.evidence_note && <p className="sit-warn">{s.modes.evidence_note}</p>}
      <div className="sit-scroll">
        {s.summary ? (
          <section className="sit-brief sit-summary">
            <span className="sit-label">Brief</span>
            <Bluf
              parts={{ headline: s.summary.headline, bullets: s.summary.bullets, more: s.brief }}
              detail={s.brief.split(/\n\s*\n/).map((p, k) => (
                <p key={k}>{p}</p>
              ))}
            />
          </section>
        ) : (
          <details className="sit-brief" open>
            <summary>Brief</summary>
            {s.brief.split(/\n\s*\n/).map((p, k) => (
              <p key={k}>{p}</p>
            ))}
          </details>
        )}

        <h3 className="sit-h">Watch for</h3>
        {latestWatch ? (
          <ul className="sit-watch">
            <li className={fresh(latestWatch.at_s) ? 'new' : ''}>
              <time>{clockText(latestWatch.at_s)}</time>
              <Bluf parts={watchParts(latestWatch)} />
            </li>
          </ul>
        ) : (
          <p className="sit-empty">Nothing yet.</p>
        )}
        {earlier.length > 0 && (
          <details className="sit-fold">
            <summary>Earlier ({earlier.length})</summary>
            <ul className="sit-watch">
              {earlier.map((w, k) => (
                <li key={`${w.at_s}-${k}`}>
                  <time>{clockText(w.at_s)}</time>
                  <Bluf parts={watchParts(w)} />
                </li>
              ))}
            </ul>
          </details>
        )}

        {happened.length > 0 && (
          <details className="sit-fold">
            <summary>What happened ({happened.length})</summary>
            <ul className="sit-tl">
              {happened.map((h, k) => (
                <li key={k} className={`sit-k-${h.kind}`} title={h.title ?? undefined}>
                  {h.t_s != null && <time>{clockText(h.t_s)}</time>}
                  {h.text}
                </li>
              ))}
            </ul>
          </details>
        )}

        {(s.problems?.length ?? 0) > 0 && (
          <ul className="sit-warn">
            {s.problems!.map((p, k) => (
              <li key={k}>{p}</li>
            ))}
          </ul>
        )}
        {s.orders.some((o) => o.error) && (
          <p className="sit-warn">
            {s.orders
              .filter((o) => o.error)
              .map((o) => `opening order ${o.id}: ${o.error}`)
              .join('; ')}
          </p>
        )}
      </div>
    </aside>
  )
}
