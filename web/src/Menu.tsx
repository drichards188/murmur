import { type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent, useEffect, useState } from 'react'
import { type AiMode, endScenario, isActive, type Launch, type LauncherState, type OperatorMode, type Scenario, type Stage, startScenario } from './launcher'
import { SwarmBackdrop } from './SwarmBackdrop'
import { DOMAIN_LABEL, type Domain } from './types'
import './menu.css'

/** The main menu (`make start`): the open world first (the default), then the situations, each
 *  with what it has; a situation's operator and AI with its own defaults chosen; the launch's
 *  progress, and the scenario running with Return and End. Over the console, which stays live
 *  behind it while a scenario runs. Behind the menu, a swarm in a strait's cross-section
 *  (SwarmBackdrop): decoration, never a run's vehicles. */

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
const DOMAINS: Domain[] = ['air', 'ground', 'surface', 'subsurface']
const pad2 = (n: number) => String(n).padStart(2, '0')

/** m:ss */
function mmss(s: number) {
  const n = Math.max(0, Math.round(s))
  return `${Math.floor(n / 60)}:${pad2(n % 60)}`
}

/** "14 Jun 2026, 10:00Z" */
function fmtAt(at: string | undefined) {
  const d = at ? new Date(at) : null
  if (!d || Number.isNaN(d.getTime())) return at ?? ''
  const hm = `${pad2(d.getUTCHours())}:${pad2(d.getUTCMinutes())}Z`
  return `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}, ${hm}`
}

const plural = (n: number | undefined, one: string, many = `${one}s`) => `${n ?? 0} ${n === 1 ? one : many}`

/** a card's facts, one short line each */
function cardFacts(s: Scenario): string[] {
  if (!s.runnable) return [s.why_not ?? '']
  if (s.kind === 'open_world') return [plural(s.vehicles, 'vehicle'), plural(s.platform_types, 'platform type'), 'tide, current, wind', plural(s.contacts, 'contact')]
  return [plural(s.vehicles, 'vehicle'), `${Math.round((s.duration_s ?? 0) / 60)} min`, `operator ${s.operator === 'live' ? 'you' : 'autoplay'}`, `AI ${AI_LABEL[s.ai ?? 'none']}`]
}

/** how long vehicles usually take, as the menu says while it waits */
function typical([lo, hi]: [number, number]) {
  return hi >= 60 ? `Vehicles usually take ${lo} to ${hi} s to be ready (ArduPilot SITL).` : `Vehicles are usually ready in ${lo} to ${hi} s (stand-ins).`
}

/** a fleet's vehicles in each domain, as a sentence */
const mixText = (mix: Record<Domain, number>) => DOMAINS.map((d) => `${mix[d]} ${DOMAIN_LABEL[d].toLowerCase()}`).join(', ')

/** A fleet by domain, the menu's strata (air on top, the deep below): a pip a vehicle, every row
 *  on one scale, squeezed into a bar when a row would not fit. With labels and counts in the
 *  detail, which a screen reader reads; on a card, the shape alone (the card's facts say it). */
function Strata({ mix, labelled = false }: { mix: Record<Domain, number>; labelled?: boolean }) {
  const width = labelled ? 168 : 56
  const most = Math.max(1, ...DOMAINS.map((d) => mix[d]))
  const pitch = Math.min(labelled ? 10 : 6, width / most)
  const pip = pitch >= 3 ? pitch - 2 - (labelled ? 1 : 0) : pitch
  const a11y = labelled ? { role: 'img', 'aria-label': `By domain: ${mixText(mix)}` } : { 'aria-hidden': true }
  return (
    <span className={`mm-strata${labelled ? ' labelled' : ''}`} {...a11y}>
      {DOMAINS.map((d) => (
        <span key={d} className={`mm-stratum d-${d}${mix[d] ? '' : ' none'}`}>
          {labelled && <span className="mm-stratum-k">{DOMAIN_LABEL[d]}</span>}
          <span className="mm-stratum-track" style={{ width }}>
            <span className="mm-stratum-pips" style={{ width: mix[d] * pitch, '--pip': `${pip}px`, '--pitch': `${pitch}px` } as CSSProperties} />
          </span>
          {labelled && <b>{mix[d]}</b>}
        </span>
      ))}
    </span>
  )
}

/** the launch's word on a card: starting, running or time up */
const cardStage = (stage: Stage) => (stage === 'ended' ? 'time up' : stage === 'running' ? 'running' : 'starting')

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
  const utc = new Date(now * 1000)
  // Escape goes back to the console, when one is up
  useEffect(() => {
    if (!onReturn) return
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onReturn()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onReturn])

  // the arrow keys walk the list, choosing as they go
  const onListKey = (e: ReactKeyboardEvent<HTMLElement>) => {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) return
    const cards = [...e.currentTarget.querySelectorAll<HTMLButtonElement>('.mm-card')]
    const at = cards.indexOf(document.activeElement as HTMLButtonElement)
    const to = e.key === 'Home' ? 0 : e.key === 'End' ? cards.length - 1 : Math.min(cards.length - 1, Math.max(0, at + (e.key === 'ArrowDown' ? 1 : -1)))
    if (at < 0 && e.key !== 'Home' && e.key !== 'End') return
    e.preventDefault()
    cards[to]?.focus()
    cards[to]?.click()
  }

  // a click chooses; in one column (menu.css), where the detail is under the list, it brings the
  // detail up too (a pointer's click only: the arrow keys stay in the list)
  const choose = (id: string, e: ReactMouseEvent) => {
    setSelId(id)
    if (e.detail > 0 && window.matchMedia('(max-width: 1000px)').matches) {
      const behavior = window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth'
      window.requestAnimationFrame(() => document.getElementById('mm-detail')?.scrollIntoView({ block: 'start', behavior }))
    }
  }

  let order = 0
  const card = (s: Scenario) => {
    const live = isActive(cur) && cur.scenario === s.id ? cur : null
    const tone = live ? (live.stage === 'running' ? 'live' : live.stage === 'ended' ? 'ended' : 'busy') : ''
    const facts = cardFacts(s)
    return (
      <button
        key={s.id}
        className={`mm-card${s.id === sel?.id ? ' sel' : ''}${s.runnable ? '' : ' off'}${s.kind === 'open_world' ? ' world' : ''}${live ? ` now ${tone}` : ''}`}
        style={{ '--i': order++ } as CSSProperties}
        aria-pressed={s.id === sel?.id}
        onClick={(e) => choose(s.id, e)}
      >
        {s.runnable && s.domains && <Strata mix={s.domains} />}
        <span className="mm-card-body">
          <span className="mm-card-top">
            <span className="mm-card-title">{s.title}</span>
            {s.default && <span className="mm-tag">Default</span>}
            {live && (
              <span className={`mm-card-state ${tone}`}>
                <i className="mm-dot" />
                {cardStage(live.stage)}
              </span>
            )}
          </span>
          <span className="mm-card-facts">
            {facts.map((f, k) => (
              <span key={k}>{f}</span>
            ))}
          </span>
        </span>
      </button>
    )
  }

  return (
    <div className="mm" role="dialog" aria-modal="true" aria-label="Main menu">
      <SwarmBackdrop />
      <div className="mm-frame" aria-hidden="true" />
      <header className="mm-top">
        <span className="mm-where">Main menu</span>
        <span className="spacer" />
        {lost && (
          <span className="mm-lost" role="status">
            The launcher is not answering: is <code>make start</code> still running?
          </span>
        )}
        <span className="mm-utc" title="The launcher's clock, UTC">
          <span>
            {utc.getUTCDate()} {MONTHS[utc.getUTCMonth()]} {utc.getUTCFullYear()}
          </span>
          <b>
            {pad2(utc.getUTCHours())}:{pad2(utc.getUTCMinutes())}:{pad2(utc.getUTCSeconds())}Z
          </b>
        </span>
        {onReturn && (
          <button className="ghost mm-return" onClick={onReturn} title="Back to the console (Esc)">
            Return to console
          </button>
        )}
      </header>
      <div className="mm-scroll">
        <div className="mm-grid">
          <div className="mm-left">
            <div className="mm-ident">
              <h1 className="mm-word" aria-label="Murmur">
                {'MURMUR'.split('').map((c, k) => (
                  <span key={k} aria-hidden="true" style={{ '--i': k } as CSSProperties}>
                    {c}
                  </span>
                ))}
              </h1>
              <p className="mm-sub">Multi-domain swarm C2</p>
              <p className="mm-pitch">One operator over autonomous vehicles in the air, on the ground, on and under the water: built to measure whether human judgment over consequential decisions stays meaningful.</p>
            </div>
            <nav className="mm-list" aria-label="Scenarios" onKeyDown={onListKey}>
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
          </div>
          <div className="mm-right">
            {cur && <Now key={cur.id} launch={cur} now={now} onUpdate={onUpdate} onReturn={onReturn} />}
            {sel && <Detail key={sel.id} s={sel} cur={cur} aiCli={state.ai_cli} onUpdate={onUpdate} onReturn={onReturn} />}
          </div>
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
  const tone = l.stage === 'failed' ? 'bad' : l.stage === 'stopped' ? 'quiet' : l.stage === 'ended' ? 'ended' : up ? 'live' : 'busy'
  const modes = l.kind === 'situation' ? [`operator ${l.options.operator === 'live' ? 'you' : 'autoplay'}`, `AI ${l.options.ai_label ?? 'none'}`] : ['free command, nothing scripted']
  const steps = l.stage === 'waiting' ? [{ stage: 'waiting' as Stage, label: 'Ending the one before' }, ...STEPS] : STEPS

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
    <section className={`mm-now mm-hud ${tone}`} aria-live="polite" aria-label="The scenario now">
      <div className="mm-now-head">
        <span className="mm-now-label">
          <i className="mm-dot" />
          {HEADLINE[l.stage]}
        </span>
        <span className="spacer" />
        {(launching || up) && (
          <span className="mm-now-clock" title="Since it was started">
            T+{mmss(now - l.started_t)}
          </span>
        )}
      </div>
      <h2>{l.title}</h2>
      <span className="mm-now-modes">
        {modes.map((m) => (
          <span key={m}>{m}</span>
        ))}
      </span>
      {launching && (
        <ol className="mm-steps" style={{ '--n': steps.length } as CSSProperties}>
          {steps.map((s, k) => {
            const state = l.stage === 'waiting' ? (k === 0 ? 'cur' : '') : at > k ? 'done' : at === k ? 'cur' : ''
            const count = s.stage === 'ready' && l.ready
            return (
              <li key={s.stage} className={state}>
                <span className="mm-step-mark" aria-hidden="true" />
                <span className="mm-step-label">
                  {s.label}
                  {count ? ` ${l.ready?.ready}/${l.ready?.of}` : ''}
                </span>
                {count && l.ready && (
                  <span className="mm-step-bar" aria-hidden="true">
                    <span style={{ transform: `scaleX(${l.ready.ready / Math.max(1, l.ready.of)})` }} />
                  </span>
                )}
              </li>
            )
          })}
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
    <section id="mm-detail" className={`mm-detail mm-hud${s.runnable ? '' : ' off'}`} aria-label={s.title}>
      <div className="mm-kicker">
        <span>{situation ? 'Situation' : 'Open world'}</span>
        {s.default && <span className="mm-tag">Default</span>}
        {s.cli && <code title="The same from a terminal">{s.cli}</code>}
      </div>
      <h2 className="mm-title">{s.title}</h2>
      <div className="mm-sheet">
        <div className="mm-spec">
          {s.runnable && s.domains && <Strata mix={s.domains} labelled />}
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
        </div>
        <div className="mm-plan">
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
                    <button className="ghost mm-again" onClick={() => setConfirm(true)}>
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
            <h3 className="mm-h">{situation ? 'Brief' : 'What it is'}</h3>
            {s.brief.split(/\n\s*\n/).map((p, k) => (
              <p key={k}>{p}</p>
            ))}
          </div>
        </div>
      </div>
    </section>
  )
}
