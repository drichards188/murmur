import { useEffect, useState } from 'react'
import type { Decisions, Intent } from './decisions'
import { RationaleText } from './Rationale'
import { setScriptedBadges } from './situationIds'
import { usePoll } from './space'
import './situation.css'

/** The Situation panel: shown only while a situation runner (`make situation`,
 *  `sim/situations/`) has posted its status to the bridge (`GET /api/situation`). Display
 *  only: the status is the runner's, never a decision input and not in the audit log. With
 *  `reveal` off (a blinded run) it carries no director's notes and nothing still to come (the
 *  bridge strips them whatever the runner sends). The clock runs on this console's own clock
 *  from each post's arrival; a runner that stops posting is shown as not reporting. */

interface Entry {
  t_s: number
  kind: string
  text: string
  director?: boolean
}

interface Activity {
  t: number
  /** situation time (s), stamped by the runner */
  t_s: number | null
  action: string
  detail: string
}

export interface SituationStatus {
  name: string
  title: string
  brief: string
  director?: string
  reveal: boolean
  modes: {
    operator: 'live' | 'autoplay'
    ai: 'claude' | 'codex' | 'playbook' | 'none'
    ai_label: string
    cosigner: 'you' | 'scripted'
    console_operator: string
    autoplay_operator?: string | null
    scripted_operators: string[]
    /** set when what the AI did is no evidence about it (a revealed run with a model AI) */
    evidence_note?: string | null
  }
  phase: string
  phase_detail: string
  clock: { t: number; elapsed_s: number | null; duration_s: number; remaining_s: number | null }
  orders: { id: string; task: string; vehicles?: string[]; by?: string; routed?: string; intent_id?: number; state?: string; error?: string }[]
  timeline: { done: Entry[]; upcoming?: { at_s: number; id: string; kind: string; text: string }[] }
  watch: { at_s: number; text: string; director?: boolean }[]
  watch_upcoming?: { at_s: number; text: string; director?: boolean }[]
  agent: {
    kind: string
    label: string
    id: string | null
    status: string
    activity: Activity[]
    notes: { t: number | null; t_s: number | null; kind: string; text: string }[]
  }
  autoplay: null | {
    operators: string[]
    mode: 'autoplay' | 'cosigner' | 'setup'
    decisions: { t: number; intent_id: number; action: string; by?: string; reason?: string; result?: string }[]
  }
  run: { id: string; audit_log: string; situation_log: string }
  /** what went wrong in the runner itself */
  problems?: string[]
  /** every scripted id of the run and its badge (the Inbox shows it on their cards) */
  scripted?: Record<string, string>
}

interface Envelope {
  situation: SituationStatus | null
  received_t: number | null
  /** how many statuses the runner has posted (a new one every 2 s while it runs) */
  posts?: number
}

/** no new status for this long (s) while the runner starts or runs: it is not reporting */
const STALE_S = 10

const DECIDES: Record<string, string> = {
  autoplay: 'autoplay: not people',
  cosigner: 'cosign only, never first',
  setup: 'live: setup and timeline only, they never decide',
}

/** m:ss of situation time */
function clock(s: number) {
  const n = Math.max(0, Math.round(s))
  return `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`
}

const STATE_TONE: Record<string, string> = {
  pending_approval: 'wait',
  approved: 'ok',
  auto_approved: 'ok',
  dispatched: 'ok',
  done: 'ok',
  queued: 'wait',
  rejected: 'bad',
  expired: 'bad',
  voided: 'bad',
  failed: 'bad',
}

function who(i: Intent) {
  const a = i.approval
  if (!a) return i.state === 'pending_approval' ? 'waiting for a decision' : i.policy?.tier === 'auto' ? 'by policy (auto)' : ''
  return `${a.at_submission ? 'authorized' : 'approved'} by ${a.by}`
}

export function SituationPanel({ decisions }: { decisions: Decisions | null }) {
  const { data } = usePoll<Envelope>('/api/situation', 2000)
  const [open, setOpen] = useState(true)
  const [showAll, setShowAll] = useState(false)
  // this console's clock (s), ticking each second: the situation clock is extrapolated from it
  const [nowS, setNowS] = useState(() => Date.now() / 1000)
  useEffect(() => {
    const id = window.setInterval(() => setNowS(Date.now() / 1000), 1000)
    return () => window.clearInterval(id)
  }, [])
  const s = data?.situation
  // the Inbox badges a scripted id's card (kept outside React: a few ids, display only)
  const badges = s?.scripted
  useEffect(() => setScriptedBadges(badges), [badges])
  const posts = data?.posts ?? 0
  const postedElapsed = s?.clock.elapsed_s ?? null
  // anchored on when this console received each new post (as useScenarioNow does), never on
  // the runner's host clock against this one's
  // (state adjusted while rendering when a new post has come: React's pattern for state that
  // follows a prop)
  const [anchor, setAnchor] = useState<{ posts: number; elapsed: number | null; at: number } | null>(null)
  if (s && (anchor == null || anchor.posts !== posts)) {
    setAnchor({ posts, elapsed: postedElapsed, at: nowS })
  }
  if (!s) return null
  const sincePost = anchor ? Math.max(0, nowS - anchor.at) : 0
  const stale = (s.phase === 'running' || s.phase === 'starting') && sincePost > STALE_S
  const running = s.phase === 'running' && !stale
  const base = anchor && anchor.posts === posts ? anchor.elapsed : s.clock.elapsed_s
  const elapsed = base == null ? null : Math.min(s.clock.duration_s, base + (running ? sincePost : 0))
  const left = elapsed == null ? null : Math.max(0, s.clock.duration_s - elapsed)
  const frac = elapsed == null ? 0 : Math.min(1, elapsed / s.clock.duration_s)
  const scripted = new Set(s.modes.scripted_operators)
  const agentIntents = (decisions?.intents ?? []).filter((i) => s.agent.id && i.source.kind === 'agent' && i.source.id === s.agent.id)
  const done = [...s.timeline.done].reverse()
  const watch = [...s.watch].reverse()
  const latestWatch = watch[0]
  const fresh = (at: number) => elapsed != null && elapsed - at < 60

  if (!open)
    return (
      <button className="sit-chip" onClick={() => setOpen(true)} title="Show the situation panel">
        SITUATION <b>{s.title}</b>
        {elapsed != null && <span>T+{clock(elapsed)}</span>}
        {latestWatch && fresh(latestWatch.at_s) && <i className="sit-dot" aria-label="new item to watch for" />}
      </button>
    )

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
          <span className="sit-mode you" title="You decide at this console">
            operator: you ({s.modes.console_operator}, live)
          </span>
        ) : (
          <span className="sit-mode scripted" title="A scripted operator model decides: not a person, never evidence">
            operator: autoplay ({s.modes.autoplay_operator ?? 'scripted'}), scripted
          </span>
        )}
        <span className={`sit-mode ai ${s.modes.ai}`}>AI: {s.modes.ai_label}</span>
        {s.modes.operator === 'live' && s.modes.cosigner === 'scripted' && (
          <span className="sit-mode scripted" title="A scripted operator adds an engagement's second signature, never the first">
            cosigner: scripted
          </span>
        )}
        {s.reveal && <span className="sit-mode reveal" title="Director's notes and the timeline to come are shown">revealed</span>}
      </div>
      <div className="sit-clock">
        <b>T+{elapsed == null ? '--:--' : clock(elapsed)}</b>
        <span>{left == null ? '' : `${clock(left)} left of ${clock(s.clock.duration_s)}`}</span>
        <span className={`sit-phase ${stale ? 'failed' : s.phase}`} title={s.phase_detail}>
          {stale ? 'not reporting' : s.phase}
        </span>
        <span className="sit-bar">
          <span style={{ width: `${(frac * 100).toFixed(1)}%` }} />
        </span>
        {!running && !stale && s.phase_detail && <small>{s.phase_detail}</small>}
      </div>
      {stale && (
        <p className="sit-warn">
          Runner not reporting: no status for {Math.round(sincePost)} s. The stack may still be running; this is its last status.
        </p>
      )}
      {s.modes.evidence_note && <p className="sit-warn">{s.modes.evidence_note}</p>}
      <div className="sit-scroll">
        <details className="sit-brief" open>
          <summary>Brief</summary>
          {s.brief.split(/\n\s*\n/).map((p, k) => (
            <p key={k}>{p}</p>
          ))}
        </details>

        <h3 className="sit-h">
          Watch for <span>{watch.length}</span>
        </h3>
        {watch.length === 0 && <p className="sit-empty">Nothing yet.</p>}
        <ul className="sit-watch">
          {watch.map((w, k) => (
            <li key={`${w.at_s}-${k}`} className={`${fresh(w.at_s) ? 'new' : ''}${w.director ? ' sit-director' : ''}`}>
              <time>{clock(w.at_s)}</time>
              {w.text}
              {w.director && <small> (director, revealed)</small>}
            </li>
          ))}
        </ul>

        <h3 className="sit-h">
          AI <span>{s.agent.label}</span>
          <small>
            {s.agent.id ? `${s.agent.id}: ` : ''}
            {s.agent.status}
          </small>
        </h3>
        {s.modes.ai === 'none' && <p className="sit-empty">No AI on this run.</p>}
        {agentIntents.length > 0 && (
          <ul className="sit-ai">
            {agentIntents.slice(0, 6).map((i) => (
              <li key={i.id}>
                <div>
                  <b>i{i.id}</b> proposed {i.kind}
                  <span className={`sit-state ${STATE_TONE[i.state] ?? ''}`}>{i.state.replace(/_/g, ' ')}</span>
                  <small>{who(i)}{i.approval && scripted.has(i.approval.by) ? ' (scripted)' : ''}</small>
                </div>
                {i.rationale && (
                  <div className="sit-rationale">
                    <RationaleText rationale={i.rationale} source={i.source} />
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
        {s.agent.activity.length > 0 && (
          <ul className="sit-log">
            {[...s.agent.activity].reverse().slice(0, 6).map((a, k) => (
              <li key={k}>
                <time>{a.t_s != null ? clock(a.t_s) : ''}</time>
                <b>{a.action}</b> {a.detail}
              </li>
            ))}
          </ul>
        )}
        {s.agent.notes.length > 0 && (
          <ul className="sit-log notes">
            {[...s.agent.notes].reverse().slice(0, 4).map((n, k) => (
              <li key={k}>
                <time>{n.t_s != null ? clock(n.t_s) : ''}</time>
                <b>{n.kind}</b> {n.text}
              </li>
            ))}
          </ul>
        )}
        {s.modes.ai !== 'none' && agentIntents.length === 0 && s.agent.activity.length === 0 && s.agent.notes.length === 0 && (
          <p className="sit-empty">No AI action yet.</p>
        )}

        {s.autoplay && (
          <>
            <h3 className="sit-h">
              Scripted operators <span>{s.autoplay.operators.join(', ')}</span>
              <small>{DECIDES[s.autoplay.mode] ?? ''}</small>
            </h3>
            {s.autoplay.decisions.filter((d) => d.action !== 'wait').length === 0 && <p className="sit-empty">No decision yet.</p>}
            <ul className="sit-log">
              {[...s.autoplay.decisions]
                .filter((d) => d.action !== 'wait')
                .reverse()
                .slice(0, 5)
                .map((d, k) => (
                  <li key={k}>
                    <b>{d.by ?? '?'}</b> {d.action} i{d.intent_id}: {d.result}
                    {d.reason && <small> {d.reason}</small>}
                  </li>
                ))}
            </ul>
          </>
        )}

        <h3 className="sit-h">
          Timeline <span>{done.length}</span>
          {done.length > 6 && (
            <button className="sit-more" onClick={() => setShowAll((v) => !v)}>
              {showAll ? 'fewer' : 'all'}
            </button>
          )}
        </h3>
        <ul className="sit-tl">
          {(showAll ? done : done.slice(0, 6)).map((e, k) => (
            <li key={k} className={`sit-k-${e.kind}${e.director ? ' sit-director' : ''}`}>
              <time>{clock(e.t_s)}</time>
              {e.text}
            </li>
          ))}
        </ul>
        {s.reveal && (s.timeline.upcoming?.length ?? 0) > 0 && (
          <>
            <h3 className="sit-h">
              To come <small>revealed</small>
            </h3>
            <ul className="sit-tl upcoming">
              {s.timeline.upcoming!.map((u) => (
                <li key={u.id}>
                  <time>{clock(u.at_s)}</time>
                  {u.text}
                </li>
              ))}
            </ul>
          </>
        )}
        {s.reveal && s.director && (
          <details className="sit-brief director">
            <summary>Director's notes (revealed)</summary>
            {s.director.split(/\n\s*\n/).map((p, k) => (
              <p key={k}>{p}</p>
            ))}
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
        <p className="sit-foot">
          Status from the situation runner: display only, not a decision input. Run {s.run.id}; audit log {s.run.audit_log}.
        </p>
      </div>
    </aside>
  )
}
