import { WEATHER_FIELDS, weatherColor, weatherValue, type WeatherGrid, type WeatherMode } from './weather'
import { DOMAIN_WORDS, SENSOR_RGB, lightState, sensorColor, sensorShort, type DomainChoice, type ReachAnswer, type ReachDomain, type ReachMode } from './sensorReach'
import './weather.css'

/** The sensor reach layer's controls (bd murmur-4tq): which vehicles get rings, which domain
 *  they show, and the key to the three rings. */
export interface ReachLegendProps {
  mode: ReachMode; onMode: (m: ReachMode) => void
  domain: DomainChoice; onDomain: (d: DomainChoice) => void
  answer: ReachAnswer | null; error: string | null
  /** how many vehicles are selected */
  selected: number
}

function ReachLegend({ mode, onMode, domain, onDomain, answer, error, selected }: ReachLegendProps) {
  const types = [...new Set((answer?.vehicles ?? []).flatMap((v) => v.sensors.map((s) => s.type)))].filter((t) => SENSOR_RGB[t])
  const status = error ? `Sensor reach unavailable: ${error}`
    : !answer ? 'Loading sensor reach…'
    : answer.model === 'legacy' ? 'Legacy sensing: weather and light do not change detections; no rings'
    : !answer.available ? answer.note
    : answer.stale ? 'Stale sensing; rings hidden'
    : mode === 'selected' && selected === 0 ? 'Select vehicles to see their reach'
    : null
  return <div className="reach-legend">
    <div><label>Sensor reach <select aria-label="Sensor reach rings" value={mode} onChange={(e) => onMode(e.target.value as ReachMode)}>
      <option value="selected">Selected</option><option value="all">All</option><option value="off">Off</option>
    </select></label>
    <label>for <select aria-label="Sensor reach domain" value={domain} onChange={(e) => onDomain(e.target.value as DomainChoice)}>
      <option value="auto">Auto</option>{(Object.keys(DOMAIN_WORDS) as ReachDomain[]).map((d) => <option key={d} value={d}>{DOMAIN_WORDS[d]}</option>)}
    </select></label></div>
    {mode !== 'off' && <p className="reach-key">
      <span><i className="ring solid" /> detect now</span><span><i className="ring dashed" /> identify now</span><span><i className="ring faint" /> clear day</span>
      {types.map((t) => <span key={t}><i className="swatch" style={{ background: sensorColor(t) }} />{sensorShort(t)}</span>)}
    </p>}
    {mode !== 'off' && status && <p>{status}</p>}
  </div>
}

export function WeatherLegend({ mode, onMode, grid, error, onDetails, reach }: {
  mode: WeatherMode; onMode: (m: WeatherMode) => void; grid: WeatherGrid | null
  error: string | null; onDetails: () => void; reach?: ReachLegendProps
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
    {grid?.sun_elevation_deg != null && !grid.stale && <p className="weather-sun">
      Sun {grid.sun_elevation_deg.toFixed(0)}° · <b className={`light-${lightState(grid.sun_elevation_deg)}`}>{lightState(grid.sun_elevation_deg).toUpperCase()}</b>
      {lightState(grid.sun_elevation_deg) !== 'day' ? ' · day cameras lose their light' : ''}</p>}
    {reach && <ReachLegend {...reach} />}
  </aside>
}
