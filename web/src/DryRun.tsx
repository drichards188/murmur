import { tierLabel } from './decisions'
import { pct, REASON_WORDS, STATE_RGB, type VehicleLink } from './coverage'
import type { DryRunAnswer, DryRunState } from './useDryRun'
import type { Vehicle } from './types'
import { callsign } from './types'
import './coverage.css'

const LEG_WORDS: Record<string, string> = { transit: 'transit', task: 'task path', return: 'return', here: 'where it is' }

function km(m: number) {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m / 10) * 10} m`
}

function VehicleLinkView({ v }: { v: VehicleLink }) {
  const name = v.callsign ?? `uid ${v.uid}`
  if (v.error) {
    return (
      <div className="lk-veh">
        <span className="muted">
          {name}: no prediction ({v.error})
        </span>
      </div>
    )
  }
  return (
    <div className="lk-veh">
      <span className="lk-sentence">{v.sentence}</span>
      <div className="lk-chips">
        {v.p_failsafe != null && (
          <span className={`lk-chip ${v.holds ? 'holds' : 'fails'}`} title="The chance its lost-link failsafe trips on the way: the transit, then five minutes on the task path">
            failsafe {pct(v.p_failsafe)}
          </span>
        )}
        <span className={`lk-chip ${v.holds ? 'holds' : 'fails'}`}>{v.holds ? 'holds' : 'does not hold'}</span>
        <span className="lk-chip plain" title={v.active_profile && v.active_profile !== v.profile ? `carried on ${v.active_profile} there` : undefined}>
          {v.profile}
          {v.alt_m != null && ` at ${Math.round(v.alt_m)} m`}
          {v.alt_basis && ` (${v.alt_basis})`}
        </span>
      </div>
      {v.legs && v.legs.some((l) => l.length_m > 0) && (
        <div className="lk-legs">
          {v.legs
            .filter((l) => l.length_m > 0)
            .map((l, k) => (
              <LegBar key={k} label={`${LEG_WORDS[l.part] ?? l.part}${l.looped ? ' (lap)' : ''} ${km(l.length_m)}`} leg={l} ret={l.part === 'return'} />
            ))}
        </div>
      )}
    </div>
  )
}

function LegBar({ label, leg, ret }: { label: string; leg: NonNullable<VehicleLink['legs']>[number]; ret: boolean }) {
  return (
    <>
      <span>{label}</span>
      <span className={`lk-bar${ret ? ' ret' : ''}`}>
        {leg.stretches.map((s, k) => (
          <i
            key={k}
            style={{ flex: Math.max(0, s.to_m - s.from_m), background: `rgb(${STATE_RGB[s.state].join(',')})` }}
            title={`${s.state} ${km(s.from_m)} to ${km(s.to_m)} (about ${Math.round(s.seconds)} s)${s.reason ? `: ${REASON_WORDS[s.reason]}` : ''}, loss up to ${pct(s.max_p_loss)}`}
          />
        ))}
      </span>
    </>
  )
}

function verdictOf(a: DryRunAnswer): { cls: string; text: string } {
  if (!a.passed) return { cls: 'fail', text: 'The rules would refuse it:' }
  if (a.would_reach === 'approved' || a.submission_authorizes) {
    return { cls: 'wait', text: `Checks pass. Sending it is your approval${a.policy ? ` (${tierLabel(a.policy.rule)}: ${a.policy.reason})` : ''}.` }
  }
  if (a.would_reach === 'auto_approved') return { cls: 'pass', text: 'Checks pass. It would go at once (auto).' }
  if (a.would_reach === 'pending_approval') return { cls: 'wait', text: `Checks pass. It would wait for approval${a.policy ? `: ${a.policy.reason}` : ''}.` }
  return { cls: 'wait', text: `Checks pass; it would reach ${a.would_reach}.` }
}

/** The order form's dry run: what the checks and the policy say now, and the predicted link
 *  along each vehicle's route. Information only: nothing here blocks the order. */
export function DryRun({ st, vehicles }: { st: DryRunState; vehicles: Vehicle[] }) {
  const a = st.answer
  if (!a && !st.loading && !st.error) return null
  const nameOf = (uid: string | null) => {
    if (uid == null) return null
    const v = vehicles.find((x) => String(x.uid) === uid)
    return v ? callsign(v) : `vehicle ${uid}`
  }
  const failed = a ? a.checks.filter((c) => c.outcome === 'fail') : []
  const unverified = a ? a.checks.filter((c) => c.outcome === 'unverified').length : 0
  const v = a ? verdictOf(a) : null
  const link = a?.link_coverage
  // rules 22: the legs the planner routed (one it could not route is a failing check above)
  const routed = Object.entries(a?.routes ?? {}).flatMap(([uid, legs]) => legs.filter((p) => !p.no_route).map((p) => ({ uid, p })))
  return (
    <div className="dry" aria-label="Dry run" aria-live="polite">
      <div className="dry-head">
        <h4>Dry run</h4>
        <span className="muted">{st.loading ? 'checking…' : 'nothing sent'}</span>
      </div>
      {st.error && <p className="fail">{st.error}</p>}
      {v && <p className={v.cls}>{v.text}</p>}
      {failed.length > 0 && (
        <ul>
          {failed.map((c, k) => (
            <li key={k} className="fail">
              <b>{c.rule}</b>
              {c.uid != null && <> ({nameOf(c.uid)})</>}: {c.detail}
            </li>
          ))}
        </ul>
      )}
      {a && unverified > 0 && <p className="muted">{unverified} check{unverified > 1 ? 's' : ''} could not be verified.</p>}
      {routed.length > 0 && (
        <ul aria-label="Routed">
          {routed.map(({ uid, p }, k) => (
            <li key={k}>
              <b>routed</b> ({nameOf(uid)}): {p.summary}
            </li>
          ))}
        </ul>
      )}
      {link && (
        <div className="lk">
          <div className="lk-head">
            <h4>Link</h4>
            <span className="lk-pred">predicted</span>
            <span className="muted">information only</span>
          </div>
          {link.error && <span className="muted">No prediction: {link.error}</span>}
          {(link.vehicles ?? []).map((x) => (
            <VehicleLinkView key={x.uid} v={x} />
          ))}
          <span className="lk-note">
            From the link emulator's geometry; its random loss and scripted events (jammers, cuts) are not in it. It never blocks the order.
            {link.sources?.terrain?.startsWith('not checked') && ' Terrain is not checked in this run.'}
          </span>
        </div>
      )}
    </div>
  )
}

