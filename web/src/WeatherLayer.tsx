import { useState } from 'react'
import { WEATHER_FIELDS, type WeatherGrid, type WeatherMode } from './weather'
import { rampCss, rampTicks, regionsOf } from './weatherField'
import type { Effects } from './weatherGl'
import { SENSOR_RGB, lightState, sensorColor, sensorShort, type ReachAnswer, type ReachMode } from './sensorReach'
import { saveLegendCollapsed, savedLegendCollapsed } from './weatherPrefs'
import './weather.css'

/** The sensor reach layer's controls (bd murmur-4tq): which vehicles get rings (each sensor's own
 *  domain), and the key to the three rings while they show. */
export interface ReachLegendProps {
  mode: ReachMode; onMode: (m: ReachMode) => void
  answer: ReachAnswer | null; error: string | null
  /** how many vehicles are selected */
  selected: number
}

function ReachLegend({ mode, onMode, answer, error, selected }: ReachLegendProps) {
  const types = [...new Set((answer?.vehicles ?? []).flatMap((v) => v.sensors.map((s) => s.type)))].filter((t) => SENSOR_RGB[t])
  // said only when something is wrong: an error, legacy sensing, a stale or unavailable answer
  const status = error ? `Sensor reach unavailable: ${error}`
    : !answer ? null
    : answer.model === 'legacy' ? 'Legacy sensing: weather and light do not change detections; no rings'
    : !answer.available ? answer.note
    : answer.stale ? 'Stale sensing; rings hidden'
    : null
  const rings = mode !== 'off' && !(mode === 'selected' && selected === 0)
  return <div className="reach-legend">
    <div><label>Sensor reach <select aria-label="Sensor reach rings" value={mode} onChange={(e) => onMode(e.target.value as ReachMode)}>
      <option value="selected">Selected</option><option value="all">All</option><option value="off">Off</option>
    </select></label></div>
    {rings && <p className="reach-key">
      <span><i className="ring solid" /> detect now</span><span><i className="ring dashed" /> identify now</span><span><i className="ring faint" /> clear day</span>
      {types.map((t) => <span key={t}><i className="swatch" style={{ background: sensorColor(t) }} />{sensorShort(t)}</span>)}
    </p>}
    {rings && status && <p>{status}</p>}
  </div>
}

/** each animated effect's toggle (the animation itself is weatherMap.ts's, as the owner chose it) */
const EFFECTS: { key: keyof Effects; label: string; title: string }[] = [
  { key: 'wind', label: 'Wind', title: 'Wind: sets sea clutter and sonar noise' },
  { key: 'fog', label: 'Fog', title: 'Fog: limits cameras' },
  { key: 'rain', label: 'Rain', title: 'Rain: drawn only, not sensed' },
  { key: 'cloud', label: 'Cloud', title: 'Cloud: drawn only, not sensed' },
]

/** A weather colour field on the map and its scale (the Weather view's, one click away). */
export function ColourField({ mode, onMode, grid }: { mode: WeatherMode; onMode: (m: WeatherMode) => void; grid: WeatherGrid | null }) {
  const regions = regionsOf(grid)
  const label = mode !== 'off' ? WEATHER_FIELDS[mode] : null
  return <div className="weather-field"><label>Colour field on the map <select aria-label="Weather colour field" value={mode} onChange={e => onMode(e.target.value as WeatherMode)}>
    <option value="off">Off</option>{Object.entries(WEATHER_FIELDS).map(([key, v]) => <option key={key} value={key}>{v.label}</option>)}
  </select></label>
    {mode !== 'off' && label && regions && <div className="weather-ramp" role="img" aria-label={`${label.label} colour scale, ${label.ticks[0]} to ${label.ticks[label.ticks.length - 1]} ${label.unit}; faintly hatched where unavailable`}>
      <div className="scale"><i className="bar" style={{ background: rampCss(mode) }} />
        <div className="ticks">{rampTicks(mode).map(t => <span key={t.value} style={{ left: `${t.at * 100}%` }}>{t.label}</span>)}</div></div>
      <span className="unit">{label.unit}</span>
      <span className="none"><i className="hatch" />none</span>
    </div>}</div>
}

/** "Night: day cameras blind", "Twilight: day cameras losing light"; nothing by day. */
function lightNote(sunDeg: number | null | undefined): string | null {
  if (sunDeg == null) return null
  const light = lightState(sunDeg)
  return light === 'night' ? 'Night: day cameras blind' : light === 'twilight' ? 'Twilight: day cameras losing light' : null
}

/** The map's weather bar, one quiet row: the animated effects' toggles, the light only when it
 *  takes cameras' sight, a word only when the weather is stale or unavailable; its open part holds
 *  the sensor reach rings' control and key. The colour field and the figures are the Weather
 *  view's (the top bar's Views). */
export function WeatherLegend({ mode, effects, onEffects, grid, error, reach }: {
  mode: WeatherMode
  effects: Effects; onEffects: (fx: Effects) => void; grid: WeatherGrid | null
  error: string | null; reach?: ReachLegendProps
}) {
  // collapsed to its row (which keeps the effect toggles), remembered per viewer
  const [collapsed, setCollapsed] = useState(savedLegendCollapsed)
  const anyOn = mode !== 'off' || Object.values(effects).some(Boolean)
  const regions = !error ? regionsOf(grid) : null
  // stale weather is a warning (what is drawn would mislead); a run with no weather source is just
  // said, quietly: nothing in it is wrong
  const stale = !!error || !!grid?.stale
  const status = error ?? (!anyOn ? null : grid?.stale ? 'Stale weather: hidden' : !regions ? (grid && !grid.available ? 'no weather data in this run' : 'no weather in this view') : null)
  const light = grid && !grid.stale ? lightNote(grid.sun_elevation_deg) : null
  return <aside className={`weather-legend${collapsed ? ' collapsed' : ''}`} aria-label="Weather map layer">
    <div><span className="weather-title">Weather</span>
      <span className="weather-toggles" role="group" aria-label="Weather effects">{EFFECTS.map(e =>
        <button key={e.key} className={`chip ${effects[e.key] ? 'on' : ''}`} aria-pressed={effects[e.key]} title={e.title}
          onClick={() => onEffects({ ...effects, [e.key]: !effects[e.key] })}>{e.label}</button>)}</span>
      {light && <span className="weather-light">{light}</span>}
      {status && <span className={`weather-status${stale ? ' warn' : ''}`}>{status}</span>}
      <button className="ghost close" aria-expanded={!collapsed} aria-label={collapsed ? 'Show the sensor reach controls' : 'Hide the sensor reach controls'} title={collapsed ? 'Sensor reach' : 'Collapse'}
        onClick={() => { setCollapsed(!collapsed); saveLegendCollapsed(!collapsed) }}>{collapsed ? '+' : '–'}</button></div>
    {!collapsed && reach && <ReachLegend {...reach} />}
  </aside>
}
