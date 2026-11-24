import type { ReactNode } from 'react'
import { holdingFor } from './blocking'
import { RANK } from './inboxOrder'
import { DeclarationItem, FlagItem, LostClaimItem, NodeLossItem, NodeLostItem, SettledFlagRow, type Props } from './ReviewSection'

/** Review items as Inbox entries (ledger 2 section 4; ReviewSection.tsx holds the cards). */
/** One Inbox item: the card, where it sorts (inboxOrder.ts) and by what within its rank. */
export interface ReviewEntry {
  key: string
  rank: number
  sort: number | null
  node: ReactNode
}

/** Every review item as an Inbox entry, in the order the engine sent them: a rule declaration or
 *  flag that alone holds up a nomination ranks first; one of ours presumed lost or silent ranks
 *  with ours in trouble; the others review; a settled flag, a loss already confirmed and a
 *  revocation notice go under For information. */
export function reviewEntries({ review, acts, now, onDone, onTrack, blocking, focus, nameOf, settled, tracks, vehicles }: Props, operator: string): ReviewEntry[] {
  const out: ReviewEntry[] = []
  const holding = new Set((blocking ?? []).map((b) => b.id))
  const focusN = (id: string) => (focus && focus.id === id ? focus.n : undefined)
  const allActs = acts ?? []
  const fleet = vehicles ?? []
  const trackById = new Map((tracks ?? []).map((t) => [t.id, t]))
  for (const d of review?.declarations ?? []) {
    out.push({
      key: d.id,
      rank: holding.has(d.id) ? RANK.blocking : RANK.review,
      sort: null,
      node: <DeclarationItem key={d.id} d={d} now={now} onDone={onDone} onTrack={onTrack} holding={holdingFor(blocking, d.id)} focusN={focusN(d.id)} nameOf={nameOf} track={trackById.get(d.track)} vehicles={fleet} acts={allActs} />,
    })
  }
  for (const f of review?.flags ?? []) {
    if (f.status.status !== 'open' && f.status.status !== 'confirmed') continue
    const why = settled?.get(f.id)
    if (why != null) {
      out.push({ key: f.id, rank: RANK.settled, sort: null, node: <SettledFlagRow key={f.id} f={f} why={why} now={now} onDone={onDone} onTrack={onTrack} focus={focus} nameOf={nameOf} acts={allActs} /> })
      continue
    }
    out.push({
      key: f.id,
      // a flag already confirmed is settled information; an open one asks
      rank: f.status.status === 'confirmed' ? RANK.settled : holding.has(f.id) ? RANK.blocking : RANK.review,
      sort: null,
      node: <FlagItem key={f.id} f={f} now={now} onDone={onDone} onTrack={onTrack} holding={holdingFor(blocking, f.id)} focusN={focusN(f.id)} nameOf={nameOf} acts={allActs} />,
    })
  }
  for (const c of review?.lost_claims ?? []) out.push({ key: c.id, rank: RANK.review, sort: null, node: <LostClaimItem key={c.id} c={c} onDone={onDone} onTrack={onTrack} /> })
  for (const x of review?.node_losses ?? []) out.push({ key: x.id, rank: RANK.ours, sort: null, node: <NodeLossItem key={x.id} n={x} now={now} onDone={onDone} focusN={focusN(x.id)} nameOf={nameOf} /> })
  for (const x of review?.nodes_lost ?? []) out.push({ key: x.id, rank: RANK.lost, sort: null, node: <NodeLostItem key={x.id} n={x} now={now} onDone={onDone} focusN={focusN(x.id)} /> })
  // a declarer's notice: their declaration revoked by someone else, said once
  for (const r of (review?.revocations ?? []).filter((x) => x.notify && x.notify === operator).slice(0, 5)) {
    out.push({
      key: `revoked-${r.id}-${r.t}`,
      rank: RANK.note,
      sort: null,
      node: (
        <p key={`${r.id}-${r.t}`} className="rv-notice" role="status">
          Your declaration of {r.track} was revoked by {r.revoked_by} {`${Math.max(0, Math.round(now() / 1000 - r.t))} s ago`}
          {r.note ? `: ${r.note}` : ''}
        </p>
      ),
    })
  }
  return out
}
