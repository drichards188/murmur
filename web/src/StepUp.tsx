import { getAuth, passkeyReady, stepUpFactor } from './auth'
import { passkeyPlace } from './passkey'
import type { StepUpState } from './stepUpState'

interface FieldProps {
  /** "Your password, to sign", "Consequential: your password to approve", ... */
  passwordLabel: string
  /** what the passkey is for, "to sign this plan" */
  purpose: string
  ariaLabel: string
  value: StepUpState
  onChange: (s: Partial<StepUpState>) => void
  busy: boolean
  bad?: boolean
  onEnter?: () => void
}

/** The step-up field of a card: the password input, or a word that the passkey will be asked
 *  for (with why it can't be, and where to go, when it can't). */
export function StepUpField({ passwordLabel, purpose, ariaLabel, value, onChange, busy, bad, onEnter }: FieldProps) {
  const factor = stepUpFactor()
  const ready = passkeyReady()
  if (value.mode === 'passkey') {
    const place = passkeyPlace(getAuth().passkey)
    return (
      <div className={`ix-stepup ix-passkey ${bad ? 'bad' : ''}`} role="note">
        <span>
          Your passkey, {purpose}: your authenticator asks for it when you go ahead
          {factor === 'passkey' ? ' (this run asks for a passkey)' : ''}.
        </span>
        {!ready.ok && (
          <span className="eg-alarm">
            {ready.why}
            {place.url && !place.here ? (
              <>
                {' '}
                <a href={place.url}>{place.url}</a>
              </>
            ) : null}
          </span>
        )}
        {factor === 'password' && (
          <button type="button" className="ghost mini" disabled={busy} onClick={() => onChange({ mode: 'password' })}>
            Use my password
          </button>
        )}
      </div>
    )
  }
  return (
    <label className={`ix-stepup ${bad ? 'bad' : ''}`}>
      {passwordLabel}
      <input
        type="password"
        autoComplete="current-password"
        value={value.password}
        disabled={busy}
        onChange={(e) => onChange({ password: e.target.value })}
        onKeyDown={(e) => e.key === 'Enter' && value.password && !busy && onEnter?.()}
        aria-label={ariaLabel}
      />
      {ready.ok && (
        <button type="button" className="ghost mini" disabled={busy} onClick={() => onChange({ mode: 'passkey', password: '' })}>
          Use my passkey
        </button>
      )}
    </label>
  )
}
