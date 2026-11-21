import { memo, useMemo } from 'react'
import { tierLabel } from './decisions'
import { orderPhrase, routing } from './commandModel'
import { LazyDetails } from './LazyDetails'
import { sameFleet } from './stableFleet'
import { pct, REASON_WORDS, STATE_RGB, type VehicleLink } from './coverage'
import type { DryRunAnswer, DryRunState } from './useDryRun'
import type { Vehicle } from './types'
import { callsign } from './types'
import { counts, foldByRule, foldLine, foldTone, headline, layoutLine, memberClasses, type GroupPreview } from './groupOrders'
import type { Check } from './decisions'
import { RefusalFold } from './RefusalFold'
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

/** Rules 34: a group of more than this many has its predicted links folded. */
const LINK_FOLD = 6

/** Rules 34: a group's predicted links folded: how many hold, the worst failsafe chance, each
 *  vehicle whose link does not hold in full, the rest one click away. Predicted along the group's
 *  own task (the members' ways to their shares are not yet predicted one by one). */
function GroupLinks({ vehicles }: { vehicles: VehicleLink[] }) {
  const holds = vehicles.filter((v) => !v.error && v.holds)
  const not = vehicles.filter((v) => v.error || !v.holds)
  const worst = vehicles.reduce((w, v) => Math.max(w, v.p_failsafe ?? 0), 0)
  return (
    <>
      <span className="lk-sentence">
        {holds.length} of {vehicles.length} predicted to hold; the worst failsafe chance {pct(worst)}. Predicted along the group's task, not yet each member's way to its share.
      </span>
      {not.map((x) => (
        <VehicleLinkView key={x.uid} v={x} />
      ))}
      <LazyDetails className="grp-shares" summary={`Each vehicle's link (${vehicles.length})`}>
        {vehicles.map((x) => (
          <VehicleLinkView key={x.uid} v={x} />
        ))}
      </LazyDetails>
    </>
  )
}

/** Rules 34: a group order's dry run, bottom line first (docs/SWARMS.md section 10): how many it
 *  sends now, how many wait and join if heard in time, how many it leaves out and why, its
 *  strength against its minimum, then the layout (each member's slot and layer, drawn on the map),
 *  then the checks folded by rule. A hundred members read as a few lines. */
export function GroupDry({ g, checks, uids, kind, phrase, nameOf, vehicles }: { g: GroupPreview; checks: Check[]; uids: string[]; kind: string; phrase: string; nameOf: (uid: string) => string; vehicles: Vehicle[] }) {
  const lines = memberClasses(uids, checks)
  const c = counts(lines, g.spec, kind)
  const folds = foldByRule(checks)
  const byLayer = new Map<number, number>()
  const altOf = new Map<number, number>()
  for (const s of Object.values(g.layout.shares ?? {})) {
    byLayer.set(s.layer ?? 0, (byLayer.get(s.layer ?? 0) ?? 0) + 1)
    if (s.alt_m != null && !altOf.has(s.layer ?? 0)) altOf.set(s.layer ?? 0, s.alt_m)
  }
  const shares = Object.entries(g.layout.shares ?? {}).sort((a, b) => a[1].slot - b[1].slot)
  return (
    <div className="grp-dry" aria-label="Group order">
      <div className="dry-head">
        <h4>Group</h4>
        <span className="muted">{g.layout.spread ?? g.spec.spread}</span>
      </div>
      <p className={c.sent + c.waiting >= c.min ? 'pass' : 'fail'}>
        <b>{phrase}</b>: {headline(c, g.spec, kind, g.spec.swarm ?? null)}.
      </p>
      <RefusalFold checks={checks} uids={uids} vehicles={vehicles} />
      {g.layout.refused ? <p className="fail">The layout does not hold: {g.layout.refused}</p> : <p className="muted">{layoutLine(g.layout)}</p>}
      {(g.layout.warnings ?? []).map((w, k) => (
        <p key={k} className="warn">
          {w}
        </p>
      ))}
      {byLayer.size > 1 && (
        <p className="grp-layers">
          {[...byLayer.entries()]
            .sort((a, b) => a[0] - b[0])
            .map(([layer, n]) => (
              <span key={layer} className="grp-layer">
                layer {layer + 1}
                {altOf.has(layer) ? ` (${Math.round(altOf.get(layer)!)} m)` : ''}: {n}
              </span>
            ))}
        </p>
      )}
      <p className="muted grp-ghosts">On the map, before it is sent: each slot a ghost at its height (filled: sent now; dashed: joins if heard), a grey ✕ on each member left out (hover for why).</p>
      <ul className="grp-folds" aria-label="Checks by rule">
        {folds
          .filter((f) => f.rule !== 'strength')
          .map((f) => (
            <li key={f.rule} className={foldTone(f)}>
              <b>{f.rule}</b> {foldLine(f)}
            </li>
          ))}
      </ul>
      {shares.length > 0 && (
        <LazyDetails className="grp-shares" summary={`Each member's share (${shares.length})`}>
          <table>
            <thead>
              <tr>
                <th>member</th>
                <th>slot</th>
                <th>layer</th>
                <th>altitude</th>
              </tr>
            </thead>
            <tbody>
              {shares.map(([uid, s]) => (
                <tr key={uid}>
                  <td>{nameOf(uid)}</td>
                  <td>{s.slot + 1}</td>
                  <td>
                    {(s.layer ?? 0) + 1}
                  </td>
                  <td>{s.alt_m == null ? 'surface' : s.alt_m < 0 ? `${Math.round(-s.alt_m)} m deep` : `${Math.round(s.alt_m)} m`}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </LazyDetails>
      )}
    </div>
  )
}

function verdictOf(a: DryRunAnswer): { cls: string; text: string } {
  const r = routing(a, tierLabel)
  return { cls: r.tone, text: r.text }
}

/** The order form's dry run: what the checks and the policy say now, and the predicted link
 *  along each vehicle's route. Information only: nothing here blocks the order. */
function DryRunView({ st, vehicles }: { st: DryRunState; vehicles: Vehicle[] }) {
  const a = st.answer
  // one lookup for the names a hundred-member answer asks for (a find over the fleet each was
  // 100 x 3,600 comparisons)
  const byUid = useMemo(() => new Map(vehicles.map((v) => [String(v.uid), v])), [vehicles])
  if (!a && !st.loading && !st.error) return null
  const nameOf = (uid: string | null) => {
    if (uid == null) return null
    const v = byUid.get(uid)
    return v ? callsign(v) : `vehicle ${uid}`
  }
  // rules 34: a group order's failures are its members' (folded by rule below), and its own
  const failed = a ? a.checks.filter((c) => c.outcome === 'fail' && !a.group) : []
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
      {a?.group && <GroupDry g={a.group} checks={a.checks} uids={a.uids ?? []} kind={st.order?.task ?? ''} phrase={orderPhrase(st.order?.task ?? '', { radius: st.order?.radius, points: st.order?.route?.length })} nameOf={(u) => nameOf(u) ?? u} vehicles={vehicles} />}
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
          {a?.group && (link.vehicles?.length ?? 0) > LINK_FOLD ? (
            <GroupLinks vehicles={link.vehicles ?? []} />
          ) : (
            (link.vehicles ?? []).map((x) => <VehicleLinkView key={x.uid} v={x} />)
          )}
          <span className="lk-note">
            From the link emulator's geometry; its random loss and scripted events (jammers, cuts) are not in it. It never blocks the order.
            {link.sources?.terrain?.startsWith('not checked') && ' Terrain is not checked in this run.'}
          </span>
        </div>
      )}
    </div>
  )
}


/** Rendered again only when the dry run's answer changes (every few seconds), or the fleet does:
 *  the vehicle list is a new array on every feed update, but the dry run reads only names. */
export const DryRun = memo(DryRunView, (a, b) => a.st === b.st && sameFleet(a.vehicles, b.vehicles))
