import { useEffect, useState } from 'react'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, submitIntent } from './decisions'
import { DryRun } from './DryRun'
import { useDryRun } from './useDryRun'
import type { Vehicle } from './types'
import type { TaskRequest } from './useBridge'

/** Rules 21 (bd murmur-bia): send one vehicle to identify this track. The bridge ranks the
 *  vehicles that could go (`POST /api/identify/options`: each one's checks now, the orbit's
 *  standoff from its camera's reach, the time to get there, the predicted link along the way;
 *  read-only); the first that passes is chosen, the operator may pick another, sees the usual dry
 *  run, and sends it. The vehicle orbits the track, following it, its camera looking for it first,
 *  until it is identified, lost or out of time; then it goes back to what it was doing. */

interface Option {
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

/** asked again this often while the form is open: vehicles move, and the track with them */
const REFRESH_MS = 5000

function fmtTime(s: number | null) {
  if (s == null) return ''
  if (s < 60) return `${Math.round(s)} s`
  return `${Math.round(s / 60)} min`
}

export function IdentifyForm({ track, vehicles, onClose }: { track: string; vehicles: Vehicle[]; onClose: () => void }) {
  const [options, setOptions] = useState<Option[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [chosen, setChosen] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [sent, setSent] = useState<string | null>(null)

  useEffect(() => {
    let gone = false
    let timer = 0
    const ask = async () => {
      try {
        const res = await operatorFetch('/api/identify/options', {
          method: 'POST',
          headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
          body: JSON.stringify({ track }),
        })
        const b = await res.json().catch(() => ({}))
        if (!res.ok) throw new Error(b.detail ?? `bridge returned ${res.status}`)
        if (gone) return
        const list = (b.vehicles ?? []) as Option[]
        setOptions(list)
        setError(null)
        setChosen((c) => (c != null && list.some((o) => o.uid === c) ? c : (list.find((o) => o.passed)?.uid ?? null)))
      } catch (e) {
        if (!gone) setError((e as Error).message)
      }
      if (!gone) timer = window.setTimeout(ask, REFRESH_MS)
    }
    void ask()
    return () => {
      gone = true
      window.clearTimeout(timer)
    }
  }, [track])

  const req: TaskRequest | null = chosen != null && !sent ? { task: 'identify', uids: [chosen], track } : null
  const dry = useDryRun(req, vehicles)

  const send = async () => {
    if (chosen == null) return
    setBusy(true)
    setError(null)
    try {
      const out = await submitIntent({ task: 'identify', uids: [chosen], track })
      const who = options?.find((o) => o.uid === chosen)?.callsign ?? `uid ${chosen}`
      setSent(out.dispatched ? `${who} sent to identify ${track}` : `waiting for a decision in the Inbox (${out.intent.state.replace('_', ' ')})`)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="tk-identify">
      <div className="tk-identify-head">
        <strong>Identify {track}</strong>
        <button className="ghost mini" onClick={onClose}>
          close
        </button>
      </div>
      {sent ? (
        <p className="muted">{sent}. Follow it in the command log.</p>
      ) : (
        <>
          {options == null && !error && <p className="muted">Ranking the vehicles that could go...</p>}
          {options?.length === 0 && <p className="muted">No vehicle of ours carries an EO/IR that could identify it.</p>}
          {options && options.length > 0 && (
            <ul className="tk-identify-list">
              {options.map((o) => (
                <li key={o.uid} className={o.passed ? '' : 'off'}>
                  <label title={o.passed ? (o.link?.sentence ?? undefined) : o.failing.join('; ')}>
                    <input type="radio" name={`identify-${track}`} disabled={!o.passed} checked={chosen === o.uid} onChange={() => setChosen(o.uid)} />
                    <b>{o.callsign ?? `uid ${o.uid}`}</b>
                    <span className="muted"> {o.platform}</span>
                    {o.passed ? (
                      <span className="muted">
                        {' '}
                        · orbit {Math.round(o.standoff_m ?? 0)} m · about {fmtTime(o.transit_s)} away
                        {o.link?.holds === false ? <span className="warn"> · link may drop</span> : o.link?.holds ? ' · link holds' : ''}
                      </span>
                    ) : (
                      <span className="muted"> · {o.failing[0] ?? 'cannot go'}</span>
                    )}
                  </label>
                </li>
              ))}
            </ul>
          )}
          {chosen != null && <DryRun st={dry} vehicles={vehicles} />}
          <button className="primary" disabled={chosen == null || busy} onClick={() => void send()}>
            {chosen == null ? 'No vehicle can go' : `Send IDENTIFY to ${options?.find((o) => o.uid === chosen)?.callsign ?? `uid ${chosen}`}`}
          </button>
        </>
      )}
      {error && <p className="tk-err">{error}</p>}
    </div>
  )
}
