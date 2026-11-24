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
  /** the weapon's catalog kind (missile, bomb, torpedo...), for its symbol */
  kind?: string
  /** the weapon's catalog profile (munition.ts loadoutProfile), for the path it is drawn on */
  profile?: string
  radius_m: number
  platform?: string
  track?: string
  assessment?: { result: 'persists' | 'ceased' | 'unknown'; t: number }
}

export const IMPACT_STATUS_MS = 45000

/** The assessment's banner (the cutdown, R14): only when the strike asks a decision, the target
 *  still detected (persists) or nobody of ours able to tell (unknown); never a ceased one. The
 *  verdict first, once an engagement (a salvo's rounds share one assessment), for IMPACT_STATUS_MS
 *  from the assessment or until dismissed (`dismissed`: intent ids). Pure: impact.check.ts. */
export function assessmentAsk(impacts: readonly ImpactEvent[], nowMs: number, dismissed: ReadonlySet<number>): { impact: ImpactEvent; text: string } | null {
  const asks = impacts.filter((i) => {
    const a = i.assessment
    return a && a.result !== 'ceased' && nowMs >= a.t && nowMs - a.t < IMPACT_STATUS_MS && !dismissed.has(i.intentId)
  })
  const impact = asks.sort((a, b) => b.assessment!.t - a.assessment!.t || b.t - a.t)[0]
  if (!impact) return null
  const target = impact.track ?? 'the target'
  const text =
    impact.assessment!.result === 'persists'
      ? `${impact.track ?? 'The target'} still detected after the strike: decide whether to engage again.`
      : `Outcome unknown: none of ours covers the strike on ${target}. Decide whether to look again.`
  return { impact, text }
}
