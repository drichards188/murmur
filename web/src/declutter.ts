// One label budget for every map text layer (murmur-02g for our vehicles; extended to tracks,
// lost contacts, estimates, group names and order labels by docs/research/saturation-display.md
// R2). Greedy, in screen space, by priority:
//
//   1  the selected and focused (focus.ts), items waiting on the person
//   2  group names (and other aggregates)
//   3  our vehicles with an alert (a lost link), orders not yet confirmed
//   4  the rest
//
// Each label has two forms: a short one (a callsign, a track id) and its full block (kind and
// level, height, silent time). Priority 1 gets the full block; the rest the short one, unless the
// view holds few labels (FEW), where everyone keeps the full block as before. A label is placed
// only where its box overlaps no label placed before it; one that does not fit in full falls back
// to its short form, then to none (the symbol stays, the full block shows on hover). A forced
// label (selected, pinned, details open) is placed whatever it overlaps. Each priority has a zoom
// floor (below it, symbols alone). Display only.

export type LabelLevel = 'full' | 'short' | 'none'
export type Priority = 1 | 2 | 3 | 4

/** [left, top, right, bottom] around the anchor, CSS pixels */
export type Box = [number, number, number, number]

export interface LabelCandidate {
  /** unique across layers: `v:<uid>` a vehicle (`vl:<uid>` its exception slot), `t:<id>` a track,
   *  `l:<id>` a lost contact, `e:...` an estimate, `s:<id>` a group, `o:...` an order label,
   *  `c:...` a cluster marker, `x:<key>` an engagement's label, `a:<key>` an observed act's label
   *  (`at:<key>` its id once the label has faded) */
  key: string
  /** where the anchor is drawn, CSS pixels in the map; null behind the camera */
  at: [number, number] | null
  short: Box
  full: Box
  priority: Priority
  /** placed whatever it overlaps (still taking its room) */
  force?: boolean
  /** takes room but is drawn by its own layer whatever the plan says (a cluster marker): not
   *  counted as a label */
  obstacle?: boolean
}

export interface LabelPlan {
  level(key: string): LabelLevel
}

/** below this zoom, symbols alone for the rest (a whole region is on screen; labels would be noise) */
export const LABEL_MIN_ZOOM = 9
/** each priority's zoom floor: group names from any zoom, our alerts and the rest from 9 */
export const ZOOM_FLOOR: Record<Priority, number> = { 1: 0, 2: 0, 3: LABEL_MIN_ZOOM, 4: LABEL_MIN_ZOOM }
/** at most this many labels in view: everyone keeps the full block (the small situations) */
export const FEW = 40
/** room kept between two labels, px */
const PAD = 2
const CELL = 64

/** A monospace label's width in px at `size` (IBM Plex Mono's advance is 0.6 em). */
export function textWidth(text: string, size: number): number {
  return Math.ceil(text.length * size * 0.6)
}

/** The box of a text drawn right of a symbol `halfW` wide (and the symbol), `size` px. */
export function rightOf(halfW: number, text: string, size: number): Box {
  const h = size * 0.75 + 2
  return [-halfW, -h, halfW + 4 + textWidth(text, size), h]
}

/** The box of texts left and right of a symbol. */
export function besides(halfW: number, left: string, leftSize: number, right: string, rightSize: number): Box {
  const h = Math.max(leftSize, rightSize) * 0.75 + 2
  return [-(halfW + 4 + (left ? textWidth(left, leftSize) : 0)), -h, halfW + 4 + textWidth(right, rightSize), h]
}

/** The box of a text centred above (dy < 0) or below (dy > 0) the anchor, its near edge `dy` away. */
export function centred(text: string, size: number, dy: number, pad = 0): Box {
  const w = textWidth(text, size) / 2 + pad
  const h = size + 2 + 2 * pad
  return dy < 0 ? [-w, dy - h, w, dy] : [-w, dy, w, dy + h]
}

/** The box of a block of `lines` at `size` px (line height 1.25 for more than one) drawn as deck.gl's
 *  TextLayer draws it: from its anchor `[dx, dy]` px off the point, `anchor` the block's horizontal
 *  side there (start: its left edge), `baseline` its vertical one, `pad` px of background round it,
 *  and `room` px kept clear beyond that. An event's label (an engagement's, an act's) keeps some
 *  room: one sitting flush under a callsign read as a sentence with it ("HUNTER 1 ASSESSED
 *  PERSISTS", bd murmur-iobd.3). */
export function textBox(lines: readonly string[], size: number, anchor: 'start' | 'middle', baseline: 'top' | 'bottom', [dx, dy]: [number, number], pad: [number, number] = [0, 0], room = 0): Box {
  const w = Math.max(0, ...lines.map((l) => textWidth(l, size))) + 2 * pad[0]
  const h = (lines.length > 1 ? lines.length * size * 1.25 : size + 2) + 2 * pad[1]
  const left = anchor === 'start' ? dx - pad[0] : dx - w / 2
  const top = baseline === 'top' ? dy - pad[1] : dy + pad[1] - h
  return [left - room, top - room, left + w + room, top + h + room]
}

const NONE: LabelPlan = { level: () => 'none' }

export interface PlanOptions {
  zoom: number
  /** the map's size, CSS px: labels anchored outside it are not placed */
  width: number
  height: number
}

/** The plan: each candidate's level. Candidates of equal priority keep their order (the caller's
 *  order is the tie-break: give it a stable one). */
export function planLabels(cands: readonly LabelCandidate[], { zoom, width, height }: PlanOptions): LabelPlan {
  if (!cands.length) return NONE
  const levels = new Map<string, LabelLevel>()
  const margin = 40
  const inView = cands.filter((c) => c.at && c.at[0] >= -margin && c.at[0] <= width + margin && c.at[1] >= -margin && c.at[1] <= height + margin)
  const eligible = inView.filter((c) => c.force || c.obstacle || zoom >= ZOOM_FLOOR[c.priority])
  const labels = eligible.filter((c) => !c.obstacle).length
  const few = labels <= FEW
  // obstacles, forced, then by priority, each in the caller's order (buckets: no sort of thousands)
  const buckets: LabelCandidate[][] = [[], [], [], [], [], []]
  for (const c of eligible) buckets[c.obstacle ? 0 : c.force ? 1 : c.priority + 1].push(c)
  // the boxes placed, in a grid of CELL px for the overlap test
  const grid = new Map<number, [number, number, number, number][]>()
  const abs = (c: LabelCandidate, b: Box): [number, number, number, number] => [c.at![0] + b[0] - PAD, c.at![1] + b[1] - PAD, c.at![0] + b[2] + PAD, c.at![1] + b[3] + PAD]
  const clear = (b: [number, number, number, number]) => {
    for (let gx = Math.floor(b[0] / CELL); gx <= Math.floor(b[2] / CELL); gx++)
      for (let gy = Math.floor(b[1] / CELL); gy <= Math.floor(b[3] / CELL); gy++) {
        const list = grid.get(gx * 100_003 + gy)
        if (list) for (const o of list) if (b[0] < o[2] && o[0] < b[2] && b[1] < o[3] && o[1] < b[3]) return false
      }
    return true
  }
  const take = (b: [number, number, number, number]) => {
    for (let gx = Math.floor(b[0] / CELL); gx <= Math.floor(b[2] / CELL); gx++)
      for (let gy = Math.floor(b[1] / CELL); gy <= Math.floor(b[3] / CELL); gy++) {
        const cell = gx * 100_003 + gy
        const list = grid.get(cell)
        if (list) list.push(b)
        else grid.set(cell, [b])
      }
  }
  for (const c of buckets.flat()) {
    const wantFull = c.force || c.obstacle || c.priority === 1 || few
    const fb = abs(c, c.full)
    if (wantFull && (c.force || c.obstacle || clear(fb))) {
      take(fb)
      if (!c.obstacle) levels.set(c.key, 'full')
      continue
    }
    const sb = abs(c, c.short)
    if (clear(sb)) {
      take(sb)
      levels.set(c.key, 'short')
    }
  }
  return { level: (k) => levels.get(k) ?? 'none' }
}

/** Every label in full: before the map has a camera, and for builders called without a plan. */
export const ALL_FULL: LabelPlan = { level: () => 'full' }
