import { useState } from 'react'
import { passkeyFor, passkeyReady, stepUpFactor, type StepUpAct } from './auth'

/** How a consequential act is stepped up at a card (rules 8 and 10): the password typed
 *  again, or the operator's passkey (asked for when the act is sent). */
export interface StepUpState {
  mode: 'password' | 'passkey'
  password: string
}

/** A card's step-up: a passkey where the run asks for one, else the password (a passkey may be
 *  chosen instead). */
export function useStepUp(): [StepUpState, (s: Partial<StepUpState>) => void] {
  const [s, setS] = useState<StepUpState>(() => ({ mode: stepUpFactor() === 'passkey' ? 'passkey' : 'password', password: '' }))
  return [s, (p) => setS((old) => ({ ...old, ...p }))]
}

/** Whether the step-up can be sent as it stands. */
export function stepUpReady(s: StepUpState): boolean {
  return s.mode === 'passkey' ? passkeyReady().ok : s.password.length > 0
}

/** What goes into the act's request: the password, or a passkey's assertion made for `act`
 *  (the authenticator prompts here). */
export async function stepUpProof(s: StepUpState, act: StepUpAct): Promise<{ password?: string; passkey?: Record<string, unknown> }> {
  return s.mode === 'passkey' ? { passkey: await passkeyFor(act) } : { password: s.password }
}
