import { useEffect, useRef, useState } from 'react'
import { enrollPasskey, login, loginWithPasskey, logout, refreshAuth, registerPasskey, useAuth } from './auth'
import { useOperator, useOperators } from './decisions'
import { passkeyBlocker, passkeyPlace } from './passkey'
import './login.css'

/** Rules 10: where passkeys work when this page is not there (127.0.0.1: WebAuthn takes no IP
 *  address), as a link to the same console on localhost. */
function PasskeyElsewhere() {
  const auth = useAuth()
  const place = passkeyPlace(auth.passkey)
  if (!auth.passkey || place.here || !place.url) return null
  return (
    <p className="muted login-foot">
      Passkeys work at <a href={place.url}>{place.url}</a> (WebAuthn takes no IP address); sign in there to use one.
    </p>
  )
}

/** The sign-in screen of a credentials run: the operator's own id and password, or (rules 10)
 *  their passkey. Nothing is stored in the browser but the bridge's HttpOnly session cookie. */
export function LoginScreen() {
  const auth = useAuth()
  const [operator, setOperator] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const first = useRef<HTMLInputElement>(null)
  useEffect(() => first.current?.focus(), [])
  const passwordOk = auth.login_factor !== 'passkey'
  const passkeyOk = !!auth.passkey && auth.login_factor !== 'password' && passkeyPlace(auth.passkey).here

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

  const withPasskey = async () => {
    setBusy(true)
    setError(null)
    try {
      await loginWithPasskey(operator)
    } catch (err) {
      setError((err as Error).message)
    }
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
          <input ref={first} value={operator} autoComplete="username webauthn" spellCheck={false} disabled={busy} onChange={(e) => setOperator(e.target.value)} required />
        </label>
        {passwordOk && (
          <label>
            Password
            <input type="password" value={password} autoComplete="current-password" disabled={busy} onChange={(e) => setPassword(e.target.value)} />
          </label>
        )}
        <div className="login-actions">
          {passwordOk && (
            <button className="ix-approve" type="submit" disabled={busy || !operator.trim() || !password}>
              {busy ? 'Signing in…' : 'Sign in'}
            </button>
          )}
          {passkeyOk && (
            <button className={passwordOk ? 'ghost' : 'ix-approve'} type="button" disabled={busy || !operator.trim()} onClick={() => void withPasskey()}>
              Sign in with a passkey
            </button>
          )}
        </div>
        {error && <p className="error">{error}</p>}
        <PasskeyElsewhere />
        <p className="muted login-foot">
          Every decision you make is recorded under your operator id. An engagement signature asks for your{' '}
          {auth.step_up_factor === 'passkey' ? 'passkey' : 'password (or your passkey)'} again at the moment you sign.
        </p>
      </form>
    </div>
  )
}

/** Rules 10: registering a passkey through a one-time invitation link (`murmur-auth
 *  passkey-invite`, `#enroll=<operator>.<code>`): no sign-in needed. The code is read from the
 *  address once and dropped from it. */
export function EnrollScreen({ invite, onDone }: { invite: { operator: string; code: string }; onDone: () => void }) {
  const auth = useAuth()
  const [label, setLabel] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [done, setDone] = useState(false)
  const blocked = auth.passkey ? passkeyBlocker(auth.passkey) : 'this run takes no passkey'
  const go = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setError(null)
    try {
      await enrollPasskey(invite.operator, invite.code, label || 'passkey')
      setDone(true)
    } catch (err) {
      setError((err as Error).message)
    }
    setBusy(false)
  }
  return (
    <div className="login">
      <form className="login-card" onSubmit={go}>
        <header>
          <span className="brand">MURMUR</span>
          <span className="sub">multi-domain swarm C2</span>
        </header>
        <h1>Register a passkey</h1>
        {done ? (
          <>
            <p>
              A passkey for <b>{invite.operator}</b> is registered. Sign in with it, and use it at every consequential act.
            </p>
            <button className="ix-approve" type="button" onClick={onDone}>
              Continue to sign in
            </button>
          </>
        ) : (
          <>
            <p className="muted">
              An invitation for operator <b>{invite.operator}</b>. Your authenticator (this device&apos;s unlock, a security key, a phone) makes the passkey; Murmur keeps its public key only.
            </p>
            <label>
              Name it (to tell your passkeys apart)
              <input value={label} maxLength={64} placeholder="laptop" disabled={busy} onChange={(e) => setLabel(e.target.value)} />
            </label>
            <button className="ix-approve" type="submit" disabled={busy || !!blocked}>
              {busy ? 'Waiting for your authenticator…' : 'Register a passkey'}
            </button>
            {blocked && <p className="login-note">{blocked}</p>}
            {error && <p className="error">{error}</p>}
            <PasskeyElsewhere />
          </>
        )}
      </form>
    </div>
  )
}

/** Rules 10: the operator menu's passkey registration: a name and the password again (a
 *  session left open must not add a credential to someone's account). */
function PasskeyMenu({ onClose }: { onClose: () => void }) {
  const auth = useAuth()
  const [label, setLabel] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [msg, setMsg] = useState<{ text: string; bad: boolean } | null>(null)
  const blocked = passkeyBlocker(auth.passkey)
  const place = passkeyPlace(auth.passkey)
  const go = async (e: React.FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setMsg(null)
    try {
      await registerPasskey(label || 'passkey', password)
      setMsg({ text: 'Registered: your passkey can sign in and sign from now on.', bad: false })
      setLabel('')
    } catch (err) {
      setMsg({ text: (err as Error).message, bad: true })
    }
    setPassword('')
    setBusy(false)
  }
  return (
    <form className="pk-menu" onSubmit={go} onKeyDown={(e) => e.key === 'Escape' && onClose()}>
      <b>Passkeys</b>
      <span className="muted">
        {auth.passkey?.registered ?? 0} registered for {auth.operator}
        {auth.step_up_factor === 'passkey' ? '; this run asks for one at every consequential act' : ''}.
      </span>
      {blocked ? (
        <span className="login-note">
          {place.url && !place.here ? (
            <>
              Passkeys work at <a href={place.url}>{place.url}</a> (WebAuthn takes no IP address).
            </>
          ) : (
            blocked
          )}
        </span>
      ) : (
        <>
          <label>
            Name
            <input value={label} maxLength={64} placeholder="laptop" disabled={busy} onChange={(e) => setLabel(e.target.value)} autoFocus />
          </label>
          <label>
            Your password
            <input type="password" autoComplete="current-password" value={password} disabled={busy} onChange={(e) => setPassword(e.target.value)} />
          </label>
          <button className="ix-approve" type="submit" disabled={busy || !password}>
            {busy ? 'Waiting for your authenticator…' : 'Register a passkey'}
          </button>
        </>
      )}
      {msg && <span className={msg.bad ? 'error' : 'muted'}>{msg.text}</span>}
      <button className="ghost mini" type="button" onClick={onClose}>
        close
      </button>
    </form>
  )
}

/** The top bar's "who am I": the signed-in operator, their passkeys and Sign out (credentials
 *  run), or the dev run's switcher with a warning that ids are asserted, not authenticated. */
export function OperatorBadge() {
  const auth = useAuth()
  const [operator, setOperator] = useOperator()
  const ops = useOperators()
  const [menu, setMenu] = useState(false)
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
      {auth.passkey && (
        <button className="ghost" onClick={() => setMenu((m) => !m)} aria-expanded={menu} title="Your passkeys: register one">
          Passkeys ({auth.passkey.registered ?? 0})
        </button>
      )}
      <button className="ghost" onClick={() => void logout()}>
        Sign out
      </button>
      {menu && <PasskeyMenu onClose={() => setMenu(false)} />}
    </span>
  )
}
