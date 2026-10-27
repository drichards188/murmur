// Rules 21 (bd murmur-bia): the identify order's console side that is not a component: the
// bridge's ranking of the vehicles that could go (`POST /api/identify/options`, read-only) and
// sending one. The track card's form and the Task panel's Identify mode both use these
// (IdentifyForm.tsx, TaskPanel.tsx).
import { useEffect, useState } from 'react'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, submitIntent } from './decisions'

export interface IdentifyOption {
  uid: number
  callsign: string | null
  platform: string | null
  passed: boolean
  failing: string[]
  standoff_m: number | null
  reach_m: number | null
  transit_s: number | null
  link: { holds?: boolean | null; p_failsafe?: number | null; sentence?: string | null } | null
}

/** asked again this often while open: vehicles move, and the track with them */
const REFRESH_MS = 5000

/** The bridge's ranking for `track` (none: nothing asked), for `uids` only when given (a vehicle
 *  named this way is answered even without a camera, with why it can't go). */
export function useIdentifyOptions(track: string | null, uids?: number[]): { options: IdentifyOption[] | null; error: string | null } {
  const [st, setSt] = useState<{ key: string | null; options: IdentifyOption[] | null; error: string | null }>({ key: null, options: null, error: null })
  const key = track ? JSON.stringify({ track, uids: uids ?? null }) : null
  useEffect(() => {
    if (!key) return
    let gone = false
    let timer = 0
    const ask = async () => {
      try {
        const res = await operatorFetch('/api/identify/options', {
          method: 'POST',
          headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
          body: key,
        })
        const b = await res.json().catch(() => ({}))
        if (!res.ok) throw new Error(b.detail ?? `bridge returned ${res.status}`)
        if (!gone) setSt({ key, options: (b.vehicles ?? []) as IdentifyOption[], error: null })
      } catch (e) {
        if (!gone) setSt((s) => ({ key, options: s.key === key ? s.options : null, error: (e as Error).message }))
      }
      if (!gone) timer = window.setTimeout(ask, REFRESH_MS)
    }
    void ask()
    return () => {
      gone = true
      window.clearTimeout(timer)
    }
  }, [key])
  return st.key === key ? { options: st.options, error: st.error } : { options: null, error: null }
}

/** Send it: what to say afterwards (dispatched, or waiting for a decision). */
export async function sendIdentify(uid: number, track: string, who: string): Promise<string> {
  const out = await submitIntent({ task: 'identify', uids: [uid], track })
  return out.dispatched ? `${who} sent to identify ${track}` : `waiting for a decision in the Inbox (${out.intent.state.replace('_', ' ')})`
}
