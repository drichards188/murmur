import { useCallback, useEffect, useRef, useState } from 'react'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, setBridgeOperators, type Decisions, type Intent, type Authorization } from './decisions'
import { perfPush, perfUpdateStart } from './perf'
import type { TracksMessage } from './tracks'
import type { BridgeState, Clusters, LngLat, TaskKind, Tasking, Vehicle, ViewSubscription } from './types'

const TRAIL_MAX = 120 // same cap as the bridge keeps

/** After the initial full state, the bridge sends only vehicles that changed and the task
 *  list only when it changed. On the level-of-detail feed (`/ws?lod=1`, murmur-and) each
 *  vehicle is a partial object merged over what the console has: the identity comes once,
 *  `trail` replaces the trail (a vehicle newly in view, or a new sampling step), `trail_add`
 *  appends to it, and a `lite` snapshot (out of view) has no trail at all. `clusters` is the
 *  tile aggregate while the view holds more vehicles than the bridge sends in full (null
 *  when that ends; absent when unchanged). */
interface Delta {
  type: 'delta'
  t: number
  vehicles: (Partial<Omit<Vehicle, 'trail'>> & { uid: number; trail?: Vehicle['trail']; trail_add?: Vehicle['trail'] })[]
  taskings?: Tasking[]
  clusters?: Clusters | null
}

function applyDelta(prev: BridgeState, d: Delta): BridgeState {
  if (!d.vehicles.length && !d.taskings && d.clusters === undefined) return { ...prev, t: d.t }
  const byUid = new Map(prev.vehicles.map((v) => [v.uid, v]))
  for (const { trail: whole, trail_add, ...v } of d.vehicles) {
    const old = byUid.get(v.uid)
    const trail =
      v.lod === 'lite' ? [] : whole ? whole.slice(-TRAIL_MAX) : trail_add?.length ? [...(old?.trail ?? []), ...trail_add].slice(-TRAIL_MAX) : (old?.trail ?? [])
    // a full update carries the link's fast fields (packet time, loss, RTT) or its slow
    // ones (plan, emulator) or both: merge them; a lite link is whole and replaces it
    const link = v.link && v.lod !== 'lite' && old?.link ? { ...old.link, ...v.link } : v.link
    byUid.set(v.uid, { ...old, ...v, ...(link ? { link } : {}), trail } as Vehicle)
  }
  return {
    ...prev,
    t: d.t,
    vehicles: [...byUid.values()].sort((a, b) => a.uid - b.uid),
    taskings: d.taskings ?? prev.taskings,
    clusters: d.clusters === undefined ? prev.clusters : d.clusters,
  }
}

/** The bridge sends when each vehicle was last heard from; age is relative to the
 *  bridge's clock (in each message), not this browser's. A lite snapshot says only whether
 *  the link is lost, and when the last packet came once it is. */
function withAges(s: BridgeState): BridgeState {
  const vehicles = s.vehicles.map((v) => ({
    ...v,
    link: { ...v.link, since_last_ms: v.link.last_rx_t == null ? null : Math.max(0, (s.t - v.link.last_rx_t) * 1000) },
  }))
  generations.set(vehicles, generation)
  return { ...s, vehicles }
}

/** A generation that carries through deltas: it moves on every whole `state` message (the first
 *  after a connect, so after a reconnect too), and every vehicle list built, from a state or a
 *  delta, records the generation current when it was built. The map draws vehicles as reported,
 *  with no glide, when it sees a list of a new generation: positions may have moved a long way
 *  since the last data (drawn.ts). Tied to the data, so neither React batching (a delta that
 *  follows the state at once, rendered alone) nor an unrelated render can lose or use up the snap. */
let generation = 0
const generations = new WeakMap<object, number>()
export function vehiclesGeneration(vehicles: object): number | undefined {
  return generations.get(vehicles)
}

export type BridgeStatus = 'connecting' | 'live' | 'down'

export function useBridge() {
  const [state, setState] = useState<BridgeState | null>(null)
  const [status, setStatus] = useState<BridgeStatus>('connecting')
  const [decisions, setDecisions] = useState<Decisions | null>(null)
  // tracks of contacts that are not ours (Phase C2): sent whole whenever one changes
  const [tracks, setTracks] = useState<TracksMessage | null>(null)
  const retry = useRef(0)
  const socket = useRef<WebSocket | null>(null)
  // the last view sent, so a reconnect starts from it
  const view = useRef<ViewSubscription | null>(null)

  useEffect(() => {
    let ws: WebSocket | null = null
    let current: BridgeState | null = null
    let timer: number | undefined
    let closed = false

    const connect = () => {
      const proto = location.protocol === 'https:' ? 'wss' : 'ws'
      ws = new WebSocket(`${proto}://${location.host}/ws?lod=1`)
      socket.current = ws
      ws.onopen = () => {
        retry.current = 0
        setStatus('live')
        if (view.current) ws?.send(JSON.stringify({ type: 'view', ...view.current }))
      }
      ws.onmessage = (ev) => {
        const msg = JSON.parse(ev.data)
        if (msg.type === 'state') {
          generation++
          current = withAges(msg as BridgeState)
          setState(current)
        } else if (msg.type === 'decisions') {
          setDecisions({ inbox: msg.inbox, intents: msg.intents, space: msg.space, operators: msg.operators, spent: msg.spent ?? [] })
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
        if (socket.current === ws) socket.current = null
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

  /** Tell the bridge what the console looks at (bounds, zoom) and which vehicles it wants
   *  at full detail whatever the view. Cheap to call often; nothing goes out unchanged. */
  const setView = useCallback((next: ViewSubscription) => {
    const cur = view.current
    if (cur && cur.zoom === next.zoom && same4(cur.bounds, next.bounds) && sameList(cur.pin, next.pin)) return
    view.current = next
    const ws = socket.current
    if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: 'view', ...next }))
  }, [])

  return { state, status, decisions, tracks, setView }
}

function same4(a: ViewSubscription['bounds'], b: ViewSubscription['bounds']) {
  if (!a || !b) return a === b
  return a[0] === b[0] && a[1] === b[1] && a[2] === b[2] && a[3] === b[3]
}

function sameList(a: number[], b: number[]) {
  return a.length === b.length && a.every((x, i) => x === b[i])
}

export interface TaskRequest {
  /** rules 21: `identify` names a `track` (the engine lays out its orbit) */
  task: TaskKind | 'identify'
  uids: number[]
  center?: LngLat
  radius?: number
  route?: LngLat[]
  target?: number
  track?: string
  standoff_m?: number
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
  const res = await operatorFetch('/api/tasks', {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
    body: JSON.stringify(req),
  })
  const body = await res.json().catch(() => ({}))
  if (!res.ok) {
    if (res.status === 422 && body.intent) throw new TaskRefused(body.detail ?? 'refused', body.intent)
    throw new Error(body.detail ?? `bridge returned ${res.status}`)
  }
  return { ...body, status: res.status }
}
