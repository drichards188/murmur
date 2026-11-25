// Where a floating panel goes (FloatingPanel.tsx): the pure part, in its float layer's own px from
// that layer's top left. It serves R20 (moving the view) and rule 6 of docs/CONSOLE-REQUIREMENTS.md
// (a panel does not hide the scene it is about): a panel the person has not placed opens on the
// free spot nearest its own corner, clear of every other open panel and of what is fixed on the map
// (the map's toolbar, the banners, the zoom buttons, the weather bar, the group strip, the coach
// card); with no free spot it cascades from its corner rather than lie exactly on another. Once the
// person drags it, their spot holds, its bar always inside the area. Pure: floatPlace.check.ts.

export interface Rect {
  x: number
  y: number
  w: number
  h: number
}

export interface Size {
  w: number
  h: number
}

export interface Point {
  x: number
  y: number
}

/** The corner a panel prefers, close to where it sat before it could be moved. */
export type Corner = 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right'

/** Where a panel sits, kept from the area's edges so it stays put as the area changes: across from
 *  the left (`ax` 'l') or the right ('r') edge to the panel's same side, down from the top ('t') or
 *  up from the bottom ('b'). A panel kept from the right stays beside a drawer that opens there; one
 *  kept from the bottom grows upward. */
export interface Spot {
  ax: 'l' | 'r'
  x: number
  ay: 't' | 'b'
  y: number
}

/** A panel's spot on screen: CSS offsets in its layer, and the height it may take there. */
export interface Placed {
  left?: number
  right?: number
  top?: number
  bottom?: number
  maxHeight: number
  /** its top left in the layer, after the clamp */
  at: Point
}

/** From the area's edges to a placed panel, px. */
export const MARGIN = 12
/** Between a placed panel and what it keeps clear of, px. */
export const GAP = 8
/** Each step of a cascade, px across and down. */
export const CASCADE = 28
/** The least of a panel's top (its bar) that always stays inside the area, px. */
export const BAR = 32
/** A move inward (across the map) costs this many times a move along the edge: panels stack down
 *  their own edge before they take the middle of the map, the scene. */
export const INWARD = 2
/** A cascade gives up after this many steps (the last step stands). */
export const CASCADE_MAX = 24

const EPS = 0.01

/** Whether two rects are nearer than `gap` (0: whether they overlap at all). */
export function overlaps(a: Rect, b: Rect, gap = 0): boolean {
  return a.x < b.x + b.w + gap - EPS && b.x < a.x + a.w + gap - EPS && a.y < b.y + b.h + gap - EPS && b.y < a.y + a.h + gap - EPS
}

/** The panel's spot in its own corner, MARGIN in from the area's edges. */
export function cornerAt(size: Size, corner: Corner, area: Rect): Point {
  return {
    x: corner.endsWith('left') ? area.x + MARGIN : area.x + area.w - MARGIN - size.w,
    y: corner.startsWith('top') ? area.y + MARGIN : area.y + area.h - MARGIN - size.h,
  }
}

/** Whether a rect lies inside the area with MARGIN to spare on every side. */
function fits(r: Rect, area: Rect): boolean {
  return r.x >= area.x + MARGIN - EPS && r.y >= area.y + MARGIN - EPS && r.x + r.w <= area.x + area.w - MARGIN + EPS && r.y + r.h <= area.y + area.h - MARGIN + EPS
}

/** A top left moved inside the area with MARGIN to spare; centred across when it is wider than
 *  that, at the top margin when taller (its height then gives way). */
export function inside(p: Point, size: Size, area: Rect): Point {
  const loX = area.x + MARGIN
  const hiX = area.x + area.w - MARGIN - size.w
  const loY = area.y + MARGIN
  const hiY = area.y + area.h - MARGIN - size.h
  return {
    x: hiX < loX ? area.x + Math.max(0, area.w - size.w) / 2 : Math.min(Math.max(p.x, loX), hiX),
    y: hiY < loY ? loY : Math.min(Math.max(p.y, loY), hiY),
  }
}

/** The free spot nearest the panel's corner: inside the area with MARGIN to spare and GAP from
 *  everything in `avoid`; null when there is none. The candidates are the corner's own spot, the
 *  area's edges and each side of everything to avoid; the cheapest wins (a move inward counts
 *  INWARD times a move along the edge), then the highest, then the leftmost. */
export function freeSpot(size: Size, corner: Corner, area: Rect, avoid: readonly Rect[]): Point | null {
  const home = cornerAt(size, corner, area)
  const xs = [home.x, area.x + MARGIN, area.x + area.w - MARGIN - size.w]
  const ys = [home.y, area.y + MARGIN, area.y + area.h - MARGIN - size.h]
  for (const r of avoid) {
    xs.push(r.x + r.w + GAP, r.x - GAP - size.w)
    ys.push(r.y + r.h + GAP, r.y - GAP - size.h)
  }
  let best: Point | null = null
  let bestCost = Infinity
  for (const x of xs) {
    for (const y of ys) {
      const r = { x, y, w: size.w, h: size.h }
      if (!fits(r, area) || avoid.some((o) => overlaps(r, o, GAP))) continue
      const cost = INWARD * Math.abs(x - home.x) + Math.abs(y - home.y)
      if (cost < bestCost - EPS || (best && Math.abs(cost - bestCost) <= EPS && (y < best.y - EPS || (Math.abs(y - best.y) <= EPS && x < best.x)))) {
        best = { x, y }
        bestCost = cost
      }
    }
  }
  return best
}

/** With no free spot: from `start` (by default the corner's own spot), CASCADE across and down
 *  (inward from its corner) until no open panel has its corner there, so a panel never lies exactly
 *  on another. */
export function cascadeSpot(size: Size, corner: Corner, area: Rect, panels: readonly Rect[], start?: Point): Point {
  const left = corner.endsWith('left')
  const top = corner.startsWith('top')
  const cornerOf = (x: number, y: number, w: number, h: number) => ({ x: left ? x : x + w, y: top ? y : y + h })
  const taken = panels.map((r) => cornerOf(r.x, r.y, r.w, r.h))
  let p = inside(start ?? cornerAt(size, corner, area), size, area)
  for (let k = 0; k < CASCADE_MAX; k++) {
    const c = cornerOf(p.x, p.y, size.w, size.h)
    if (!taken.some((t) => Math.abs(t.x - c.x) < CASCADE / 2 && Math.abs(t.y - c.y) < CASCADE / 2)) return p
    const next = inside({ x: p.x + (left ? CASCADE : -CASCADE), y: p.y + (top ? CASCADE : -CASCADE) }, size, area)
    if (Math.abs(next.x - p.x) < EPS && Math.abs(next.y - p.y) < EPS) break
    p = next
  }
  return p
}

/** Where a panel opens: the free spot nearest its corner (clear of `obstacles` and the open
 *  `panels`), else a cascade over the open panels from the spot nearest its corner that is clear of
 *  the obstacles alone (so it still keeps off the map's toolbar and buttons), or from its corner. */
export function place(size: Size, corner: Corner, area: Rect, obstacles: readonly Rect[], panels: readonly Rect[]): { at: Point; free: boolean } {
  const free = freeSpot(size, corner, area, [...obstacles, ...panels])
  if (free) return { at: free, free: true }
  return { at: cascadeSpot(size, corner, area, panels, freeSpot(size, corner, area, obstacles) ?? undefined), free: false }
}

/** A top left kept so the panel's bar stays inside the area: its whole width inside (at the area's
 *  left when wider), its top from the area's top to BAR above its bottom. */
export function clampBar(p: Point, size: Size, area: Rect): Point {
  return {
    x: Math.min(Math.max(p.x, area.x), Math.max(area.x, area.x + area.w - size.w)),
    y: Math.min(Math.max(p.y, area.y), Math.max(area.y, area.y + area.h - BAR)),
  }
}

/** The spot of a panel whose top left is `p`: from the nearer side across (by its centre); from the
 *  bottom only when `fromBottom` asks, its centre is in the lower half and the whole of it is inside
 *  (else from the top). Whole px. */
export function spotOf(p: Point, size: Size, area: Rect, fromBottom: boolean): Spot {
  const ax = p.x + size.w / 2 > area.x + area.w / 2 ? 'r' : 'l'
  const below = area.y + area.h - (p.y + size.h)
  const ay = fromBottom && below >= -0.5 && p.y + size.h / 2 > area.y + area.h / 2 ? 'b' : 't'
  return {
    ax,
    x: Math.round(ax === 'l' ? p.x - area.x : area.x + area.w - (p.x + size.w)),
    ay,
    y: Math.round(ay === 't' ? p.y - area.y : Math.max(0, below)),
  }
}

/** The top left a spot gives a panel of this size, its bar kept inside the area (clampBar). */
export function spotAt(s: Spot, size: Size, area: Rect): Point {
  return clampBar(
    {
      x: s.ax === 'l' ? area.x + s.x : area.x + area.w - s.x - size.w,
      y: s.ay === 't' ? area.y + s.y : area.y + area.h - s.y - size.h,
    },
    size,
    area,
  )
}

/** A spot laid out in a layer of size `layer`: offsets from the layer's edges on the spot's own
 *  sides (so the panel grows away from them), after the clamp, and the height it may take, from its
 *  kept edge to the area's far edge (never less than BAR). */
export function layoutOf(s: Spot, size: Size, area: Rect, layer: Size): Placed {
  const at = spotAt(s, size, area)
  const out: Placed = { maxHeight: 0, at }
  if (s.ax === 'l') out.left = at.x
  else out.right = layer.w - (at.x + size.w)
  if (s.ay === 't') {
    out.top = at.y
    out.maxHeight = Math.max(BAR, area.y + area.h - at.y)
  } else {
    out.bottom = layer.h - (at.y + size.h)
    out.maxHeight = Math.max(BAR, at.y + size.h - area.y)
  }
  return out
}

/** Whether two areas are the same, to the half px. */
export function sameRect(a: Rect, b: Rect): boolean {
  return Math.abs(a.x - b.x) < 0.5 && Math.abs(a.y - b.y) < 0.5 && Math.abs(a.w - b.w) < 0.5 && Math.abs(a.h - b.h) < 0.5
}
