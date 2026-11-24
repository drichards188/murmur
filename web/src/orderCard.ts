// A command log card's name and number (TaskPanel.tsx): the order a tasking carries out, named by
// what it is, numbered as the Inbox, the map and the Decisions list number it (the intent's `i<n>`),
// never by the task layer's own count. An engagement is flown as an orbit at its point by the task
// layer; its card says "ENGAGE T-0008 with REAPER 1", not "ORBIT #2". Display only. Pure:
// orderCard.check.ts.
import { cardLabel } from './commandModel.ts'

interface TaskingLike {
  id: number
  intent_id?: number
  task: string
}

interface OrderLike {
  id: number
  kind: string
  uids: readonly string[]
  /** an engagement's `track`, a lay order's `count` and `weapon` */
  params: Record<string, unknown>
  /** rules 34: a member order's group order */
  parent?: number
  group?: { swarm?: string } | null
}

export interface OrderCard {
  /** "ENGAGE T-0008 with REAPER 1", "PATROL", "ORBIT · North picket" */
  label: string
  /** the order's number: "i2" (a member's, its group order's) */
  id: string
  /** what the number is, and the task layer's own, for the title */
  idTitle: string
}

/** The card for tasking `t`, given its order (`intent`; absent once it left the intents the bridge
 *  sends) and that order's group order (`parent`). `name`: one of ours by uid. */
export function orderCard(t: TaskingLike, intent: OrderLike | undefined, parent: OrderLike | undefined, name: (uid: string) => string): OrderCard {
  const order = parent ?? intent
  const num = order?.id ?? t.intent_id
  const id = num != null ? `i${num}` : `#${t.id}`
  const idTitle =
    num == null
      ? `tasking #${t.id}`
      : parent && intent
        ? `order i${parent.id} (this vehicle's share is i${intent.id}); tasking #${t.id} in the task layer`
        : `order i${num}; tasking #${t.id} in the task layer`
  const group = intent?.group?.swarm ?? parent?.group?.swarm ?? null
  if (intent?.kind === 'engage') {
    const track = typeof intent.params.track === 'string' ? intent.params.track : 'its target'
    const who = intent.uids.length ? intent.uids.map(name).join(', ') : 'its platform'
    return { label: `ENGAGE ${track} with ${who}`, id, idTitle }
  }
  if (intent?.kind === 'lay') {
    const who = intent.uids.length ? intent.uids.map(name).join(', ') : 'its platform'
    const n = typeof intent.params.count === 'number' ? `${intent.params.count} ` : ''
    const w = typeof intent.params.weapon === 'string' ? intent.params.weapon : 'mines'
    return { label: `LAY ${n}${w} with ${who}`, id, idTitle }
  }
  return { label: cardLabel(t.task, group), id, idTitle }
}

/** The command log's cards, one per order: a group order's members each carry a tasking of their
 *  own (an intent each under the group order, rules 34), which made a card per member, twelve
 *  "TRANSIT · STRAIT i78" for one transit to a swarm. Members of one group order become one
 *  tasking, the lowest numbered member's, carrying every member's ops (the card then summarises
 *  them, as an order to many vehicles is). Any other tasking passes through, in order. */
export function oneCardPerOrder<T extends TaskingLike & { ops: readonly unknown[] }>(taskings: readonly T[], intentOf: (id: number) => { parent?: number } | undefined): T[] {
  const byParent = new Map<number, T>()
  const out: T[] = []
  for (const t of taskings) {
    const parent = t.intent_id != null ? intentOf(t.intent_id)?.parent : undefined
    if (parent == null) {
      out.push(t)
      continue
    }
    const seen = byParent.get(parent)
    if (!seen) {
      const card = { ...t, ops: [...t.ops] }
      byParent.set(parent, card)
      out.push(card)
    } else {
      ;(seen.ops as unknown[]).push(...t.ops)
      if (t.id < seen.id) Object.assign(seen, { id: t.id, intent_id: t.intent_id })
    }
  }
  return out
}
