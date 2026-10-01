import { useState } from 'react'
import type { Draft } from './MapView'
import type { TaskKind, Tasking, Vehicle } from './types'
import { callsign, LIFECYCLE } from './types'
import { sendTask } from './useBridge'

interface Props {
  vehicles: Vehicle[]
  selected: Set<number>
  draft: Draft
  setDraft: (d: Draft) => void
  taskings: Tasking[]
}

const TASKS: { kind: TaskKind; label: string; hint: string }[] = [
  { kind: 'orbit', label: 'Orbit', hint: 'Click the map to set the orbit center.' },
  { kind: 'patrol', label: 'Patrol', hint: 'Click the map to add waypoints. The route loops.' },
  { kind: 'hold', label: 'Hold', hint: 'Planes loiter in place, boats hold position.' },
  { kind: 'rtl', label: 'RTL', hint: 'Return to launch.' },
]

function age(t: number) {
  const s = Math.max(0, Math.round(Date.now() / 1000 - t))
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m${String(s % 60).padStart(2, '0')}s`
}

function Lifecycle({ state }: { state: string }) {
  const failed = state === 'failed' || state === 'superseded'
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

export function TaskPanel({ vehicles, selected, draft, setDraft, taskings }: Props) {
  const [kind, setKind] = useState<TaskKind>('orbit')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const bySysid = new Map(vehicles.map((v) => [v.sysid, v]))
  const sel = [...selected].sort((a, b) => a - b)

  const choose = (k: TaskKind) => {
    setKind(k)
    setError(null)
    setDraft({ ...draft, mode: k === 'orbit' || k === 'patrol' ? k : null })
  }

  const ready =
    sel.length > 0 &&
    (kind === 'orbit' ? draft.center != null : kind === 'patrol' ? draft.route.length >= 2 : true)

  const submit = async () => {
    setBusy(true)
    setError(null)
    try {
      await sendTask({
        task: kind,
        sysids: sel,
        ...(kind === 'orbit' ? { center: draft.center!, radius: draft.radius } : {}),
        ...(kind === 'patrol' ? { route: draft.route } : {}),
      })
      setDraft({ ...draft, center: null, route: [] })
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <aside className="panel tasking">
      <header className="panel-head">
        <h2>Task</h2>
        <span className="muted">{sel.length ? sel.map((s) => callsign(bySysid.get(s) ?? { sysid: s, kind: null })).join(' · ') : 'no group selected'}</span>
      </header>
      <div className="taskform">
        <div className="seg wide">
          {TASKS.map((t) => (
            <button key={t.kind} className={kind === t.kind ? 'on' : ''} onClick={() => choose(t.kind)}>
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
              min={100}
              max={800}
              step={25}
              value={draft.radius}
              onChange={(e) => setDraft({ ...draft, radius: Number(e.target.value) })}
            />
            <output>{draft.radius} m</output>
          </label>
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
          {sel.length === 0 ? 'Select vehicles first' : `Send ${kind.toUpperCase()} to ${sel.length} vehicle${sel.length > 1 ? 's' : ''}`}
        </button>
        {error && <p className="error">{error}</p>}
      </div>

      <header className="panel-head sub">
        <h2>Command log</h2>
        <span className="muted">sent → acked → active → done</span>
      </header>
      <div className="log">
        {taskings.length === 0 && <p className="empty">Commands you send show up here and track each vehicle through to completion.</p>}
        {taskings.map((t) => (
          <section key={t.id} className="tcard">
            <div className="tcard-head">
              <strong>{t.task.toUpperCase()}</strong>
              <span className="muted">#{t.id}</span>
              <span className="muted right">{age(t.created)} ago</span>
            </div>
            {t.ops.map((o) => {
              const v = bySysid.get(o.sysid)
              return (
                <div key={o.id} className={`op ${o.state}`}>
                  <span className="op-name">{v ? callsign(v) : o.sysid}</span>
                  <Lifecycle state={o.state} />
                  <span className="op-detail">
                    {o.state === 'failed' || o.state === 'superseded' ? <b>{o.state}: </b> : null}
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
