// Sensor reach now (bd murmur-4tq; docs/WEATHER.md, "Seeing the effect on sensors"): what
// `GET /api/sensing/reach` answers, and the map's rings and the fleet list's badges drawn from it.
// Information only: the bridge computes it outside every decision and logs nothing; the decision
// engine's assessment still reads the catalog's clear-day reach.
import type { LngLat } from './types'

export type ReachDomain = 'air' | 'ground' | 'surface' | 'subsurface'
export type Light = 'day' | 'twilight' | 'night'

export interface ReachFactor {
  /** what it would take back: `fog`, `haze`, `night`, `twilight`, `low sun`, `sea clutter`,
   *  `sea noise`, `shallow water` */
  factor: string
  /** the detection reach without it (that input at the clear-day reference), m */
  detect_m: number
}
export interface DomainReach {
  domain: ReachDomain
  detect_m: number
  recognise_m: number
  identify_m: number
  /** the catalog's `physical_reach_m` (clear day, at the platform's operating height); null: none */
  clear_day_m: number | null
  ratio: number | null
  limited_by: string | null
  factors: ReachFactor[]
  horizon_limited: boolean
}
export interface SensorReach {
  type: string
  primary_domain: ReachDomain
  domains: DomainReach[]
}
export interface ReachConditions {
  visibility_m: number
  sun_elevation_deg: number
  light: Light
  wind_mps: number
  sea_state: number
  synthetic: boolean
  source: string | null
  model: string | null
}
export interface VehicleReach {
  uid: number
  callsign: string | null
  platform: string
  alt_msl_m: number
  submerged: boolean
  age_s: number
  conditions: ReachConditions
  sensors: SensorReach[]
}
export interface ReachAnswer {
  available: boolean
  model?: string
  t?: number
  stale?: boolean
  vehicles: VehicleReach[]
  note: string
}

/** The map layer: rings for the selected vehicles (the default), for every vehicle, or none. */
export type ReachMode = 'selected' | 'all' | 'off'
/** Which domain's reach the rings show: each sensor's own (`auto`, the bridge's
 *  `primary_domain`), or one for all (a sensor that does not see it draws none). */
export type DomainChoice = 'auto' | ReachDomain

export const DOMAIN_WORDS: Record<ReachDomain, string> = { air: 'Air', ground: 'Ground', surface: 'Surface', subsurface: 'Subsurface' }

/** Short sensor names for badges and the legend. */
export function sensorShort(type: string): string {
  return type === 'eo_ir' ? 'EO' : type === 'radar' ? 'RADAR' : type === 'sonar' ? 'SONAR' : type.toUpperCase()
}

/** Ring colours by sensor type (RGB). */
export const SENSOR_RGB: Record<string, [number, number, number]> = {
  eo_ir: [255, 196, 92],
  radar: [92, 200, 245],
  sonar: [86, 214, 170],
}
export function sensorColor(type: string, alpha = 1): string {
  const [r, g, b] = SENSOR_RGB[type] ?? [210, 210, 210]
  return `rgba(${r},${g},${b},${alpha})`
}

/** The sun's state, as the bridge says it (and the physical model's light: a day camera has
 *  full light with the sun up, none from 6 degrees below the horizon). */
export function lightState(sunDeg: number): Light {
  return sunDeg >= 0 ? 'day' : sunDeg >= -6 ? 'twilight' : 'night'
}

/** The domain a sensor's rings show under `choice`; null: the sensor does not see that domain. */
export function pickDomain(s: SensorReach, choice: DomainChoice): DomainReach | null {
  const want = choice === 'auto' ? s.primary_domain : choice
  return s.domains.find((d) => d.domain === want) ?? null
}

/** A closed ring `radiusM` about `center` ([lon, lat]), on a local flat earth (rings are at most
 *  tens of km). */
export function circle(center: LngLat, radiusM: number, n = 96): LngLat[] {
  const [lon, lat] = center
  const mLat = 111_320
  const mLon = 111_320 * Math.cos((lat * Math.PI) / 180)
  const out: LngLat[] = []
  for (let i = 0; i <= n; i++) {
    const a = (2 * Math.PI * i) / n
    out.push([lon + (radiusM * Math.sin(a)) / mLon, lat + (radiusM * Math.cos(a)) / mLat])
  }
  return out
}

export type RingKind = 'detect' | 'identify' | 'clear'

/** The rings to draw: for each vehicle shown (`uids`; null: every vehicle answered) and each of
 *  its sensors, at the vehicle's position now (`positions`), a solid ring at the detection reach
 *  for the chosen domain, a dashed one at the identification reach, and a faint one at the
 *  clear-day reference. A reach of 0 draws no ring. */
export function reachFeatures(
  answer: ReachAnswer | null,
  positions: Map<number, LngLat>,
  uids: Set<number> | null,
  choice: DomainChoice,
): GeoJSON.FeatureCollection {
  const features: GeoJSON.Feature[] = []
  if (!answer?.available || answer.stale) return { type: 'FeatureCollection', features }
  for (const v of answer.vehicles) {
    if (uids && !uids.has(v.uid)) continue
    const at = positions.get(v.uid)
    if (!at) continue
    for (const s of v.sensors) {
      const d = pickDomain(s, choice)
      if (!d) continue
      const ring = (kind: RingKind, m: number | null) => {
        if (m == null || !(m > 0)) return
        features.push({
          type: 'Feature',
          properties: { uid: v.uid, sensor: s.type, domain: d.domain, kind, color: sensorColor(s.type) },
          geometry: { type: 'LineString', coordinates: circle(at, m) },
        })
      }
      ring('clear', d.clear_day_m)
      ring('detect', d.detect_m)
      ring('identify', d.identify_m)
    }
  }
  return { type: 'FeatureCollection', features }
}

/** A vehicle's degraded-reach badge: its worst sensor whose detection reach (in the chosen
 *  domain) is under half its clear-day reference, e.g. "EO 18% · fog"; null when none is. */
export function reachBadge(v: VehicleReach, choice: DomainChoice): { text: string; title: string } | null {
  let worst: { s: SensorReach; d: DomainReach; ratio: number } | null = null
  for (const s of v.sensors) {
    const d = pickDomain(s, choice)
    if (!d || d.ratio == null || d.ratio >= 0.5) continue
    if (!worst || d.ratio < worst.ratio) worst = { s, d, ratio: d.ratio }
  }
  if (!worst) return null
  const pct = Math.round(worst.ratio * 100)
  const why = worst.d.limited_by ? ` · ${worst.d.limited_by}` : ''
  return {
    text: `${sensorShort(worst.s.type)} ${pct}%${why}`,
    title: `${sensorShort(worst.s.type)} detects a reference ${worst.d.domain} contact out to ${km(worst.d.detect_m)}, ${pct}% of its clear-day ${km(worst.d.clear_day_m ?? 0)}${worst.d.limited_by ? `, limited by ${worst.d.limited_by}` : ''}. Information only: the decision engine still assumes the clear-day reach.`,
  }
}

export function km(m: number): string {
  return m >= 10_000 ? `${(m / 1000).toFixed(0)} km` : m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m)} m`
}
