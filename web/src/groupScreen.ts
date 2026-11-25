// Groups on the map, in screen space (bd murmur-4nik): how each group's members stand on the screen
// now, where its chip goes, and which groups fill a cluster marker's tile. Pure, checked by
// swarms.check.ts; MapView feeds it the camera and GroupChips.tsx draws the chips.
//
// Level of detail by on-screen size, not by zoom (no source gives thresholds; these were picked on
// the Group command run), member by member and group by group:
//   apart   a member with no other member of its group within CROWD_PX on the screen: its ring in
//           its group's colour, its group's code under it, its callsign by the label budget as any
//           vehicle's
//   crowded a member with one that near: its ring only (its callsign on hover or when selected);
//           a group whose members crowd PILE_SHARE of the time or more is a pile, its chip the
//           name over it
//   cells   the bridge counts the view in tiles (clusterLayers.ts): one chip per tile with the
//           groups in it and their counts
import type { LngLat } from './types'
import { textWidth } from './declutter.ts'
import { chipExceptions, type GroupMark } from './swarmsModel.ts'

export type GroupLod = 'apart' | 'pile'

/** centre to centre, CSS px: two members of a group nearer than this crowd each other (a symbol is
 *  about 30 px across, its ring 36; its code tag 14 px under it) */
export const CROWD_PX = 44
/** a group whose members crowd each other this often or more is a pile */
export const PILE_SHARE = 0.35

export interface MemberOnScreen {
  uid: number
  group: string
  /** where it is drawn, CSS px */
  x: number
  y: number
  /** where it is, rendered metres (its drawn height, or 0 when it is drawn at its foot) */
  lon: number
  lat: number
  z: number
  /** its symbol's half height, px */
  halfH: number
}

export interface GroupOnScreen {
  id: string
  lod: GroupLod
  /** members on screen */
  n: number
  /** the chip's centre is where this point is drawn (the members' mean position at the height of
   *  the one drawn highest) */
  xAt: [number, number, number]
  /** and its bottom `lift` px above where this point is drawn (the member drawn highest) */
  yAt: [number, number, number]
  lift: number
}

/** Each group's standing on the screen from its members drawn there, and the members that crowd
 *  another of their group. */
export function groupsOnScreen(members: readonly MemberOnScreen[]): { groups: Map<string, GroupOnScreen>; crowded: Set<number> } {
  const crowdedUids = new Set<number>()
  const by = new Map<string, MemberOnScreen[]>()
  for (const m of members) {
    const list = by.get(m.group)
    if (list) list.push(m)
    else by.set(m.group, [m])
  }
  const out = new Map<string, GroupOnScreen>()
  for (const [id, ms] of by) {
    // crowded: another member within CROWD_PX (a grid of that size: each member looks at 9 cells)
    const grid = new Map<number, MemberOnScreen[]>()
    const cell = (x: number, y: number) => Math.floor(x / CROWD_PX) * 100_003 + Math.floor(y / CROWD_PX)
    for (const m of ms) {
      const k = cell(m.x, m.y)
      const list = grid.get(k)
      if (list) list.push(m)
      else grid.set(k, [m])
    }
    let crowded = 0
    const r2 = CROWD_PX * CROWD_PX
    for (const m of ms) {
      const gx = Math.floor(m.x / CROWD_PX)
      const gy = Math.floor(m.y / CROWD_PX)
      let near = false
      for (let dx = -1; dx <= 1 && !near; dx++)
        for (let dy = -1; dy <= 1 && !near; dy++)
          for (const o of grid.get((gx + dx) * 100_003 + gy + dy) ?? [])
            if (o !== m && (o.x - m.x) ** 2 + (o.y - m.y) ** 2 < r2) {
              near = true
              break
            }
      if (near) {
        crowded++
        crowdedUids.add(m.uid)
      }
    }
    let top = ms[0]
    let lon = 0
    let lat = 0
    for (const m of ms) {
      if (m.y - m.halfH < top.y - top.halfH) top = m
      lon += m.lon
      lat += m.lat
    }
    out.set(id, {
      id,
      lod: ms.length > 1 && crowded >= PILE_SHARE * ms.length ? 'pile' : 'apart',
      n: ms.length,
      xAt: [lon / ms.length, lat / ms.length, top.z],
      yAt: [top.lon, top.lat, top.z],
      lift: top.halfH + 10,
    })
  }
  return { groups: out, crowded: crowdedUids }
}

// ---- the chips' sizes and places --------------------------------------------------------------

/** px: the chip's two lines, the second only with an exception (GroupChips.tsx, swarms.css .gchip) */
const LINE1_H = 24
const LINE2_H = 19
const PAD_X = 8
/** the name after the id, cut to this many characters on the chip (whole on hover) */
export const NAME_MAX = 18

export function chipName(name: string): string {
  return name.length > NAME_MAX ? `${name.slice(0, NAME_MAX - 1)}…` : name
}

/** A group chip's line two: only what is wrong with it (swarmsModel chipExceptions); '' when nothing. */
export function chipLine(m: Pick<GroupMark, 'parts'>): string {
  return chipExceptions(m.parts).map((p) => p.text).join(' · ')
}

/** A group chip's size, px, as swarms.css lays it out (IBM Plex Mono's advance is 0.6 em; the name
 *  and the exception line are in the condensed sans, narrower, counted as mono to be safe): one
 *  line, two with an exception. */
export function chipSize(m: Pick<GroupMark, 'code' | 'id' | 'name' | 'parts'>): { w: number; h: number } {
  const line1 = PAD_X + textWidth(m.code, 11) + 10 + 6 + textWidth(m.id, 14) + (m.name ? 6 + textWidth(chipName(m.name), 12) : 0) + PAD_X
  const text = chipLine(m)
  if (!text) return { w: line1, h: LINE1_H }
  return { w: Math.max(line1, PAD_X + textWidth(text, 11) + PAD_X), h: LINE1_H + LINE2_H }
}

export interface CellChip {
  /** `x:y`, the tile */
  key: string
  total: number
  /** the groups in it, most first: [group id, members in the tile] */
  groups: [string, number][]
  /** members of no group */
  none: number
}

/** A cell chip's text: its largest group's id and count in the tile, then the next two groups by
 *  code and count, how many more groups, and how many of no group. */
export function cellChipText(c: CellChip, codeOf: (id: string) => string): { head: string; rest: string } {
  const [first, ...others] = c.groups
  const rest = others.slice(0, 2).map(([id, n]) => `${codeOf(id)} ${n}`)
  if (others.length > 2) rest.push(`+${others.length - 2} groups`)
  if (c.none) rest.push(`${c.none} ungrouped`)
  return { head: first ? `${first[0]} ${first[1]}` : `${c.total}`, rest: rest.join(' · ') }
}

export function cellChipSize(c: CellChip, codeOf: (id: string) => string): { w: number; h: number } {
  const { head, rest } = cellChipText(c, codeOf)
  const first = c.groups[0]
  const w = PAD_X + (first ? textWidth(codeOf(first[0]), 10) + 8 + 5 : 0) + textWidth(head, 12) + (rest ? 6 + textWidth(rest, 10.5) : 0) + PAD_X
  return { w, h: 22 }
}

/** Web Mercator tile of a point at tile zoom `z`, as the bridge counts its cells (push.rs `tile`). */
export function tileOf(lon: number, lat: number, z: number): [number, number] {
  const n = 2 ** z
  const x = Math.floor(((lon + 180) / 360) * n)
  const r = (Math.max(-85.0511, Math.min(85.0511, lat)) * Math.PI) / 180
  const y = Math.floor(((1 - Math.log(Math.tan(r) + 1 / Math.cos(r)) / Math.PI) / 2) * n)
  return [x, y]
}

/** Which groups fill each tile at `z`: every vehicle with a position (those the bridge sends lite
 *  too), by its group. Only tiles with a group member. */
export function cellGroups(vehicles: readonly { uid: number; position: LngLat | null }[], groupOf: (uid: number) => string | null, z: number): Map<string, CellChip> {
  const out = new Map<string, { total: number; by: Map<string, number>; none: number }>()
  for (const v of vehicles) {
    if (!v.position) continue
    const [x, y] = tileOf(v.position[0], v.position[1], z)
    const key = `${x}:${y}`
    let c = out.get(key)
    if (!c) out.set(key, (c = { total: 0, by: new Map(), none: 0 }))
    c.total++
    const g = groupOf(v.uid)
    if (g) c.by.set(g, (c.by.get(g) ?? 0) + 1)
    else c.none++
  }
  const cells = new Map<string, CellChip>()
  for (const [key, c] of out) {
    if (!c.by.size) continue
    cells.set(key, { key, total: c.total, none: c.none, groups: [...c.by].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1)) })
  }
  return cells
}

/** Stack chips that would overlap, the most important first (the caller's order): each chip takes
 *  the first of `tries` places, from over its group up in steps of its height, where it overlaps no
 *  chip placed before it (a leader line then runs down to its group); one with no free place stays
 *  over its group. Returns each one's lift above its own place, px, and whether it found room. */
export function stackChips(chips: readonly { x: number; y: number; w: number; h: number }[], tries = 6): { up: number; free: boolean }[] {
  const placed: [number, number, number, number][] = []
  const clear = (b: [number, number, number, number]) => placed.every((o) => b[0] >= o[2] || o[0] >= b[2] || b[1] >= o[3] || o[1] >= b[3])
  return chips.map((c) => {
    for (let k = 0; k < tries; k++) {
      const up = k * (c.h + 4)
      const b: [number, number, number, number] = [c.x - c.w / 2 - 2, c.y - c.h - up - 2, c.x + c.w / 2 + 2, c.y - up + 2]
      if (clear(b)) {
        placed.push(b)
        return { up, free: true }
      }
    }
    return { up: 0, free: false }
  })
}
