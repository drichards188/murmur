import type { Check } from './decisions'
import type { Vehicle } from './types'
import { callsign } from './types'
import { usePlatformLimits } from './taskDefaults'
import { foldRefusal } from './refusalFold'
import './refusalFold.css'

/** A group order's failing checks: the bottom line, then one line a rule (members behind a
 *  toggle) and what would fix it. Renders nothing for a one-vehicle order: keep its own list. */
export function RefusalFold({ checks, uids, vehicles }: { checks: Check[]; uids: string[]; vehicles: Vehicle[] }) {
  const limits = usePlatformLimits()
  const byUid = new Map(vehicles.map((v) => [String(v.uid), v]))
  const cruiseOf = (u: string) => {
    const v = byUid.get(u)
    return (v && limits?.get(v.identity?.platform ?? '')?.cruise_mps) || null
  }
  const f = foldRefusal(checks, uids, cruiseOf)
  if (!f.folded) return null
  const name = (u: string) => {
    const v = byUid.get(u)
    return v ? callsign(v) : `vehicle ${u}`
  }
  return (
    <div className="rf">
      {f.bottom && <p className={`rf-bottom ${f.refused ? 'fail' : ''}`}>{f.bottom}</p>}
      {f.lines.length > 0 && (
        <ul className="rf-lines">
          {f.lines.map((l, k) => (
            <li key={k} className={l.class === 'waiting' ? 'warn' : 'fail'}>
              {l.text}
              {l.hint && <span className="rf-hint">{l.hint}</span>}
              <details className="rf-members" onClick={(e) => e.stopPropagation()}>
                <summary>show the {l.count}</summary>
                <ul>
                  {l.members.map((m) => (
                    <li key={m.uid}>
                      <b>{name(m.uid)}</b>: {m.detail}
                    </li>
                  ))}
                </ul>
              </details>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
