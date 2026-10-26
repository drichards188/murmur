import type { Munition } from './munition'
import type { LngLat } from './types'

/** A strike: where and when the effect lands, as recorded, not the earlier predicted effect
 *  time or point. With a munition (bd murmur-64z) that is its impact, the time of flight after
 *  the release; without one (a platform that is its own munition) the engine's release. */
export interface ImpactEvent {
  key: string
  intentId: number
  point: LngLat
  t: number
  /** the munition on its way to this strike, if the platform launched one */
  munition?: Munition
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
  label.textContent = 'IMPACT'
  el.appendChild(label)
  return el
}
