import { useSyncExternalStore } from 'react'
import { createCredential, getAssertion, passkeyBlocker, type PasskeyInfo } from './passkey'

/** Who this console acts for (rules 8). In a credentials run (`MURMUR_AUTH=credentials`;
 *  every study) the operator signs in with their own password; the bridge keeps the session
 *  in an HttpOnly cookie this code never sees, and every request from this page carries it. A
 *  consequential act sends the password again (a step-up): every engagement signature, and
 *  whatever the policy's `step_up_acts` lists (a hostile declaration, ...). In a dev run (the
 *  default for `make up`) there is no login: the console names its operator in a header, the
 *  "Acting as" switcher picks it, and the bridge records the run as dev (no human-control
 *  claim). The password is never stored here: it goes into the one request and is dropped.
 *  Rules 10: an operator may register passkeys (WebAuthn, from the operator menu, or through a
 *  one-time invitation link), sign in with one, and step up with one: the bridge derives each
 *  step-up's challenge from the act, so the authenticator signs that act. Where the policy (or
 *  a study) asks for a passkey, the password is not taken. Passkeys need the console at
 *  http://localhost:<port> (WebAuthn takes no IP address). */
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
  /** credentials run: acts besides signatures that re-enter the password (the policy's
   *  `[approval] step_up_acts`: `declare_hostile`, `accept_unknown`, `confirm_declaration`) */
  step_up_acts?: string[]
  /** rules 10: what a step-up must be (`passkey` where the policy, or a study, asks for one)
   *  and how people sign in */
  step_up_factor?: 'password' | 'passkey'
  login_factor?: 'either' | 'password' | 'passkey'
  /** rules 10: the run's relying party and console origins, and (signed in) how many passkeys
   *  the operator has here; absent when the run takes none */
  passkey?: PasskeyInfo
  /** rules 10: a study run (its participant, and whether password step-ups were allowed) */
  study?: { participant: string; password_step_up: boolean }
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
  step_up_acts?: string[]
  step_up_factor?: 'password' | 'passkey'
  login_factor?: 'either' | 'password' | 'passkey'
  passkey?: PasskeyInfo
  study?: { participant: string; password_step_up: boolean }
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
    step_up_acts: dev ? undefined : b.step_up_acts ?? [],
    step_up_factor: dev ? undefined : b.step_up_factor,
    login_factor: dev ? undefined : b.login_factor,
    passkey: dev ? undefined : b.passkey,
    study: dev ? undefined : b.study,
  }
}

/** A credentials run asks for the password again for `act` (the policy lists it). */
export function stepsUpAct(act: 'declare_hostile' | 'accept_unknown' | 'confirm_declaration'): boolean {
  return state.auth === 'credentials' && (state.step_up_acts ?? []).includes(act)
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
    set(signedOut())
  }
}

/** Signed out, keeping what the sign-in screen needs to know (rules 10: whether a passkey can
 *  sign in here, and on which host). */
function signedOut(ended?: string): AuthState {
  const pk = state.passkey ? { rp_id: state.passkey.rp_id, origins: state.passkey.origins } : undefined
  return { auth: 'credentials', signed_in: false, ended, passkey: pk, login_factor: state.login_factor }
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
    if (!b.step_up) set(signedOut(b.detail ?? 'your session has ended'))
  }
  return res
}

// ---- passkeys (rules 10) ------------------------------------------------------------------

/** What a step-up needs in this run, for the signed-in operator: `passkey` where the policy (or
 *  a study) asks people for one; `password` otherwise (a passkey is accepted there too). */
export function stepUpFactor(): 'password' | 'passkey' {
  return state.step_up_factor === 'passkey' ? 'passkey' : 'password'
}

/** Whether this operator can step up with a passkey on this page (one registered, the console
 *  on the relying party's host), and if not, why. */
export function passkeyReady(): { ok: boolean; why: string | null } {
  const blocked = passkeyBlocker(state.passkey)
  if (blocked) return { ok: false, why: blocked }
  if ((state.passkey?.registered ?? 0) === 0) {
    return { ok: false, why: 'you have no passkey here: register one from your operator menu (top right)' }
  }
  return { ok: true, why: null }
}

/** What a passkey step-up is for (the bridge derives its challenge from it). */
export type StepUpAct =
  | { act: 'sign'; intent: number; token?: number }
  | { act: 'declare_hostile'; track: string }
  | { act: 'confirm_declaration'; track: string }
  /** rules 8 and 16: accepting lost contacts with a nomination (`intent` null) or for one
   *  waiting (its id), each `{track, since}` exactly as the request names it; rules 19: and
   *  live tracks as behaving now, each `{track}` (the same act and step-up) */
  | { act: 'accept_unknown'; intent: number | null; accept_lost?: { track: string; since: number }[]; accept_behaving?: { track: string }[] }
  | { act: 'unspend'; uid: number }

async function postJson(path: string, body: unknown): Promise<Record<string, unknown>> {
  const res = await operatorFetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  const b = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(b.detail ?? `bridge returned ${res.status}`)
  return b
}

/** A passkey's assertion for one act: the challenge for it from the bridge, signed by the
 *  operator's authenticator (it asks for their PIN, fingerprint or unlock). Goes into the
 *  act's request as `passkey`. */
export async function passkeyFor(act: StepUpAct): Promise<Record<string, unknown>> {
  const blocked = passkeyBlocker(state.passkey)
  if (blocked) throw new Error(blocked)
  const options = await postJson('/api/auth/step_up', act)
  return getAssertion(options as never)
}

/** Sign in with a passkey (the operator names themselves first). */
export async function loginWithPasskey(operator: string): Promise<void> {
  const blocked = passkeyBlocker(state.passkey)
  if (blocked) throw new Error(blocked)
  const options = await postJson('/api/auth/login/passkey', { operator: operator.trim() })
  const passkey = await getAssertion(options as never)
  const res = await fetch('/api/auth/login', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ operator: operator.trim(), passkey }),
  })
  const b = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(b.detail ?? `bridge returned ${res.status}`)
  set(fromBody(b))
}

/** Register a passkey for the signed-in operator, the password re-entered with it (a session
 *  left open does not add a credential to someone's account). */
export async function registerPasskey(label: string, password: string): Promise<void> {
  const blocked = passkeyBlocker(state.passkey)
  if (blocked) throw new Error(blocked)
  const options = await postJson('/api/auth/passkeys/register/options', {})
  const credential = await createCredential(options as never)
  await postJson('/api/auth/passkeys/register', { credential, label, password })
  await refreshAuth()
}

/** Register a passkey through a one-time invitation (`murmur-auth passkey-invite`; no sign-in
 *  needed). */
export async function enrollPasskey(operator: string, code: string, label: string): Promise<void> {
  const blocked = passkeyBlocker(state.passkey)
  if (blocked) throw new Error(blocked)
  const options = await postJson('/api/auth/passkeys/enroll/options', { operator, code })
  const credential = await createCredential(options as never)
  await postJson('/api/auth/passkeys/enroll', { operator, code, credential, label })
}
