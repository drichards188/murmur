import type { LngLat } from './types'

/** The engine's recorded release, not the earlier predicted effect time or point. */
export interface ImpactEvent {
  key: string
  intentId: number
  point: LngLat
  t: number
  radius_m: number
  platform?: string
  track?: string
  assessment?: { result: 'persists' | 'ceased' | 'unknown'; t: number }
}

export const IMPACT_ANIMATION_MS = 6000
export const IMPACT_STATUS_MS = 45000

/** A CSS animation anchored to the recorded map position. A negative delay catches up if
 *  the WebSocket message arrives after the release, without replaying an old effect. */
export function impactElement(impact: ImpactEvent, ageMs: number): HTMLDivElement {
  const el = document.createElement('div')
  el.className = 'impact-marker'
  el.dataset.impact = impact.key
  el.setAttribute('aria-hidden', 'true')
  el.style.setProperty('--impact-age', `${-Math.max(0, ageMs)}ms`)
  for (const name of ['smoke', 'ring ring-outer', 'ring ring-inner', 'flash', 'core']) {
    const part = document.createElement('span')
    part.className = `impact-${name.replaceAll(' ', ' impact-')}`
    el.appendChild(part)
  }
  const label = document.createElement('span')
  label.className = 'impact-label'
  label.textContent = 'EFFECT RELEASED'
  el.appendChild(label)
  return el
}
