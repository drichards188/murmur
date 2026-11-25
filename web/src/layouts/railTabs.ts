// The rail layout's pure part (Rail.tsx, `?layout=rail`, bd epic murmur-iobd). The rail shows one
// list at a time (Fleet, Tracks, Command), so each tab says on its face what its list holds and,
// when something in it wants a look, how many and how badly: ours in trouble (R2), hostile or
// suspect contacts (R6), orders that failed since the person last read the log (R12). This file
// works out those faces, which tab a request to show a row brings forward, what a click or a key on
// the tabs does, and the rail's state as this browser remembers it. Display only: it decides
// nothing and asks the bridge nothing. Pure: railTabs.check.ts runs it under node.
import { oneCardPerOrder } from '../orderCard.ts'
import { opWord, orderOver } from '../orderWords.ts'
import type { Glance, Trouble } from '../topbar'
import type { Tasking } from '../types'

export type RailTab = 'fleet' | 'tracks' | 'command'

/** The tabs, top to bottom. */
export const RAIL_TABS: readonly RailTab[] = ['fleet', 'tracks', 'command']

export const TAB_LABEL: Readonly<Record<RailTab, string>> = { fleet: 'Fleet', tracks: 'Tracks', command: 'Command' }

/** A tab's mark: how many in its list want a look, in the tone of the worst of them (red only for
 *  what is urgent), and the words its hover reads. */
export interface TabMark {
  n: number
  tone: 'bad' | 'warn' | 'settled'
  says: string
}

/** What a tab shows on its face. */
export interface TabFace {
  id: RailTab
  label: string
  /** the count under the label; null while it is not known */
  count: number | null
  /** what the count counts, for the hover */
  counts: string
  mark: TabMark | null
}

/** The glance's trouble line without its last sentence, which is about the glance's own button. */
function troubleWords(says: string): string {
  return says.replace(/\s*A click shows[^.]*\.\s*$/, '')
}

/** Fleet: ours, and ours in trouble by the rows' own exception slot (topbar.ts `troubleOf`). */
export function fleetFace(glance: Pick<Glance, 'ours'> | null | undefined, trouble: Pick<Trouble, 'n' | 'tone' | 'says'> | null | undefined): TabFace {
  const ours = glance?.ours ?? null
  const mark = trouble && trouble.n > 0 ? { n: trouble.n, tone: trouble.tone ?? 'warn', says: troubleWords(trouble.says) } : null
  return { id: 'fleet', label: TAB_LABEL.fleet, count: ours, counts: ours == null ? 'Ours' : `${ours} of ours`, mark }
}

/** Tracks: the contacts our sensors hold, marked by the hostile (red) or else the suspect (amber). */
export function tracksFace(glance: Pick<Glance, 'contacts' | 'hostile' | 'suspect'> | null | undefined): TabFace {
  const n = glance?.contacts ?? null
  const hostile = glance?.hostile ?? 0
  const suspect = glance?.suspect ?? 0
  const both = [hostile ? `${hostile} hostile` : '', suspect ? `${suspect} suspect` : ''].filter(Boolean).join(', ')
  const mark: TabMark | null = hostile ? { n: hostile, tone: 'bad', says: both } : suspect ? { n: suspect, tone: 'warn', says: both } : null
  return { id: 'tracks', label: TAB_LABEL.tracks, count: n, counts: n == null ? 'Contacts' : `${n} contact${n === 1 ? '' : 's'} our sensors hold`, mark }
}

/** The command log's orders, as the log lists them: under way, and each failure of one of them. */
export interface OrderTally {
  /** orders with a vehicle still starting it, running it, or waiting for a fix to start it */
  running: number
  /** each vehicle's failed start of an order, by a key that stays the same while the log shows it;
   *  `order`: the log card it is on */
  failures: { key: string; order: number }[]
}

/** The words of an op that is under way (orderWords.ts `opWord`). */
const UNDER_WAY = new Set(['waiting for a fix', 'starting', 'running'])

/** The log's orders (TaskPanel.tsx: an identify order's newest tasking, a group order's members as
 *  one card), each under way or not, and their failed starts. An engagement whose own state says it
 *  is over is not under way, though its orbit runs on. */
export function orderTally(
  taskings: readonly Pick<Tasking, 'id' | 'intent_id' | 'task' | 'params' | 'ops'>[],
  intents: readonly { id: number; kind?: string; state?: string; parent?: number }[] | null,
): OrderTally {
  const byId = new Map((intents ?? []).map((i) => [i.id, i]))
  // an identify order's taskings (its orbit moved with its track): the newest only
  const newest = new Map<number, number>()
  for (const t of taskings) if (t.intent_id != null && t.params.look) newest.set(t.intent_id, Math.max(newest.get(t.intent_id) ?? t.id, t.id))
  for (const t of taskings) if (t.intent_id != null && newest.has(t.intent_id)) newest.set(t.intent_id, Math.max(newest.get(t.intent_id)!, t.id))
  const shown = taskings.filter((t) => t.intent_id == null || !newest.has(t.intent_id) || newest.get(t.intent_id) === t.id)
  const cards = oneCardPerOrder(shown, (id) => byId.get(id))
  let running = 0
  const failures: OrderTally['failures'] = []
  for (const c of cards) {
    const i = c.intent_id != null ? byId.get(c.intent_id) : undefined
    const over = orderOver(i?.kind, i?.state) != null
    let under = false
    for (const o of c.ops) {
      const w = opWord(o, c.task)
      if (!over && UNDER_WAY.has(w)) under = true
      if (w === 'failed' || w === 'not sent') failures.push({ key: `${o.tasking}/${o.id}`, order: c.id })
    }
    if (under) running++
  }
  return { running, failures }
}

/** The failures the person has not seen (a failure is seen once the log was in front of them after
 *  it), counted by order. */
export function unseenOrders(failures: OrderTally['failures'], seen: ReadonlySet<string>): number {
  return new Set(failures.filter((f) => !seen.has(f.key)).map((f) => f.order)).size
}

/** Command: the orders under way, marked by the orders that failed since the log was last read
 *  (amber: a failure waits for a look, it is not an alarm). */
export function commandFace(tally: OrderTally | null, unseen: number): TabFace {
  const n = tally?.running ?? null
  const mark: TabMark | null = unseen > 0 ? { n: unseen, tone: 'warn', says: `${unseen} order${unseen === 1 ? '' : 's'} failed since you last read the command log` } : null
  return { id: 'command', label: TAB_LABEL.command, count: n, counts: n == null ? 'Orders' : `${n} order${n === 1 ? '' : 's'} under way`, mark }
}

/** The rail as the person left it: the tab in front, whether the rail's lists show (else only the
 *  tabs), whether Needs you shows (else only its strip). */
export interface RailState {
  tab: RailTab
  open: boolean
  needs: boolean
}

/** The first time: the lists folded to their tabs (the map takes their room; a tab's count and
 *  mark say what is in each), Needs you shown. */
export const RAIL_DEFAULT: RailState = { tab: 'fleet', open: false, needs: true }

/** This browser's remembered state (localStorage), each part read on its own: what is missing or
 *  not understood is the default's. */
export function parseRailState(raw: string | null | undefined): RailState {
  let v: unknown = null
  try {
    v = raw ? JSON.parse(raw) : null
  } catch {
    v = null
  }
  const o = v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {}
  const tab = RAIL_TABS.find((t) => t === o.tab) ?? RAIL_DEFAULT.tab
  const open = typeof o.open === 'boolean' ? o.open : RAIL_DEFAULT.open
  const needs = typeof o.needs === 'boolean' ? o.needs : RAIL_DEFAULT.needs
  return { tab, open, needs }
}

export function railStateText(s: RailState): string {
  return JSON.stringify({ tab: s.tab, open: s.open, needs: s.needs })
}

/** A click on a tab: with the lists hidden, shows them on that tab; on the tab in front, hides
 *  them (the activity bar's way: the tabs stay); on another, brings it forward. */
export function clickTab(s: RailState, id: RailTab): RailState {
  if (!s.open) return { ...s, tab: id, open: true }
  if (s.tab === id) return { ...s, open: false }
  return { ...s, tab: id }
}

/** The tab an arrow key on the tabs moves to (Up and Down step, wrapping; Home, End). Null for any
 *  other key. */
export function tabAfterKey(tab: RailTab, key: string): RailTab | null {
  const k = RAIL_TABS.indexOf(tab)
  const n = RAIL_TABS.length
  if (key === 'ArrowDown') return RAIL_TABS[(k + 1) % n]
  if (key === 'ArrowUp') return RAIL_TABS[(k + n - 1) % n]
  if (key === 'Home') return RAIL_TABS[0]
  if (key === 'End') return RAIL_TABS[n - 1]
  return null
}

/** A request to show rows in a list (reveal.ts): a track's brings Tracks forward and shows the lists,
 *  since its card is read there (R7); one of ours brings Fleet forward unless the order form is in
 *  front (the person may be picking vehicles for it on the map), and never opens hidden lists (the
 *  map shows the vehicle selected). */
export function revealed(s: RailState, list: 'fleet' | 'tracks'): RailState {
  if (list === 'tracks') return s.tab === 'tracks' && s.open ? s : { ...s, tab: 'tracks', open: true }
  if (s.tab === 'command' || s.tab === 'fleet') return s
  return { ...s, tab: 'fleet' }
}

/** Whether to offer the way to the order form: vehicles are selected and the form is out of sight. */
export function offerOrder(s: RailState, selected: number): boolean {
  return selected > 0 && (!s.open || s.tab !== 'command')
}
