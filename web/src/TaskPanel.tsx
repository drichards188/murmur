import { useEffect, useMemo, useState } from 'react'
import { EMPTY_ROUTE, routeDrawing, type LinkRouteDrawing } from './coverage'
import { DryRun } from './DryRun'
import { useDryRun } from './useDryRun'
import type { Draft } from './MapView'
import type { TaskKind, Tasking, Vehicle } from './types'
import { callsign, LIFECYCLE, opEnding } from './types'
import { TaskResult, type TaskOutcome } from './TaskResult'
import { tierLabel, type Intent, type SpaceRuns } from './decisions'
import { scenarioNow } from './decisions'
import { SpaceRunCard } from './SpaceRuns'
import { sendTask, TaskRefused, type TaskRequest } from './useBridge'
import { availableTasks, orbitRange, usePlatformLimits } from './taskDefaults'
import { OptionLine } from './IdentifyForm'
import { sendIdentify, useIdentifyOptions } from './identify'

interface Props {
  vehicles: Vehicle[]
  /** Keep names in completed commands after a spent vehicle leaves the live fleet. */
  historyVehicles?: Vehicle[]
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
  /** the drafted order's predicted link, for the map (the dry run's `link_coverage`) */
  onLinkRoute?: (d: LinkRouteDrawing | null) => void
  /** the run's latest intents: how an identify order's look ended (rules 21) */
  intents?: Intent[]
}

/** The task buttons' kinds: the task layer's, and (rules 21) identify, one vehicle sent to
 *  identify the contact clicked on the map. */
type PanelKind = TaskKind | 'identify'

const TASKS: { kind: PanelKind; label: string; hint: string }[] = [
  { kind: 'orbit', label: 'Orbit', hint: 'Click the map to set the orbit center.' },
  { kind: 'patrol', label: 'Patrol', hint: 'Click the map to add waypoints. The route loops.' },
  { kind: 'hold', label: 'Hold', hint: 'Planes loiter in place, boats hold position.' },
  { kind: 'rtl', label: 'RTL', hint: 'Return to launch.' },
  { kind: 'track', label: 'Track', hint: 'Select one antenna tracker and the one vehicle it should point at.' },
  {
    kind: 'identify',
    label: 'Identify',
    hint: "Click the contact to identify on the map. The vehicle orbits it at its camera's standoff, following it, until it is identified, then goes back to what it was doing.",
  },
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

export function TaskPanel({ vehicles, historyVehicles = vehicles, selected, draft, setDraft, taskings, pendingIds, onInbox, space, now, onLinkRoute, intents }: Props) {
  const runs = space?.runs ?? []
  const scen = now ? scenarioNow(space, now) : null
  // rules 21: an identify order's taskings (its orbit each time it moved with its track, then the
  // order it went back to) as one card, the newest, saying how its look ended
  const identify = new Map<number, { track: string; newest: number; moved: number; ended: string | null }>()
  for (const t of taskings) {
    if (t.intent_id == null || !t.params.look) continue
    const e = identify.get(t.intent_id) ?? { track: t.params.look, newest: t.id, moved: -1, ended: null }
    e.moved += 1
    identify.set(t.intent_id, e)
  }
  for (const t of taskings) {
    const e = t.intent_id != null ? identify.get(t.intent_id) : undefined
    if (e && t.id > e.newest) e.newest = t.id
  }
  for (const i of intents ?? []) {
    const e = identify.get(i.id)
    const last = i.history[i.history.length - 1]
    if (e && i.kind === 'identify' && last && (i.state === 'done' || i.state === 'failed' || /identified|not identified|lost contact|not in the picture/.test(last.reason))) e.ended = last.reason
  }
  const identifyLabel = (t: Tasking) => {
    const e = t.intent_id != null ? identify.get(t.intent_id) : undefined
    if (!e) return t.task.toUpperCase()
    return t.params.look ? `IDENTIFY ${e.track}` : `IDENTIFY ${e.track}, then ${t.task.toUpperCase()}`
  }
  const [kind, setKind] = useState<PanelKind>('orbit')
  // rules 21: what sending an identify order said (sent, or waiting in the Inbox)
  const [idSent, setIdSent] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<TaskOutcome | null>(null)
  /** command log notes: tasking id -> the human tier its sender authorized */
  const [authorized, setAuthorized] = useState<Map<number, string>>(new Map())
  const byUid = new Map(vehicles.map((v) => [v.uid, v]))
  const historyByUid = new Map(historyVehicles.map((v) => [v.uid, v]))
  const sel = [...selected].sort((a, b) => a - b)
  const limits = usePlatformLimits()
  const selVehicles = sel.map((s) => byUid.get(s)).filter((v): v is Vehicle => v != null)
  const orbit = orbitRange(selVehicles, limits)
  const offered = availableTasks(selVehicles, limits)
  const selKey = sel.join(',')
  // whether the selection can take `k`: identify sends exactly one vehicle (its camera's reach is
  // the bridge's to judge, below)
  const can = (k: PanelKind) => (k === 'identify' ? sel.length === 1 : offered == null || offered.has(k))
  // rules 21: the identify target's answer for the one vehicle selected (why it can't go, or its
  // orbit, time and link)
  const idTrack = kind === 'identify' && sel.length === 1 ? (draft.track ?? null) : null
  const { options: idOptions, error: idError } = useIdentifyOptions(idTrack, idTrack ? sel : undefined)
  const idOption = idOptions?.find((o) => o.uid === sel[0]) ?? null
  // the map's ring on the target is the orbit the vehicle would fly
  const standoff = idOption?.standoff_m ?? null
  useEffect(() => {
    if (kind === 'identify' && standoff != null && standoff !== draft.radius) setDraft({ ...draft, radius: standoff })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind, standoff])

  // a new selection gets that platform's own defaults: its typical orbit, and a task it can do;
  // none selected, no order to draw, so nothing of one stays on the map
  useEffect(() => {
    if (!sel.length) {
      setDraft({ ...draft, center: null, route: [], track: null, note: null })
      return
    }
    setDraft({ ...draft, radius: orbit.value, track: null, note: null })
    if (!can(kind)) {
      const next = TASKS.find((t) => can(t.kind))?.kind
      if (next) choose(next)
    }
    // only when the selection (or the catalog) changes, not on every draft edit
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selKey, limits])

  const choose = (k: PanelKind) => {
    setKind(k)
    setError(null)
    setIdSent(null)
    setDraft({ ...draft, mode: k === 'orbit' || k === 'patrol' || k === 'identify' ? k : null, track: null, note: null })
  }

  const trackers = sel.filter((s) => byUid.get(s)?.kind === 'tracker')
  const others = sel.filter((s) => byUid.get(s)?.kind !== 'tracker')
  const trackPair = trackers.length === 1 && others.length === 1

  const radiusOk = draft.radius >= orbit.min
  const ready =
    sel.length > 0 &&
    can(kind) &&
    (kind !== 'orbit' || radiusOk) &&
    (kind === 'orbit'
      ? draft.center != null
      : kind === 'patrol'
        ? draft.route.length >= 2
        : kind === 'track'
          ? trackPair
          : kind === 'identify'
            ? draft.track != null && idOption?.passed === true
            : true)

  /** the order as drafted, once it is complete: what Send sends and the dry run asks about */
  const request = useMemo<TaskRequest | null>(
    () =>
      ready
        ? {
            task: kind,
            uids: kind === 'track' ? trackers : sel,
            ...(kind === 'track' ? { target: others[0] } : {}),
            ...(kind === 'orbit' ? { center: draft.center!, radius: draft.radius } : {}),
            ...(kind === 'patrol' ? { route: draft.route } : {}),
            ...(kind === 'identify' ? { track: draft.track! } : {}),
          }
        : null,
    // the selection by its key; the rest as drafted
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [ready, kind, selKey, draft.center, draft.radius, draft.route, draft.track],
  )
  // the dry run (checks, policy, predicted link) of the order while it stands; information only
  const dry = useDryRun(busy ? null : request, vehicles)
  const linkRoute = useMemo(
    () => (dry.answer?.link_coverage && dry.order ? routeDrawing(dry.answer.link_coverage, dry.order, dry.at) : EMPTY_ROUTE),
    [dry.answer, dry.order, dry.at],
  )
  useEffect(() => {
    onLinkRoute?.(linkRoute.lines.length || linkRoute.dots.length || linkRoute.marks.length ? linkRoute : null)
  }, [linkRoute, onLinkRoute])
  useEffect(() => () => onLinkRoute?.(null), [onLinkRoute])

  const submit = async () => {
    if (!request) return
    // rules 21: an identify order goes as an intent with its track (the task layer's route takes none)
    if (kind === 'identify' && draft.track) {
      setBusy(true)
      setError(null)
      try {
        setIdSent(await sendIdentify(sel[0], draft.track, callsign(byUid.get(sel[0]) ?? { uid: sel[0], kind: null })))
        setDraft({ ...draft, track: null, note: null })
      } catch (e) {
        setError((e as Error).message)
      } finally {
        setBusy(false)
      }
      return
    }
    setBusy(true)
    setError(null)
    setOutcome(null)
    try {
      const answer = await sendTask(request)
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
      {/* the order form only while vehicles are selected (an order's outcome stays until dismissed) */}
      {(sel.length > 0 || outcome) && (
        <>
          <header className="panel-head">
            <h2>Task</h2>
            <span className="muted">{sel.map((s) => callsign(byUid.get(s) ?? { uid: s, kind: null })).join(' · ')}</span>
          </header>
          <div className="taskform">
            {sel.length > 0 && (
              <>
              <div className="seg wide">
                {TASKS.map((t) => (
                  <button
                    key={t.kind}
                    className={kind === t.kind ? 'on' : ''}
                    disabled={!can(t.kind)}
                    title={
                      can(t.kind)
                        ? undefined
                        : t.kind === 'identify'
                          ? 'Identify sends exactly one vehicle: select one'
                          : 'Not every selected vehicle can do this'
                    }
                    onClick={() => choose(t.kind)}
                  >
                    {t.label}
                  </button>
                ))}
              </div>
              <p className="hint">
                {kind === 'identify' && draft.note
                  ? draft.note
                  : kind === 'identify' && draft.track
                    ? `Target ${draft.track}: click another contact to change it.`
                    : TASKS.find((t) => t.kind === kind)!.hint}
              </p>
              {idTrack && (
                <p className={`hint ${idOption && !idOption.passed ? 'error' : ''}`}>
                  {idOption ? <OptionLine o={idOption} name={false} /> : idError ? null : 'Asking the bridge...'}
                </p>
              )}
              {idError && <p className="error">{idError}</p>}
              {kind === 'identify' && idSent && <p className="muted">{idSent}. Follow it in the command log.</p>}
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
                {kind === 'track'
                  ? trackPair
                    ? `Point ${callsign(byUid.get(trackers[0])!)} at ${callsign(byUid.get(others[0])!)}`
                    : 'Select 1 tracker + 1 vehicle'
                  : kind === 'identify'
                    ? draft.track
                      ? `Send IDENTIFY ${draft.track} to ${callsign(byUid.get(sel[0]) ?? { uid: sel[0], kind: null })}`
                      : 'Click a contact first'
                    : `Send ${kind.toUpperCase()} to ${sel.length} vehicle${sel.length > 1 ? 's' : ''}`}
              </button>
              </>
            )}
            {error && <p className="error">{error}</p>}
            {request && !outcome && <DryRun st={dry} vehicles={vehicles} />}
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
        </>
      )}

      <header className="panel-head sub">
        <h2>Command log</h2>
        <span className="muted">sent → acked → active → done</span>
      </header>
      <div className="log">
        {taskings.length === 0 && runs.length === 0 && <p className="empty">Commands you send show up here and track each vehicle through to completion.</p>}
        {space &&
          scen != null &&
          runs.map((r) => <SpaceRunCard key={`s${r.intent_id}`} run={r} map={space.clock} scen={scen} onOpen={onInbox} cancellable={space.cancellable ?? false} />)}
        {taskings.filter((t) => !identify.has(t.intent_id ?? -1) || identify.get(t.intent_id ?? -1)?.newest === t.id).map((t) => (
          <section key={t.id} className="tcard">
            <div className="tcard-head">
              <strong>{identifyLabel(t)}</strong>
              <span className="muted">#{t.id}</span>
              {(identify.get(t.intent_id ?? -1)?.moved ?? 0) > 0 && <span className="muted">moved {identify.get(t.intent_id ?? -1)?.moved}x with it</span>}
              <span className="muted right">{age(t.created, now ? now() : Date.now())} ago</span>
            </div>
            {authorized.has(t.id) && <p className="muted">authorized by you ({authorized.get(t.id)})</p>}
            {identify.get(t.intent_id ?? -1)?.ended && <p className="muted">{identify.get(t.intent_id ?? -1)?.ended}</p>}
            {t.ops.map((o) => {
              const v = historyByUid.get(o.uid)
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
