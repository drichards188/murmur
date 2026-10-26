import { useEffect, useState } from 'react'
import { type AiMode, endScenario, isActive, type Launch, type LauncherState, type OperatorMode, type Scenario, type Stage, startScenario } from './launcher'
import './menu.css'

/** The main menu (`make start`): the open world first (the default), then the situations, each
 *  with what it has; a situation's operator and AI with its own defaults chosen; the launch's
 *  progress, and the scenario running with Return and End. Over the console, which stays live
 *  behind it while a scenario runs. */

const AI_LABEL: Record<AiMode, string> = { claude: 'Claude', codex: 'ChatGPT (Codex)', playbook: 'Playbook', none: 'None' }
const AI_NOTE: Record<AiMode, string> = {
  claude: 'The Claude watch officer, through your own Claude Code login (Murmur never sees your credentials).',
  codex: 'The watch officer through your own Codex CLI login (Murmur never sees your credentials).',
  playbook: 'The playbook engine: deterministic, no model, no account.',
  none: 'No AI on this run.',
}
const OPERATOR_LABEL: Record<OperatorMode, string> = { live: 'You (live)', autoplay: 'Autoplay' }
const OPERATOR_NOTE: Record<OperatorMode, string> = {
  live: 'You decide at the console, as op-1.',
  autoplay: 'A scripted operator (op-auto) decides and scripted signers sign: you watch. Scripts, never people.',
}
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/** m:ss */
function mmss(s: number) {
  const n = Math.max(0, Math.round(s))
  return `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`
}

/** "14 Jun 2026, 10:00Z" */
function fmtAt(at: string | undefined) {
  const d = at ? new Date(at) : null
  if (!d || Number.isNaN(d.getTime())) return at ?? ''
  const hm = `${String(d.getUTCHours()).padStart(2, '0')}:${String(d.getUTCMinutes()).padStart(2, '0')}Z`
  return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}, ${hm}`
}

const plural = (n: number | undefined, one: string, many = `${one}s`) => `${n ?? 0} ${n === 1 ? one : many}`

/** a card's one line of facts */
function cardFacts(s: Scenario): string {
  if (!s.runnable) return s.why_not ?? ''
  if (s.kind === 'open_world') return [plural(s.vehicles, 'vehicle'), plural(s.platform_types, 'platform type'), 'tide, current, wind', plural(s.contacts, 'contact')].join(' · ')
  return [
    plural(s.vehicles, 'vehicle'),
    `${Math.round((s.duration_s ?? 0) / 60)} min`,
    `operator ${s.operator === 'live' ? 'you' : 'autoplay'}`,
    `AI ${AI_LABEL[s.ai ?? 'none']}`,
  ].join(' · ')
}

/** how long vehicles usually take, as the menu says while it waits */
function typical([lo, hi]: [number, number]) {
  return hi >= 60 ? `Vehicles usually take ${lo} to ${hi} s to be ready (ArduPilot SITL).` : `Vehicles are usually ready in ${lo} to ${hi} s (stand-ins).`
}

interface MenuProps {
  state: LauncherState
  /** this browser's clock (ms) when the state came */
  received: number
  lost: boolean
  onUpdate: (current: Launch | null) => void
  /** back to the console, when a scenario is up */
  onReturn?: () => void
}

export function Menu({ state, received, lost, onUpdate, onReturn }: MenuProps) {
  const cur = state.current
  const scenarios = state.scenarios
  const startable = scenarios.filter((s) => s.runnable)
  const openWorld = startable.filter((s) => s.kind === 'open_world')
  const situations = startable.filter((s) => s.kind === 'situation')
  const others = scenarios.filter((s) => !s.runnable)
  // chosen: the scenario running, else the default (the open world)
  const [selId, setSelId] = useState(() => (isActive(cur) ? cur.scenario : (scenarios.find((s) => s.default)?.id ?? scenarios[0]?.id)))
  const sel = scenarios.find((s) => s.id === selId) ?? scenarios[0]
  // the launcher's clock, ticking here between its answers
  const [nowMs, setNowMs] = useState(() => Date.now())
  useEffect(() => {
    const id = window.setInterval(() => setNowMs(Date.now()), 1000)
    return () => window.clearInterval(id)
  }, [])
  const now = state.now + (nowMs - received) / 1000
  // Escape goes back to the console, when one is up
  useEffect(() => {
    if (!onReturn) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onReturn()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onReturn])

  const card = (s: Scenario) => {
    const live = isActive(cur) && cur.scenario === s.id
    return (
      <button key={s.id} className={`mm-card${s.id === sel?.id ? ' sel' : ''}${s.runnable ? '' : ' off'}`} aria-pressed={s.id === sel?.id} onClick={() => setSelId(s.id)}>
        <span className="mm-card-top">
          {s.default && <span className="mm-tag">Default</span>}
          <span className="mm-card-title">{s.title}</span>
          {live && <span className="mm-tag live">{cur.stage === 'ended' ? 'time up' : cur.stage === 'running' ? 'running' : 'starting'}</span>}
        </span>
        <span className="mm-card-facts">{cardFacts(s)}</span>
      </button>
    )
  }

  return (
    <div className="mm" role="dialog" aria-modal="true" aria-label="Main menu">
      <header className="mm-top">
        <span className="brand">MURMUR</span>
        <span className="sub">multi-domain swarm C2</span>
        <span className="mm-where">Main menu</span>
        <span className="spacer" />
        {lost && (
          <span className="mm-lost" role="status">
            The launcher is not answering: is <code>make start</code> still running?
          </span>
        )}
        {onReturn && (
          <button className="ghost mm-return" onClick={onReturn} title="Back to the console (Esc)">
            Return to console
          </button>
        )}
      </header>
      <div className="mm-scroll">
        {cur && <Now key={cur.id} launch={cur} now={now} onUpdate={onUpdate} onReturn={onReturn} />}
        <div className="mm-cols">
          <nav className="mm-list" aria-label="Scenarios">
            <h2 className="mm-h">Open world</h2>
            <div className="mm-cards">{openWorld.map(card)}</div>
            <h2 className="mm-h">
              Situations <span>{situations.length}</span>
            </h2>
            <div className="mm-cards">{situations.map(card)}</div>
            {others.length > 0 && (
              <>
                <h2 className="mm-h">
                  Not started from here <span>{others.length}</span>
                </h2>
                <div className="mm-cards">{others.map(card)}</div>
              </>
            )}
          </nav>
          {sel && <Detail key={sel.id} s={sel} cur={cur} aiCli={state.ai_cli} onUpdate={onUpdate} onReturn={onReturn} />}
        </div>
      </div>
    </div>
  )
}

// ---- the launch: progress, running, ended -------------------------------------------------------

const STEPS: { stage: Stage; label: string }[] = [
  { stage: 'starting', label: 'Stack starting' },
  { stage: 'bridge', label: 'Bridge up' },
  { stage: 'ready', label: 'Fleet ready' },
  { stage: 'running', label: 'Running' },
]
const LAUNCHING: Stage[] = ['waiting', 'starting', 'bridge', 'ready']
const HEADLINE: Record<Stage, string> = {
  waiting: 'Starting',
  starting: 'Starting',
  bridge: 'Starting',
  ready: 'Starting',
  running: 'Running',
  ended: 'Time up',
  stopping: 'Ending',
  stopped: 'Ended',
  failed: 'Failed',
}

function Now({ launch: l, now, onUpdate, onReturn }: { launch: Launch; now: number; onUpdate: (c: Launch | null) => void; onReturn?: () => void }) {
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const launching = LAUNCHING.includes(l.stage)
  const up = l.stage === 'running' || l.stage === 'ended'
  const at = STEPS.findIndex((s) => s.stage === l.stage)
  const tone = l.stage === 'failed' ? 'bad' : l.stage === 'stopped' ? 'quiet' : up ? 'live' : 'busy'
  const modes =
    l.kind === 'situation' ? `operator ${l.options.operator === 'live' ? 'you' : 'autoplay'} · AI ${l.options.ai_label ?? 'none'}` : 'free command, nothing scripted'

  const end = async () => {
    setBusy(true)
    setError(null)
    try {
      onUpdate(await endScenario())
      setConfirm(false)
    } catch (e) {
      setError((e as Error).message)
    }
    setBusy(false)
  }

  return (
    <section className={`mm-now ${tone}`} aria-live="polite" aria-label="The scenario now">
      <div className="mm-now-head">
        <span className="mm-now-label">{HEADLINE[l.stage]}</span>
        <h2>{l.title}</h2>
        <span className="mm-now-modes">{modes}</span>
        <span className="spacer" />
        {(launching || up) && <span className="mm-now-clock">T+{mmss(now - l.started_t)}</span>}
      </div>
      {launching && (
        <ol className="mm-steps">
          {l.stage === 'waiting' && <li className="cur">Ending the one before</li>}
          {STEPS.map((s, k) => (
            <li key={s.stage} className={at > k ? 'done' : at === k ? 'cur' : ''}>
              {s.label}
              {s.stage === 'ready' && l.ready ? ` ${l.ready.ready}/${l.ready.of}` : ''}
            </li>
          ))}
        </ol>
      )}
      {l.stage !== 'failed' && l.detail && <p className="mm-line">{l.detail}</p>}
      {launching && <p className="mm-note">{typical(l.typical_s)}</p>}
      {l.stage === 'failed' && (
        <>
          <p className="mm-fail">{l.error ?? l.detail}</p>
          <p className="mm-note">
            Its output: <code>{l.log}</code>
            {l.run?.situation_log && (
              <>
                {' '}· the situation's log: <code>{l.run.situation_log}</code>
              </>
            )}
          </p>
        </>
      )}
      {l.stage === 'stopped' && l.run?.situation_log && (
        <p className="mm-note">
          The situation's log and summary: <code>{l.run.situation_log.replace(/situation\.jsonl$/, '')}</code>
        </p>
      )}
      {l.leftovers.length > 0 && (
        <p className="mm-warn">
          Still held after it ended: {l.leftovers.join('; ')}. <code>make down</code> stops a stack.
        </p>
      )}
      {(launching || up) &&
        (confirm ? (
          <div className="mm-confirm" role="alertdialog" aria-label="End the scenario">
            <span>
              End {l.title}? Everything it started stops: the vehicles, the bridge, the link emulator, tidewater
              {l.kind === 'situation' ? `, the situation runner${l.options.ai && l.options.ai !== 'none' ? ' and the AI' : ''}` : ''}.
            </span>
            <button className="mm-danger" disabled={busy} onClick={end}>
              {busy ? 'Ending…' : 'End it'}
            </button>
            <button className="ghost" disabled={busy} onClick={() => setConfirm(false)}>
              Keep it running
            </button>
          </div>
        ) : (
          <div className="mm-actions">
            {onReturn && l.stack_up && (
              <button className="primary" onClick={onReturn}>
                {launching ? 'Open the console now' : 'Return to console'}
              </button>
            )}
            <button className="mm-danger" onClick={() => setConfirm(true)}>
              {launching ? 'Cancel' : 'End scenario'}
            </button>
          </div>
        ))}
      {error && <p className="mm-fail">{error}</p>}
    </section>
  )
}

// ---- a scenario's detail, its options and Start ---------------------------------------------------

function Fact({ k, children }: { k: string; children: React.ReactNode }) {
  return (
    <>
      <dt>{k}</dt>
      <dd>{children}</dd>
    </>
  )
}

function Detail({
  s,
  cur,
  aiCli,
  onUpdate,
  onReturn,
}: {
  s: Scenario
  cur: Launch | null
  aiCli: Record<string, boolean>
  onUpdate: (c: Launch | null) => void
  onReturn?: () => void
}) {
  const running = isActive(cur) ? cur : null
  // the situation's own defaults, or what the run of it now was started with
  const mine = running?.scenario === s.id ? running.options : null
  const [operator, setOperator] = useState<OperatorMode | undefined>(mine?.operator ?? s.operator)
  const [ai, setAi] = useState<AiMode | undefined>(mine?.ai ?? s.ai)
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const situation = s.kind === 'situation'
  const sitlNote = s.sitl ? `${s.sitl} in ArduPilot SITL, ${(s.vehicles ?? 0) - s.sitl} stand-ins` : 'all stand-ins'

  const start = async (replace: boolean) => {
    setBusy(true)
    setError(null)
    try {
      onUpdate(await startScenario({ scenario: s.id, ...(situation ? { operator, ai } : {}), replace }))
      setConfirm(false)
    } catch (e) {
      setError((e as Error).message)
    }
    setBusy(false)
  }

  return (
    <section className="mm-detail" aria-label={s.title}>
      <div className="mm-kicker">
        {situation ? 'Situation' : 'Open world · default'}
        {s.cli && <code title="The same from a terminal">{s.cli}</code>}
      </div>
      <h1>{s.title}</h1>
      {s.runnable && (
        <dl className="mm-facts">
          {situation ? (
            <>
              <Fact k="Fleet">
                <b>{plural(s.vehicles, 'vehicle')}</b> ({sitlNote})
              </Fact>
              <Fact k="Duration">
                <b>{Math.round((s.duration_s ?? 0) / 60)} min</b> of situation time{s.rate && s.rate !== 1 ? ` at ${s.rate}x` : ''}, from {fmtAt(s.at)}
              </Fact>
              <Fact k="Conditions">{s.tidewater && s.tidewater !== 'none' ? `Real tide, current and wind (tidewater ${s.tidewater})` : 'No tidewater conditions'}</Fact>
              <Fact k="Traffic">{plural(s.contacts, 'contact')} that are not ours</Fact>
              <Fact k="Scripted">
                {plural(s.orders, 'opening order')}, {plural(s.steps, 'timeline step')}; {plural(s.watch, 'thing', 'things')} to watch for
              </Fact>
              <Fact k="Scored">Nothing: a live run is recorded (audit log, situation log, summary), not scored</Fact>
            </>
          ) : (
            <>
              <Fact k="Fleet">
                <b>{plural(s.vehicles, 'vehicle')}</b>, one of each of {s.platform_types} platform types ({sitlNote})
              </Fact>
              <Fact k="Conditions">
                Real tide, current and wind from tidewater ({s.tidewater}), from {fmtAt(s.at)}
              </Fact>
              <Fact k="Traffic">{plural(s.contacts, 'contact')} that are not ours: harbor traffic</Fact>
              <Fact k="Duration">No limit: it runs until you end it</Fact>
              <Fact k="Scripted">Nothing: no opening orders, no timeline, no AI on watch</Fact>
              <Fact k="Scored">Nothing</Fact>
            </>
          )}
          {s.typical_s && <Fact k="Ready in">{typical(s.typical_s)}</Fact>}
        </dl>
      )}
      {!s.runnable && <p className="mm-why">Not started from the menu: {s.why_not}</p>}
      {s.runnable && situation && (
        <div className="mm-opts">
          <fieldset className="mm-opt">
            <legend>Operator</legend>
            <div className="seg" role="radiogroup" aria-label="Operator">
              {(['live', 'autoplay'] as const).map((op) => (
                <button key={op} role="radio" aria-checked={operator === op} className={operator === op ? 'on' : ''} disabled={!s.operators?.includes(op)} onClick={() => setOperator(op)}>
                  {OPERATOR_LABEL[op]}
                  {op === s.operator && <small>default</small>}
                </button>
              ))}
            </div>
            {operator && <p className="mm-note">{OPERATOR_NOTE[operator]}</p>}
          </fieldset>
          <fieldset className="mm-opt">
            <legend>AI watch officer</legend>
            <div className="seg" role="radiogroup" aria-label="AI watch officer">
              {(s.ais ?? []).map((a) => (
                <button key={a} role="radio" aria-checked={ai === a} className={ai === a ? 'on' : ''} onClick={() => setAi(a)}>
                  {AI_LABEL[a]}
                  {a === s.ai && <small>default</small>}
                </button>
              ))}
            </div>
            {ai && (
              <p className="mm-note">
                {AI_NOTE[ai]}
                {(ai === 'claude' || ai === 'codex') && aiCli[ai] === false && <span className="mm-warn-inline"> Its CLI is not installed here: the run would refuse to start.</span>}
              </p>
            )}
          </fieldset>
        </div>
      )}
      {s.runnable && (
        <div className="mm-go">
          {confirm && running ? (
            <div className="mm-confirm" role="alertdialog" aria-label="End the running scenario first">
              <span>
                {running.title} is {running.stage === 'ended' ? 'still up' : 'running'}. End it and start {s.title}?
              </span>
              <button className="primary" disabled={busy} onClick={() => start(true)}>
                {busy ? 'Starting…' : 'End it and start'}
              </button>
              <button className="ghost" disabled={busy} onClick={() => setConfirm(false)}>
                Cancel
              </button>
            </div>
          ) : running?.scenario === s.id ? (
            <>
              {running.stack_up && onReturn ? (
                <button className="primary mm-start" onClick={onReturn}>
                  Return to console
                </button>
              ) : (
                <span className="mm-note">{running.stage === 'stopping' ? 'Ending: see above.' : 'Starting: its progress is above.'}</span>
              )}
              {running.stage !== 'stopping' && (
                <button className="ghost" onClick={() => setConfirm(true)}>
                  Start it again
                </button>
              )}
            </>
          ) : (
            <button className="primary mm-start" disabled={busy || running?.stage === 'stopping'} onClick={() => (running ? setConfirm(true) : start(false))}>
              {busy ? 'Starting…' : situation ? 'Start this situation' : 'Start the open world'}
            </button>
          )}
          {!confirm && running && running.scenario !== s.id && <span className="mm-note">{running.title} is up: starting this one ends it first (you confirm).</span>}
        </div>
      )}
      {error && <p className="mm-fail">{error}</p>}
      <div className="mm-brief">
        <h2 className="mm-h">{situation ? 'Brief' : 'What it is'}</h2>
        {s.brief.split(/\n\s*\n/).map((p, k) => (
          <p key={k}>{p}</p>
        ))}
      </div>
    </section>
  )
}
