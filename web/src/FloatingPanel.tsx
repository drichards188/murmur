// The panels that pop out over the map (Weather, Links, Platforms, the globe, the vehicle details,
// the Situation panel) and the nomination form, each moved by the bar at its top (R20, moving the
// view; rule 6 of docs/CONSOLE-REQUIREMENTS.md, a panel does not hide the scene it is about):
// - a panel the person has not placed opens on the free spot nearest its own corner, clear of the
//   other open panels and of what is fixed on the map (floatPlace.ts), placed once, then left;
// - dragged by its bar (pointer capture: mouse, pen or touch), its bar always inside the area, it
//   keeps the person's spot when closed and opened again (floatLayout.ts, this browser only);
// - double-clicking its bar puts it back on its own free spot;
// - the one clicked or dragged last is in front of the others (never above the top bar or menus).
// Display only: nothing a panel shows or asks changes, only where it sits.
import { type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode, type RefObject, useContext, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react'
import { FloatBarContext, type FloatBarProps, FloatContext, FloatStore, NOT_A_HANDLE, openRects } from './floating'
import { savedLayout, saveSpot } from './floatLayout'
import { clampBar, type Corner, MARGIN, layoutOf, place, type Rect, type Size, type Spot, spotOf } from './floatPlace'
import './floating.css'

/** A press on the bar moves the panel once the pointer has gone this far, px (less is a click). */
const DRAG_START = 4

/** The layer a set of floating panels share: `map` in the map (the area is the map less a docked
 *  drawer or timeline), `rail` in the right rail for the nomination form (its area reaches over the
 *  map; `over` is the map). */
export function FloatLayer({ kind, over, children }: { kind: 'map' | 'rail'; over?: RefObject<HTMLElement | null>; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null)
  const [store] = useState(() => new FloatStore(kind))
  useLayoutEffect(() => {
    const el = ref.current
    const host = el?.parentElement
    if (!el || !host) return
    const map = over?.current ?? null
    store.attach(el, map)
    const again = () => {
      store.measure()
    }
    again()
    // the window, the map or the layer resized
    const ro = new ResizeObserver(again)
    ro.observe(el)
    if (map) ro.observe(map)
    // a drawer or the Space timeline docked or gone: the map's own children change
    const mo = new MutationObserver(again)
    if (kind === 'map') mo.observe(host, { childList: true })
    return () => {
      ro.disconnect()
      mo.disconnect()
      store.attach(null, null)
    }
  }, [store, kind, over])
  return (
    <FloatContext.Provider value={store}>
      <div ref={ref} className={`floats ${kind}`}>
        {children}
      </div>
    </FloatContext.Provider>
  )
}

interface PanelProps {
  /** what its spot is remembered by ("weather", "vehicle-details") */
  id: string
  /** the corner it opens in when that is free */
  prefer: Corner
  /** the height it is placed for, px, when it opens smaller and fills in as its data comes */
  reserve?: number
  /** placed afresh when this changes (the Situation chip opening into the panel), unless the
   *  person placed it */
  variant?: string
  /** its own spot is the layer's dock (the nomination form's rail) until it is dragged out */
  docked?: boolean
  children: ReactNode
}

/** One floating panel. Its header spreads `useFloatBar()` (floating.ts) and shows a FloatGrip; the
 *  panel's own CSS keeps its width, the height it takes here is the room from its spot. */
export function FloatingPanel(props: PanelProps) {
  const store = useContext(FloatContext)
  return store ? <Floating store={store} {...props} /> : <>{props.children}</>
}

/** The grip mark at the start of a panel's bar: it can be dragged. Nothing outside a FloatingPanel. */
export function FloatGrip() {
  const bar = useContext(FloatBarContext)
  return bar ? <span className="float-grip" aria-hidden="true" title="Drag this bar to move the panel. Double-click it to put the panel back." /> : null
}

interface Grab {
  pointer: number
  /** where the press was, and the last move */
  px: number
  py: number
  lx: number
  ly: number
  /** the panel then, in the layer's px */
  x: number
  y: number
  w: number
  h: number
  moved: boolean
}

function Floating({ store, id, prefer, reserve = 0, variant = '', docked = false, children }: PanelProps & { store: FloatStore }) {
  useSyncExternalStore(store.subscribe, store.version)
  const ref = useRef<HTMLDivElement>(null)
  // the person's spot (dragged here, or remembered from before), else the one it was placed on
  const [mine, setMine] = useState<Spot | null>(() => savedLayout()[id] ?? null)
  const [auto, setAuto] = useState<Spot | null>(null)
  const [size, setSize] = useState<Size | null>(null)
  const [drag, setDrag] = useState<Rect | null>(null)
  // a double-click on the bar: placed afresh
  const [fresh, setFresh] = useState(0)
  const grab = useRef<Grab | null>(null)

  // in front when it opens; out of the stack when it closes. Its layer measured now: one that opens
  // with its layer (after a sign-in) is laid out before the layer's own effect has measured it.
  useLayoutEffect(() => {
    store.measure(ref.current?.parentElement)
    store.raise(id)
    return () => store.leave(id)
  }, [store, id])

  // its size as it renders (it may fill in after it opens: the spot holds, only the clamp reads it)
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const read = () => {
      const r = el.getBoundingClientRect()
      setSize((s) => (s && Math.abs(s.w - r.width) < 0.5 && Math.abs(s.h - r.height) < 0.5 ? s : { w: r.width, h: r.height }))
    }
    read()
    const ro = new ResizeObserver(read)
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // placed once when it opens (again when its variant changes or the person resets it), before the
  // browser paints it; never while the person's spot holds
  useLayoutEffect(() => {
    if (mine || docked) return
    const el = ref.current
    const layer = store.layer(el?.parentElement)
    if (!el || !layer) return
    const g = store.measure(layer)
    const r = el.getBoundingClientRect()
    const room = Math.max(0, g.area.h - 2 * MARGIN)
    const sized = { w: r.width, h: Math.max(Math.min(r.height, room), Math.min(reserve, room)) }
    const { at } = place(sized, prefer, g.area, g.obstacles, openRects(layer, el, g.area))
    setAuto(spotOf(at, sized, g.area, prefer.startsWith('bottom')))
  }, [store, mine, docked, prefer, reserve, variant, fresh])

  // the bar's handlers: stable, so the panel inside never re-renders for a drag
  const bar = useMemo<FloatBarProps>(() => {
    const end = (e: ReactPointerEvent<HTMLElement>) => {
      const g = grab.current
      if (!g || g.pointer !== e.pointerId) return
      grab.current = null
      if (!g.moved) return
      setDrag(null)
      const area = store.geometry().area
      // where the last move left it (a lost capture's own coordinates are not to be trusted)
      const at = clampBar({ x: g.x + g.lx - g.px, y: g.y + g.ly - g.py }, g, area)
      const s = spotOf(at, g, area, true)
      setMine(s)
      saveSpot(id, s)
    }
    return {
      'data-float-bar': '',
      onPointerDown: (e) => {
        if (e.pointerType === 'mouse' && e.button !== 0) return
        if (e.target instanceof Element && e.target.closest(NOT_A_HANDLE)) return
        const el = ref.current
        const layer = store.layer(el?.parentElement)
        if (!el || !layer) return
        store.measure(layer)
        const r = el.getBoundingClientRect()
        const L = layer.getBoundingClientRect()
        grab.current = { pointer: e.pointerId, px: e.clientX, py: e.clientY, lx: e.clientX, ly: e.clientY, x: r.left - L.left, y: r.top - L.top, w: r.width, h: r.height, moved: false }
        try {
          e.currentTarget.setPointerCapture(e.pointerId)
        } catch {
          // the pointer is already gone: the move and the release still reach the bar
        }
      },
      onPointerMove: (e) => {
        const g = grab.current
        if (!g || g.pointer !== e.pointerId) return
        // the button already up (the capture lost with a bar the panel replaced): the drag is over
        if (e.pointerType === 'mouse' && e.buttons === 0) return end(e)
        const dx = e.clientX - g.px
        const dy = e.clientY - g.py
        if (!g.moved && Math.hypot(dx, dy) < DRAG_START) return
        g.moved = true
        g.lx = e.clientX
        g.ly = e.clientY
        const at = clampBar({ x: g.x + dx, y: g.y + dy }, g, store.geometry().area)
        setDrag({ x: at.x, y: at.y, w: g.w, h: g.h })
      },
      onPointerUp: end,
      onPointerCancel: end,
      onLostPointerCapture: end,
      onDoubleClick: (e) => {
        if (e.target instanceof Element && e.target.closest(NOT_A_HANDLE)) return
        saveSpot(id, null)
        setMine(null)
        setFresh((n) => n + 1)
      },
    }
  }, [store, id])

  const g = store.geometry()
  const spot = mine ?? auto
  let state: 'placing' | 'docked' | 'placed' | 'dragging'
  let pos: CSSProperties
  if (drag) {
    state = 'dragging'
    pos = { left: drag.x, top: drag.y, maxHeight: drag.h }
  } else if (docked && !mine) {
    state = 'docked'
    pos = g.dock ? { left: g.dock.x, top: g.dock.y, width: g.dock.w, height: g.dock.h } : {}
  } else if (spot && size) {
    state = 'placed'
    const p = layoutOf(spot, size, g.area, g.layer)
    pos = { left: p.left, right: p.right, top: p.top, bottom: p.bottom, maxHeight: p.maxHeight }
  } else {
    state = 'placing'
    pos = { left: g.area.x + MARGIN, top: g.area.y + MARGIN, ...(g.area.h > 0 ? { maxHeight: Math.max(0, g.area.h - 2 * MARGIN) } : {}) }
  }
  const style: CSSProperties & Record<`--${string}`, string> = {
    ...pos,
    zIndex: store.z(id),
    ...(state !== 'docked' && g.area.w > 0 ? { maxWidth: Math.max(0, g.area.w - 2 * MARGIN) } : {}),
    '--area-w': `${g.area.w}px`,
    '--area-h': `${g.area.h}px`,
  }
  return (
    <div
      ref={ref}
      className={`float ${state}`}
      style={style}
      data-float={id}
      data-placing={state === 'placing' ? '' : undefined}
      data-reserve={reserve || undefined}
      data-ay={spot?.ay}
      onPointerDownCapture={() => store.raise(id)}
    >
      <FloatBarContext.Provider value={bar}>{children}</FloatBarContext.Provider>
    </div>
  )
}
