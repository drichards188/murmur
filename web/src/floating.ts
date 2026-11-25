// The floating panels' layers (FloatingPanel.tsx): what each layer measures of the page (its area,
// what is fixed in it, the panels open on it) and the order they stack in. Display only: where a
// panel sits never changes what it shows or asks. The placement itself is floatPlace.ts.
import { createContext, useContext, type PointerEventHandler, type MouseEventHandler } from 'react'
import { MARGIN, type Rect, type Size, sameRect } from './floatPlace'

/** What a layer measures: its own size, the area its panels keep to (its px), what is fixed there
 *  and the panels keep clear of, and (the rail's layer) the rail, where its panel docks. */
export interface FloatGeometry {
  layer: Size
  area: Rect
  obstacles: Rect[]
  dock: Rect | null
}

const NOTHING: FloatGeometry = { layer: { w: 0, h: 0 }, area: { x: 0, y: 0, w: 0, h: 0 }, obstacles: [], dock: null }

/** What is fixed on the map that an opening panel keeps clear of: the map's toolbar (top left), the
 *  banners (top centre), the zoom buttons, compass, attribution and scale (maplibre's corners), the
 *  weather bar and the group strip (bottom), the tutorial's coach card, the perf overlay. */
const MAP_FIXED = [
  '.viewtoggle',
  ':scope > .act-status',
  ':scope > .impact-status',
  ':scope > .banner',
  '.maplibregl-ctrl-top-right > *',
  '.maplibregl-ctrl-bottom-right > *',
  '.maplibregl-ctrl-bottom-left > *',
  '.weather-legend',
  '.gstrip > *',
  '.tut',
  '.tut-chip',
  '.perf',
].join(', ')

const relTo = (o: DOMRect) => (r: DOMRect): Rect => ({ x: r.left - o.left, y: r.top - o.top, w: r.width, h: r.height })

/** The map's layer (in the map, `.mapwrap`): the area is the map less the drawer docked on its right
 *  and the Space timeline docked at its bottom (nothing is placed under either, and a panel's bar
 *  never goes there). */
export function measureMap(layer: HTMLElement): FloatGeometry {
  const L = layer.getBoundingClientRect()
  const rel = relTo(L)
  const host = layer.parentElement
  let w = L.width
  let h = L.height
  const obstacles: Rect[] = []
  if (host) {
    for (const d of host.querySelectorAll(':scope > .ix')) {
      const r = d.getBoundingClientRect()
      if (r.width > 0) w = Math.min(w, r.left - L.left)
    }
    for (const s of host.querySelectorAll(':scope > .space')) {
      const r = s.getBoundingClientRect()
      if (r.height > 0) h = Math.min(h, r.top - L.top)
    }
    for (const el of host.querySelectorAll(MAP_FIXED)) {
      if (layer.contains(el)) continue
      const r = el.getBoundingClientRect()
      if (r.width > 0 && r.height > 0) obstacles.push(rel(r))
    }
  }
  return { layer: { w: L.width, h: L.height }, area: { x: 0, y: 0, w: Math.max(0, w), h: Math.max(0, h) }, obstacles, dock: null }
}

/** The rail's layer (the nomination form's): it reaches left over the map (`over`), so the form can
 *  be dragged out of the rail over the map; it docks on the rail. */
export function measureRail(layer: HTMLElement, over: HTMLElement | null): FloatGeometry {
  const host = layer.parentElement
  if (host && over) {
    // from the rail's padding edge (inside its border), where the layer's `left` counts from
    const reach = over.getBoundingClientRect().left - (host.getBoundingClientRect().left + host.clientLeft)
    const want = `${Math.min(0, reach)}px`
    if (layer.style.left !== want) layer.style.left = want
  }
  const L = layer.getBoundingClientRect()
  // the rail inside its border, where the form always sat
  const H = host?.getBoundingClientRect()
  const dock = host && H ? { x: H.left + host.clientLeft - L.left, y: H.top + host.clientTop - L.top, w: host.clientWidth, h: host.clientHeight } : null
  return { layer: { w: L.width, h: L.height }, area: { x: 0, y: 0, w: L.width, h: L.height }, obstacles: [], dock }
}

/** The panels open on a layer but `self` (in its px), each as tall as the height it was placed
 *  for when it has not filled in yet (`data-reserve`; upward for one kept from the bottom), so a
 *  panel opened next keeps clear of where the first one will be. One still being placed is not
 *  counted. */
export function openRects(layer: HTMLElement, self: Element | null, area: Rect): Rect[] {
  const L = layer.getBoundingClientRect()
  const out: Rect[] = []
  for (const el of layer.children) {
    if (el === self || !(el instanceof HTMLElement) || !el.classList.contains('float') || el.hasAttribute('data-placing')) continue
    const r = el.getBoundingClientRect()
    if (r.width <= 0 || r.height <= 0) continue
    const rect = { x: r.left - L.left, y: r.top - L.top, w: r.width, h: r.height }
    const reserve = Math.min(Number(el.dataset.reserve) || 0, area.h - 2 * MARGIN)
    if (reserve > rect.h) {
      if (el.dataset.ay === 'b') rect.y -= reserve - rect.h
      rect.h = reserve
    }
    out.push(rect)
  }
  return out
}

/** A layer's panels: their stacking order (the one used last in front) and its geometry, read again
 *  whenever the page changes under it; the panels re-render on either. The layer element is the
 *  one FloatLayer attaches, or until then the one a panel names (its own parent: a panel's layout
 *  effects run before its layer's). */
export class FloatStore {
  private order: string[] = []
  private gen = 0
  private geo: FloatGeometry = NOTHING
  private readonly subs = new Set<() => void>()
  private readonly kind: 'map' | 'rail'
  private el: HTMLElement | null = null
  private over: HTMLElement | null = null

  constructor(kind: 'map' | 'rail') {
    this.kind = kind
  }

  /** The layer element, and the map the rail's layer reaches over. */
  attach(el: HTMLElement | null, over: HTMLElement | null) {
    this.el = el
    this.over = over
  }

  subscribe = (fn: () => void) => {
    this.subs.add(fn)
    return () => {
      this.subs.delete(fn)
    }
  }

  version = () => this.gen

  private bump() {
    this.gen++
    for (const fn of this.subs) fn()
  }

  /** The layer element (its panels are its children); `named` until FloatLayer attached one. */
  layer(named?: HTMLElement | null): HTMLElement | null {
    return this.el ?? named ?? null
  }

  /** Measure now; the panels hear of it when the area, the layer or the dock moved (whoever
   *  measured: a panel placed in the same commit as a drawer opened must not leave the others
   *  where the drawer was). */
  measure(named?: HTMLElement | null): FloatGeometry {
    const el = this.layer(named)
    if (!el) return this.geo
    const before = this.geo
    const after = this.kind === 'map' ? measureMap(el) : measureRail(el, this.over)
    this.geo = after
    const same =
      sameRect(before.area, after.area) &&
      Math.abs(before.layer.w - after.layer.w) < 0.5 &&
      Math.abs(before.layer.h - after.layer.h) < 0.5 &&
      (before.dock == null) === (after.dock == null) &&
      (before.dock == null || after.dock == null || sameRect(before.dock, after.dock))
    if (!same) this.bump()
    return after
  }

  /** As last measured. */
  geometry(): FloatGeometry {
    return this.geo
  }

  /** A panel's place in the stack: 1 at the back. */
  z(id: string): number {
    const i = this.order.indexOf(id)
    return i < 0 ? this.order.length + 1 : i + 1
  }

  /** In front of the others (opened, clicked or dragged). */
  raise(id: string) {
    if (this.order[this.order.length - 1] === id) return
    this.order = [...this.order.filter((x) => x !== id), id]
    this.bump()
  }

  /** Closed: out of the stack (the others keep their order). */
  leave(id: string) {
    this.order = this.order.filter((x) => x !== id)
  }
}

export const FloatContext = createContext<FloatStore | null>(null)

/** What a panel's bar takes (its header row): the drag and the double-click that puts it back. */
export interface FloatBarProps {
  onPointerDown: PointerEventHandler<HTMLElement>
  onPointerMove: PointerEventHandler<HTMLElement>
  onPointerUp: PointerEventHandler<HTMLElement>
  onPointerCancel: PointerEventHandler<HTMLElement>
  onLostPointerCapture: PointerEventHandler<HTMLElement>
  onDoubleClick: MouseEventHandler<HTMLElement>
  'data-float-bar': ''
}

export const FloatBarContext = createContext<FloatBarProps | null>(null)

/** The props a panel spreads on its header row to make it the drag bar; nothing outside a
 *  FloatingPanel (the panel then sits as its own CSS has it). */
export function useFloatBar(): Partial<FloatBarProps> {
  return useContext(FloatBarContext) ?? {}
}

/** What a drag never starts from: the bar's own controls keep working as before. */
export const NOT_A_HANDLE = 'button, a, input, select, textarea, summary, label, [role="button"], [contenteditable="true"], [data-no-drag]'
