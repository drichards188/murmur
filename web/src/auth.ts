import { useSyncExternalStore } from 'react'

/** Who this console acts for (rules 7). In a credentials run (the bridge's default) the
 *  operator signs in with their own password; the bridge keeps the session in an HttpOnly
 *  cookie this code never sees, and every request from this page carries it. A consequential
 *  signature (every engagement signature) sends the password again (a step-up). In a dev run
 *  (`MURMUR_AUTH=dev`) there is no login: the console names its operator in a header, the
 *  "Acting as" switcher picks it, and the bridge records the run as dev (no human-control
 *  claim). The password is never stored here: it goes into the one request and is dropped. */
export interface AuthState {
  /** unknown: the bridge has not answered yet */
  auth: 'credentials' | 'dev' | 'unknown'
  signed_in: boolean
  operator?: string
  /** the signed-in operator holds engagement authority */
  engage?: boolean
  /** seconds until the session ends unless the operator acts */
  ends_in_s?: number
  /** dev run: the ids a request may name, and those with engagement authority */
  operators?: string[]
  engage_list?: string[]
  /** why the console asks to sign in again (the session ended on the bridge) */
  ended?: string
}

let state: AuthState = { auth: 'unknown', signed_in: false }
const listeners = new Set<() => void>()

function set(s: AuthState) {
  state = s
  listeners.forEach((l) => l())
}

export function getAuth(): AuthState {
  return state
}

export function useAuth(): AuthState {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    },
    () => state,
  )
}

interface SessionBody {
  auth?: 'credentials' | 'dev'
  signed_in?: boolean
  operator?: string
  engage?: boolean | string[]
  ends_in_s?: number
  operators?: string[]
}

function fromBody(b: SessionBody): AuthState {
  const dev = b.auth === 'dev'
  return {
    auth: b.auth ?? 'unknown',
    signed_in: !!b.signed_in,
    operator: b.operator,
    engage: dev ? undefined : b.engage === true,
    ends_in_s: b.ends_in_s,
    operators: dev ? b.operators : undefined,
    engage_list: dev && Array.isArray(b.engage) ? b.engage : undefined,
  }
}

/** Ask the bridge who is signed in on this browser (not counted as activity). */
export async function refreshAuth(): Promise<void> {
  try {
    const res = await fetch('/api/auth/session')
    if (!res.ok) return
    set(fromBody(await res.json()))
  } catch {
    // bridge unreachable: keep what we had (the app shows the bridge as down)
  }
}

/** Sign in. Throws an Error with the bridge's reason (wrong password, locked, not on roster). */
export async function login(operator: string, password: string): Promise<void> {
  const res = await fetch('/api/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ operator: operator.trim(), password }),
  })
  const b = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(b.detail ?? `bridge returned ${res.status}`)
  set(fromBody(b))
}

export async function logout(): Promise<void> {
  try {
    await fetch('/api/auth/logout', { method: 'POST' })
  } finally {
    set({ auth: 'credentials', signed_in: false })
  }
}

/** The headers an operator's request carries: in a dev run the asserted operator id, in a
 *  credentials run none (the session cookie goes by itself). */
export function operatorHeaders(devOperator: string): Record<string, string> {
  return state.auth === 'dev' ? { 'x-murmur-operator': devOperator } : {}
}

/** `fetch` for an operator's request: a 401 that is not a step-up refusal means the session
 *  ended on the bridge (logout elsewhere, expiry, the credential removed or changed), so the
 *  console asks to sign in again. */
export async function operatorFetch(input: string, init: RequestInit): Promise<Response> {
  const res = await fetch(input, init)
  if (res.status === 401 && state.auth === 'credentials') {
    const b = await res.clone().json().catch(() => ({}))
    if (!b.step_up) set({ auth: 'credentials', signed_in: false, ended: b.detail ?? 'your session has ended' })
  }
  return res
}
