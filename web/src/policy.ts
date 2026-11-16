import { useEffect, useState } from 'react'

/** The policy in force as the bridge serves it (`GET /api/policy`, read-only, no identity needed;
 *  docs/SCENARIO-SETTINGS.md): what MCP `get_policy` gives agents (`policy`, the effective `watch`),
 *  with the decision rules it runs under and the defaults a nomination takes where the operator
 *  names none. The console's forms read their defaults here, never a copy of them. */
export interface PolicyAnswer {
  t: number
  rules_version: number
  policy: unknown
  watch: Record<string, unknown> | null
  engage_defaults: EngageDefaults
}

/** A nomination's effect radius (m) and window (s) where the operator names none: the policy's
 *  `[rules.engage] default_effect_radius_m` and `default_window_s`. */
export interface EngageDefaults {
  effect_radius_m: number
  window_s: number
}

export async function fetchPolicy(): Promise<PolicyAnswer> {
  const res = await fetch('/api/policy')
  const body = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(body.detail ?? `bridge returned ${res.status}`)
  return body as PolicyAnswer
}

/** The nomination's defaults, read once when the form opens: null until they come, or if they
 *  could not be read (then `error` says why, and the operator names a radius and a window). */
export function useEngageDefaults(): { defaults: EngageDefaults | null; error: string | null } {
  const [defaults, setDefaults] = useState<EngageDefaults | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let gone = false
    fetchPolicy()
      .then((p) => {
        if (gone) return
        if (p.engage_defaults) setDefaults(p.engage_defaults)
        else setError('the policy names no nomination defaults')
      })
      .catch((e: Error) => !gone && setError(e.message))
    return () => {
      gone = true
    }
  }, [])
  return { defaults, error }
}
