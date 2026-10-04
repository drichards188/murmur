import { useEffect, useRef, useState } from 'react'
import { getOperator, setBridgeOperators, type Decisions, type Intent, type Authorization } from './decisions'
import { perfPush, perfUpdateStart } from './perf'
import type { TracksMessage } from './tracks'
import type { BridgeState, LngLat, TaskKind, Tasking, Vehicle } from './types'

const TRAIL_MAX = 120 // same cap as the bridge keeps

/** After the initial full state, the bridge sends only vehicles that changed (with just
 *  their new trail points) and the task list only when it changed. */
interface Delta {
  type: 'delta'
  t: number
  vehicles: (Omit<Vehicle, 'trail'> & { trail_add: Vehicle['trail'] })[]
  taskings?: Tasking[]
}

function applyDelta(prev: BridgeState, d: Delta): BridgeState {
  if (!d.vehicles.length && !d.taskings) return { ...prev, t: d.t }
  const byUid = new Map(prev.vehicles.map((v) => [v.uid, v]))
  for (const { trail_add, ...v } of d.vehicles) {
    const old = byUid.get(v.uid)
    const trail = [...(old?.trail ?? []), ...trail_add].slice(-TRAIL_MAX)
    byUid.set(v.uid, { ...(v as Omit<Vehicle, 'trail'>), trail } as Vehicle)
  }
  return {
    ...prev,
    t: d.t,
    vehicles: [...byUid.values()].sort((a, b) => a.uid - b.uid),
    taskings: d.taskings ?? prev.taskings,
  }
}

/** The bridge sends when each vehicle was last heard from; age is relative to the
 *  bridge's clock (in each message), not this browser's. */
function withAges(s: BridgeState): BridgeState {
  return {
    ...s,
    vehicles: s.vehicles.map((v) => ({
      ...v,
      link: { ...v.link, since_last_ms: v.link.last_rx_t == null ? null : Math.max(0, (s.t - v.link.last_rx_t) * 1000) },
    })),
  }
}

export type BridgeStatus = 'connecting' | 'live' | 'down'

export function useBridge() {
  const [state, setState] = useState<BridgeState | null>(null)
  const [status, setStatus] = useState<BridgeStatus>('connecting')
  const [decisions, setDecisions] = useState<Decisions | null>(null)
  // tracks of contacts that are not ours (Phase C2): sent whole whenever one changes
  const [tracks, setTracks] = useState<TracksMessage | null>(null)
  const retry = useRef(0)

  useEffect(() => {
    let ws: WebSocket | null = null
    let current: BridgeState | null = null
    let timer: number | undefined
    let closed = false

    const connect = () => {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws'
      ws = new WebSocket(`${proto}://${location.host}/ws`)
      ws.onopen = () => {
        retry.current = 0
        setStatus('live')
      }
      ws.onmessage = (ev) => {
        const msg = JSON.parse(ev.data)
        if (msg.type === 'state') {
          current = withAges(msg as BridgeState)
          setState(current)
        } else if (msg.type === 'decisions') {
          setDecisions({ inbox: msg.inbox, intents: msg.intents, space: msg.space, operators: msg.operators })
          setBridgeOperators(msg.operators)
        } else if (msg.type === 'tracks') {
          setTracks(msg as TracksMessage)
        } else if (msg.type === 'delta' && current) {
          const t0 = performance.now()
          current = withAges(applyDelta(current, msg as Delta))
          perfPush('applyDelta', performance.now() - t0)
          perfUpdateStart()
          setState(current)
        }
      }
      ws.onclose = () => {
        if (closed) return
        setStatus('down')
        const delay = Math.min(5000, 500 * 2 ** retry.current++)
        timer = window.setTimeout(connect, delay)
      }
    }
    connect()
    return () => {
      closed = true
      window.clearTimeout(timer)
      ws?.close()
    }
  }, [])

  return { state, status, decisions, tracks }
}

export interface TaskRequest {
  task: TaskKind
  uids: number[]
  center?: LngLat
  radius?: number
  route?: LngLat[]
  target?: number
}

/** What POST /api/tasks answers: 200 dispatched (with `authorization` when the task was in
 *  a human tier and sending it was the operator's own approval); 202 waiting for approval
 *  (`dispatched` is false and `intent_id` names the inbox item; a tier where the sender may
 *  not authorize their own task); 422 refused by the rules (TaskRefused). */
export interface TaskAnswer {
  /** the tasking (command log entry), when dispatched */
  id?: number | null
  intent_id?: number
  authorization?: Authorization
  state?: string
  dispatched?: boolean
  detail?: string
  expires_t?: number | null
  intent?: Intent
}

/** The rules rejected the task: `intent` holds the failed checks. */
export class TaskRefused extends Error {
  intent: Intent
  constructor(message: string, intent: Intent) {
    super(message)
    this.intent = intent
  }
}

export async function sendTask(req: TaskRequest): Promise<TaskAnswer & { status: number }> {
  const res = await fetch('/api/tasks', {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-murmur-operator': getOperator() },
    body: JSON.stringify(req),
  })
  const body = await res.json().catch(() => ({}))
  if (!res.ok) {
    if (res.status === 422 && body.intent) throw new TaskRefused(body.detail ?? 'refused', body.intent)
    throw new Error(body.detail ?? `bridge returned ${res.status}`)
  }
  return { ...body, status: res.status }
}
