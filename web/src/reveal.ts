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

/** A ref for a question or a plan that opens on a person's click (the engagement screens): it
 *  takes the focus, so the Enter that opened it does not also answer it, and comes into view
 *  inside its own scroll box, never scrolling the page. A plain function, so a ref with it runs
 *  on mount alone and a live re-render never takes the focus back. */
export function focusInView(el: HTMLElement | null) {
  if (!el) return
  el.focus({ preventScroll: true })
  let box = el.parentElement
  while (box && !/(auto|scroll)/.test(getComputedStyle(box).overflowY)) box = box.parentElement
  if (!box) return
  const top = el.getBoundingClientRect().top - box.getBoundingClientRect().top + box.scrollTop
  // with a little room under it, so its last button is not at the very edge
  scrollSpanIntoView(box, top, Math.min(el.offsetHeight + 16, box.clientHeight))
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
