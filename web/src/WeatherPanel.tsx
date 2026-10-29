import { usePoll } from './space'
import type { LngLat, Vehicle } from './types'
import { callsign } from './types'
import './weather.css'

type Values = Record<string, number | null>
interface Weather {
  available: boolean; lat?: number; lon?: number; t?: number; age_s?: number; stale?: boolean; note: string
  sensing_model?: string
  sensing?: { visibility_m: number; wind_mps: number }
  weather?: { values: Values; source: string; model: string | null; synthetic: boolean; error: string | null }
}
const fmt = (v: number | null | undefined, unit: string) => v == null ? 'Unavailable' : `${v.toFixed(1)} ${unit}`
export function WeatherPanel({ center, vehicle, onClose }: { center: LngLat; vehicle: Vehicle | null; onClose: () => void }) {
  const point = vehicle?.position ?? center
  const { data: response, error } = usePoll<Weather>(`/api/weather?lat=${point[1].toFixed(3)}&lon=${point[0].toFixed(3)}`, 2000)
  const data = !error && (response?.lat == null || (response.lat === Number(point[1].toFixed(3)) && response.lon === Number(point[0].toFixed(3)))) ? response : null
  const w = data?.weather
  const v = w?.values
  const wind = v?.wind_east_mps != null && v?.wind_north_mps != null ? Math.hypot(v.wind_east_mps, v.wind_north_mps) : null
  const direction = wind != null && wind > 0 ? (Math.atan2(-v!.wind_east_mps!, -v!.wind_north_mps!) * 180 / Math.PI + 360) % 360 : null
  return <aside className="weather-panel" aria-label="Weather">
    <header><strong>Weather</strong><button className="ghost" aria-label="Close weather" onClick={onClose}>×</button></header>
    <p>{vehicle ? callsign(vehicle) : 'Map center'} · {point[1].toFixed(3)}, {point[0].toFixed(3)}</p>
    {error && <p role="alert">Weather unavailable: {error}</p>}
    {!data && !error && <p>Loading weather…</p>}
    {data && <>
      <p className={data.stale ? 'weather-warning' : ''}>{w?.synthetic ? 'SCRIPTED WEATHER' : data.available ? 'ARCHIVED MODEL' : 'WEATHER UNAVAILABLE'}{data.stale ? ' · STALE' : ''}
        {data.t != null && <> · {new Date(data.t * 1000).toISOString().slice(11, 19)} UTC</>}</p>
      <dl>
        <dt>Visibility</dt><dd className={v?.visibility_m != null && v.visibility_m < 1000 ? 'weather-warning' : ''}>{fmt(v?.visibility_m, 'm')}</dd>
        <dt>Surface wind (10 m)</dt><dd>{fmt(wind, 'm/s')}{direction != null && ` from ${Math.round(direction)}°`}</dd>
        <dt>Gust (hourly max)</dt><dd>{fmt(v?.gust_mps, 'm/s')}</dd>
        <dt>Rain / snow (hourly total)</dt><dd>{fmt(v?.precipitation_mm, 'mm')}</dd>
        <dt>Cloud cover</dt><dd>{fmt(v?.cloud_cover_pct, '%')}</dd>
        <dt>Temperature</dt><dd>{fmt(v?.temperature_c, '°C')}</dd>
        <dt>Humidity</dt><dd>{fmt(v?.humidity_pct, '%')}</dd>
        <dt>Pressure</dt><dd>{fmt(v?.pressure_hpa, 'hPa')}</dd>
      </dl>
      {w?.error && <p className="weather-warning">{w.error}</p>}
      {!data.available && <p>No weather data at this point. Fixed sensing conditions apply.</p>}
      {data.sensing_model === 'physical' && data.sensing && <p>Sensor inputs: visibility {Math.round(data.sensing.visibility_m)} m; mean wind {data.sensing.wind_mps.toFixed(1)} m/s.</p>}
      {data.sensing_model === 'legacy' && <p>Legacy sensing: weather does not change detections.</p>}
      <details><summary>Source and limits</summary>
        <p>{data.note}</p><p>{w?.source} {w?.model}</p>
        {w?.model && <p><a href="https://open-meteo.com/" target="_blank" rel="noreferrer">Weather data by Open-Meteo</a> · NOAA HRRR</p>}
      </details>
    </>}
  </aside>
}
