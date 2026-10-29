import { useEffect, useState } from 'react'
import type { WeatherGrid } from './weather'

export function useWeatherGrid(bounds: [number, number, number, number] | null, enabled: boolean) {
  const [grid, setGrid] = useState<WeatherGrid | null>(null)
  const [error, setError] = useState<string | null>(null)
  const bbox = bounds ? [Math.max(-180, bounds[0]), Math.max(-90, bounds[1]), Math.min(180, bounds[2]), Math.min(90, bounds[3])].map(v => v.toFixed(3)).join(',') : null
  useEffect(() => {
    if (!enabled || !bbox) return
    const ctl = new AbortController()
    let timer: number
    const poll = async () => {
      try {
        const res = await fetch(`/api/weather/grid?bbox=${bbox}`, { signal: ctl.signal })
        if (!res.ok) throw new Error(`Weather layer unavailable (${res.status})`)
        const next = await res.json() as WeatherGrid
        if (!ctl.signal.aborted) { setGrid(next); setError(null) }
      } catch (e) {
        if (!ctl.signal.aborted) { setGrid(null); setError((e as Error).message) }
      } finally {
        if (!ctl.signal.aborted) timer = window.setTimeout(poll, 2000)
      }
    }
    timer = window.setTimeout(poll, 250)
    return () => { ctl.abort(); window.clearTimeout(timer) }
  }, [bbox, enabled])
  return { grid: enabled ? grid : null, error: enabled ? error : null }
}
