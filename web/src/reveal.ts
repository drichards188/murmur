import { useEffect, useRef, useState } from 'react'

/** A request to show rows in a list: the operator selected them somewhere other than the list.
 *  A new object is a new request, even for the same ids. */
export interface Reveal {
  ids: string[]
}

/** how long a revealed row stays marked, ms (the CSS animation in index.css runs a little less) */
export const FLASH_MS = 1400

export function reducedMotion(): boolean {
  try {
    return window.matchMedia('(prefers-reduced-motion: reduce)').matches
  } catch {
    return false
  }
}

export function scrollBehavior(): ScrollBehavior {
  return reducedMotion() ? 'auto' : 'smooth'
}

/** Scroll `box` the least that brings [top, top + height) into view (a row already in view stays put). */
export function scrollSpanIntoView(box: HTMLElement, top: number, height: number) {
  const view = box.clientHeight
  let to = box.scrollTop
  if (top < to) to = top
  else if (top + height > to + view) to = top + height - view
  if (to !== box.scrollTop) box.scrollTo({ top: Math.max(0, to), behavior: scrollBehavior() })
}

/** The id marked after each reveal request (the one `pick` chooses), cleared after FLASH_MS. */
export function useFlash(reveal: Reveal | null, pick: (ids: string[]) => string | null): string | null {
  const [flash, setFlash] = useState<string | null>(null)
  const pickRef = useRef(pick)
  useEffect(() => {
    pickRef.current = pick
  })
  useEffect(() => {
    if (!reveal) return
    const id = pickRef.current(reveal.ids)
    if (id == null) return
    // clear first so a repeat on the same row restarts the animation
    setFlash(null)
    const raf = requestAnimationFrame(() => setFlash(id))
    const t = window.setTimeout(() => setFlash(null), FLASH_MS)
    return () => {
      cancelAnimationFrame(raf)
      clearTimeout(t)
    }
  }, [reveal])
  return flash
}
