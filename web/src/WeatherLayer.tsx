import { WEATHER_FIELDS, type WeatherField, type WeatherGrid, type WeatherMode } from './weather'
import { describeField, rampCss, rampTicks, regionsOf } from './weatherField'
import type { Effects } from './weatherGl'
import { reducedMotion } from './reveal'
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

/** each animated effect: its toggle's name, the field it shows, and what the simulation does with it */
const EFFECTS: { key: keyof Effects; label: string; field: WeatherField; uses: string }[] = [
  { key: 'wind', label: 'Wind', field: 'wind', uses: 'sea clutter, sonar noise' },
  { key: 'fog', label: 'Fog', field: 'visibility', uses: 'limits cameras' },
  { key: 'rain', label: 'Rain', field: 'precipitation', uses: 'display only, not sensed' },
  { key: 'cloud', label: 'Cloud', field: 'cloud', uses: 'display only, not sensed' },
]

export function WeatherLegend({ mode, onMode, effects, onEffects, grid, error, onDetails, reach }: {
  mode: WeatherMode; onMode: (m: WeatherMode) => void
  effects: Effects; onEffects: (fx: Effects) => void; grid: WeatherGrid | null
  error: string | null; onDetails: () => void; reach?: ReachLegendProps
}) {
  const regions = !error ? regionsOf(grid) : null
  const anyOn = mode !== 'off' || Object.values(effects).some(Boolean)
  const label = mode !== 'off' ? WEATHER_FIELDS[mode] : null
  const reduced = reducedMotion()
  const status = error ?? (!anyOn ? 'Weather off' : grid?.stale ? 'Stale weather; layer hidden' : !regions ? 'Weather unavailable in this view' : null)
  return <aside className="weather-legend" aria-label="Weather map layer">
    <div><span className="weather-title">Weather</span>
      <span className="weather-toggles" role="group" aria-label="Weather effects">{EFFECTS.map(e =>
        <button key={e.key} className={`chip ${effects[e.key] ? 'on' : ''}`} aria-pressed={effects[e.key]}
          onClick={() => onEffects({ ...effects, [e.key]: !effects[e.key] })}>{e.label}</button>)}</span>
      <button className="ghost" onClick={onDetails}>Details</button></div>
    {status ? <p>{status}</p> : regions && <>
      <dl className="weather-now">{EFFECTS.filter(e => effects[e.key]).map(e => <div key={e.key}>
        <dt>{e.label}</dt><dd>{describeField(regions, e.field)} <span>· {e.uses}</span></dd></div>)}</dl>
      <p>{grid?.model ?? 'Archived'} at {grid ? `${new Date(grid.t * 1000).toISOString().slice(11, 19)}Z` : 'now'}: each model point's region (nearest, ~3 km), the values the sensing model uses there.
        {' '}{reduced ? 'Still (reduced motion); arrows point toward travel.' : 'Shapes drawn to be seen; drift and streaks follow each region\'s wind, pace exaggerated.'}</p>
    </>}
    <div className="weather-field"><label>Colour field <select aria-label="Weather colour field" value={mode} onChange={e => onMode(e.target.value as WeatherMode)}>
      <option value="off">Off</option>{Object.entries(WEATHER_FIELDS).map(([key, v]) => <option key={key} value={key}>{v.label}</option>)}
    </select></label>
      {mode !== 'off' && label && regions && <div className="weather-ramp" role="img" aria-label={`${label.label} colour scale, ${label.ticks[0]} to ${label.ticks[label.ticks.length - 1]} ${label.unit}; faintly hatched where unavailable`}>
        <div className="scale"><i className="bar" style={{ background: rampCss(mode) }} />
          <div className="ticks">{rampTicks(mode).map(t => <span key={t.value} style={{ left: `${t.at * 100}%` }}>{t.label}</span>)}</div></div>
        <span className="unit">{label.unit}</span>
        <span className="none"><i className="hatch" />none</span>
      </div>}</div>
    {grid?.sun_elevation_deg != null && !grid.stale && <p className="weather-sun">
      Sun {grid.sun_elevation_deg.toFixed(0)}° · <b className={`light-${lightState(grid.sun_elevation_deg)}`}>{lightState(grid.sun_elevation_deg).toUpperCase()}</b>
      {lightState(grid.sun_elevation_deg) !== 'day' ? ' · day cameras lose their light' : ''}</p>}
    {reach && <ReachLegend {...reach} />}
  </aside>
}
