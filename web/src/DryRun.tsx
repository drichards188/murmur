import { memo, useMemo } from 'react'
import { orderPhrase, ruleName } from './commandModel'
import { LazyDetails } from './LazyDetails'
import { sameFleet } from './stableFleet'
import { pct, type VehicleLink } from './coverage'
import type { DryRunState } from './useDryRun'
import type { Vehicle } from './types'
import { callsign } from './types'
import { counts, foldByRule, foldLine, foldTone, headline, layoutLine, memberClasses, type GroupPreview } from './groupOrders'
import type { Check } from './decisions'
import { RefusalFold } from './RefusalFold'
import './coverage.css'

/** One vehicle's predicted link: where it drops, in a sentence, and whether it holds; the chance
 *  its lost-link failsafe trips, behind it. */
function VehicleLinkView({ v }: { v: VehicleLink }) {
  const name = v.callsign ?? `vehicle ${v.uid}`
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
      <span className="lk-sentence">
        {v.sentence} <span className={`lk-chip ${v.holds ? 'holds' : 'fails'}`}>{v.holds ? 'holds' : 'does not hold'}</span>
      </span>
      {v.p_failsafe != null && (
        <details className="lk-fold">
          <summary>Failsafe chance</summary>
          {pct(v.p_failsafe)}: the chance its lost-link failsafe trips on the way, the transit then five minutes on the task path.
        </details>
      )}
    </div>
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
 *  strength against its minimum; a layout that does not hold and its warnings; then, folded, the
 *  layout (each member's slot and layer, drawn on the map) and the checks by rule. A hundred
 *  members read as a few lines. */
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
      <p className={c.sent + c.waiting >= c.min ? 'pass' : 'fail'}>
        <b>{phrase}</b>: {headline(c, g.spec, kind, g.spec.swarm ?? null)}.
      </p>
      <RefusalFold checks={checks} uids={uids} vehicles={vehicles} />
      {g.layout.refused && <p className="fail">The layout does not hold: {g.layout.refused}</p>}
      {(g.layout.warnings ?? []).map((w, k) => (
        <p key={k} className="warn">
          {w}
        </p>
      ))}
      <LazyDetails className="grp-shares" summary="Layout and checks">
        {!g.layout.refused && <p className="muted">{layoutLine(g.layout)}</p>}
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
        <ul className="grp-folds" aria-label="Checks by rule">
          {folds
            .filter((f) => f.rule !== 'strength')
            .map((f) => (
              <li key={f.rule} className={foldTone(f)}>
                <b>{f.rule}</b> {foldLine(f)}
              </li>
            ))}
        </ul>
      </LazyDetails>
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

/** The order form's dry run, only what blocks or bears on the order: the checks that would refuse
 *  it (in words), those that cannot be verified, the planner's detours, a group order's bottom line,
 *  and the predicted link along each vehicle's route. Whether it is checking and where it would go
 *  are the Send button's (TaskPanel.tsx). Information only: nothing here blocks the order. */
function DryRunView({ st, vehicles }: { st: DryRunState; vehicles: Vehicle[] }) {
  const a = st.answer
  // one lookup for the names a hundred-member answer asks for (a find over the fleet each was
  // 100 x 3,600 comparisons)
  const byUid = useMemo(() => new Map(vehicles.map((v) => [String(v.uid), v])), [vehicles])
  if (!a && !st.error) return null
  const nameOf = (uid: string | null) => {
    if (uid == null) return null
    const v = byUid.get(uid)
    return v ? callsign(v) : `vehicle ${uid}`
  }
  // rules 34: a group order's failures are its members' (folded by rule below), and its own
  const failed = a ? a.checks.filter((c) => c.outcome === 'fail' && !a.group) : []
  const unverified = a ? a.checks.filter((c) => c.outcome === 'unverified').length : 0
  const link = a?.link_coverage
  // rules 22: the legs the planner routed (one it could not route is a failing check above)
  const routed = Object.entries(a?.routes ?? {}).flatMap(([uid, legs]) => legs.filter((p) => !p.no_route).map((p) => ({ uid, p })))
  if (!st.error && !failed.length && !unverified && !a?.group && !routed.length && !link) return null
  return (
    <div className="dry" aria-label="Dry run" aria-live="polite">
      {st.error && <p className="fail">{st.error}</p>}
      {failed.length > 0 && (
        <ul>
          {failed.map((c, k) => (
            <li key={k} className="fail">
              <b>{ruleName(c.rule)}</b>
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
              Routed ({nameOf(uid)}): {p.summary}
            </li>
          ))}
        </ul>
      )}
      {link && (
        <div className="lk">
          <div className="lk-head">
            <h4>Link (predicted)</h4>
          </div>
          {link.error && <span className="muted">No prediction: {link.error}</span>}
          {a?.group && (link.vehicles?.length ?? 0) > LINK_FOLD ? (
            <GroupLinks vehicles={link.vehicles ?? []} />
          ) : (
            (link.vehicles ?? []).map((x) => <VehicleLinkView key={x.uid} v={x} />)
          )}
          {link.sources?.terrain?.startsWith('not checked') && <span className="lk-note">Terrain is not checked in this run.</span>}
        </div>
      )}
    </div>
  )
}


/** Rendered again only when the dry run's answer changes (every few seconds), or the fleet does:
 *  the vehicle list is a new array on every feed update, but the dry run reads only names. */
export const DryRun = memo(DryRunView, (a, b) => a.st === b.st && sameFleet(a.vehicles, b.vehicles))
