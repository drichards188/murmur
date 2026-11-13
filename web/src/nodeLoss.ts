// Rules 30 (docs/NODE-LOSS.md, bd murmur-zb1d.5): one of ours silent, presumed lost, confirmed
// lost, as the console shows it. The bridge's review carries `node_losses` (presumed lost by the
// engine on evidence, or silent and flagged by an operator) and `nodes_lost` (confirmed) only in a
// run deciding under rules 30; a run without them shows what it always did (the LOST badge of a
// stale link). Display only: a person confirms or revokes a loss at its Review card; nothing here
// decides, and no agent can.
//
// Pure (no React, no map): node runs nodeLoss.check.ts on it, as make check does.
import { sensorsText } from './engageSteps.ts'

/** A vehicle's standing: silent (its position stale), presumed lost (the engine's flag on
 *  evidence), lost (an operator with engagement authority confirmed it). */
export type NodeStanding = 'silent' | 'presumed_lost' | 'lost'

/** One item of evidence: the engine's (cited by the event it is in), or an operator's own. */
export interface NodeEvidence {
  kind: 'act' | 'struck' | 'damage' | 'spent' | 'operator'
  seq?: number | null
  /** bridge seconds */
  t: number
  detail: string
}

/** A shape of a silent vehicle's region as the engine counts it (stage 2,
 *  `murmur_decide::silent::Shape`): one point a disc, two a corridor, more a polygon, each
 *  padded by `pad_m`. */
export interface NodeShape {
  points: [number, number][]
  pad_m: number
}

/** Where a silent vehicle can be (docs/NODE-LOSS.md section 3): the union of its parts (each
 *  under a hypothesis: running its order, its failsafe fired, adrift, its top speed), capped by
 *  its reach. Null until the bridge serves it (the one place it is passed in:
 *  `decision/nodes.rs` `silent_region`). */
export interface NodeRegion {
  p0: [number, number]
  t0: number
  at: number
  span_s: number
  reach_mps: number
  reach: NodeShape
  parts: { hypothesis: 'order' | 'failsafe' | 'drift' | 'top_speed'; kind: string; shape: NodeShape; what: string }[]
}

/** Rules 30: one of ours presumed lost by the engine, or silent and flagged by hand (the
 *  review's `node_losses`): what an operator with engagement authority confirms or not. */
export interface ReviewNodeLoss {
  kind: 'node_loss'
  id: string
  uid: number
  callsign: string | null
  platform: string | null
  state: 'presumed_lost' | 'silent'
  /** the engine's milliseconds of its last position report: what a confirmation names (null:
   *  it never reported one) */
  since: number | null
  since_t: number | null
  silent_s: number | null
  last_position: [number, number] | null
  presumed_t: number | null
  evidence: NodeEvidence[]
  /** nothing presumes it lost: a confirmation says why (decision D2) */
  needs_note: boolean
  /** what confirming it would end now: the waiting orders voided, the running ones ended on it,
   *  its own lost-link values owed back closed */
  would_void: number[]
  would_end: number[]
  would_close: Record<string, number> | null
  /** why the engine would refuse a confirmation now (an engagement holding it, ...) */
  refused: string | null
  region: NodeRegion | null
  flagged?: { by: string; t: number; note: string }
  actions: ('confirm' | 'dismiss')[]
}

/** Rules 30: one of ours confirmed lost (the review's `nodes_lost`): any operator revokes it,
 *  saying why; a report from it revokes it on its own. */
export interface ReviewNodeLost {
  kind: 'node_lost'
  id: string
  uid: number
  callsign: string | null
  platform: string | null
  since: number | null
  since_t: number | null
  silent_s: number | null
  last_position: [number, number] | null
  by: string
  /** when it was confirmed, bridge seconds */
  t: number
  note: string | null
  /** the lost-link values its confirmation closed (owed back again if it is revoked) */
  closed: Record<string, number>
  /** a loss carried over from an earlier run holds only the vehicle it was confirmed on */
  holds: boolean
  from?: string
  not_held?: string
  region: NodeRegion | null
  actions: 'revoke'[]
}

/** The run's node standings, from its review: `on` only in a run deciding under rules 30. */
export interface NodeLossView {
  on: boolean
  /** presumed lost or flagged by hand, by uid */
  review: Map<number, ReviewNodeLoss>
  /** confirmed lost, by uid */
  lost: Map<number, ReviewNodeLost>
}

export const NO_NODE_LOSS: NodeLossView = { on: false, review: new Map(), lost: new Map() }

/** The standings a review carries (none before rules 30: the review has no node lists). */
export function nodeLossView(review: { node_losses?: ReviewNodeLoss[]; nodes_lost?: ReviewNodeLost[] } | undefined): NodeLossView {
  if (!review || (review.node_losses === undefined && review.nodes_lost === undefined)) return NO_NODE_LOSS
  return {
    on: true,
    review: new Map((review.node_losses ?? []).map((n) => [n.uid, n])),
    lost: new Map((review.nodes_lost ?? []).map((n) => [n.uid, n])),
  }
}

/** A vehicle's standing in `view`: lost, presumed lost, silent (its link stale: `stale`), or
 *  none (reporting, or a run before rules 30, which shows what it always did). */
export function standingOf(view: NodeLossView, uid: number, stale: boolean): NodeStanding | null {
  if (!view.on) return null
  if (view.lost.has(uid)) return 'lost'
  if (view.review.get(uid)?.state === 'presumed_lost') return 'presumed_lost'
  return stale ? 'silent' : null
}

export const STANDING_LABEL: Record<NodeStanding, string> = {
  silent: 'SILENT',
  presumed_lost: 'PRESUMED LOST',
  lost: 'LOST',
}

/** The badge under a vehicle whose link is stale: "LOST 12s" in a run before rules 30 (as it
 *  always was: the link is lost), else its standing ("SILENT 12s", "PRESUMED LOST 12s", and a
 *  confirmed loss "LOST", which has no clock: a person decided it). */
export function staleBadge(standing: NodeStanding | null, silentS: number): string {
  const s = `${Math.max(0, Math.floor(silentS))}s`
  if (standing === null) return `LOST ${s}`
  if (standing === 'lost') return 'LOST'
  return `${STANDING_LABEL[standing]} ${s}`
}

const EVIDENCE_LABEL: Record<NodeEvidence['kind'], string> = {
  act: 'Hostile act against it',
  struck: 'Its struck report',
  damage: 'Its damage report',
  spent: 'Spent by its own effect',
  operator: 'An operator flagged it',
}

/** The engine's words name our observing sensors as `<uid>:<sensor>` ("observed by 1:eo_ir,
 *  3:eo_ir"): each such list as a person names them ("HUNTER 1 EO/IR; HUNTER 2 EO/IR"), by
 *  `nameOf` (a uid to its callsign). The rest of the text as it is. */
export function namedSensors(text: string, nameOf: (uid: string) => string): string {
  return text.replace(/\b\d+:[a-z][a-z_]*(?:, \d+:[a-z][a-z_]*)*/g, (list) => sensorsText(list.split(', '), nameOf))
}

/** One item of evidence as a line: what, how long ago (`ago`: "2m 10s ago"), and the detail,
 *  its sensors by callsign when `nameOf` is given. */
export function evidenceLine(e: NodeEvidence, ago: string, nameOf?: (uid: string) => string): string {
  return `${EVIDENCE_LABEL[e.kind]}, ${ago}: ${nameOf ? namedSensors(e.detail, nameOf) : e.detail}`
}

const ids = (xs: number[]) => xs.map((i) => `i${i}`).join(', ')

/** What confirming a loss does, as the card says it before the person confirms (the spec's
 *  section 4, with what it would end now). */
export function confirmEffects(n: Pick<ReviewNodeLoss, 'would_void' | 'would_end' | 'would_close'>): string[] {
  const out = ['It leaves every engagement keep-out and the line of fire: a nomination it blocks is no longer blocked by it.']
  if (n.would_void.length) out.push(`Its waiting order${n.would_void.length === 1 ? ' is' : 's are'} voided: ${ids(n.would_void)}.`)
  if (n.would_end.length) out.push(`Its running order${n.would_end.length === 1 ? ' ends' : 's end'} on it: ${ids(n.would_end)}.`)
  out.push('It takes no order, not even a hold or an rtl, while it is lost.')
  const owed = Object.keys(n.would_close ?? {})
  if (owed.length) out.push(`The lost-link values owed back to it (${owed.join(', ')}) close: their restore stops.`)
  out.push('Undone at once if it reports again: it counts in every keep-out again, and the inbox says so.')
  return out
}

const M_PER_DEG = 111_320

type XY = [number, number]

/** The convex hull of `pts` (Andrew's monotone chain), counter-clockwise, no point repeated. */
function hull(pts: XY[]): XY[] {
  const p = [...pts].sort((a, b) => a[0] - b[0] || a[1] - b[1])
  if (p.length < 3) return p.filter((q, i) => i === 0 || q[0] !== p[i - 1][0] || q[1] !== p[i - 1][1])
  const cross = (o: XY, a: XY, b: XY) => (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
  const lower: XY[] = []
  for (const q of p) {
    while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], q) <= 0) lower.pop()
    lower.push(q)
  }
  const upper: XY[] = []
  for (const q of [...p].reverse()) {
    while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], q) <= 0) upper.pop()
    upper.push(q)
  }
  return [...lower.slice(0, -1), ...upper.slice(0, -1)]
}

/** The outline of `points` padded by `padM`, as the engine's shapes are (a convex outline and a
 *  pad around it, flat around the first point): one point a circle, two a corridor's capsule,
 *  more a hull with rounded corners. A closed ring of [lon, lat]; empty for no point. */
export function paddedRing(points: [number, number][], padM: number, steps = 64): [number, number][] {
  if (!points.length) return []
  const [lon0, lat0] = points[0]
  const kx = M_PER_DEG * Math.cos((lat0 * Math.PI) / 180)
  const h = hull(points.map(([lon, lat]) => [(lon - lon0) * kx, (lat - lat0) * M_PER_DEG] as XY))
  const pad = Math.max(0, padM)
  const ring: XY[] = []
  if (h.length === 1 || pad === 0) {
    if (h.length === 1) {
      for (let k = 0; k < steps; k++) {
        const a = (2 * Math.PI * k) / steps
        ring.push([h[0][0] + pad * Math.cos(a), h[0][1] + pad * Math.sin(a)])
      }
    } else ring.push(...h)
  } else {
    // each corner: an arc from the outward normal of the edge in to that of the edge out
    const normal = (a: XY, b: XY): number => Math.atan2(-(b[0] - a[0]), b[1] - a[1])
    for (let i = 0; i < h.length; i++) {
      const prev = h[(i + h.length - 1) % h.length]
      const here = h[i]
      const next = h[(i + 1) % h.length]
      const a0 = normal(prev, here)
      let sweep = normal(here, next) - a0
      while (sweep < 0) sweep += 2 * Math.PI
      const n = Math.max(1, Math.ceil((sweep / (2 * Math.PI)) * steps))
      for (let k = 0; k <= n; k++) {
        const a = a0 + (sweep * k) / n
        ring.push([here[0] + pad * Math.cos(a), here[1] + pad * Math.sin(a)])
      }
    }
  }
  const out = ring.map(([x, y]) => [x / kx + lon0, y / M_PER_DEG + lat0] as [number, number])
  if (out.length) out.push(out[0])
  return out
}

/** A region as the map draws it: each part's padded outline (under its hypothesis, with the
 *  engine's words for it), and the reach that caps them. Nothing without a region. */
export interface RegionRings {
  parts: { ring: [number, number][]; hypothesis: string; what: string }[]
  reach: [number, number][] | null
}

export function regionRings(r: NodeRegion | null | undefined): RegionRings {
  if (!r) return { parts: [], reach: null }
  return {
    parts: r.parts
      .map((p) => ({ ring: paddedRing(p.shape.points, p.shape.pad_m), hypothesis: p.hypothesis, what: p.what }))
      .filter((p) => p.ring.length > 0),
    reach: r.reach.points.length ? paddedRing(r.reach.points, r.reach.pad_m) : null,
  }
}

/** Where a region counts its vehicle, as the lines a person reads: one per distinct part, in the
 *  engine's own words (each says its hypothesis: "on its patrol (intent i2): ...", "if it lost us
 *  too, its rtl failsafe: ...", "adrift at up to ...", "anywhere its top speed ..."), in the
 *  engine's order, with the number of shapes on the map where it covers several (the engine
 *  widens each of its order's shapes by its failsafe, all under one description). */
export function regionLines(parts: NodeRegion['parts']): string[] {
  const n = new Map<string, number>()
  for (const p of parts) n.set(p.what, (n.get(p.what) ?? 0) + 1)
  return [...n].map(([what, k]) => (k > 1 ? `${what} (${k} shapes)` : what))
}
