// Checks for scenarioClock.ts (no test runner in web/): node web/src/scenarioClock.check.ts, as make
// check does. The claims (bd murmur-iobd.5): the top bar shows scenario time, the start plus the
// bridge's time since it times the rate, whatever the browser's clock says; without a start it is
// the bridge's own clock; before the bridge answers it shows no time. Throws on the first failure,
// prints "scenarioClock: ok" otherwise.
import { clockText, fmtWorldZ, runClock, scenarioMs, setWorldClock, type ScenarioClockAnswer } from './scenarioClock.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`scenarioClock: ${what}`)
}

const started = Date.parse('2026-11-24T15:00:00Z') // the bridge's clock when it started (its wall time)
const at = Date.parse('2026-06-14T17:00:00Z')
const situation: ScenarioClockAnswer = { map: { engine_origin: started, scenario_origin: at, rate: 1 }, rate: 1, source: 'MURMUR_AT', at: '2026-06-14T17:00:00.000Z' }

// a situation at 17:00Z, ten minutes in, while the browser and the bridge read 15:10Z
check(clockText(situation, started + 10 * 60_000).text === '17:10:00Z', `the world's time, not the wall clock's (${clockText(situation, started + 10 * 60_000).text})`)
check(clockText(situation, started).title.includes('2026-06-14T17:00:00.000Z'), 'the hover says where the time comes from')

// at a rate: the scenario runs faster than the bridge's clock
const fast: ScenarioClockAnswer = { ...situation, map: { ...situation.map, rate: 60 }, rate: 60 }
check(scenarioMs(fast.map, started + 60_000) === at + 3_600_000, 'a minute at 60x is an hour')
check(clockText(fast, started + 60_000).text === '18:00:00Z' && clockText(fast, started).title.includes('60x'), 'the rate is shown and said')

// no start: the identity map, the bridge's own clock
const own: ScenarioClockAnswer = { map: { engine_origin: 0, scenario_origin: 0, rate: 1 }, rate: 1, source: 'wall clock', at: null }
check(clockText(own, started).text === '15:00:00Z' && clockText(own, started).title.includes("bridge's own clock"), 'without a start: the bridge clock, said so')

// before the bridge answers
check(clockText(null, started).text === '--:--:--Z', 'no time before the bridge answers')

// a card's time on the bridge's clock is said in the world's time once the map is known
check(fmtWorldZ(started + 10 * 60_000) === '15:10:00Z', "before the map: the bridge's own clock")
setWorldClock(situation.map)
check(fmtWorldZ(started + 10 * 60_000) === '17:10:00Z', `a card's impact time agrees with the top bar (${fmtWorldZ(started + 10 * 60_000)})`)
setWorldClock(null)

// a situation's time beside it, once (new piece 8)
check(runClock({ elapsed: 207, left: 513 }) === 'T+3:27 · 8:33 left', 'T+ and time left')
check(runClock({ elapsed: null, left: null }) === 'T+--:--', 'before the situation clock starts')
check(runClock(null) === null, 'no situation: nothing beside the clock')

console.log('scenarioClock: ok')
