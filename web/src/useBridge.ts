import { useEffect, useRef, useState } from 'react'
import type { BridgeState, LngLat, TaskKind } from './types'

export type BridgeStatus = 'connecting' | 'live' | 'down'

export function useBridge() {
  const [state, setState] = useState<BridgeState | null>(null)
  const [status, setStatus] = useState<BridgeStatus>('connecting')
  const retry = useRef(0)

  useEffect(() => {
    let ws: WebSocket | null = null
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
        if (msg.type === 'state') setState(msg)
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
