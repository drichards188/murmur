// Group orders (decision rules 34; docs/SWARMS.md sections 2 to 5 and 10, bd murmur-4nik): an
// order of an ordinary kind to more than one vehicle is one group order, which the decision
// engine lays out member by member (each member's slot, layer and altitude: its share), checks
// share by share, routes once and approves once. What the person approves is the envelope (the
// task, how the members share it, the band, the minimum, the selection); the engine lays the
// members out inside it.
//
// The console reads three things of one: the order form's dry run (`group`: its spec, layout and
// each member's task, POST /api/intents/preview), the inbox's card (the intent's `group` spec and
// its checks, each member's failing ones carrying its class, `member`), and the swarms summary's
// groups in force (GET /api/swarms). This module is the pure part, checked by
// groupOrders.check.ts; display only, nothing here decides.
import type { Check } from './decisions'
import type { LngLat } from './types'

/** How a group's members share its task. */
export type Spread = 'ring' | 'along' | 'lanes' | 'here' | 'home' | 'stations' | 'block'

/** A group order's own spec, as a proposal names it and the engine logs it (the defaults filled). */
export interface GroupSpec {
  swarm?: string
  spread?: Spread
  min_members?: number
  alt_m?: number
  wait_s?: number
  reform?: boolean
  lanes?: number
  lane_spacing_m?: number
}

/** One member's share of the task: what its member order flies. */
export interface Share {
  slot: number
  layer?: number
  alt_m?: number
  phase_deg?: number
  entry_m?: number
  at?: LngLat
  ring_m?: number
  lane?: number
  width_m?: number
}

export interface DomainLayout {
  domain: string
  members: number
  spacing_m?: number
  layers: number
  rings_m?: number[]
  lateral_m: number
  vertical_m?: number
  racetrack_m?: number
  path_m?: number
}

export interface Layout {
  spread?: Spread
  shares: Record<string, Share>
  domains?: DomainLayout[]
  warnings?: string[]
  refused?: string
}

/** The dry run's `group`: the spec as it would be logged, the layout and each member's task. */
export interface GroupPreview {
  spec: GroupSpec
  layout: Layout
  tasks: Record<string, { kind: string; params: Record<string, unknown> }>
}

/** The spreads a kind's group order takes, its default first. */
export const SPREADS: Record<string, Spread[]> = {
  orbit: ['ring'],
  patrol: ['along', 'lanes'],
  transit: ['stations', 'block'],
  hold: ['here'],
  rtl: ['home'],
}

export const SPREAD_WORDS: Record<Spread, string> = {
  ring: 'ring: evenly round the circle',
  along: 'along: evenly round the route',
  lanes: 'lanes: copies of the route side by side',
  here: 'here: each holds where it is',
  home: 'home: each to its own home',
  stations: 'stations: evenly along the route, each holding at its own',
  block: 'block: a grid round the route’s end',
}

/** Whether a kind's group order re-forms itself when members drop (never a hold, an rtl or a
 *  transit). */
export function reforms(kind: string): boolean {
  return kind === 'orbit' || kind === 'patrol'
}

/** The minimum strength the engine fills when the order names none: 80 % of the selection,
 *  rounded up (`[group] min_share`, an estimate), one for a hold or an rtl. */
export function defaultMin(kind: string, n: number): number {
  if (kind === 'hold' || kind === 'rtl') return 1
  return Math.min(n, Math.max(1, Math.ceil(n * 0.8 - 1e-9)))
}

export type MemberClass = 'sent' | 'waiting' | 'excluded'

export interface MemberLine {
  uid: string
  class: MemberClass
  /** its failing checks, "rule: detail" */
  why: string[]
}

/** Each member's class from its group's checks (a failing check of a member carries `member`):
 *  excluded when any says so, else waiting when any says so, else sent. The members in the order's
 *  own order. */
export function memberClasses(uids: string[], checks: Check[]): MemberLine[] {
  const by = new Map<string, MemberLine>(uids.map((u) => [u, { uid: u, class: 'sent', why: [] }]))
  for (const c of checks) {
    if (c.uid == null || c.outcome !== 'fail' || !c.member) continue
    const m = by.get(c.uid)
    if (!m) continue
    m.why.push(`${c.rule}: ${c.detail}`)
    if (c.member === 'excluded') m.class = 'excluded'
    else if (m.class !== 'excluded') m.class = 'waiting'
  }
  return uids.map((u) => by.get(u)!)
}

/** A reason as a key that groups the same reason across members: numbers stand for any number. */
export function reasonKey(detail: string): string {
  return detail.replace(/-?\d+(\.\d+)?/g, 'n').trim()
}

export interface RuleFold {
  rule: string
  pass: number
  waiting: number
  excluded: number
  /** a check of the whole group (the layout's separation, the strength): its own outcome */
  group?: Check
  /** not passing (failed, unverified, not evaluated), grouped by reason: one example each */
  reasons: { example: string; outcome: Check['outcome']; class?: Check['member']; uids: string[] }[]
}

/** A group's checks folded by rule, in the order the checks come (the engine's rule order), each
 *  rule's members counted by outcome and its other reasons grouped (docs/SWARMS.md section 5:
 *  "readiness: 97 pass, 3 wait: navigation status stale"); a hundred members' checks read as a
 *  few lines, never one per member per rule. */
export function foldByRule(checks: Check[]): RuleFold[] {
  const out: RuleFold[] = []
  const at = new Map<string, RuleFold>()
  for (const c of checks) {
    let f = at.get(c.rule)
    if (!f) {
      f = { rule: c.rule, pass: 0, waiting: 0, excluded: 0, reasons: [] }
      at.set(c.rule, f)
      out.push(f)
    }
    if (c.uid == null) {
      f.group = c
      continue
    }
    if (c.outcome === 'pass') {
      f.pass += 1
      continue
    }
    if (c.member === 'waiting') f.waiting += 1
    if (c.member === 'excluded') f.excluded += 1
    const key = `${c.outcome}|${c.member ?? ''}|${reasonKey(c.detail)}`
    const r = f.reasons.find((x) => `${x.outcome}|${x.class ?? ''}|${reasonKey(x.example)}` === key)
    if (r) r.uids.push(c.uid)
    else f.reasons.push({ example: c.detail, outcome: c.outcome, class: c.member, uids: [c.uid] })
  }
  return out
}

/** A folded rule's line: "97 pass, 3 waiting, 1 excluded" (or the group check's own detail). */
export function foldLine(f: RuleFold): string {
  if (f.group && f.pass + f.waiting + f.excluded + f.reasons.length === 0) return f.group.detail
  const parts = [`${f.pass} pass`]
  if (f.waiting) parts.push(`${f.waiting} waiting`)
  if (f.excluded) parts.push(`${f.excluded} excluded`)
  const other = f.reasons.filter((r) => !r.class).reduce((n, r) => n + r.uids.length, 0)
  if (other) parts.push(`${other} not passed`)
  return parts.join(', ')
}

/** A folded rule's tone: bad when the group check fails or a member is excluded, warn when one
 *  waits or a check is not passed, ok otherwise. */
export function foldTone(f: RuleFold): 'ok' | 'warn' | 'bad' {
  if (f.group?.outcome === 'fail' || f.excluded > 0) return 'bad'
  if (f.waiting > 0 || f.reasons.length > 0 || (f.group && f.group.outcome !== 'pass')) return 'warn'
  return 'ok'
}

export interface Counts {
  n: number
  sent: number
  waiting: number
  excluded: number
  min: number
}

export function counts(lines: MemberLine[], spec: GroupSpec | undefined, kind: string): Counts {
  const n = lines.length
  return {
    n,
    sent: lines.filter((l) => l.class === 'sent').length,
    waiting: lines.filter((l) => l.class === 'waiting').length,
    excluded: lines.filter((l) => l.class === 'excluded').length,
    min: spec?.min_members ?? defaultMin(kind, n),
  }
}

/** The card's and the dry run's bottom line (docs/SWARMS.md section 5): how many take it now, how
 *  many join if heard in time, what the minimum is and whether it re-forms. */
export function headline(c: Counts, spec: GroupSpec | undefined, kind: string, label: string | null): string {
  const of = label ? ` of ${label}` : ''
  const parts = [`${c.sent} of ${c.n}${of} now`]
  if (c.waiting) parts.push(`${c.waiting} join if heard${spec?.wait_s != null ? ` within ${spec.wait_s} s` : ''}`)
  if (c.excluded) parts.push(`${c.excluded} left out`)
  const re = spec?.reform ?? reforms(kind)
  parts.push(re ? `re-forms itself above ${c.min}` : `minimum ${c.min}`)
  return parts.join('; ')
}

/** What approving a group order does, in a sentence. */
export function approvalMeans(c: Counts, spec: GroupSpec | undefined, kind: string): string {
  const re = spec?.reform ?? reforms(kind)
  if (re) return `Approving sends each member its own share; the engine re-forms the group inside these limits as members drop, and below ${c.min} it stops and asks you.`
  if (kind === 'transit') return `Approving sends each member to its own place; a gap a member leaves stays open (a transit does not re-form).`
  return `Approving sends each member its own order; nothing re-forms.`
}

/** The layout in words: per domain, how many, how far apart, in how many layers or rings. */
export function layoutLine(l: Layout): string {
  const spread = l.spread ?? ''
  if (!l.domains?.length) return `${Object.keys(l.shares).length} member(s)${spread ? `, ${spread}` : ''}: each by its type`
  const parts = l.domains.map((d) => {
    let p = `${d.members} ${d.domain}`
    if (d.spacing_m != null) p += ` ${Math.round(d.spacing_m)} m apart`
    if (d.rings_m && d.rings_m.length > 1) p += ` on rings of ${d.rings_m.map((r) => Math.round(r)).join(', ')} m`
    if (d.layers > 1) p += ` in ${d.layers} layers ${Math.round(d.vertical_m ?? 0)} m apart`
    if (d.racetrack_m != null) p += `, a racetrack ${Math.round(d.racetrack_m)} m wide`
    return p
  })
  return `${spread}: ${parts.join('; ')}`
}

/** A layer's colour on the map (the layers of one domain apart at a glance), RGBA. */
export function layerColor(layer: number): [number, number, number, number] {
  const palette: [number, number, number][] = [
    [127, 184, 255],
    [242, 193, 78],
    [79, 209, 197],
    [196, 155, 255],
    [255, 140, 105],
    [160, 220, 120],
    [255, 120, 180],
  ]
  const [r, g, b] = palette[layer % palette.length]
  return [r, g, b, 255]
}

/** A member's slot on the map: where it goes, from where it is, its layer and altitude, its class. */
export interface SlotMark {
  uid: string
  at: LngLat
  from: LngLat | null
  /** the points its member order flies through on the way (a block's transit, its route moved to
   *  its cell), before its slot */
  via: LngLat[]
  layer: number
  alt_m: number | null
  class: MemberClass
  label: string
}

/** The slots the dry run's layout gives, each with the member's position now (`posOf`) and its
 *  class; labelled with the member's name only for a small group (the map stays readable at a
 *  hundred). */
export function slotMarks(g: GroupPreview, lines: MemberLine[], posOf: (uid: string) => LngLat | null, nameOf: (uid: string) => string): SlotMark[] {
  const cls = new Map(lines.map((l) => [l.uid, l.class]))
  const shares = Object.entries(g.layout.shares ?? {})
  const small = shares.length <= 12
  const out: SlotMark[] = []
  for (const [uid, s] of shares) {
    if (!s.at) continue
    const alt = s.alt_m ?? null
    const name = small ? nameOf(uid) : ''
    const altText = alt == null ? '' : alt < 0 ? `${Math.round(-alt)} m deep` : `${Math.round(alt)} m`
    // a transit's member order flies its own route: through each point before its slot
    const t = g.tasks?.[uid]
    const route = t?.kind === 'transit' && Array.isArray(t.params.route) ? (t.params.route as LngLat[]) : []
    out.push({
      uid,
      at: s.at,
      from: posOf(uid),
      via: route.slice(0, -1),
      layer: s.layer ?? 0,
      alt_m: alt,
      class: cls.get(uid) ?? 'sent',
      label: small ? [name, altText].filter(Boolean).join(' ') : '',
    })
  }
  return out
}

/** A group in force as GET /api/swarms lists it (`groups`; the bridge's `GroupView`). */
export interface GroupInForce {
  intent: number
  kind: string
  swarm?: string
  state: string
  selection: number
  members: number[]
  on_share: number
  dropped: Record<string, string>
  back: number[]
  taken: number
  strength: number
  min: number
  reforming: boolean
  reforms: number
  suspended: boolean
  last_t?: number
  refused?: string
}

/** A swarm's share of a group in force (each swarm's `groups` in GET /api/swarms). */
export interface SwarmGroup {
  intent: number
  kind: string
  members_here: number
  strength: number
  min: number
  dropped: number
  back: number
  reforms: number
  suspended: boolean
  refused?: string | null
}

/** A group in force in one line: "i41 orbit: 90 of a minimum 80, 10 dropped, re-formed 2x". */
export function strengthLine(g: Pick<GroupInForce, 'intent' | 'kind' | 'strength' | 'min' | 'reforms' | 'suspended'> & { dropped: number; back: number }): string {
  const parts = [`i${g.intent} ${g.kind}: ${g.strength} of a minimum ${g.min}`]
  if (g.dropped) parts.push(`${g.dropped} dropped`)
  if (g.back) parts.push(`${g.back} back, waiting to rejoin`)
  if (g.reforms) parts.push(`re-formed ${g.reforms}x`)
  if (g.suspended) parts.push('suspended below its minimum')
  return parts.join(', ')
}

/** A group's tone: bad suspended or below its minimum, warn with a refused re-form or drops. */
export function strengthTone(g: { strength: number; min: number; suspended: boolean; dropped: number; refused?: string | null }): 'ok' | 'warn' | 'bad' {
  if (g.suspended || g.strength < g.min) return 'bad'
  if (g.refused || g.dropped > 0) return 'warn'
  return 'ok'
}
