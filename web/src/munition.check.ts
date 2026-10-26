// Checks for munition.ts: there is no test runner in web/, so run them with Node 22.18 or later,
// which strips the types itself (make check does this):
//   node web/src/munition.check.ts
// Throws on the first failure, prints "munition: ok" otherwise. Not part of the bundle.
import {
  dashesAhead,
  DASHES,
  flightFraction,
  inFlight,
  launchFlash,
  munitionAt,
  munitionLabel,
  munitionShort,
  munitionShown,
  screenAngle,
  secondsToImpact,
  trailPoints,
  TRAIL_MS,
  type Munition,
} from './munition.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`munition check failed: ${what}`)
}
const near = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps

// the contract's example: released 7,600 m up, 18.4 s to the surface
const m: Munition = {
  name: 'Hellfire-class missile',
  speed_mps: 425,
  launch_t: 1_793_001_234_000,
  launch_point: [-122.525, 37.8],
  launch_alt_m: 7600,
  impact_t: 1_793_001_252_400,
  impact_point: [-122.51, 37.79],
  impact_alt_m: 0,
  tof_s: 18.4,
}
const T0 = m.launch_t
const mid = T0 + 9_200

// position: linear in lon, lat and altitude, held at either end
check(flightFraction(m, T0) === 0 && flightFraction(m, m.impact_t) === 1, 'ends')
check(near(flightFraction(m, mid), 0.5), 'half way at half the time')
const [lon, lat, alt] = munitionAt(m, mid)
check(near(lon, -122.5175) && near(lat, 37.795) && near(alt, 3800), 'half way in lon, lat and altitude')
check(munitionAt(m, T0 - 5000)[2] === 7600, 'before the launch: at the launch point')
check(munitionAt(m, m.impact_t + 5000)[2] === 0 && munitionAt(m, m.impact_t + 5000)[0] === -122.51, 'after the impact: at the impact point')

// in flight from the launch up to (not including) the impact
check(!inFlight(m, T0 - 1) && inFlight(m, T0) && inFlight(m, m.impact_t - 1) && !inFlight(m, m.impact_t), 'in flight between its ends')

// seconds to impact, rounded up: 0 only at the impact
check(secondsToImpact(m, T0) === 19, '18.4 s reads 19 s at the launch')
check(secondsToImpact(m, m.impact_t - 8_600) === 9, '8.6 s reads 9 s')
check(secondsToImpact(m, m.impact_t - 1) === 1 && secondsToImpact(m, m.impact_t) === 0, 'the last second, then the impact')
check(secondsToImpact(m, m.impact_t + 3000) === 0, 'never negative')

// labels
check(munitionShort('Hellfire-class missile') === 'HELLFIRE', 'short name')
check(munitionShort('JAGM') === 'JAGM', 'a one-word name')
check(munitionLabel(m, m.impact_t - 8_600) === 'HELLFIRE · 9 s', 'map label')

// the launch flash: from the launch, under a second
check(launchFlash(m, T0 - 1) === null && launchFlash(m, T0) === 0, 'flash starts at the launch')
check(launchFlash(m, T0 + 450)! > 0.4 && launchFlash(m, T0 + 450)! < 0.6, 'flash half way')
check(launchFlash(m, T0 + 5000) === null, 'flash over')
check(munitionShown(m, T0 + 5000) && !munitionShown(m, m.impact_t), 'shown in flight, gone at the impact')
// a flight shorter than the flash still shows the flash out
const quick: Munition = { ...m, impact_t: T0 + 300, tof_s: 0.3 }
check(munitionShown(quick, T0 + 600) && !inFlight(quick, T0 + 600), 'the flash outlasts a short flight')

// the trail: ends at the munition, covers TRAIL_MS of flight, less just after the launch
const trail = trailPoints(m, mid)
check(trail.length === 9, 'trail points')
check(near(trail[8][2], munitionAt(m, mid)[2]) && near(trail[8][0], munitionAt(m, mid)[0]), 'trail ends at the munition')
check(near(trail[0][2], munitionAt(m, mid - TRAIL_MS)[2], 1e-6), 'trail starts TRAIL_MS back')
check(near(trailPoints(m, T0 + 500)[0][2], 7600), 'just after the launch the trail starts at the launch point')
check(trailPoints(m, T0).length === 0, 'no trail before it moves')

// the dashes ahead: from the munition to the impact, fixed in space as it flies
const ahead = dashesAhead(m, T0)
check(ahead.length === DASHES, 'a dash per period at the launch')
check(near(ahead[0][1][2], 0) && near(ahead[0][1][0], -122.51), 'the first dash ends at the impact point')
const later = dashesAhead(m, mid + 100)
check(later.length < ahead.length && later.length > 0, 'fewer dashes as it flies')
check(near(later[0][0][2], ahead[0][0][2]) && near(later[1][1][2], ahead[1][1][2]), 'the dashes ahead do not move')
const cut = later[later.length - 1]
check(cut[0][2] <= munitionAt(m, mid + 100)[2] + 1e-6, 'none behind the munition')
check(dashesAhead(m, m.impact_t).length === 0, 'none at the impact')

// screen angle: an icon drawn nose up turned to point along a screen direction (y down)
check(near(screenAngle(0, -1), 0), 'up: no turn')
check(near(screenAngle(1, 0), -90), 'right: a quarter turn clockwise')
check(near(screenAngle(-1, 0), 90), 'left: a quarter turn counter-clockwise')
check(near(Math.abs(screenAngle(0, 1)), 180), 'down: half a turn')

console.log('munition: ok')
