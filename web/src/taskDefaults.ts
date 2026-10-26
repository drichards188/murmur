import { useEffect, useState } from 'react'
import type { Kind, TaskKind, Vehicle } from './types'

/** The catalog fields the task form needs (from /api/platforms). */
export interface PlatformLimits {
  id: string
  min_turn_radius_m: number | null
  loiter_radius_m?: number | null
  tasks: string[]
  // the rest of the catalog entry, for the vehicle details card
  cruise_mps?: number | null
  max_mps?: number | null
  climb_mps?: number | null
  endurance_h?: number | null
  alt_m?: number | null
  protocol?: string
  protocol_note?: string
  authenticity?: string
  authenticity_blocker?: string
  space_class?: string
  /** one-way: spent by its own effect (a loitering munition), so it is its own munition */
  expended_by_effect?: boolean
}

// Must match sim/policy.toml [rules]: the decision core rejects an orbit tighter than this.
const ORBIT_MARGIN = 1.1
const FIXED_WING_DEFAULT_MIN_TURN_M = 60
const FIXED_WING: Kind[] = ['plane', 'vtol']

// Typical orbit when the catalog gives no loiter radius (SITL platforms use ArduPilot defaults).
const KIND_ORBIT_M: Partial<Record<Kind, number>> = {
  plane: 250,
  vtol: 250,
  copter: 50,
  blimp: 100,
  boat: 100,
  ugv: 50,
  sub: 50,
}

let cache: Promise<Map<string, PlatformLimits>> | null = null

/** Platform catalog keyed by id, fetched once per page load. */
export function usePlatformLimits(): Map<string, PlatformLimits> | null {
  const [limits, setLimits] = useState<Map<string, PlatformLimits> | null>(null)
  useEffect(() => {
    cache ??= fetch('/api/platforms')
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`HTTP ${r.status}`))))
      .then((d: { platforms: PlatformLimits[] }) => new Map(d.platforms.map((p) => [p.id, p])))
      .catch((e) => {
        cache = null // retry on the next mount
        throw e
      })
    let live = true
    cache.then((m) => live && setLimits(m)).catch(() => {})
    return () => {
      live = false
    }
  }, [])
  return limits
}

export interface OrbitRange {
  min: number
  value: number
  max: number
  step: number
  /** which vehicle sets the minimum, and why */
  limitedBy: string | null
}

function niceStep(span: number) {
  const raw = span / 100
  const p = 10 ** Math.floor(Math.log10(raw))
  return [1, 2, 5, 10].map((k) => k * p).find((s) => s >= raw) ?? raw
}

/** Orbit radius range for the selected vehicles: the minimum is the decision core's own rule
 *  (1.1 x minimum turn radius, or ArduPlane's default for fixed-wing without figures), so the
 *  console can't offer a radius that would be rejected. */
export function orbitRange(vs: Vehicle[], limits: Map<string, PlatformLimits> | null): OrbitRange {
  let min = 10
  let typical = 0
  let limitedBy: string | null = null
  for (const v of vs) {
    const p = v.identity ? limits?.get(v.identity.platform) : undefined
    const turn = p?.min_turn_radius_m ?? (v.kind && FIXED_WING.includes(v.kind) ? FIXED_WING_DEFAULT_MIN_TURN_M : null)
    if (turn != null) {
      const need = Math.ceil(turn * ORBIT_MARGIN)
      if (need > min) {
        min = need
        limitedBy = `${v.identity?.callsign ?? v.uid}: turn radius ${Math.round(turn)} m`
      }
    }
    typical = Math.max(typical, p?.loiter_radius_m ?? (v.kind ? KIND_ORBIT_M[v.kind] ?? 0 : 0))
  }
  const value0 = Math.max(min, typical || 250)
  const max0 = Math.max(800, value0 * 3, min * 3)
  const step = niceStep(max0 - min)
  const snap = (x: number) => Math.round(x / step) * step
  const max = Math.ceil(max0 / step) * step
  // the slider moves in steps from min, so keep min itself reachable and the default on a step
  const value = Math.min(max, Math.max(min, snap(value0)))
  return { min, value, max, step, limitedBy }
}

/** Tasks every selected vehicle supports (the catalog's capability list). Track is offered
 *  when the selection contains an antenna tracker. */
export function availableTasks(vs: Vehicle[], limits: Map<string, PlatformLimits> | null): Set<TaskKind> | null {
  if (!limits || vs.length === 0) return null
  const all: TaskKind[] = ['orbit', 'patrol', 'hold', 'rtl']
  const ok = new Set<TaskKind>(all.filter((k) => vs.every((v) => (v.identity ? limits.get(v.identity.platform)?.tasks.includes(k) : true))))
  if (vs.some((v) => v.kind === 'tracker')) ok.add('track')
  return ok
}
