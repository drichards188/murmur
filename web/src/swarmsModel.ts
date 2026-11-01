// Swarms (docs/DECISIONS.md, "Swarm units"): a fleet-level grouping of our vehicles from the fleet
// file's [[swarm]] tables (sim/swarm.py). The bridge names each vehicle's swarm (`swarm`, in its
// identity and in /api/state), summarises every swarm about once a second at GET /api/swarms, and
// folds its members' alerts into one swarm alert each in the inbox (`swarm_alerts`; each member
// alert folded into one carries `folded_into`). All of it is a view: an order to a swarm is one
// ordinary intent naming all its members, and nothing here is logged.
//
// This module is the pure part (types and helpers, checked by swarms.check.ts); swarms.ts has the
// feed and the map's layers, SwarmsPanel.tsx the panel, InboxPanel.tsx the swarm cards and
// FleetPanel.tsx the list grouped by swarm.
import type { Alert, Severity } from './decisions'
import type { LngLat, Vehicle } from './types'

export type SwarmRole = 'picket' | 'search' | 'relay' | 'surface' | 'undersea' | 'reserve' | 'other'

export interface SwarmTask {
  intent: number | string | null
  kind: string
  /** how many of the swarm's members run it */
  count: number
}

/** One swarm as GET /api/swarms summarises it. */
export interface Swarm {
  id: string
  name: string
  role: SwarmRole | string
  color: string | null
  count: number
  members: number[]
  ready: number
  lost_link: number
  low_battery: number
  centroid: LngLat | null
  hull: LngLat[] | null
  tasks: SwarmTask[]
}

export interface SwarmsDoc {
  t: number
  swarms: Swarm[]
}

/** A swarm's members' alerts of one kind, folded into one (the inbox's `swarm_alerts`). */
export interface SwarmAlert {
  id: string
  swarm: string
  kind: string
  severity: Severity
  /** members with the alert, of `of` in the swarm */
  count: number
  of: number
  uids: number[]
  /** why, as the bridge says it: a list of lines, of {cause, count}, or a map cause -> count */
  causes?: unknown
  since_t: number
}

export const ROLE_LABEL: Record<string, string> = {
  picket: 'Picket',
  search: 'Search',
  relay: 'Relay',
  surface: 'Surface',
  undersea: 'Undersea',
  reserve: 'Reserve',
  other: 'Other',
}

export const ROLE_ORDER = ['picket', 'search', 'relay', 'surface', 'undersea', 'reserve', 'other']

export type RGBA = [number, number, number, number]

const ROLE_COLOR: Record<string, string> = {
  picket: '#7fb8ff',
  search: '#f2c14e',
  relay: '#c49bff',
  surface: '#4fd1c5',
  undersea: '#5b8cff',
  reserve: '#ff6b6b',
  other: '#d7dde4',
}

/** The swarm a vehicle belongs to: the bridge's top-level `swarm`, or its identity's. */
export function swarmOf(v: Pick<Vehicle, 'swarm' | 'identity'>): string | null {
  return v.swarm ?? v.identity?.swarm ?? null
}

export function swarmColor(s: Pick<Swarm, 'color' | 'role'>): string {
  return s.color && /^#[0-9a-fA-F]{6}$/.test(s.color) ? s.color : (ROLE_COLOR[s.role] ?? ROLE_COLOR.other)
}

export function rgba(hex: string, alpha = 255): RGBA {
  const n = parseInt(hex.slice(1), 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, alpha]
}

const KIND_VERB: Record<string, string> = { orbit: 'orbit', patrol: 'patrol', hold: 'hold', rtl: 'return home', track: 'track', identify: 'identify' }

/** "orbit i12" for each task its members run, the largest first, with how many when not all;
 *  "no task" without one. */
export function taskLine(s: Swarm): string {
  const tasks = [...(s.tasks ?? [])].sort((a, b) => b.count - a.count)
  if (!tasks.length) return 'no task'
  return tasks
    .map((t) => `${KIND_VERB[t.kind] ?? t.kind}${t.intent != null ? ` i${t.intent}` : ''}${t.count !== s.count ? ` (${t.count})` : ''}`)
    .join(', ')
}

/** Whether a swarm needs a look: a tenth of it or more unheard (bad), some unheard or low on
 *  battery (warn). */
export function swarmTone(s: Swarm): 'ok' | 'warn' | 'bad' {
  if (s.lost_link > 0 && s.lost_link * 10 >= s.count) return 'bad'
  if (s.lost_link > 0 || s.low_battery > 0) return 'warn'
  return 'ok'
}

/** The swarm whose members are exactly this selection, if one is. */
export function selectedSwarm(swarms: Swarm[] | undefined, selected: ReadonlySet<number>): string | null {
  if (!swarms || selected.size === 0) return null
  for (const s of swarms) if (s.members.length === selected.size && s.members.every((u) => selected.has(u))) return s.id
  return null
}

/** The causes of a swarm alert as lines, whatever shape the bridge sends them in. */
export function causeLines(causes: unknown): string[] {
  if (!causes) return []
  if (typeof causes === 'string') return [causes]
  if (Array.isArray(causes))
    return causes.map((c) => {
      if (typeof c === 'string') return c
      if (c && typeof c === 'object') {
        const o = c as Record<string, unknown>
        const what = String(o.cause ?? o.detail ?? o.kind ?? '')
        return o.count != null ? `${what} (${o.count})` : what
      }
      return String(c)
    })
  if (typeof causes === 'object') return Object.entries(causes as Record<string, unknown>).map(([k, n]) => `${k} (${n})`)
  return [String(causes)]
}

/** The member alerts the Inbox lists on their own: those not folded into a swarm alert. */
export function unfolded(alerts: Alert[]): Alert[] {
  return alerts.some((a) => a.folded_into) ? alerts.filter((a) => !a.folded_into) : alerts
}

/** The member alerts folded into this swarm alert, by uid. */
export function foldedInto(alerts: Alert[], swarmAlertId: string): Map<number, Alert[]> {
  const out = new Map<number, Alert[]>()
  for (const a of alerts) {
    if (a.folded_into !== swarmAlertId) continue
    const list = out.get(a.uid)
    if (list) list.push(a)
    else out.set(a.uid, [a])
  }
  return out
}

/** The fleet list's groups when the fleet has swarms: each swarm in the summary's order (else as
 *  first met), then the vehicles of none under `rest`. Null for a fleet without swarms. */
export function swarmGroups<V extends Pick<Vehicle, 'swarm' | 'identity'>>(
  vehicles: V[],
  swarms: Pick<Swarm, 'id' | 'name'>[] | undefined,
): { id: string | null; title: string; vehicles: V[] }[] | null {
  if (!vehicles.some((v) => swarmOf(v))) return null
  const by = new Map<string, V[]>()
  const rest: V[] = []
  for (const v of vehicles) {
    const s = swarmOf(v)
    if (!s) rest.push(v)
    else {
      const list = by.get(s)
      if (list) list.push(v)
      else by.set(s, [v])
    }
  }
  const names = new Map((swarms ?? []).map((s) => [s.id, s.name]))
  const order = [...(swarms ?? []).map((s) => s.id).filter((id) => by.has(id)), ...[...by.keys()].filter((id) => !names.has(id))]
  const out: { id: string | null; title: string; vehicles: V[] }[] = order.map((id) => ({
    id,
    title: names.get(id) ? `${id} · ${names.get(id)}` : id,
    vehicles: by.get(id)!,
  }))
  if (rest.length) out.push({ id: null, title: 'Not in a swarm', vehicles: rest })
  return out
}

// ---- the map's outlines (swarms.ts draws them) --------------------------------------------------

export interface Outline {
  id: string
  ring: [number, number, number][]
  line: RGBA
  fill: RGBA
}

export interface OutlineLabel {
  id: string
  at: [number, number, number]
  text: string
  color: RGBA
}

const LOST_TEXT: RGBA = [255, 154, 138, 255]

/** Each swarm's hull at sea level and a label at its centroid ("PICKET-1", and how many have lost
 *  their link when any); the selected swarm's drawn stronger. */
export function swarmOutlines(doc: SwarmsDoc | null, selected: string | null): { outlines: Outline[]; labels: OutlineLabel[] } {
  const outlines: Outline[] = []
  const labels: OutlineLabel[] = []
  for (const s of doc?.swarms ?? []) {
    const hex = swarmColor(s)
    const sel = s.id === selected
    if (s.hull && s.hull.length >= 3)
      outlines.push({ id: s.id, ring: s.hull.map(([lon, lat]) => [lon, lat, 0]), line: rgba(hex, sel ? 255 : 190), fill: rgba(hex, sel ? 46 : 18) })
    const at = s.centroid ?? s.hull?.[0]
    if (!at) continue
    labels.push({ id: s.id, at: [at[0], at[1], 0], text: s.lost_link ? `${s.id} · ${s.lost_link} lost` : s.id, color: s.lost_link ? LOST_TEXT : rgba(hex) })
  }
  return { outlines, labels }
}
