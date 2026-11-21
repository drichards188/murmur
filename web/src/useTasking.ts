// The operator's tasking of the AI watch officer: the feed (GET /api/tasking) and the two calls
// (POST /api/tasking, POST /api/tasking/{id}/deactivate). See tasking.ts. A write carries the
// operator identity as any operator POST does and needs no step-up: it is guidance, not a decision.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator } from './decisions'
import { deactivated, type TaskingDoc, type TaskingItem } from './tasking'

export type TaskingStatus = 'loading' | 'ok' | 'missing' | 'error'

export interface TaskingApi {
  items: TaskingItem[]
  status: TaskingStatus
  error: string | null
  /** post guidance; the item as the bridge made it, or null (error says why) */
  post: (text: string) => Promise<TaskingItem | null>
  deactivate: (id: string) => Promise<boolean>
}

const POLL_MS = 4000
const MISSING_MS = 30_000
const NONE: TaskingItem[] = []

const refusal = (res: Response, json: Record<string, unknown>) => String(json.detail ?? json.error ?? `the bridge returned ${res.status}`)

async function write(url: string, body?: unknown): Promise<{ res: Response; json: Record<string, unknown> }> {
  const res = await operatorFetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  return { res, json: (await res.json().catch(() => ({}))) as Record<string, unknown> }
}

export function useTasking(on = true): TaskingApi {
  const [items, setItems] = useState<TaskingItem[]>(NONE)
  const [status, setStatus] = useState<TaskingStatus>('loading')
  const [error, setError] = useState<string | null>(null)
  // a poll asked before a write must not overwrite the write's answer
  const epoch = useRef(0)
  const refetch = useRef<() => Promise<void>>(async () => {})
  const itemsRef = useRef(items)
  useEffect(() => {
    itemsRef.current = items
  }, [items])

  useEffect(() => {
    if (!on) return
    let stop = false
    let timer: number | undefined
    const once = async (): Promise<number> => {
      const asked = epoch.current
      try {
        const res = await fetch('/api/tasking')
        if (stop) return POLL_MS
        if (res.status === 404) {
          setStatus('missing')
          setItems(NONE)
          return MISSING_MS
        }
        if (!res.ok) throw new Error(`the bridge returned ${res.status}`)
        const doc = (await res.json()) as TaskingDoc
        if (stop || asked !== epoch.current) return POLL_MS
        setItems(Array.isArray(doc?.items) ? doc.items : NONE)
        setStatus('ok')
        setError(null)
      } catch (e) {
        if (!stop) {
          setStatus((s) => (s === 'ok' ? s : 'error'))
          setError((e as Error).message)
        }
      }
      return POLL_MS
    }
    refetch.current = async () => void (await once())
    const loop = async () => {
      const wait = await once()
      if (!stop) timer = window.setTimeout(() => void loop(), wait)
    }
    void loop()
    return () => {
      stop = true
      window.clearTimeout(timer)
    }
  }, [on])

  const post = useCallback<TaskingApi['post']>(async (text) => {
    epoch.current++
    setError(null)
    try {
      const { res, json } = await write('/api/tasking', { text: text.trim() })
      if (!res.ok) throw new Error(refusal(res, json))
      const item = json as unknown as TaskingItem
      setItems((l) => [item, ...l.filter((i) => i.id !== item.id)])
      setStatus('ok')
      return item
    } catch (e) {
      setError((e as Error).message)
      return null
    }
  }, [])

  const deactivate = useCallback<TaskingApi['deactivate']>(async (id) => {
    epoch.current++
    setError(null)
    const before = itemsRef.current
    setItems((l) => deactivated(l, id))
    try {
      const { res, json } = await write(`/api/tasking/${encodeURIComponent(id)}/deactivate`)
      if (!res.ok) throw new Error(refusal(res, json))
      const item = json as unknown as TaskingItem
      if (item?.id) setItems((l) => l.map((i) => (i.id === item.id ? item : i)))
      return true
    } catch (e) {
      setError((e as Error).message)
      setItems(before)
      void refetch.current()
      return false
    }
  }, [])

  return useMemo(() => ({ items, status, error, post, deactivate }), [items, status, error, post, deactivate])
}
