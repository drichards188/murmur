// Public environmental fields only. No truth contacts and no future weather schedule.
import type { LngLat } from './types'
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
  /** the sun's elevation at the grid's centre now (the scenario clock, or the scenario's stated
   *  sun), degrees */
  sun_elevation_deg?: number | null
}
export interface WeatherImage { url: string; coordinates: [LngLat, LngLat, LngLat, LngLat] }
export const WEATHER_FIELDS: Record<WeatherField, { label: string; unit: string; ticks: number[] }> = {
  visibility: { label: 'Visibility', unit: 'm', ticks: [50, 1000, 5000, 20000] },
  wind: { label: 'Surface wind', unit: 'm/s', ticks: [0, 5, 10, 20] },
  cloud: { label: 'Cloud cover', unit: '%', ticks: [0, 30, 70, 100] },
  precipitation: { label: 'Precipitation', unit: 'mm (hour)', ticks: [0, 1, 5, 10] },
}
export function weatherValue(v: WeatherValues, field: WeatherField): number | null {
  if (field === 'visibility') return v.visibility_m
  if (field === 'cloud') return v.cloud_cover_pct
  if (field === 'precipitation') return v.precipitation_mm
  return v.wind_east_mps == null || v.wind_north_mps == null ? null : Math.hypot(v.wind_east_mps, v.wind_north_mps)
}
export function weatherColor(value: number | null, field: WeatherField): [number, number, number, number] {
  if (value == null || !Number.isFinite(value)) return [120, 133, 150, 24]
  if (field === 'visibility') {
    if (value < 200) return [218, 229, 240, 150]
    if (value < 1000) return [181, 171, 237, 125]
    if (value < 5000) return [103, 153, 216, 80]
    return [60, 170, 200, 20]
  }
  const f = Math.max(0, Math.min(1, value / (field === 'wind' ? 20 : field === 'cloud' ? 100 : 10)))
  if (field === 'cloud') return [205, 218, 235, Math.round(155 * f)]
  return [Math.round(55 + 110 * f), Math.round(180 - 70 * f), 240, Math.round(10 + 135 * f)]
}
/** Raster row 0 is north; server row 0 is south. Arrows point TOWARD the wind's velocity. */
export function weatherImage(g: WeatherGrid, field: WeatherField): WeatherImage | null {
  if (!g.available || g.stale || !g.cells || g.cells.length !== g.nlon * g.nlat) return null
  const scale = 24
  const canvas = document.createElement('canvas')
  canvas.width = g.nlon * scale; canvas.height = g.nlat * scale
  const ctx = canvas.getContext('2d')
  if (!ctx) return null
  for (let row = 0; row < g.nlat; row++) for (let col = 0; col < g.nlon; col++) {
    const v = g.cells[row * g.nlon + col]
    const value = weatherValue(v, field)
    const [r, green, b, alpha] = weatherColor(value, field)
    const x = col * scale, y = (g.nlat - row - 1) * scale
    ctx.fillStyle = `rgba(${r},${green},${b},${alpha / 255})`
    ctx.fillRect(x, y, scale, scale)
    if (value == null) {
      ctx.strokeStyle = '#8a9db83b'; ctx.lineWidth = 1
      ctx.beginPath(); ctx.moveTo(x, y + scale); ctx.lineTo(x + scale, y); ctx.stroke()
    }
    if (field === 'wind' && value != null && value > 0.1 && col % 4 === 2 && row % 4 === 2) {
      const angle = Math.atan2(-v.wind_north_mps!, v.wind_east_mps!)
      ctx.save(); ctx.translate(x + scale / 2, y + scale / 2); ctx.rotate(angle)
      ctx.beginPath(); ctx.moveTo(-16, 0); ctx.lineTo(16, 0); ctx.lineTo(7, -6)
      ctx.moveTo(16, 0); ctx.lineTo(7, 6)
      ctx.strokeStyle = '#13273a'; ctx.lineWidth = 5; ctx.stroke()
      ctx.strokeStyle = '#d8f4ff'; ctx.lineWidth = 2; ctx.stroke(); ctx.restore()
    }
  }
  const [w, s, e, n] = g.bounds
  return { url: canvas.toDataURL('image/png'), coordinates: [[w, n], [e, n], [e, s], [w, s]] }
}
