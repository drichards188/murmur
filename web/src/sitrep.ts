// The situation report, the pure part: the whole picture assembled top-down
// from the feeds the console already has (vehicles, swarms, the operator's groups, tracks, the
// inbox, the situation panel's status), summarised, then expanded on demand. Read-only and display
// only: it decides nothing, sends nothing and re-derives nothing the bridge already says (what
// holds up a person is the inbox's `blocking`, read as it is). Built from the ~1 Hz feeds, never per
// frame. Checked by sitrep.check.ts.
import type { OperatorGroup } from './groups.ts'
import { STATE_ORDER, STATE_WORDS, vehicleState, type LatestOp, type VehicleState } from './exceptions.ts'
import { swarmTone, taskLine, type GroupMark, type Swarm } from './swarmsModel.ts'
import type { Track } from './tracks.ts'
import { callsign, isStale, staleMs, type Vehicle } from './types.ts'

/** Below this a vehicle counts as low on battery when the swarm summary does not say (it does:
 *  `low_battery_below_pct`). */
export const LOW_BATTERY_PCT = 25

// ---- the header: the bottom line ------------------------------------------------------------

export interface Headline {
  /** the scenario's title and phase, when a situation runs */
  scenario: string | null
  up: number
  total: number
  lost: number
  contacts: number
  hostile: number
  suspect: number
  /** what waits for a person: pending approvals, review items, queued orders */
  waiting: number
  /** the single most urgent thing, in a line: the bridge's own blocking item, else the oldest waiting */
  urgent: string | null
}

export interface HeadlineInput {
  scenario: { title?: string; phase?: string } | null
  vehicles: readonly Pick<Vehicle, 'link'>[]
  tracks: readonly Pick<Track, 'affiliation' | 'declared' | 'state'>[]
  counts: { pending?: number; review?: number; queued?: number } | null | undefined
  blocking: readonly { text: string }[] | null | undefined
  /** the review's oldest waiting item, as the waiting strip words it */
  oldest?: string | null
}

export function headline(i: HeadlineInput): Headline {
  let lost = 0
  for (const v of i.vehicles) if (isStale(v)) lost++
  let hostile = 0
  let suspect = 0
  for (const t of i.tracks) {
    const a = t.declared?.affiliation ?? t.affiliation
    if (a === 'hostile') hostile++
    else if (a === 'suspect') suspect++
  }
  const c = i.counts ?? {}
  const phase = [i.scenario?.title, i.scenario?.phase].filter(Boolean).join(': ')
  return {
    scenario: phase || null,
    up: i.vehicles.length - lost,
    total: i.vehicles.length,
    lost,
    contacts: i.tracks.length,
    hostile,
    suspect,
    waiting: (c.pending ?? 0) + (c.review ?? 0) + (c.queued ?? 0),
    urgent: i.blocking?.[0]?.text ?? i.oldest ?? null,
  }
}

// ---- by group and swarm ---------------------------------------------------------------------

export interface GroupRow {
  key: string
  /** a fleet swarm (fixed by the fleet file), one of the operator's own groups, or one the AI watch
   *  officer made for a tasking request (groups.ts `byOfficer`) */
  kind: 'swarm' | 'yours' | 'officer'
  name: string
  color: string | null
  total: number
  counts: Record<VehicleState, number>
  lowBattery: number
  /** the order it runs, in a line ("orbit i12 (95)"; swarms only) */
  task: string
  tone: 'ok' | 'warn' | 'bad'
  /** the uids, for the drill-down */
  members: number[]
}

export interface SitrepContext {
  latest: ReadonlyMap<number, LatestOp>
  damaged: ReadonlySet<number>
  lowBelowPct?: number
}

const emptyCounts = () => Object.fromEntries(STATE_ORDER.map((s) => [s, 0])) as Record<VehicleState, number>

/** How many of `members` are in each state, and how many are low on battery. */
export function tally(members: readonly number[], by: ReadonlyMap<number, Vehicle>, ctx: SitrepContext): { counts: Record<VehicleState, number>; lowBattery: number; here: number } {
  const counts = emptyCounts()
  let lowBattery = 0
  let here = 0
  const low = ctx.lowBelowPct ?? LOW_BATTERY_PCT
  for (const u of members) {
    const v = by.get(u)
    if (!v) continue
    here++
    counts[vehicleState(v, ctx.latest, ctx.damaged)]++
    if (v.battery_pct != null && v.battery_pct < low) lowBattery++
  }
  return { counts, lowBattery, here }
}

/** A group's tone from its own counts: a tenth or more unheard or any damaged is bad, any unheard
 *  or low on battery warn (as a swarm's, swarmTone). */
function toneOf(total: number, counts: Record<VehicleState, number>, low: number): 'ok' | 'warn' | 'bad' {
  if (counts.damaged > 0 || (counts.lost > 0 && counts.lost * 10 >= total)) return 'bad'
  return counts.lost > 0 || low > 0 ? 'warn' : 'ok'
}

const worse = (a: 'ok' | 'warn' | 'bad', b: 'ok' | 'warn' | 'bad') => (a === 'bad' || b === 'bad' ? 'bad' : a === 'warn' || b === 'warn' ? 'warn' : 'ok')

/** The operator's groups, then the fleet's swarms, one row each: strength by state, the order,
 *  the trouble first within each kind. A swarm under a group order in force reads as its chip and
 *  the group strip do (swarmsModel.ts `groupMarks`, `markOf`): the order and its strength against
 *  its minimum ("orbit i12: 41/60 · under min 45"), its tone with theirs. */
export function groupRows(
  swarms: readonly Swarm[],
  groups: readonly OperatorGroup[],
  by: ReadonlyMap<number, Vehicle>,
  ctx: SitrepContext,
  looks: (kind: 'swarm' | 'yours' | 'officer', index: number) => string | null,
  markOf: (id: string) => Pick<GroupMark, 'doing' | 'parts' | 'tone' | 'intent'> | undefined = () => undefined,
): GroupRow[] {
  const rank = { bad: 0, warn: 1, ok: 2 }
  const rows: GroupRow[] = []
  swarms.forEach((s, i) => {
    const t = tally(s.members, by, ctx)
    // the bridge's own tone where it has one (it counts its own lost link and reserve)
    let tone = swarmTone(s) !== 'ok' ? swarmTone(s) : toneOf(s.count, t.counts, t.lowBattery)
    let task = taskLine(s)
    const m = markOf(s.id)
    if (m?.intent != null) {
      task = `${m.doing}: ${m.parts.map((p) => p.text).join(' · ')}`
      tone = worse(tone, m.tone)
    }
    rows.push({ key: `s:${s.id}`, kind: 'swarm', name: s.name ? `${s.id} · ${s.name}` : s.id, color: looks('swarm', i), total: s.count, counts: t.counts, lowBattery: t.lowBattery, task, tone, members: s.members })
  })
  const mine: GroupRow[] = groups.map((g, i) => {
    const t = tally(g.members, by, ctx)
    const kind = g.agent === true ? 'officer' : 'yours'
    return { key: `g:${g.id}`, kind, name: g.name, color: looks(kind, i) ?? g.color, total: g.members.length, counts: t.counts, lowBattery: t.lowBattery, task: '', tone: toneOf(t.here, t.counts, t.lowBattery), members: g.members }
  })
  const trouble = (a: GroupRow, b: GroupRow) => rank[a.tone] - rank[b.tone]
  return [...mine.sort(trouble), ...rows.sort(trouble)]
}

/** A strength line: "97 of 100: 94 on station, 3 returning; 2 lost link". */
export function strengthWords(r: Pick<GroupRow, 'total' | 'counts'>): string {
  const up = r.total - r.counts.lost - r.counts.damaged
  const normal = (['station', 'moving', 'returning', 'idle'] as VehicleState[]).filter((s) => r.counts[s] > 0).map((s) => `${r.counts[s]} ${STATE_WORDS[s]}`)
  const bad = (['lost', 'damaged'] as VehicleState[]).filter((s) => r.counts[s] > 0).map((s) => `${r.counts[s]} ${STATE_WORDS[s]}`)
  return `${up} of ${r.total}${normal.length ? `: ${normal.join(', ')}` : ''}${bad.length ? `; ${bad.join(', ')}` : ''}`
}

// ---- the nodes ------------------------------------------------------------------------------

export interface NodeRow {
  uid: number
  name: string
  state: VehicleState
  /** link health as the fleet list reads it */
  link: 'ok' | 'warn' | 'bad'
  battery: number | null
  lowBattery: boolean
  /** its latest order ("orbit active"), empty when none */
  order: string
  /** the groups it is in, by name (the operator's first) */
  groups: string[]
}

export function linkHealth(v: Pick<Vehicle, 'link'>): 'ok' | 'warn' | 'bad' {
  const since = v.link.since_last_ms ?? (v.link.stale == null || v.link.stale ? Infinity : 0)
  const loss = v.link.loss_pct ?? 0
  if (since > staleMs(v)) return 'bad'
  if (since > (v.link.expected_interval_ms ?? 1000) || loss > 10) return 'warn'
  return 'ok'
}

export function nodeRows(vehicles: readonly Vehicle[], ctx: SitrepContext, groupsOf: ReadonlyMap<number, string[]>): NodeRow[] {
  const low = ctx.lowBelowPct ?? LOW_BATTERY_PCT
  return vehicles.map((v) => {
    const op = ctx.latest.get(v.uid)
    return {
      uid: v.uid,
      name: callsign(v),
      state: vehicleState(v, ctx.latest, ctx.damaged),
      link: linkHealth(v),
      battery: v.battery_pct,
      lowBattery: v.battery_pct != null && v.battery_pct < low,
      order: op ? `${op.task} ${op.state}` : '',
      groups: groupsOf.get(v.uid) ?? [],
    }
  })
}

export interface NodeFilter {
  state: VehicleState | 'all'
  /** only those not heard now */
  unheard: boolean
  /** only the low on battery */
  low: boolean
  /** names containing this, any case */
  text: string
  /** only the members of these uids (a group opened), or null for every node */
  within: ReadonlySet<number> | null
}

export const ANY: NodeFilter = { state: 'all', unheard: false, low: false, text: '', within: null }

export function filterNodes(rows: readonly NodeRow[], f: NodeFilter): NodeRow[] {
  const q = f.text.trim().toLowerCase()
  return rows.filter(
    (r) =>
      (f.state === 'all' || r.state === f.state) &&
      (!f.unheard || r.state === 'lost') &&
      (!f.low || r.lowBattery) &&
      (!q || r.name.toLowerCase().includes(q)) &&
      (!f.within || f.within.has(r.uid)),
  )
}

/** The worst first (damaged, lost, low battery), then by name: what a filtered list opens on. */
export function worstFirst(rows: readonly NodeRow[]): NodeRow[] {
  const rank = (r: NodeRow) => (r.state === 'damaged' ? 0 : r.state === 'lost' ? 1 : r.lowBattery ? 2 : r.link !== 'ok' ? 3 : 4)
  return [...rows].sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name, 'en', { numeric: true }))
}

/** The rows in view of a scrolling list of fixed-height rows (virtualised: hundreds of nodes build
 *  only what can be seen). */
export function windowOf(count: number, scrollTop: number, viewH: number, rowH: number, overscan = 6): { from: number; to: number } {
  const from = Math.max(0, Math.floor(scrollTop / rowH) - overscan)
  const to = Math.min(count, Math.ceil((scrollTop + viewH) / rowH) + overscan)
  return { from, to: Math.max(from, to) }
}
