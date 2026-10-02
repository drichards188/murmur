import { useEffect, useRef, useState } from 'react'
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
  const bySysid = new Map(prev.vehicles.map((v) => [v.sysid, v]))
  for (const { trail_add, ...v } of d.vehicles) {
    const old = bySysid.get(v.sysid)
    const trail = [...(old?.trail ?? []), ...trail_add].slice(-TRAIL_MAX)
    bySysid.set(v.sysid, { ...(v as Omit<Vehicle, 'trail'>), trail } as Vehicle)
  }
  return {
    ...prev,
    t: d.t,
    vehicles: [...bySysid.values()].sort((a, b) => a.sysid - b.sysid),
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
        } else if (msg.type === 'delta' && current) {
          current = withAges(applyDelta(current, msg as Delta))
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

  return { state, status }
}

export interface TaskRequest {
  task: TaskKind
  sysids: number[]
  center?: LngLat
  radius?: number
  route?: LngLat[]
  target?: number
}

export async function sendTask(req: TaskRequest) {
  const res = await fetch('/api/tasks', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(req),
  })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new Error(body.detail ?? `bridge returned ${res.status}`)
  }
  return res.json()
}
