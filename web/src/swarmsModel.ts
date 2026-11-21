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
import type { GroupInForce, SwarmGroup } from './groupOrders'

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
  /** rules 34: the group orders in force over its members, its share of each */
  groups?: SwarmGroup[]
}

export interface SwarmsDoc {
  t: number
  swarms: Swarm[]
  /** rules 34: every group order in force (only when there is one) */
  groups?: GroupInForce[]
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

/** The swarm a vehicle belongs to: the bridge's top-level `swarm`, or its identity's. */
export function swarmOf(v: Pick<Vehicle, 'swarm' | 'identity'>): string | null {
  return v.swarm ?? v.identity?.swarm ?? null
}

/** The groups' colours, each with the ink its text takes (at least 4.5:1 on it; each colour at
 *  least 4:1 against the dark map). None is an affiliation colour (blue friendly, red hostile,
 *  green neutral, yellow unknown: the tracks use them, and a group in one would read as that), the
 *  selection's orange or the sea's teal: violet, ivory, pink, deep magenta, orchid, in that order so
 *  that neighbours in the summary differ. A group's colour is its place in the summary's order, the
 *  same in the fleet list, the Swarms panel and the map; past five they repeat, and the code
 *  (groupCodes) tells them apart, in greyscale too. The fleet file's `color` is not used: it chose
 *  among the affiliation hues. Never on a vehicle's frame or fill (its affiliation): only on the
 *  chip, the outline, the ring under each member and its stem. */
export const GROUP_PALETTE: readonly { color: string; ink: string }[] = [
  { color: '#c49bff', ink: '#120b1f' },
  { color: '#ebe5d8', ink: '#16130d' },
  { color: '#ff92cc', ink: '#1f0a15' },
  { color: '#c0399f', ink: '#ffffff' },
  { color: '#ea8cf2', ink: '#1b0a1d' },
]

/** The colour (and its ink) of the group at `index` in the summary's order. */
export function groupColor(index: number): { color: string; ink: string } {
  const n = GROUP_PALETTE.length
  return GROUP_PALETTE[((index % n) + n) % n]
}

/** Each swarm's colour by id, from the summary's order (the lists' swatches and the map). */
export function swarmColors(swarms: readonly Pick<Swarm, 'id'>[] | undefined): Map<string, string> {
  return new Map((swarms ?? []).map((s, i) => [s.id, groupColor(i).color]))
}

/** Each group's short code, on its chip and under each of its members, so membership reads without
 *  colour: the first letters of its id's leading word, as few as make every code unique (the same
 *  number of letters for all), then its number: PICKET "PI" beside SEARCH "SE"; PICKET-3 "P3"
 *  where no other word starts with P; SEARCH-3 "SE3" beside SURFACE-3 "SU3". An id without
 *  letters is its own code. */
export function groupCodes(ids: readonly string[]): Map<string, string> {
  const parts = ids.map((id) => {
    const word = (/^[^A-Za-z]*([A-Za-z]+)/.exec(id)?.[1] ?? '').toUpperCase()
    const num = /(\d+)\D*$/.exec(id)?.[1] ?? ''
    return { id, word, num }
  })
  const longest = Math.max(1, ...parts.map((p) => p.word.length))
  for (let n = 1; n <= longest; n++) {
    const codes = parts.map((p) => (p.word ? p.word.slice(0, n) + p.num : p.id))
    if (new Set(codes).size === codes.length) return new Map(parts.map((p, i) => [p.id, codes[i]]))
  }
  // the same word and number twice (ids that differ only in punctuation): numbered in order
  return new Map(parts.map((p, i) => [p.id, `${p.word ? p.word.slice(0, 2) + p.num : p.id}.${i + 1}`]))
}

export function rgba(hex: string, alpha = 255): RGBA {
  const n = parseInt(hex.slice(1), 16)
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255, alpha]
}

const KIND_VERB: Record<string, string> = { orbit: 'orbit', patrol: 'patrol', hold: 'hold', rtl: 'return home', track: 'track', identify: 'identify', transit: 'transit' }

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

// ---- each group on the map: its marks, its chip, its outline -----------------------------------
//
// One chip per group over its members (GroupChips.tsx), a ring under each member and its stem in
// the group's colour (vehicleLayers.ts, map3d.ts), its code under each member while they stand
// apart, and its outline at sea level round the members heard now (swarms.ts). Built from one
// summary (about once a second), never per frame.

export type Tone = 'ok' | 'warn' | 'bad'

/** A piece of a chip's status line, its tone colouring it (none: the line's own). */
export interface ChipPart {
  text: string
  tone?: Tone
}

/** One group as the map marks it. */
export interface GroupMark {
  id: string
  name: string
  code: string
  /** its colour and the ink its text takes (GROUP_PALETTE) */
  color: string
  ink: string
  rgb: RGBA
  members: number[]
  /** what it does: "orbit i1", "patrol i100", "no order" */
  doing: string
  /** its strength, honestly: on its share of the group order against its selection and minimum,
   *  members unheard, back, dropped; else how many run its task */
  parts: ChipPart[]
  /** the worst of its parts: the chip's edge */
  tone: Tone
  /** its group order is suspended below its minimum: drawn apart from merely under strength */
  suspended: boolean
  /** its group order is under its minimum, not (yet) suspended */
  under: boolean
  /** its group order's last re-form was refused: why */
  refused: string | null
  /** when its group order last re-formed (bridge ms), if it has */
  reformedT: number | null
  /** the group order in force over most of it (rules 34), for the chip's hover: its members dropped
   *  (uid -> why) and back */
  dropped: [number, string][]
  back: number[]
  intent: number | null
}

export interface GroupMarks {
  list: GroupMark[]
  byId: ReadonlyMap<string, GroupMark>
  ofUid: ReadonlyMap<number, GroupMark>
}

export const NO_MARKS: GroupMarks = { list: [], byId: new Map(), ofUid: new Map() }

const worse = (a: Tone, b: Tone): Tone => (a === 'bad' || b === 'bad' ? 'bad' : a === 'warn' || b === 'warn' ? 'warn' : 'ok')

/** A group's chip status from the summary: under a group order in force (the one over most of its
 *  members), "60/60 · min 45" (its strength, on their shares and back, of its selection, against
 *  its minimum), "41/60 · under min 45" or "SUSPENDED · 5 of min 7"; members back and dropped when
 *  any; else how many run its largest task; and how many have lost their link. Never a bar: the
 *  numbers, and the chip's hover names who is missing and why. */
export function chipStatus(s: Swarm, inForce: readonly GroupInForce[] = []): Pick<GroupMark, 'doing' | 'parts' | 'tone' | 'suspended' | 'under' | 'refused' | 'reformedT' | 'dropped' | 'back' | 'intent'> {
  const parts: ChipPart[] = []
  let tone: Tone = 'ok'
  let suspended = false
  let under = false
  let refused: string | null = null
  let reformedT: number | null = null
  let doing = 'no order'
  let dropped: [number, string][] = []
  let back: number[] = []
  let intent: number | null = null
  const g = [...(s.groups ?? [])].sort((a, b) => b.members_here - a.members_here)[0]
  if (g) {
    const whole = inForce.find((x) => x.intent === g.intent)
    const sel = whole?.selection ?? g.members_here
    intent = g.intent
    doing = `${KIND_VERB[g.kind] ?? g.kind} i${g.intent}`
    if (g.suspended) {
      suspended = true
      tone = 'bad'
      parts.push({ text: `SUSPENDED · ${g.strength} of min ${g.min}`, tone: 'bad' })
    } else if (g.strength < g.min) {
      tone = 'bad'
      under = true
      parts.push({ text: `${g.strength}/${sel} · under min ${g.min}`, tone: 'bad' })
    } else {
      tone = g.strength < sel ? 'warn' : 'ok'
      parts.push({ text: `${g.strength}/${sel}`, tone }, { text: `min ${g.min}` })
    }
    if (g.back) parts.push({ text: `${g.back} back` })
    if (g.dropped) {
      parts.push({ text: `${g.dropped} dropped`, tone: 'warn' })
      tone = worse(tone, 'warn')
    }
    dropped = Object.entries(whole?.dropped ?? {}).map(([u, why]) => [Number(u), why])
    back = whole?.back ?? []
    refused = g.refused || whole?.refused || null
    reformedT = g.reforms > 0 ? (whole?.last_t ?? null) : null
  } else {
    const t = [...(s.tasks ?? [])].sort((a, b) => b.count - a.count)[0]
    if (t) {
      doing = `${KIND_VERB[t.kind] ?? t.kind}${t.intent != null ? ` i${t.intent}` : ''}`
      parts.push({ text: t.count === s.count ? `${s.count}` : `${t.count} of ${s.count}` })
    } else parts.push({ text: `${s.count}` })
  }
  if (s.lost_link) {
    const t = swarmTone(s)
    parts.push({ text: `${s.lost_link} lost link`, tone: t === 'ok' ? 'warn' : t })
    tone = worse(tone, t === 'ok' ? 'warn' : t)
  }
  return { doing, parts, tone, suspended, under, refused, reformedT, dropped, back, intent }
}

// ---- a group's order on the map only on demand -------------------------------------------------

/** The group a tasking's vehicles all belong to, if they do (a group order's member orders, each
 *  one vehicle's share); null for an order to vehicles of no group or of several. */
export function taskingGroup(t: { ops: readonly { uid: number }[] }, ofUid: ReadonlyMap<number, Pick<GroupMark, 'id'>>): string | null {
  let g: string | null = null
  for (const o of t.ops) {
    const m = ofUid.get(o.uid)
    if (!m || (g && m.id !== g)) return null
    g = m.id
  }
  return g
}

/** The taskings the map draws (bd murmur-4nik): a group's orders (its rings, lanes, routes) only on
 *  demand, while the group is open (selected, hovered, lit up), one of the order's vehicles is
 *  selected, or `all` (Space held); every other order as before. A hundred members' shares drawn at
 *  once were the clutter the owner saw. */
export function shownTaskings<T extends { ops: readonly { uid: number }[] }>(taskings: readonly T[], ofUid: ReadonlyMap<number, Pick<GroupMark, 'id'>>, open: ReadonlySet<string>, selected: ReadonlySet<number>, all: boolean): T[] {
  if (all || !ofUid.size) return taskings as T[]
  return taskings.filter((t) => {
    const g = taskingGroup(t, ofUid)
    return g == null || open.has(g) || t.ops.some((o) => selected.has(o.uid))
  })
}

// ---- group alarms on the map's banner, once they have held --------------------------------------

export type GroupAlarm = 'suspended' | 'under' | 'refused'

/** An alarm's banner head and tone, the worst first. */
export const GROUP_ALARM: Record<GroupAlarm, { head: string; tone: Tone; rank: number }> = {
  suspended: { head: 'GROUP SUSPENDED', tone: 'bad', rank: 0 },
  under: { head: 'GROUP UNDER STRENGTH', tone: 'bad', rank: 1 },
  refused: { head: 'GROUP RE-FORM REFUSED', tone: 'warn', rank: 2 },
}

/** how long a group's alarm must hold before the banner raises it, ms: a link's blip that drops a
 *  member for a second or two and back never flashes it */
export const ALARM_HOLD_MS = 5000

export interface AlarmSeen {
  alarm: GroupAlarm
  /** when it began, bridge ms */
  since: number
}

/** A group's alarm now: its order suspended below its minimum, under its minimum, or its last
 *  re-form refused. */
export function groupAlarm(m: Pick<GroupMark, 'suspended' | 'under' | 'refused'>): GroupAlarm | null {
  return m.suspended ? 'suspended' : m.under ? 'under' : m.refused ? 'refused' : null
}

/** Each group's alarm and since when, from the last summary's and this one's (`nowT`): an alarm
 *  that goes on keeps its start, a new or changed one starts now, a gone one is dropped. */
export function stepAlarms(prev: ReadonlyMap<string, AlarmSeen>, marks: readonly GroupMark[], nowT: number): Map<string, AlarmSeen> {
  const out = new Map<string, AlarmSeen>()
  for (const m of marks) {
    const alarm = groupAlarm(m)
    if (!alarm) continue
    const was = prev.get(m.id)
    out.set(m.id, was && was.alarm === alarm ? was : { alarm, since: nowT })
  }
  return out
}

/** A held alarm's key, for dismissing that one (a new one of the group comes up again). */
export const alarmKey = (id: string, a: AlarmSeen) => `${id}:${a.alarm}:${a.since}`

/** The alarms held ALARM_HOLD_MS or more and not dismissed, the worst and oldest first. */
export function heldAlarms(seen: ReadonlyMap<string, AlarmSeen>, marks: GroupMarks, nowT: number, dismissed: ReadonlySet<string>): { m: GroupMark; seen: AlarmSeen }[] {
  const out: { m: GroupMark; seen: AlarmSeen }[] = []
  for (const [id, a] of seen) {
    const m = marks.byId.get(id)
    if (m && nowT - a.since >= ALARM_HOLD_MS && !dismissed.has(alarmKey(id, a))) out.push({ m, seen: a })
  }
  return out.sort((x, y) => GROUP_ALARM[x.seen.alarm].rank - GROUP_ALARM[y.seen.alarm].rank || x.seen.since - y.seen.since)
}

// ---- the group strip (GroupStrip.tsx): each group's state in a word -----------------------------

export type GroupState = 'suspended' | 'under' | 'reforming' | 'station' | 'moving' | 'returning' | 'idle' | 'lost' | 'damaged'

/** A state's icon and word, as the strip shows them (never colour alone). */
export const GROUP_STATE: Record<GroupState, { icon: string; word: string; tone: Tone }> = {
  suspended: { icon: '⏸', word: 'suspended', tone: 'bad' },
  under: { icon: '▼', word: 'under strength', tone: 'bad' },
  reforming: { icon: '↻', word: 're-forming', tone: 'warn' },
  station: { icon: '●', word: 'on station', tone: 'ok' },
  moving: { icon: '➜', word: 'moving', tone: 'ok' },
  returning: { icon: '↩', word: 'returning', tone: 'ok' },
  idle: { icon: '○', word: 'idle', tone: 'ok' },
  lost: { icon: '✕', word: 'lost link', tone: 'bad' },
  damaged: { icon: '✕', word: 'damaged', tone: 'bad' },
}

/** how long after a re-form the strip says "re-forming", ms */
export const REFORMING_MS = 30_000

/** A group's state in a word: its group order's (suspended, under strength, re-formed in the last
 *  REFORMING_MS of `nowT`), else what most of its members do (counted by exceptions.ts's
 *  vehicleState: on station, moving, returning, idle, lost link, damaged). */
export function groupState(m: Pick<GroupMark, 'suspended' | 'under' | 'reformedT'>, counts: Partial<Record<Exclude<GroupState, 'suspended' | 'under' | 'reforming'>, number>>, nowT: number): GroupState {
  if (m.suspended) return 'suspended'
  if (m.under) return 'under'
  if (m.reformedT != null && nowT - m.reformedT < REFORMING_MS) return 'reforming'
  let best: GroupState = 'idle'
  let n = 0
  for (const s of ['station', 'moving', 'returning', 'idle', 'lost', 'damaged'] as const)
    if ((counts[s] ?? 0) > n) {
      best = s
      n = counts[s] ?? 0
    }
  return best
}

/** Every swarm's marks from one summary: its code, colour (by its place in the summary) and chip. */
export function groupMarks(doc: SwarmsDoc | null): GroupMarks {
  const swarms = doc?.swarms ?? []
  if (!swarms.length) return NO_MARKS
  const codes = groupCodes(swarms.map((s) => s.id))
  const list: GroupMark[] = swarms.map((s, i) => {
    const { color, ink } = groupColor(i)
    return { id: s.id, name: s.name, code: codes.get(s.id)!, color, ink, rgb: rgba(color), members: s.members, ...chipStatus(s, doc?.groups) }
  })
  const ofUid = new Map<number, GroupMark>()
  for (const m of list) for (const u of m.members) ofUid.set(u, m)
  return { list, byId: new Map(list.map((m) => [m.id, m])), ofUid }
}

/** Who is missing from a group and why, for its chip's hover: those its group order dropped, by
 *  reason; those back and waiting to rejoin; those whose link is lost now. Names by `nameOf`, at
 *  most `max` a line. */
export function missingLines(m: GroupMark, lostNow: readonly number[], nameOf: (uid: number) => string, max = 6): string[] {
  const names = (uids: number[]) => uids.slice(0, max).map(nameOf).join(', ') + (uids.length > max ? ` +${uids.length - max}` : '')
  const out: string[] = []
  const why = new Map<string, number[]>()
  for (const [u, w] of m.dropped) {
    const k = w.split(':')[0]
    const list = why.get(k)
    if (list) list.push(u)
    else why.set(k, [u])
  }
  for (const [k, uids] of why) out.push(`${uids.length} dropped (${k}): ${names(uids)}`)
  if (m.back.length) out.push(`${m.back.length} back, waiting to rejoin: ${names(m.back)}`)
  if (lostNow.length) out.push(`${lostNow.length} link lost now: ${names([...lostNow])}`)
  return out
}

/** The convex hull of points ([lon, lat]), counter-clockwise (Andrew's monotone chain). */
export function convexHull(points: readonly LngLat[]): LngLat[] {
  const p = [...points].sort((a, b) => a[0] - b[0] || a[1] - b[1])
  if (p.length < 3) return p
  const cross = (o: LngLat, a: LngLat, b: LngLat) => (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
  const lower: LngLat[] = []
  for (const q of p) {
    while (lower.length >= 2 && cross(lower[lower.length - 2], lower[lower.length - 1], q) <= 0) lower.pop()
    lower.push(q)
  }
  const upper: LngLat[] = []
  for (let i = p.length - 1; i >= 0; i--) {
    const q = p[i]
    while (upper.length >= 2 && cross(upper[upper.length - 2], upper[upper.length - 1], q) <= 0) upper.pop()
    upper.push(q)
  }
  return [...lower.slice(0, -1), ...upper.slice(0, -1)]
}

export interface Outline {
  id: string
  ring: [number, number, number][]
  line: RGBA
  fill: RGBA
  width: number
}

/** Each group's outline at sea level round its members heard now (`heardAt`: a member's position
 *  while its link is not stale, else null): a silent or stale member is left out, never wrapped in
 *  a tidy shape at a place it may have left (it shows on its own, dimmed and dashed). The selected
 *  or hovered group drawn stronger. */
export function swarmOutlines(marks: GroupMarks, heardAt: (uid: number) => LngLat | null, selected: string | null, hovered: string | null = null): Outline[] {
  const out: Outline[] = []
  for (const m of marks.list) {
    const pts: LngLat[] = []
    for (const u of m.members) {
      const p = heardAt(u)
      if (p) pts.push(p)
    }
    const hull = convexHull(pts)
    if (hull.length < 3) continue
    const strong = m.id === selected || m.id === hovered
    out.push({ id: m.id, ring: hull.map(([lon, lat]) => [lon, lat, 0]), line: rgba(m.color, strong ? 255 : 200), fill: rgba(m.color, strong ? 52 : 24), width: strong ? 4 : 2.5 })
  }
  return out
}
