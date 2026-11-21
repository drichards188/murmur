// The operator groups' feed and calls (GET/POST/PATCH/DELETE /api/groups; groups.ts has the model).
// Polled while the console is open, kept apart from the fleet swarms' feed (swarms.ts). A change is
// shown at once (optimistic) and the bridge's answer, or the next poll, replaces it; a refusal puts
// the list back and says why. No step-up and no decision: an operator identity is all a write needs.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator } from './decisions'
import { applyPatch, type GroupPatch, type GroupsDoc, type OperatorGroup } from './groups'

export type GroupsStatus = 'loading' | 'ok' | 'missing' | 'error'

export interface GroupsApi {
  groups: OperatorGroup[]
  status: GroupsStatus
  /** the last refusal or failure, until the next call or poll succeeds */
  error: string | null
  /** make a group; the answer names uids the bridge did not know (`dropped`) */
  create: (name: string, members?: number[], color?: string | null) => Promise<{ group: OperatorGroup; dropped: number[] } | null>
  patch: (id: string, p: GroupPatch) => Promise<boolean>
  remove: (id: string) => Promise<boolean>
}

const POLL_MS = 2000
/** a bridge without groups is asked again this rarely */
const MISSING_MS = 20_000
const EMPTY: OperatorGroup[] = []

async function write(method: string, url: string, body?: unknown): Promise<{ res: Response; json: Record<string, unknown> }> {
  const res = await operatorFetch(url, {
    method,
    headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const json = (await res.json().catch(() => ({}))) as Record<string, unknown>
  return { res, json }
}

/** the bridge's group, without the `dropped` list that rides beside its fields */
function withoutDropped(json: Record<string, unknown>): OperatorGroup {
  const g = { ...json }
  delete g.dropped
  return g as unknown as OperatorGroup
}

const refusal = (res: Response, json: Record<string, unknown>) => String(json.detail ?? json.error ?? `the bridge returned ${res.status}`)

export function useGroups(on = true): GroupsApi {
  const [groups, setGroups] = useState<OperatorGroup[]>(EMPTY)
  const [status, setStatus] = useState<GroupsStatus>('loading')
  const [error, setError] = useState<string | null>(null)
  // a poll that was asked before a write must not overwrite what the write answered
  const epoch = useRef(0)
  const refetchRef = useRef<() => Promise<void>>(async () => {})
  // the list the calls below restore on a refusal
  const groupsRef = useRef(groups)
  useEffect(() => {
    groupsRef.current = groups
  }, [groups])

  useEffect(() => {
    if (!on) return
    let stop = false
    let timer: number | undefined
    const once = async (): Promise<number> => {
      const asked = epoch.current
      try {
        const res = await fetch('/api/groups')
        if (stop) return POLL_MS
        if (res.status === 404) {
          setStatus('missing')
          setGroups(EMPTY)
          return MISSING_MS
        }
        if (!res.ok) throw new Error(`the bridge returned ${res.status}`)
        const doc = (await res.json()) as GroupsDoc
        if (stop || asked !== epoch.current) return POLL_MS
        setGroups(Array.isArray(doc?.groups) ? doc.groups : EMPTY)
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
    refetchRef.current = async () => void (await once())
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

  const create = useCallback<GroupsApi['create']>(async (name, members = [], color = null) => {
    epoch.current++
    setError(null)
    try {
      const { res, json } = await write('POST', '/api/groups', { name: name.trim(), members, ...(color ? { color } : {}) })
      if (!res.ok) throw new Error(refusal(res, json))
      // the 201 body is the group (with `dropped` beside its fields, when any uid was unknown)
      const rest = withoutDropped(json)
      const dropped = (json.dropped as number[] | undefined) ?? []
      setGroups((gs) => [...gs.filter((g) => g.id !== rest.id), rest])
      setStatus('ok')
      return { group: rest, dropped }
    } catch (e) {
      setError((e as Error).message)
      return null
    }
  }, [])

  const patch = useCallback<GroupsApi['patch']>(async (id, p) => {
    epoch.current++
    setError(null)
    const before = groupsRef.current
    setGroups((gs) => gs.map((g) => (g.id === id ? applyPatch(g, p) : g)))
    try {
      const { res, json } = await write('PATCH', `/api/groups/${encodeURIComponent(id)}`, p)
      if (!res.ok) throw new Error(refusal(res, json))
      const rest = withoutDropped(json)
      setGroups((gs) => gs.map((g) => (g.id === id ? rest : g)))
      return true
    } catch (e) {
      setError((e as Error).message)
      setGroups(before)
      void refetchRef.current()
      return false
    }
  }, [])

  const remove = useCallback<GroupsApi['remove']>(async (id) => {
    epoch.current++
    setError(null)
    const before = groupsRef.current
    setGroups((gs) => gs.filter((g) => g.id !== id))
    try {
      const { res, json } = await write('DELETE', `/api/groups/${encodeURIComponent(id)}`)
      if (!res.ok && res.status !== 404) throw new Error(refusal(res, json))
      return true
    } catch (e) {
      setError((e as Error).message)
      setGroups(before)
      void refetchRef.current()
      return false
    }
  }, [])

  return useMemo(() => ({ groups, status, error, create, patch, remove }), [groups, status, error, create, patch, remove])
}
