// What one of ours is doing, in the words of its order (R3, ledger 1 "Needs a new piece" 1): the
// fleet row and the vehicle details say "patrolling", "en route to its orbit, 2.1 km", "holding",
// "returning home", "striking T-0008", not the autopilot's mode ("LOITER", "AUTO"). Each vehicle is
// joined to its latest order (the newest tasking that names it, its op's state, the intent it
// carries out and that intent's group order, as the command log's card names it: orderCard.ts);
// until it has one, it is moving or idle as `vehicleState` says, and returning home by its mode
// whatever its order (a failsafe's RTL is not the order). The command log's one state word for an
// op is here too (`opWord`), so the log, the row and the details say one thing.
//
// Display only: nothing here decides or is logged. Pure (no browser): orderWords.check.ts.
import { distanceM } from './geo.ts'
import { isReturning } from './exceptions.ts'
import type { LngLat, Op, OpState, Tasking, Vehicle } from './types'

/** An intent as the order's words need it (decisions.ts `Intent`). */
export interface OrderIntent {
  id: number
  kind: string
  params: Record<string, unknown>
  uids: readonly string[]
  parent?: number
  group?: { swarm?: string } | null
  /** the order's own state (an engagement's ends after its effect: effected, then done) */
  state?: string
}

/** A vehicle's latest order: its tasking, its own op in it, the intent and its group order. */
export interface OrderRef {
  tasking: Pick<Tasking, 'id' | 'intent_id' | 'task' | 'params'>
  op: Pick<Op, 'state' | 'detail'> & Partial<Pick<Op, 'progress'>>
  intent?: OrderIntent
  parent?: OrderIntent
}

/** Each vehicle's latest order: of the newest tasking that names it (as `latestOps`). */
export function latestOrders(taskings: readonly Pick<Tasking, 'id' | 'intent_id' | 'task' | 'params' | 'ops'>[], intents: ReadonlyMap<number, OrderIntent>): Map<number, OrderRef> {
  const out = new Map<number, OrderRef>()
  for (const t of [...taskings].sort((a, b) => a.id - b.id)) {
    const intent = t.intent_id != null ? intents.get(t.intent_id) : undefined
    const parent = intent?.parent != null ? intents.get(intent.parent) : undefined
    for (const o of t.ops) out.set(o.uid, { tasking: t, op: o, intent, parent })
  }
  return out
}

function km(m: number): string {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m / 10) * 10} m`
}

/** Within this of its orbit's ring, or of its transit's end, it is there. */
const THERE_M = 100

const NOUN: Record<string, string> = { orbit: 'orbit', patrol: 'patrol', transit: 'transit', hold: 'hold', rtl: 'return home', track: 'tracking', lay: 'mine laying', identify: 'look' }

type V = Pick<Vehicle, 'mode' | 'groundspeed' | 'position'>

/** Moving or idle by its speed (`vehicleState` without an order), returning by its mode. */
function unordered(v: V): string {
  if (v.mode && isReturning(v.mode)) return 'returning home'
  return (v.groundspeed ?? 0) >= 1 ? 'moving' : 'idle'
}

/** The order's words while it runs (its op active or done). */
function running(v: V, o: OrderRef, name: (uid: number) => string): string {
  const t = o.tasking
  const kind = o.intent?.kind
  const away = (to: LngLat | undefined, less = 0) => (to && v.position ? Math.max(0, distanceM(v.position, to) - less) : null)
  const center = t.params.center
  const radius = t.params.radius ?? 0
  // an engagement is flown as an orbit at its point; an identify as an orbit about its track
  if (kind === 'engage') {
    const track = typeof o.intent?.params.track === 'string' ? o.intent.params.track : 'its target'
    // released and then assessed: the strike is over, the platform waits on station
    if (o.intent?.state === 'effected') return `on station, the strike on ${track} being assessed`
    if (o.intent?.state === 'done' || o.intent?.state === 'failed' || o.intent?.state === 'expired') return `on station after the strike on ${track}`
    const d = o.op.state === 'active' ? away(center, radius) : null
    return d != null && d > THERE_M ? `en route to strike ${track}, ${km(d)}` : `striking ${track}`
  }
  if (t.params.look) {
    const d = o.op.state === 'active' ? away(center, radius) : null
    return d != null && d > THERE_M ? `en route to identify ${t.params.look}, ${km(d)}` : `identifying ${t.params.look}`
  }
  if (kind === 'lay' || (t.task as string) === 'lay') return o.op.state === 'done' ? 'mines laid' : 'laying mines'
  switch (t.task) {
    case 'orbit': {
      const d = o.op.state === 'active' ? away(center, radius) : null
      return d != null && d > THERE_M ? `en route to its orbit, ${km(d)}` : 'orbiting'
    }
    case 'patrol':
      return 'patrolling'
    case 'transit': {
      const route = t.params.route ?? []
      const d = o.op.state === 'active' ? away(route[route.length - 1]) : null
      return o.op.state === 'done' || (d != null && d <= THERE_M) ? 'holding at the end of its route' : d != null ? `en route, ${km(d)} to go` : 'en route'
    }
    case 'hold':
      return 'holding'
    case 'rtl':
      return o.op.state === 'done' ? 'home' : 'returning home'
    case 'track':
      return t.params.target != null ? `pointing at ${name(t.params.target)}` : 'tracking'
    default:
      return String(t.task)
  }
}

/** What it is doing, in words. `name`: one of ours by its uid (its callsign). */
export function doingWords(v: V, o: OrderRef | undefined, name: (uid: number) => string): string {
  if (!o) return unordered(v)
  const task = o.tasking.params.look ? 'identify' : o.intent?.kind === 'lay' ? 'lay' : o.tasking.task
  // an engagement is flown as an orbit: its words name the strike, not the orbit
  const target = o.intent?.kind === 'engage' && typeof o.intent.params.track === 'string' ? o.intent.params.track : null
  const noun = target ? `strike on ${target}` : (NOUN[task] ?? task)
  // a mode that takes it home overrides an order that does not (its failsafe, someone's RTL)
  if (v.mode && isReturning(v.mode) && o.tasking.task !== 'rtl') return 'returning home'
  const group = o.intent?.group?.swarm ?? o.parent?.group?.swarm ?? null
  const withGroup = (s: string) => (group ? `${s} · ${group}` : s)
  switch (o.op.state as OpState) {
    case 'queued':
      return withGroup(`waiting for a position fix to start its ${noun}`)
    case 'sent':
    case 'starting':
    case 'acked':
      return withGroup(`starting its ${noun}`)
    case 'active':
    case 'done':
      return withGroup(running(v, o, name))
    case 'requeued':
      return `${unordered(v)}; its ${noun} back in the queue`
    case 'unconfirmed':
      return opWord(o.op, o.tasking.task) === 'not running' ? unordered(v) : withGroup(`may be running its ${noun}`)
    default:
      // failed or replaced: the order no longer says what it does
      return unordered(v)
  }
}

/** What the row and the details say it is doing, given what is wrong with it: one that cannot
 *  move is stopped whatever its order says (its slot says why: "DAMAGED: CANNOT MOVE"), and one
 *  not heard from is said as last heard, since nothing says it still does it. */
export function doingNow(v: V, o: OrderRef | undefined, name: (uid: number) => string, trouble: { cannotMove?: boolean; silent?: boolean } = {}): string {
  if (trouble.cannotMove) return 'stopped'
  const words = doingWords(v, o, name)
  return trouble.silent ? `last heard ${words}` : words
}

/** Tasks that go on once their op is done (an orbit is done once established, a patrol after its
 *  first lap, a hold at once): still running. */
const GOES_ON = new Set(['orbit', 'patrol', 'hold', 'track'])

/** An op's state in one plain word or two (the command log's, in place of the task layer's
 *  SENT STARTING ACKED ACTIVE DONE): "starting", "running", "done", "failed"... A failure's why
 *  is the caller's to add. */
export function opWord(o: Pick<Op, 'state' | 'detail'>, task: string): string {
  switch (o.state as OpState) {
    case 'queued':
      return 'waiting for a fix'
    case 'sent':
    case 'starting':
    case 'acked':
      return 'starting'
    case 'active':
      return 'running'
    case 'done':
      return GOES_ON.has(task) ? 'running' : 'done'
    case 'requeued':
      return 'back to the queue'
    case 'superseded':
      return 'replaced'
    case 'unconfirmed':
      return o.detail.includes('does not run it now') || o.detail.includes('is not ') ? 'not running' : 'may be running'
    case 'failed':
      return o.detail.startsWith('not sent before the bridge restarted') ? 'not sent' : 'failed'
    default:
      return o.state
  }
}

/** The word for an order whose own state says it is over though its vehicle task goes on (an
 *  engagement is flown as an orbit, which "runs" on station after the release): "released",
 *  then "over". Null for any other order or state. */
export function orderOver(kind: string | undefined, state: string | undefined): string | null {
  if (kind !== 'engage') return null
  return state === 'effected' ? 'released' : state === 'done' ? 'over' : null
}

/** Whether `opWord` is an ending that wants its why beside it. */
export function opWordIsBad(word: string): boolean {
  return word === 'failed' || word === 'not sent' || word === 'back to the queue' || word === 'may be running' || word === 'not running'
}
