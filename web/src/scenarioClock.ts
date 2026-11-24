// The top bar's clock (bd murmur-iobd.5): the world's time, the scenario clock every process reads
// (murmur_space::scenario_clock: MURMUR_AT plus the time since the start times MURMUR_RATE), not
// the browser's. It showed 15:10Z while the world ran at 10:00Z (the open world) or 17:00Z (a
// situation), whose sun, weather and tides follow the scenario clock. The bridge serves the clock
// as a map from its own clock (`scenario` in GET /api/weather), and the console keeps the
// bridge's clock from its state frames (decisions.ts useBridgeClock).
//
// Pure (no browser): scenarioClock.check.ts runs it under node.

/** From the bridge's clock to scenario time, ms (murmur_decide::ScenarioMap). */
export interface ClockMap {
  engine_origin: number
  scenario_origin: number
  rate: number
}

/** The bridge's answer: the map, and where its start came from. */
export interface ScenarioClockAnswer {
  map: ClockMap
  rate: number
  /** "MURMUR_AT", or "wall clock" (no start: the bridge's own clock) */
  source: string
  /** the start, RFC 3339 */
  at?: string | null
}

/** Scenario time (ms) at the bridge's clock `bridgeMs`. */
export function scenarioMs(m: ClockMap, bridgeMs: number): number {
  return m.scenario_origin + Math.floor((bridgeMs - m.engine_origin) * m.rate)
}

/** m:ss of `s` seconds, never negative. */
function mss(s: number): string {
  const n = Math.max(0, Math.round(s))
  return `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`
}

/** A situation's time beside the world's in the top bar (the cutdown's new piece 8: the clock once,
 *  in the top bar): "T+3:27 · 8:33 left", "T+--:--" before its clock starts; null with no
 *  situation. */
export function runClock(t: { elapsed: number | null; left: number | null } | null): string | null {
  if (!t) return null
  if (t.elapsed == null) return 'T+--:--'
  return `T+${mss(t.elapsed)}${t.left != null ? ` · ${mss(t.left)} left` : ''}`
}

/** The top bar's clock and its hover: "17:04:31Z" in scenario time, or a placeholder until the
 *  bridge has said what its clock is. */
export function clockText(answer: ScenarioClockAnswer | null, bridgeMs: number): { text: string; title: string } {
  if (!answer) return { text: '--:--:--Z', title: 'Scenario time: waiting for the bridge' }
  const text = new Date(scenarioMs(answer.map, bridgeMs)).toISOString().slice(11, 19) + 'Z'
  if (answer.source !== 'MURMUR_AT') return { text, title: "Scenario time: the bridge's own clock (no scenario start set), UTC" }
  const rate = answer.rate !== 1 ? `, at ${answer.rate}x` : ''
  return { text, title: `Scenario time, UTC: the world's clock (its sun, weather and tides), from ${answer.at ?? 'the scenario start'} at the run's start${rate}` }
}

// Times on cards (an impact, an estimate, a keep-out's wait) are the bridge's clock: said as Z
// they must be the same world time the top bar shows, or a card read "Impact at 20:13Z" beside a
// bar at 17:11Z. App sets the map once the bridge has said it; until then, the bridge's own clock.
let worldMap: ClockMap | null = null

/** App: the bridge's map from its clock to scenario time (null: not known yet). */
export function setWorldClock(m: ClockMap | null): void {
  worldMap = m
}

/** "17:12:44Z": a time on the bridge's clock (ms), said in scenario time, as the top bar says it. */
export function fmtWorldZ(bridgeMs: number): string {
  return new Date(worldMap ? scenarioMs(worldMap, bridgeMs) : bridgeMs).toISOString().slice(11, 19) + 'Z'
}
