// The words an order is asked and decided in, one sentence per kind, said the same way on the
// approval card, the waiting rows at the head of every drawer, the Queue and the Decisions list
// (murmur-i9zw, ledger 2 section 3): "Send 10 PICKET vehicles to patrol a 4.2 km loop". Built from
// the order's kind, its parameters and the fleet, never from the proposer's text; a part with no
// data behind it is left out, never filled. Display only: nothing here decides or is logged.
// Pure: approvalText.check.ts.
import { distanceM } from './geo.ts'
import type { LngLat } from './types'

/** An order as the sentence needs it (decisions.ts `Intent`). */
export interface AskIntent {
  kind: string
  params: Record<string, unknown>
  uids: readonly string[]
  group?: { swarm?: string; spread?: string } | null
}

/** The fleet, as the sentence needs it: one of ours by its uid. */
export interface AskFleet {
  callsign(uid: string): string
  /** the model's short label ("PUMA"), or null when unknown */
  model(uid: string): string | null
  /** the swarm it belongs to, or null */
  swarm(uid: string): string | null
}

function km(m: number): string {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m / 10) * 10} m`
}

function loopM(r: readonly LngLat[]): number {
  let len = 0
  for (let k = 0; k < r.length; k++) len += distanceM(r[k], r[(k + 1) % r.length])
  return len
}

function pathM(r: readonly LngLat[]): number {
  let len = 0
  for (let k = 1; k < r.length; k++) len += distanceM(r[k - 1], r[k])
  return len
}

function commonOf(xs: (string | null)[]): string | null {
  const first = xs[0]
  return first && xs.every((x) => x === first) ? first : null
}

/** Who the order goes to: one or a few by callsign, more by their group and count. */
export function subjectOf(i: AskIntent, fleet: AskFleet): string {
  const n = i.uids.length
  if (n === 0) return 'no vehicle'
  if (n <= 3) return i.uids.map((u) => fleet.callsign(u)).join(', ')
  const swarm = i.group?.swarm ?? commonOf(i.uids.map((u) => fleet.swarm(u)))
  if (swarm) return `${n} ${swarm} vehicles`
  const model = commonOf(i.uids.map((u) => fleet.model(u)))
  return model ? `${n} ${model} vehicles` : `${n} vehicles`
}

/** What the order has them do, after "Send X": "to orbit, 2.0 km radius". Null for a kind that is
 *  not sent to vehicles (a collect, a coverage watch). */
function predicate(i: AskIntent, fleet: AskFleet): string | null {
  const p = i.params as { center?: LngLat; radius_m?: number; route?: LngLat[]; target?: string; track?: string }
  switch (i.kind) {
    case 'orbit':
      return p.radius_m ? `to orbit, ${km(p.radius_m)} radius` : 'to orbit'
    case 'patrol': {
      const r = p.route ?? []
      return r.length > 1 ? `to patrol a ${km(loopM(r))} loop of ${r.length} waypoints` : 'to patrol'
    }
    case 'transit': {
      const r = p.route ?? []
      return r.length > 1 ? `along a ${km(pathM(r))} route of ${r.length} points, to hold at its end` : 'to a point, to hold there'
    }
    case 'track':
      return p.target ? `to point at ${fleet.callsign(String(p.target))}` : 'to point at a vehicle'
    case 'rtl':
      return 'to return to launch'
    case 'hold':
      return 'to hold position'
    case 'identify':
      return p.track ? `to identify ${p.track}` : 'to identify a contact'
    default:
      return null
  }
}

/** The ask as one sentence: "Send 10 PICKET vehicles to patrol a 4.2 km loop of 6 waypoints". In
 *  the past tense ("Sent ...", for the Decisions list) when `past`. */
export function askSentence(i: AskIntent, fleet: AskFleet, past = false): string {
  if (i.kind === 'collect') return past ? 'Tasked a satellite collection' : 'Task a satellite collection'
  if (i.kind === 'cover') return past ? 'Asked for coverage over the area' : 'Ask for coverage over the area'
  if (i.kind === 'engage') {
    const track = typeof i.params.track === 'string' ? i.params.track : 'a contact'
    return `${past ? 'Engaged' : 'Engage'} ${track} with ${subjectOf(i, fleet)}`
  }
  if (i.kind === 'lay') {
    const count = typeof i.params.count === 'number' ? `${i.params.count} ` : ''
    const weapon = typeof i.params.weapon === 'string' ? i.params.weapon : 'mines'
    return `${past ? 'Laid' : 'Lay'} ${count}${weapon} with ${subjectOf(i, fleet)}`
  }
  const what = predicate(i, fleet)
  const verb = past ? 'Sent' : 'Send'
  const who = subjectOf(i, fleet)
  return what ? `${verb} ${who} ${what}` : `${verb} ${who}: ${i.kind}`
}

/** The place an order names, in coordinates for a fold (a place list would say "west of the bridge"). */
export function placeLine(i: AskIntent): string | null {
  const p = i.params as { center?: LngLat; route?: LngLat[] }
  const ll = (q: LngLat) => `${q[1].toFixed(4)}, ${q[0].toFixed(4)}`
  if (p.center) return `centre ${ll(p.center)}`
  if (p.route?.length) return `${p.route.length === 1 ? 'point' : 'route from'} ${ll(p.route[0])}${p.route.length > 1 ? ` to ${ll(p.route[p.route.length - 1])}` : ''}`
  return null
}

const SEND_LABEL: Record<string, string> = {
  orbit: 'Send the orbit',
  patrol: 'Send the patrol',
  transit: 'Send the transit',
  track: 'Send the pointing order',
  rtl: 'Send the return',
  hold: 'Send the hold',
  identify: 'Send the look',
  collect: 'Task the collection',
  cover: 'Ask for the coverage',
}

/** The approve button, named for what it does. */
export function sendLabel(kind: string): string {
  return SEND_LABEL[kind] ?? 'Send the order'
}

export const DONT_SEND = "Don't send"
export const NOTHING_SENT = 'Nothing is sent unless you approve.'
