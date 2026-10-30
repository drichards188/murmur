// Public environmental fields only. No truth contacts and no future weather schedule.
// weatherField.ts turns the grid's regions into the map's raster and the legend's ramp;
// weatherMap.ts draws them (the field, the wind streaks, the haze).
import type { WeatherRegions } from './weatherField.ts'
export type WeatherField = 'visibility' | 'wind' | 'cloud' | 'precipitation'
export type WeatherMode = WeatherField | 'off'
export interface WeatherValues {
  visibility_m: number | null; wind_east_mps: number | null; wind_north_mps: number | null
  cloud_cover_pct: number | null; precipitation_mm: number | null
}
export interface WeatherGrid {
  available: boolean; bounds: [number, number, number, number]; nlon: number; nlat: number
  cells: WeatherValues[]; t: number; age_s: number; stale: boolean
  source: string; model: string | null; synthetic: boolean
  /** bd murmur-vr17: each archived model point and its values now, and what holds outside the
   *  archive's bounds (the sampler's own answer everywhere; weatherField.ts) */
  regions?: WeatherRegions
  /** the sun's elevation at the grid's centre now (the scenario clock, or the scenario's stated
   *  sun), degrees */
  sun_elevation_deg?: number | null
}
export const WEATHER_FIELDS: Record<WeatherField, { label: string; unit: string; ticks: number[] }> = {
  visibility: { label: 'Visibility', unit: 'm', ticks: [50, 200, 1000, 5000, 20000] },
  wind: { label: 'Surface wind', unit: 'm/s', ticks: [0, 5, 10, 15, 20] },
  cloud: { label: 'Cloud cover', unit: '%', ticks: [0, 25, 50, 75, 100] },
  precipitation: { label: 'Precipitation', unit: 'mm (hour)', ticks: [0, 2, 5, 10] },
}
export function weatherValue(v: WeatherValues, field: WeatherField): number | null {
  if (field === 'visibility') return v.visibility_m
  if (field === 'cloud') return v.cloud_cover_pct
  if (field === 'precipitation') return v.precipitation_mm
  return v.wind_east_mps == null || v.wind_north_mps == null ? null : Math.hypot(v.wind_east_mps, v.wind_north_mps)
}
