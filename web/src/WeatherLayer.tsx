import { WEATHER_FIELDS, weatherColor, weatherValue, type WeatherGrid, type WeatherMode } from './weather'
import './weather.css'

export function WeatherLegend({ mode, onMode, grid, error, onDetails }: {
  mode: WeatherMode; onMode: (m: WeatherMode) => void; grid: WeatherGrid | null
  error: string | null; onDetails: () => void
}) {
  const values = mode !== 'off' ? (grid?.cells ?? []).map(v => weatherValue(v, mode)).filter((v): v is number => v != null) : []
  const available = values.length > 0 && !grid?.stale && !error
  const label = mode !== 'off' ? WEATHER_FIELDS[mode] : null
  return <aside className="weather-legend" aria-label="Weather map layer">
    <div><label>Weather <select aria-label="Weather map field" value={mode} onChange={e => onMode(e.target.value as WeatherMode)}>
      <option value="off">Off</option>{Object.entries(WEATHER_FIELDS).map(([key, v]) => <option key={key} value={key}>{v.label}</option>)}
    </select></label><button className="ghost" onClick={onDetails}>Details</button></div>
    {mode !== 'off' && label && <>
      <p>{error ?? (grid?.stale ? 'Stale weather; layer hidden' : available ? `${grid?.synthetic ? `SCRIPTED SCENARIO · ${grid?.model ?? 'local weather'}` : `${grid?.model ?? 'Archived'} · nearest model point`}` : 'Weather unavailable in this view')}
        {available && grid && <span> · {new Date(grid.t * 1000).toISOString().slice(11, 19)}Z</span>}</p>
      <div className="weather-scale">{label.ticks.map(v => {
        const [r, g, b, a] = weatherColor(v, mode)
        return <span key={v}><i style={{ background: `rgba(${r},${g},${b},${a / 255})` }} />{v >= 1000 ? `${v / 1000}k` : v}</span>
      })}<span>{label.unit}</span></div>
      <p>{mode === 'wind' ? 'Arrows point toward wind travel · 10 m height' : mode === 'cloud' || mode === 'precipitation' ? 'Context only; no sensor or radio effect modeled' : 'Local visibility affects camera detection'} · hatched = unavailable</p>
    </>}
  </aside>
}
