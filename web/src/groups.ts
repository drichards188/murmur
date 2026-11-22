// Operator groups: a named, mutable set of our vehicles that the operator
// makes at run time and the bridge shares (GET/POST/PATCH/DELETE /api/groups). Not a fleet swarm
// (swarmsModel.ts: fixed by the fleet file, read-only at /api/swarms): a vehicle can be in a swarm
// and in any number of these. A group is an organising convenience and a named selection: it
// decides nothing, it is not logged, and commanding one is the order form's ordinary group order to
// the members' uids (the group's name as the order's label). The AI watch officer may make groups
// too, for one of the operator's tasking items (its own MCP tool, never these routes): such a group
// says so (`agent`, `tasking`), is listed apart from the operator's own, and is the operator's to
// change or delete like any other. This module is the pure part, checked by groups.check.ts;
// useGroups.ts has the feed and the calls.
import { GROUP_PALETTE, sameMembers } from './swarmsModel.ts'

/** One group as the bridge serves it. */
export interface OperatorGroup {
  id: string
  name: string
  color: string | null
  /** our vehicle uids, de-duplicated, in the order they were added */
  members: number[]
  created_t: number
  updated_t: number
  /** who made it: an operator's id, or an agent's (then `agent`); absent from an older bridge */
  by?: string
  /** made by an agent (the AI watch officer) for the operator's tasking item `tasking` */
  agent?: boolean
  tasking?: string
  /** who changed it last: once an operator changes the officer's group, the officer may not */
  changed_by?: string
}

/** Whether the AI watch officer made the group (an agent), not the operator. */
export function byOfficer(g: Pick<OperatorGroup, 'agent'>): boolean {
  return g.agent === true
}

/** The operator's own groups and the officer's, each in the bridge's order. */
export function splitByMaker<G extends Pick<OperatorGroup, 'agent'>>(groups: readonly G[]): { yours: G[]; officer: G[] } {
  return { yours: groups.filter((g) => !byOfficer(g)), officer: groups.filter(byOfficer) }
}

/** An officer's group in words, for its badge's title. */
export function officerNote(g: Pick<OperatorGroup, 'by' | 'tasking' | 'changed_by'>): string {
  const made = `Made by the AI watch officer (${g.by ?? 'an agent'}) for tasking ${g.tasking ?? '?'}: organisation only, it orders nothing.`
  if (g.changed_by && g.changed_by !== g.by) return `${made} Changed by ${g.changed_by} since, so the officer can no longer change it.`
  return `${made} You may change or delete it; once you change it, the officer may not.`
}

/** The groups the officer made for one tasking item, in the bridge's order. */
export function groupsFor<G extends Pick<OperatorGroup, 'agent' | 'tasking'>>(groups: readonly G[], taskingId: string): G[] {
  return groups.filter((g) => byOfficer(g) && g.tasking === taskingId)
}

export interface GroupsDoc {
  t: number
  groups: OperatorGroup[]
}

/** The longest name the bridge takes. */
export const NAME_MAX = 64

/** Why `name` will not do (the bridge refuses an empty or over-long one, counting characters as
 *  it does, not UTF-16 units), or null. */
export function nameProblem(name: string): string | null {
  const n = name.trim()
  if (!n) return 'A group needs a name.'
  if ([...n].length > NAME_MAX) return `A name is at most ${NAME_MAX} characters.`
  return null
}

/** What a PATCH may carry: `add` and `remove` are the combine and separate operations, `members`
 *  replaces the whole set. */
export interface GroupPatch {
  name?: string
  color?: string | null
  add?: number[]
  remove?: number[]
  members?: number[]
}

const dedupe = (uids: readonly number[]) => [...new Set(uids)]

/** The group as the bridge will make of it after `p`: the same rules as its PATCH (a wholesale
 *  `members` first, then `add`, then `remove`, de-duplicated, order kept: groups.rs
 *  `GroupStore::patch`), for the optimistic update before its answer comes. The bridge's answer
 *  replaces it (it also drops uids not in the fleet, which the console cannot know). */
export function applyPatch(g: OperatorGroup, p: GroupPatch, t: number = g.updated_t): OperatorGroup {
  let members = p.members ? dedupe(p.members) : g.members
  if (p.add?.length) members = dedupe([...members, ...p.add])
  if (p.remove?.length) {
    const gone = new Set(p.remove)
    members = members.filter((u) => !gone.has(u))
  }
  return { ...g, name: p.name != null ? p.name.trim() : g.name, color: p.color !== undefined ? p.color : g.color, members, updated_t: t }
}

/** A colour for a new group: the first of the palette no group has yet, else the palette round
 *  again by the number of groups. */
export function nextColor(groups: readonly Pick<OperatorGroup, 'color'>[]): string {
  const used = new Set(groups.map((g) => g.color))
  return (GROUP_PALETTE.find((p) => !used.has(p.color)) ?? GROUP_PALETTE[groups.length % GROUP_PALETTE.length]).color
}

/** A group's colour with the ink its text takes: the palette's own pair, or black or white by the
 *  colour's lightness for a colour the palette does not have (a group made elsewhere). */
export function lookOf(g: Pick<OperatorGroup, 'color'>, index: number): { color: string; ink: string } {
  const own = g.color ? GROUP_PALETTE.find((p) => p.color.toLowerCase() === g.color!.toLowerCase()) : undefined
  if (own) return own
  const color = g.color && /^#[0-9a-f]{6}$/i.test(g.color) ? g.color : GROUP_PALETTE[((index % GROUP_PALETTE.length) + GROUP_PALETTE.length) % GROUP_PALETTE.length].color
  const n = parseInt(color.slice(1), 16)
  const lum = (0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255)) / 255
  return { color, ink: lum > 0.55 ? '#10141a' : '#ffffff' }
}

/** The group the selection is exactly, if any (a selection of its members and no one else), by
 *  the same rule as a fleet swarm's (swarmsModel.ts `selectedSwarm`). */
export function groupOfSelection(groups: readonly OperatorGroup[], selected: ReadonlySet<number>): OperatorGroup | null {
  return groups.find((g) => sameMembers(g.members, selected)) ?? null
}

/** The selected vehicles not yet in the group: what "add the selection" would add. */
export function toAdd(g: Pick<OperatorGroup, 'members'>, selected: ReadonlySet<number>): number[] {
  const have = new Set(g.members)
  return [...selected].filter((u) => !have.has(u)).sort((a, b) => a - b)
}

/** The group's members that are selected: what "remove the selection from it" would take out. */
export function toSeparate(g: Pick<OperatorGroup, 'members'>, selected: ReadonlySet<number>): number[] {
  return g.members.filter((u) => selected.has(u))
}

/** The groups a vehicle is in. */
export function groupsOf(groups: readonly OperatorGroup[], uid: number): OperatorGroup[] {
  return groups.filter((g) => g.members.includes(uid))
}

/** The members of `g` that are in the fleet now, and how many are not (a vehicle may have left or
 *  not been heard yet): the group's strength as the console can see it. */
export function strength(g: Pick<OperatorGroup, 'members'>, present: ReadonlySet<number>): { here: number; away: number } {
  let here = 0
  for (const u of g.members) if (present.has(u)) here++
  return { here, away: g.members.length - here }
}

/** A default name for a new group made from a selection ("Group 3"): the lowest number no group
 *  has taken. */
export function defaultName(groups: readonly Pick<OperatorGroup, 'name'>[]): string {
  const names = new Set(groups.map((g) => g.name))
  for (let i = 1; ; i++) if (!names.has(`Group ${i}`)) return `Group ${i}`
}
