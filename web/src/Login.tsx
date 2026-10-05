import { useEffect, useRef, useState } from 'react'
import { login, logout, refreshAuth, useAuth } from './auth'
import { useOperator, useOperators } from './decisions'
import './login.css'

/** The sign-in screen of a credentials run: the operator's own id and password. Nothing is
 *  stored in the browser but the bridge's HttpOnly session cookie. */
export function LoginScreen() {
  const auth = useAuth()
  const [operator, setOperator] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const first = useRef<HTMLInputElement>(null)
  useEffect(() => first.current?.focus(), [])

  const submit = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError(null)
    try {
      await login(operator, password)
    } catch (err) {
      setError((err as Error).message)
    }
    setPassword('')
    setBusy(false)
  }

  return (
    <div className="login">
      <form className="login-card" onSubmit={submit}>
        <header>
          <span className="brand">MURMUR</span>
          <span className="sub">multi-domain swarm C2</span>
        </header>
        <h1>Sign in</h1>
        {auth.ended && <p className="login-note">{auth.ended}. Sign in again.</p>}
        <label>
          Operator
          <input ref={first} value={operator} autoComplete="username" spellCheck={false} disabled={busy} onChange={(e) => setOperator(e.target.value)} required />
        </label>
        <label>
          Password
          <input type="password" value={password} autoComplete="current-password" disabled={busy} onChange={(e) => setPassword(e.target.value)} required />
        </label>
        <button className="ix-approve" type="submit" disabled={busy || !operator.trim() || !password}>
          {busy ? 'Signing in…' : 'Sign in'}
        </button>
        {error && <p className="error">{error}</p>}
        <p className="muted login-foot">
          Every decision you make is recorded under your operator id. An engagement signature asks for your password again at the moment you sign.
        </p>
      </form>
    </div>
  )
}

/** The top bar's "who am I": the signed-in operator and Sign out (credentials run), or the dev
 *  run's switcher with a warning that ids are asserted, not authenticated. */
export function OperatorBadge() {
  const auth = useAuth()
  const [operator, setOperator] = useOperator()
  const ops = useOperators()
  // keep the session's remaining time roughly current (a status query, not activity)
  useEffect(() => {
    if (auth.auth !== 'credentials' || !auth.signed_in) return
    const id = window.setInterval(() => void refreshAuth(), 60_000)
    return () => window.clearInterval(id)
  }, [auth.auth, auth.signed_in])

  if (auth.auth === 'dev') {
    return (
      <span className="op-badge dev" title="A dev run (the default; MURMUR_AUTH=credentials signs operators in): the bridge believes whatever operator id a request names. The run is recorded as dev and is not evidence about people.">
        <span className="pill down">DEV: IDS NOT AUTHENTICATED</span>
        <label>
          Acting as
          <select value={operator} onChange={(e) => setOperator(e.target.value)} aria-label="Operator">
            {(ops.operators.includes(operator) ? ops.operators : [operator, ...ops.operators]).map((o) => (
              <option key={o} value={o}>
                {o}
                {ops.engage.includes(o) ? ' (engagement authority)' : ''}
              </option>
            ))}
          </select>
        </label>
      </span>
    )
  }
  if (auth.auth !== 'credentials' || !auth.signed_in) return null
  return (
    <span className="op-badge">
      <span className="op-name" title={auth.ends_in_s != null ? `session ends in about ${Math.ceil(auth.ends_in_s / 60)} min without an action` : undefined}>
        <b>{auth.operator}</b>
        {auth.engage ? <span className="muted"> engagement authority</span> : null}
      </span>
      <button className="ghost" onClick={() => void logout()}>
        Sign out
      </button>
    </span>
  )
}
