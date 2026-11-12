import { type CSSProperties, type ElementType, type ReactNode, useEffect, useRef, useState } from 'react'

/** An element whose children are built only while it is near the viewport of its scrolling
 *  ancestor. A long list of tall items (the command log's cards, the tracks) otherwise puts
 *  every one in the DOM and re-renders every one on each bridge update: at 3,600 vehicles
 *  and 500 tracks that was 40,000 nodes (murmur-4xq3). Out of view the element keeps its box
 *  (the height it last had, else `estimate`) so the scrollbar and the items below do not
 *  move, and keeps its own class so margins and gaps are the same.
 *
 *  `force` keeps it built whatever the scroll (a selected or revealed item). */
interface Props {
  as?: ElementType
  className?: string
  /** height in px before it has ever been built */
  estimate: number
  force?: boolean
  /** how far past the scroller's edge to build, px */
  margin?: number
  /** a function, so that nothing is built (not even elements) while it is out of view */
  children: ReactNode | (() => ReactNode)
  [attr: `data-${string}`]: string | undefined
}

type Listener = (near: boolean) => void
const observers = new WeakMap<Element | Document, { io: IntersectionObserver; on: Map<Element, Listener> }>()

function scroller(el: Element): Element | null {
  let p = el.parentElement
  while (p && !/(auto|scroll)/.test(getComputedStyle(p).overflowY)) p = p.parentElement
  return p
}

function watch(el: Element, root: Element | null, margin: number, on: Listener) {
  const key = root ?? document
  let o = observers.get(key)
  if (!o) {
    const map = new Map<Element, Listener>()
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) map.get(e.target)?.(e.isIntersecting)
      },
      { root, rootMargin: `${margin}px 0px` },
    )
    o = { io, on: map }
    observers.set(key, o)
  }
  const mine = o
  mine.on.set(el, on)
  mine.io.observe(el)
  return () => {
    mine.on.delete(el)
    mine.io.unobserve(el)
  }
}

export function Lazy({ as: Tag = 'div', className, estimate, force = false, margin = 600, children, ...rest }: Props) {
  const el = useRef<HTMLElement>(null)
  const [near, setNear] = useState(false)
  // the box it keeps while not built: its last measured height
  const [height, setHeight] = useState(estimate)
  const built = near || force
  useEffect(() => {
    // remember the real height of whatever is built, for the placeholder
    const node = el.current
    if (!built || !node) return
    const ro = new ResizeObserver(() => setHeight(node.getBoundingClientRect().height))
    ro.observe(node)
    return () => ro.disconnect()
  }, [built])
  useEffect(() => {
    const node = el.current
    if (!node) return
    return watch(node, scroller(node), margin, setNear)
  }, [margin])
  const style: CSSProperties | undefined = built ? undefined : { height, boxSizing: 'border-box', flex: 'none' }
  return (
    <Tag ref={el} className={className} style={style} {...rest}>
      {built ? (typeof children === 'function' ? children() : children) : null}
    </Tag>
  )
}
