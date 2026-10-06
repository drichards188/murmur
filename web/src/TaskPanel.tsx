import { useEffect, useState } from 'react'
import type { Draft } from './MapView'
import type { TaskKind, Tasking, Vehicle } from './types'
import { callsign, LIFECYCLE, opEnding } from './types'
import { TaskResult, type TaskOutcome } from './TaskResult'
import { tierLabel, type SpaceRuns } from './decisions'
import { scenarioNow } from './decisions'
import { SpaceRunCard } from './SpaceRuns'
import { sendTask, TaskRefused } from './useBridge'
import { availableTasks, orbitRange, usePlatformLimits } from './taskDefaults'

interface Props {
  vehicles: Vehicle[]
  selected: Set<number>
  draft: Draft
  setDraft: (d: Draft) => void
  taskings: Tasking[]
  /** intents waiting in the inbox, and a way to open one (or its Decisions entry) */
  pendingIds?: Set<number>
  onInbox?: (intentId: number) => void
  /** satellite collects and covers, and the bridge clock (for their countdowns) */
  space?: SpaceRuns
  now?: () => number
}

const TASKS: { kind: TaskKind; label: string; hint: string }[] = [
  { kind: 'orbit', label: 'Orbit', hint: 'Click the map to set the orbit center.' },
  { kind: 'patrol', label: 'Patrol', hint: 'Click the map to add waypoints. The route loops.' },
  { kind: 'hold', label: 'Hold', hint: 'Planes loiter in place, boats hold position.' },
  { kind: 'rtl', label: 'RTL', hint: 'Return to launch.' },
  { kind: 'track', label: 'Track', hint: 'Select one antenna tracker and the one vehicle it should point at.' },
]

/** How long ago `t` (the bridge's clock, s) was, at `nowMs` (the bridge's clock as the
 *  console keeps it, ms: `useBridgeClock`), not the browser's clock, which can differ. */
function age(t: number, nowMs: number) {
  const s = Math.max(0, Math.round(nowMs / 1000 - t))
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`
}

function Lifecycle({ state }: { state: string }) {
  const failed = state === 'failed' || state === 'superseded' || state === 'requeued' || state === 'unconfirmed'
  const idx = LIFECYCLE.indexOf(state as never)
  return (
    <span className={`steps ${failed ? 'failed' : ''}`}>
      {LIFECYCLE.slice(1).map((s, i) => (
        <span key={s} className={`step ${!failed && idx >= i + 1 ? 'on' : ''} ${!failed && idx === i + 1 ? 'cur' : ''}`}>
          {s}
        </span>
      ))}
    </span>
  )
}

export function TaskPanel({ vehicles, selected, draft, setDraft, taskings, pendingIds, onInbox, space, now }: Props) {
  const runs = space?.runs ?? []
  const scen = now ? scenarioNow(space, now) : null
  const [kind, setKind] = useState<TaskKind>('orbit')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<TaskOutcome | null>(null)
  /** command log notes: tasking id -> the human tier its sender authorized */
  const [authorized, setAuthorized] = useState<Map<number, string>>(new Map())
  const byUid = new Map(vehicles.map((v) => [v.uid, v]))
  const sel = [...selected].sort((a, b) => a - b)
  const limits = usePlatformLimits()
  const selVehicles = sel.map((s) => byUid.get(s)).filter((v): v is Vehicle => v != null)
  const orbit = orbitRange(selVehicles, limits)
  const offered = availableTasks(selVehicles, limits)
  const selKey = sel.join(',')

  // a new selection gets that platform's own defaults: its typical orbit, and a task it can do
  useEffect(() => {
    setDraft({ ...draft, radius: orbit.value })
    if (offered && !offered.has(kind)) {
      const next = TASKS.find((t) => offered.has(t.kind))?.kind
      if (next) choose(next)
    }
    // only when the selection (or the catalog) changes, not on every draft edit
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selKey, limits])

  const choose = (k: TaskKind) => {
    setKind(k)
    setError(null)
    setDraft({ ...draft, mode: k === 'orbit' || k === 'patrol' ? k : null })
  }

  const trackers = sel.filter((s) => byUid.get(s)?.kind === 'tracker')
  const others = sel.filter((s) => byUid.get(s)?.kind !== 'tracker')
  const trackPair = trackers.length === 1 && others.length === 1

  const radiusOk = draft.radius >= orbit.min
  const ready =
    sel.length > 0 &&
    (offered == null || offered.has(kind)) &&
    (kind !== 'orbit' || radiusOk) &&
    (kind === 'orbit'
      ? draft.center != null
      : kind === 'patrol'
        ? draft.route.length >= 2
        : kind === 'track'
          ? trackPair
          : true)

  const submit = async () => {
    setBusy(true)
    setError(null)
    setOutcome(null)
    try {
      const answer = await sendTask({
        task: kind,
        uids: kind === 'track' ? trackers : sel,
        ...(kind === 'track' ? { target: others[0] } : {}),
        ...(kind === 'orbit' ? { center: draft.center!, radius: draft.radius } : {}),
        ...(kind === 'patrol' ? { route: draft.route } : {}),
      })
      if (answer.status === 202 && answer.intent_id != null && answer.state === 'queued') {
        setOutcome({ kind: 'queued', intentId: answer.intent_id, detail: answer.detail ?? '' })
      } else if (answer.status === 202 && answer.intent_id != null) {
        setOutcome({ kind: 'wait', intentId: answer.intent_id, detail: answer.detail ?? '', expires_t: answer.expires_t ?? null })
      } else if (answer.authorization && answer.intent_id != null) {
        const a = answer.authorization
        setOutcome({ kind: 'authorized', intentId: answer.intent_id, authorization: a })
        if (answer.id != null) setAuthorized((m) => new Map(m).set(answer.id!, tierLabel(a.tier)))
      }
      setDraft({ ...draft, center: null, route: [] })
    } catch (e) {
      // a refusal keeps the draft, so it can be adjusted and sent again
      if (e instanceof TaskRefused) setOutcome({ kind: 'refused', intent: e.intent, detail: e.message })
      else setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <aside className="panel tasking">
      <header className="panel-head">
        <h2>Task</h2>
        <span className="muted">{sel.length ? sel.map((s) => callsign(byUid.get(s) ?? { uid: s, kind: null })).join(' · ') : 'no group selected'}</span>
      </header>
      <div className="taskform">
        <div className="seg wide">
          {TASKS.map((t) => (
            <button
              key={t.kind}
              className={kind === t.kind ? 'on' : ''}
              disabled={offered != null && !offered.has(t.kind)}
              title={offered != null && !offered.has(t.kind) ? 'Not every selected vehicle can do this' : undefined}
              onClick={() => choose(t.kind)}
            >
              {t.label}
            </button>
          ))}
        </div>
        <p className="hint">{TASKS.find((t) => t.kind === kind)!.hint}</p>
        {kind === 'orbit' && (
          <label className="slider">
            <span>Radius</span>
            <input
              type="range"
              min={orbit.min}
              max={orbit.max}
              step={orbit.step}
              value={Math.min(orbit.max, Math.max(orbit.min, draft.radius))}
              onChange={(e) => setDraft({ ...draft, radius: Number(e.target.value) })}
            />
            <output>{draft.radius >= 10000 ? `${(draft.radius / 1000).toFixed(1)} km` : `${draft.radius} m`}</output>
          </label>
        )}
        {kind === 'orbit' && orbit.limitedBy && (
          <p className={`hint ${radiusOk ? '' : 'error'}`}>
            Minimum {orbit.min} m ({orbit.limitedBy} x {1.1}).
          </p>
        )}
        {kind === 'patrol' && (
          <div className="row">
            <span className="muted">{draft.route.length} waypoint{draft.route.length === 1 ? '' : 's'}</span>
            <button className="ghost" disabled={!draft.route.length} onClick={() => setDraft({ ...draft, route: draft.route.slice(0, -1) })}>
              Undo
            </button>
            <button className="ghost" disabled={!draft.route.length} onClick={() => setDraft({ ...draft, route: [] })}>
              Clear
            </button>
          </div>
        )}
        <button className="primary" disabled={!ready || busy} onClick={submit}>
          {sel.length === 0
            ? 'Select vehicles first'
            : kind === 'track'
              ? trackPair
                ? `Point ${callsign(byUid.get(trackers[0])!)} at ${callsign(byUid.get(others[0])!)}`
                : 'Select 1 tracker + 1 vehicle'
              : `Send ${kind.toUpperCase()} to ${sel.length} vehicle${sel.length > 1 ? 's' : ''}`}
        </button>
        {error && <p className="error">{error}</p>}
        {outcome && (
          <TaskResult
            outcome={outcome}
            vehicles={vehicles}
            stillPending={outcome.kind === 'wait' && (pendingIds?.has(outcome.intentId) ?? false)}
            onInbox={(id) => onInbox?.(id)}
            onDismiss={() => setOutcome(null)}
          />
        )}
      </div>

      <header className="panel-head sub">
        <h2>Command log</h2>
        <span className="muted">sent → acked → active → done</span>
      </header>
      <div className="log">
        {taskings.length === 0 && runs.length === 0 && <p className="empty">Commands you send show up here and track each vehicle through to completion.</p>}
        {space &&
          scen != null &&
          runs.map((r) => <SpaceRunCard key={`s${r.intent_id}`} run={r} map={space.clock} scen={scen} onOpen={onInbox} />)}
        {taskings.map((t) => (
          <section key={t.id} className="tcard">
            <div className="tcard-head">
              <strong>{t.task.toUpperCase()}</strong>
              <span className="muted">#{t.id}</span>
              <span className="muted right">{age(t.created, now ? now() : Date.now())} ago</span>
            </div>
            {authorized.has(t.id) && <p className="muted">authorized by you ({authorized.get(t.id)})</p>}
            {t.ops.map((o) => {
              const v = byUid.get(o.uid)
              return (
                <div key={o.id} className={`op ${o.state}`}>
                  <span className="op-name" title={v?.identity ? `${v.identity.name} · ${v.identity.role}` : undefined}>
                    {v ? callsign(v) : o.uid}
                  </span>
                  <Lifecycle state={o.state} />
                  <span className="op-detail">
                    {opEnding(o) ? <b>{opEnding(o)}: </b> : null}
                    {o.detail}
                    {o.progress && o.progress !== o.detail ? ` · ${o.progress}` : ''}
                  </span>
                </div>
              )
            })}
          </section>
        ))}
      </div>
    </aside>
  )
}
