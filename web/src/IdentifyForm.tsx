import { useState } from 'react'
import { DryRun } from './DryRun'
import { type IdentifyOption, sendIdentify, useIdentifyOptions } from './identify'
import { useDryRun } from './useDryRun'
import type { Vehicle } from './types'
import type { TaskRequest } from './useBridge'

/** Rules 21 (bd murmur-bia): send one vehicle to identify one track. The bridge ranks the
 *  vehicles that could go (`POST /api/identify/options`: each one's checks now, the orbit's
 *  standoff from its camera's reach, the time to get there, the predicted link along the way;
 *  read-only; identify.ts). Two ways in: from a track's card (`IdentifyForm`: the first vehicle that
 *  passes is chosen, the operator may pick another) and from the Task panel with one vehicle
 *  selected and the contact clicked on the map (identify.ts for that vehicle, and `OptionLine`).
 *  The vehicle orbits the track, following it, its camera looking for it first, until it is
 *  identified, lost or out of time; then it goes back to what it was doing. */

function fmtTime(s: number | null) {
  if (s == null) return ''
  if (s < 60) return `${Math.round(s)} s`
  return `${Math.round(s / 60)} min`
}

/** One vehicle's answer, in a line: its orbit, how far away, its link; or why it can't go. */
export function OptionLine({ o, name = true }: { o: IdentifyOption; name?: boolean }) {
  return (
    <>
      {name && (
        <>
          <b>{o.callsign ?? `uid ${o.uid}`}</b>
          <span className="muted"> {o.platform}</span>
        </>
      )}
      {o.passed ? (
        <span className="muted">
          {name ? ' · ' : ''}orbit {Math.round(o.standoff_m ?? 0)} m · about {fmtTime(o.transit_s)} away
          {o.link?.holds === false ? <span className="warn"> · link may drop</span> : o.link?.holds ? ' · link holds' : ''}
        </span>
      ) : (
        <span className="muted">
          {name ? ' · ' : ''}
          {o.failing[0] ?? 'cannot go'}
        </span>
      )}
    </>
  )
}

export function IdentifyForm({ track, vehicles, onClose }: { track: string; vehicles: Vehicle[]; onClose: () => void }) {
  const { options, error: optError } = useIdentifyOptions(track)
  const [error, setError] = useState<string | null>(null)
  const [picked, setPicked] = useState<number | null>(null)
  const [busy, setBusy] = useState(false)
  const [sent, setSent] = useState<string | null>(null)
  // the operator's pick while it can still go, else the first that passes
  const chosen = picked != null && options?.some((o) => o.uid === picked && o.passed) ? picked : (options?.find((o) => o.passed)?.uid ?? null)
  const req: TaskRequest | null = chosen != null && !sent ? { task: 'identify', uids: [chosen], track } : null
  const dry = useDryRun(req, vehicles)
  const who = options?.find((o) => o.uid === chosen)?.callsign ?? `uid ${chosen}`

  const send = async () => {
    if (chosen == null) return
    setBusy(true)
    setError(null)
    try {
      setSent(await sendIdentify(chosen, track, who))
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
          {options == null && !optError && <p className="muted">Ranking the vehicles that could go...</p>}
          {options?.length === 0 && <p className="muted">No vehicle of ours carries an EO/IR that could identify it.</p>}
          {options && options.length > 0 && (
            <ul className="tk-identify-list">
              {options.map((o) => (
                <li key={o.uid} className={o.passed ? '' : 'off'}>
                  <label title={o.passed ? (o.link?.sentence ?? undefined) : o.failing.join('; ')}>
                    <input type="radio" name={`identify-${track}`} disabled={!o.passed} checked={chosen === o.uid} onChange={() => setPicked(o.uid)} />
                    <OptionLine o={o} />
                  </label>
                </li>
              ))}
            </ul>
          )}
          {chosen != null && <DryRun st={dry} vehicles={vehicles} />}
          <button className="primary" disabled={chosen == null || busy} onClick={() => void send()}>
            {chosen == null ? 'No vehicle can go' : `Send IDENTIFY ${track} to ${who}`}
          </button>
        </>
      )}
      {(error ?? optError) && <p className="tk-err">{error ?? optError}</p>}
    </div>
  )
}
