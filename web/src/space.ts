// Space segment: types for /api/space/*, fetch helpers and time formatting.
// Times from the bridge are scenario time (MURMUR_AT on the bridge's clock), RFC 3339 UTC.
import { useEffect, useRef, useState } from 'react'
import type { LngLat } from './types'

export type Capability = 'optical' | 'sar' | 'satcom' | 'gnss' | 'reference'

export interface TimeMap {
  mode: string
  shift_days: number
  reference: string
}

export interface Envelope {
  scenario: { now: string; rate: number; source: string; at: string | null }
  time_map: TimeMap
  time_shifted: boolean
  time_note: string
}

export interface AreaInfo {
  name: string
  title: string
  ring: LngLat[]
  center: LngLat
  radius_km: number
  utc_offset_h: number
}

export interface Geometry {
  elevation_deg: number
  azimuth_deg: number
  range_km: number
  off_nadir_deg: number
  incidence_deg: number
  sat_alt_km: number
}

export interface SatClassInfo {
  id: string
  name: string
  capability: Capability
  role: string
  satellites: number
  timing: { tasking_lead_min?: number; downlink_latency_min?: number; processing_min?: number } | null
  sensor: { swath_km?: number; taskable?: boolean } | null
  guessed: string[]
  members: { norad_id: number; name: string; label: string | null }[]
}

export interface Catalog extends Envelope {
  classes: SatClassInfo[]
  reference: string
  area: AreaInfo
  areas: AreaInfo[]
  satcom_by_uid: Record<string, string[]>
}

export interface AccessWindow {
  class_id: string
  capability: Capability
  satellite: string
  norad_id: number
  label: string | null
  start: string
  end: string
  duration_s: number
  best_time: string
  best: Geometry
  resolution_m: number | null
  sun_elevation_deg: number
  daylight_ok: boolean | null
  taskable: boolean
  element_age_days: number
  clipped: boolean
  scene?: LngLat[] | null
  subpoint?: [number, number, number]
}

export interface Passes extends Envelope {
  from: string
  to: string
  windows: AccessWindow[]
  classes: { class_id: string; name: string; capability: Capability; satellites: number; windows: number; night_excluded: number }[]
  sun: [number, number][]
  area: AreaInfo
  compute_ms: number
}

export interface Interval {
  start: string
  end: string
}

export interface LinkClassReport {
  class_id: string
  name: string
  coverage_fraction: number
  outages: Interval[]
  longest_outage_s: number
  visible_min: number
  visible_max: number
  latency_ms_median: number | null
  mask_deg: number
}

export interface Links extends Envelope {
  classes: LinkClassReport[]
  compute_ms: number
}

export interface Gnss extends Envelope {
  samples: { t: string; visible: number; dop: { pdop: number } | null }[]
  visible_min: number
  visible_max: number
  pdop_max: number | null
}

export interface LinkNow {
  uid: number
  callsign: string | null
  platform: string | null
  position: [number, number, number]
  class_id: string
  visible: number
  best: {
    norad_id: number
    satellite: string
    elevation_deg: number
    azimuth_deg: number
    range_km: number
    latency_ms: number | null
    position: [number, number, number] | null
  } | null
}

export interface LinksNow extends Envelope {
  t: string
  links: LinkNow[]
}

export interface Overhead extends Envelope {
  t: string
  overhead: {
    class_id: string
    capability: Capability
    satellite: string
    norad_id: number
    label: string | null
    subpoint: [number, number, number]
    geometry: Geometry
    field_of_regard: LngLat[]
    scene: LngLat[] | null
    daylight_ok: boolean | null
    taskable: boolean
  }[]
}

export interface Positions extends Envelope {
  t: string
  classes: { class_id: string; capability: Capability; total: number; shown: number; sats: [number, number, number, number][] }[]
}

export interface Track extends Envelope {
  norad_id: number
  satellite: string
  class_id: string
  track: LngLat[]
}

export interface TaskOption {
  rank: number
  class_id: string
  class_name: string
  capability: Capability
  satellite: string
  norad_id: number
  label: string | null
  collect_time: string
  geometry: Geometry
  resolution_m: number | null
  sun_elevation_deg: number
  earliest_collect: string
  product_time: string | null
  taskable: boolean
  scene: LngLat[] | null
  covers_area: boolean | null
  element_age_days: number
  notes: string[]
}

export interface Exclusion {
  class_id: string
  satellite: string
  norad_id: number
  pass_time: string
  reason: string
  detail: string
}

export interface TaskingResult extends Envelope {
  options: TaskOption[]
  exclusions: Exclusion[]
  exclusions_total: number
  exclusion_summary: { class_id: string; reason: string; passes: number }[]
  classes_without_passes: string[]
  cloud_model: string
  compute_ms: number
  area: AreaInfo
}

export interface TaskQuery {
  capabilities: ('optical' | 'sar')[]
  area: { bbox: [number, number, number, number] } | string
  deadline?: string
  to?: string
  rank: 'earliest_product' | 'best_resolution'
  max_resolution_m?: number
}

export async function getJson<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init)
  const body = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(body.detail ?? `bridge returned ${res.status}`)
  return body as T
}

export function requestOptions(q: TaskQuery) {
  return getJson<TaskingResult>('/api/space/task', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...q, exclusions: 300 }),
  })
}

/** Poll a URL every `everyMs` while `url` is non-null; keeps the last good value. */
export function usePoll<T>(url: string | null, everyMs: number) {
  const [data, setData] = useState<T | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    if (!url) return
    let stop = false
    let timer: number | undefined
    const tick = () => {
      getJson<T>(url)
        .then((d) => {
          if (stop) return
          setData(d)
          setError(null)
        })
        .catch((e: Error) => !stop && setError(e.message))
        .finally(() => {
          if (!stop) timer = window.setTimeout(tick, everyMs)
        })
    }
    tick()
    return () => {
      stop = true
      window.clearTimeout(timer)
    }
  }, [url, everyMs])
  return { data, error }
}

/** The scenario clock (ms), extrapolated from the last response that carried it. */
export function useScenarioNow(env: Envelope | null | undefined, tickMs = 1000) {
  const anchor = useRef<{ t: number; wall: number; rate: number } | null>(null)
  const [now, setNow] = useState<number | null>(null)
  const scenarioNow = env?.scenario.now
  const rate = env?.scenario.rate ?? 1
  useEffect(() => {
    if (!scenarioNow) return
    anchor.current = { t: Date.parse(scenarioNow), wall: Date.now(), rate }
  }, [scenarioNow, rate])
  useEffect(() => {
    const tick = () => {
      const a = anchor.current
      if (a) setNow(a.t + (Date.now() - a.wall) * a.rate)
    }
    const id = window.setInterval(tick, tickMs)
    return () => window.clearInterval(id)
  }, [tickMs])
  return now ?? (scenarioNow ? Date.parse(scenarioNow) : null)
}

/** HH:MM in the area's local time (utc offset in hours). */
export function hhmm(t: number | string, offsetH: number, seconds = false) {
  const ms = (typeof t === 'string' ? Date.parse(t) : t) + offsetH * 3_600_000
  return new Date(ms).toISOString().slice(11, seconds ? 19 : 16)
}

export function dayLabel(t: number, offsetH: number) {
  return new Date(t + offsetH * 3_600_000).toISOString().slice(5, 10)
}

export function offsetLabel(h: number) {
  return h === 0 ? 'UTC' : `UTC${h > 0 ? '+' : '−'}${Math.abs(h)}`
}

export const CLASS_COLOR: Record<string, string> = {
  worldview: '#ffb347',
  skysat: '#f2c14e',
  dove: '#bfa76a',
  capella: '#c49bff',
  iceye: '#9d8cff',
  starlink: '#7fb8ff',
  iridium: '#6fe0c4',
  wgs: '#5fd38d',
  gps: '#d7dde4',
  iss: '#ff8fa3',
}

export const classColor = (id: string) => CLASS_COLOR[id] ?? '#aab6c3'

/** Expected product in hand for a pass: best time + downlink + processing (class guesses). */
export function productTime(w: AccessWindow, cls: SatClassInfo | undefined) {
  const t = cls?.timing
  if (!t || (t.downlink_latency_min == null && t.processing_min == null)) return null
  return Date.parse(w.best_time) + ((t.downlink_latency_min ?? 0) + (t.processing_min ?? 0)) * 60_000
}

/** Space overlays the theater map draws (all optional). */
export interface SpaceOverlay {
  area: AreaInfo | null
  overhead: Overhead['overhead']
  /** highlighted scene (selected pass or option) */
  scene: { ring: LngLat[]; label: string } | null
  links: LinkNow[]
  box: LngLat[] | null
}

export const EMPTY_OVERLAY: SpaceOverlay = { area: null, overhead: [], scene: null, links: [], box: null }
