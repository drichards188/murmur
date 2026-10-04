import { PathLayer, TextLayer } from '@deck.gl/layers'
import type { Intent } from './decisions'
import { fmtDuration } from './decisions'
import { leadingAt } from './estimate'
import { circle, distanceM } from './geo'
import type { ViewMode } from './MapView'
import type { LngLat, Tasking, Vehicle } from './types'
import { isStale } from './types'

/** Ordered versus confirmed on the map (rules 5, murmur-5p4), through MapView's `layers`
 *  prop:
 *
 *  - an order held in the queue (approved, its vehicle unreachable now): its path dashed in
 *    amber, "QUEUED" with why and how long it may still wait, and a QUEUED badge under the
 *    vehicle (under its dead-reckoning estimate while it is silent and has one);
 *  - an order sent but not yet confirmed by the vehicle's own telemetry (its op not active
 *    yet: waiting for a fix, sent, accepted): "ORDERED ... not yet confirmed" at its path,
 *    or at the vehicle for a mode (hold, rtl). The path itself is MapView's tasking line:
 *    dashed until the vehicle is seen executing it, solid after;
 *  - an order carried over a bridge restart that a vehicle had started, waiting for its
 *    vehicles to connect: "RESUMED ... waiting for the vehicle; followed, not sent again";
 *  - an order whose start went out before the link was lost, unconfirmed, and no longer in
 *    force (voided, queued, in the inbox): "MAY BE RUNNING", its tasking line kept.
 *
 *  Drawn at ground level in both views, over terrain. */

type RGBA = [number, number, number, number]
const QUEUED: RGBA = [242, 193, 78, 235]
const ORDERED: RGBA = [127, 184, 255, 235]
const MAYBE: RGBA = [255, 128, 96, 240]
const TEXT: RGBA = [235, 239, 244, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'

interface Dash {
  path: LngLat[]
  color: RGBA
}

interface Label {
  at: LngLat
  text: string
  color: RGBA
  /** under a vehicle symbol rather than beside a path */
  badge?: boolean
}

/** The intent's path: an orbit's ring, a patrol's closed route; none for a mode. */
function pathOf(i: Intent): LngLat[] | null {
  const p = i.params
  if (i.kind === 'orbit' && p.center) return circle(p.center, p.radius_m ?? 200, 72)
  if (i.kind === 'patrol' && p.route && p.route.length > 1) return [...p.route, p.route[0]]
  return null
}

/** A path cut into dashes (deck.gl's PathLayer draws solid lines; this keeps the console free
 *  of the extensions package): about 36 dashes along it, each half drawn. */
function dashes(path: LngLat[]): LngLat[][] {
  const seg: number[] = []
  let total = 0
  for (let k = 1; k < path.length; k++) {
    const d = distanceM(path[k - 1], path[k])
    seg.push(d)
    total += d
  }
  if (total <= 0) return []
  const step = total / 72
  const out: LngLat[][] = []
  let cur: LngLat[] = []
  let draw = true
  let left = step
  for (let k = 1; k < path.length; k++) {
    let a = path[k - 1]
    const b = path[k]
    let d = seg[k - 1]
    if (draw && cur.length === 0) cur.push(a)
    while (d > left) {
      const f = left / d
      const m: LngLat = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
      if (draw) {
        cur.push(m)
        out.push(cur)
        cur = []
      } else cur = [m]
      draw = !draw
      d -= left
      a = m
      left = step
    }
    left -= d
    if (draw) cur.push(b)
  }
  if (draw && cur.length > 1) out.push(cur)
  return out
}

/** Where a label for the intent goes: its orbit's center, its patrol's first point, or the
 *  first vehicle (a mode). */
function anchorOf(i: Intent, vehicles: Vehicle[]): LngLat | null {
  const p = i.params
  if (i.kind === 'orbit' && p.center) return p.center
  if (i.kind === 'patrol' && p.route?.length) return p.route[0]
  const v = vehicles.find((x) => i.uids.includes(String(x.uid)))
  return v?.position ?? null
}

/** Where a vehicle is drawn now: its estimate's leading position while it is silent and the
 *  bridge sent a motion model, else its last report. */
function shownAt(v: Vehicle, nowMs: number): LngLat | null {
  if (v.estimate && v.position && isStale(v)) return leadingAt(v.estimate, nowMs).at.position
  return v.position ?? null
}

/** An op state in words for a label (the op's own "queued" is a wait for a position fix,
 *  not the decision queue). */
const OP_WORDS: Record<string, string> = {
  queued: 'waiting for a fix',
  sent: 'sent',
  starting: 'start sent',
  acked: 'accepted',
}

function shortReason(i: Intent): string {
  const r = (i.reach_now ?? i.queue?.reach ?? []).find((x) => !x.reachable)
  if (!r) return 'reachable: re-checking'
  // "silent for 12.3 s (its link ...)" -> "silent for 12.3 s"
  return r.detail.replace(/ \(.*$/, '').replace(/:.*$/, '')
}

export interface OrderInput {
  /** the queue (inbox.queued) and the latest intents */
  queued: Intent[]
  intents: Intent[]
  taskings: Tasking[]
  vehicles: Vehicle[]
  nowMs: number
}

export function buildOrderLayers(input: OrderInput, _view: ViewMode) {
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const dashed: Dash[] = []
  const labels: Label[] = []
  const badged = new Set<string>()
  for (const i of input.queued) {
    const path = pathOf(i)
    if (path) for (const d of dashes(path)) dashed.push({ path: d, color: QUEUED })
    const left = i.queue ? fmtDuration((i.queue.expires_t - input.nowMs) / 1000) : '?'
    const at = anchorOf(i, input.vehicles)
    if (at) labels.push({ at, text: `QUEUED i${i.id} ${i.kind} · ${shortReason(i)} · ${left} left`, color: QUEUED })
    for (const u of i.uids) badged.add(u)
  }
  for (const v of input.vehicles) {
    const at = badged.has(String(v.uid)) ? shownAt(v, input.nowMs) : null
    if (at) labels.push({ at, text: 'QUEUED', color: QUEUED, badge: true })
  }
  // resumed under way after a bridge restart: followed once its vehicles connect
  for (const i of input.intents) {
    if (!i.follow_pending) continue
    const at = anchorOf(i, input.vehicles)
    if (at) labels.push({ at, text: `RESUMED i${i.id} ${i.kind} · waiting for the vehicle; followed, not sent again`, color: ORDERED })
  }
  // sent, not yet confirmed by telemetry: the tasking's ops still sent or acked
  const byId = new Map(input.taskings.map((t) => [t.id, t]))
  for (const i of input.intents) {
    if (i.state !== 'dispatched' || i.tasking_id == null) continue
    const t = byId.get(i.tasking_id)
    if (!t) continue
    const waiting = t.ops.filter((o) => ['queued', 'sent', 'starting', 'acked'].includes(o.state))
    if (waiting.length === 0) continue
    const at = anchorOf(i, input.vehicles)
    if (!at) continue
    const states = [...new Set(waiting.map((o) => OP_WORDS[o.state] ?? o.state))].join(', ')
    const lost = waiting.some((o) => o.detail.startsWith('link lost'))
    labels.push({
      at,
      text: `ORDERED i${i.id} ${i.kind} · not yet confirmed (${lost ? 'link lost, waiting' : states})`,
      color: ORDERED,
    })
  }
  // its start went out and nothing confirmed it, and its intent is no longer dispatched
  // (voided, back to the queue or the inbox): it may be running all the same
  for (const i of input.intents) {
    if (i.state === 'dispatched' || i.tasking_id == null) continue
    const t = byId.get(i.tasking_id)
    if (!t || !t.ops.some((o) => o.state === 'unconfirmed' && !o.detail.includes('does not run it now'))) continue
    const at = anchorOf(i, input.vehicles)
    if (at) labels.push({ at, text: `MAY BE RUNNING i${i.id} ${i.kind} · its start went out unconfirmed (${i.state})`, color: MAYBE })
  }
  const tick = Math.floor(input.nowMs / 1000)
  return [
    new PathLayer<Dash>({
      id: 'order-queued-paths',
      data: dashed,
      getPath: (d) => d.path,
      getColor: (d) => d.color,
      getWidth: 2.5,
      widthUnits: 'pixels',
      capRounded: false,
      parameters: onTop,
    }),
    new TextLayer<Label>({
      id: 'order-labels',
      data: labels,
      getPosition: (l) => l.at,
      getText: (l) => l.text,
      getSize: (l) => (l.badge ? 10 : 11),
      getColor: (l) => l.color ?? TEXT,
      getTextAnchor: (l) => (l.badge ? 'middle' : 'start'),
      getAlignmentBaseline: (l) => (l.badge ? 'top' : 'bottom'),
      getPixelOffset: (l) => (l.badge ? [0, 18] : [10, -24]),
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: { sdf: true, fontSize: 48, buffer: 6 },
      characterSet: 'auto',
      updateTriggers: { getText: tick },
      parameters: onTop,
    }),
  ]
}
