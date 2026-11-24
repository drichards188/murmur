// Checks for munition.ts: there is no test runner in web/, so run them with Node 22.18 or later,
// which strips the types itself (make check does this):
//   node web/src/munition.check.ts
// Throws on the first failure, prints "munition: ok" otherwise. Not part of the bundle.
import {
  dashesAhead,
  DASHES,
  DIVE_CLEAR_M,
  DIVE_CRUISE_ABOVE_M,
  DIVE_CRUISE_MPS,
  DIVE_END_M,
  DIVE_END_MPS,
  DIVE_FLOOR_M,
  DIVE_MAX_M,
  DIVE_POP_M,
  DIVE_POP_MPS,
  divePath,
  dives,
  flightFraction,
  flightPath,
  headingPoint,
  loadoutProfile,
  inFlight,
  munitionAt,
  munitionLabel,
  munitionShort,
  pointAlong,
  screenAngle,
  secondsToImpact,
  straightPath,
  type FlightPath,
  type GroundAt,
  type Munition,
  type Pt3,
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

// drawn in flight only: no flash at its launch
check(inFlight(m, T0 + 5000) && !inFlight(m, m.impact_t) && !inFlight(m, T0 - 1), 'drawn in flight, gone at the impact')

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

// ---- a drone's dive (bd murmur-4nik.5): its own path over the model's ends and times ----

// the shore battery's shot: a launcher UGV 60 m up on the bluff at the bridge's south end, a boat
// mid-strait 1.7 km off, the catalog's 20 m/s along the straight slant line (the model's time)
const COS = Math.cos((37.8064 * Math.PI) / 180)
const metres = (a: Pt3, b: Pt3) => Math.hypot((b[0] - a[0]) * 111_320 * COS, (b[1] - a[1]) * 111_320, b[2] - a[2])
const groundM = (a: Pt3, b: Pt3) => Math.hypot((b[0] - a[0]) * 111_320 * COS, (b[1] - a[1]) * 111_320)
function shot(launch: [number, number], launchAlt: number, impact: [number, number], impactAlt: number): Munition {
  const slant = metres([...launch, launchAlt], [...impact, impactAlt])
  const tof = slant / 20
  return { name: 'HIV3-class launcher', speed_mps: 20, launch_t: T0, launch_point: launch, launch_alt_m: launchAlt, impact_t: T0 + Math.round(tof * 1000), impact_point: impact, impact_alt_m: impactAlt, tof_s: tof }
}
const d1 = shot([-122.4768, 37.8064], 60, [-122.495, 37.812], 0)
const L0: Pt3 = [...d1.launch_point, d1.launch_alt_m]
const I0: Pt3 = [...d1.impact_point, d1.impact_alt_m]
const same = (a: Pt3, b: Pt3, eps = 1e-6) => metres(a, b) <= eps

// which flights dive: a catalog missile of the dive profile; every other stays straight
check(dives('missile', 'dive') && dives('Missile', ' DIVE '), 'a dive missile dives')
check(!dives('missile', 'lofted') && !dives('missile', 'direct') && !dives('missile', null) && !dives('self', 'dive') && !dives(undefined, 'dive'), 'nothing else dives')
check(flightPath({ m: d1, kind: 'missile', profile: 'dive' }).shaped, 'a dive is shaped')
check(!flightPath({ m: d1, kind: 'missile', profile: 'lofted' }).shaped && !flightPath({ m, kind: 'bomb', profile: 'glide' }).shaped, 'a lofted missile and a bomb are straight')
// the straight path is the model's line, exactly as before
const line = straightPath(m)
check(near(line.length_m, Math.hypot((m.impact_point[0] - m.launch_point[0]) * 111_320 * Math.cos((37.8 * Math.PI) / 180), (m.impact_point[1] - m.launch_point[1]) * 111_320, 7600)), 'the straight length')
for (const t of [T0 - 1000, T0, T0 + 3000, mid, m.impact_t - 1, m.impact_t + 1000]) {
  const [x, y] = [munitionAt(m, t), munitionAt(m, t, line)]
  check(x[0] === y[0] && x[1] === y[1] && x[2] === y[2], 'the straight path is the line, at every time')
}
check(JSON.stringify(dashesAhead(m, mid, line)) === JSON.stringify(dashesAhead(m, mid)), 'and its dashes')

const dive = divePath(d1)
const at = (t: number, p: FlightPath = dive) => munitionAt(d1, t, p)
const T1 = d1.impact_t
// the same ends at the same times: the model's, which the engine and the truth side read
check(same(at(T0), L0) && same(at(T0 - 5000), L0), 'it leaves from the launch point at the launch')
check(same(at(T1), I0) && same(at(T1 + 5000), I0), 'it reaches the impact point at the impact')
check(same(dive.point(0), L0) && same(dive.point(1), I0) && dive.flown(0) === 0 && dive.flown(1) === 1, 'the path between the two ends')
check(inFlight(d1, T1 - 1) && !inFlight(d1, T1) && secondsToImpact(d1, T0) === Math.ceil(d1.tof_s), 'in flight and timed as the model')
check(dive.length_m > metres(L0, I0), 'longer than the slant line')

// progress: never back, along the path in length and along the track on the ground
let lastU = -1
let lastD = -1
let lowest = Infinity
for (let k = 0; k <= 2000; k++) {
  const f = k / 2000
  const u = dive.flown(f)
  const p = dive.point(u)
  check(u >= lastU - 1e-12, `flown never goes back (${f})`)
  check(groundM(L0, p) >= lastD - 1e-6, `along the track, never back (${f})`)
  lastU = u
  lastD = groundM(L0, p)
  lowest = Math.min(lowest, p[2])
}
check(lowest >= 0, 'never under the sea')

// the pop-up: straight up out of the tube in the first second or two, then away
const pop = at(T0 + (DIVE_POP_M / DIVE_POP_MPS) * 1000 * 0.9)
check(groundM(L0, pop) < 1 && pop[2] > L0[2] + DIVE_POP_M * 0.8, 'it pops up out of the tube')
check(groundM(L0, at(T0 + 5000)) > 20, 'then flies off along its track')
// the cruise: above the higher end, at about the catalog's speed
const cruiseH = Math.max(d1.launch_alt_m, d1.impact_alt_m) + DIVE_CRUISE_ABOVE_M
const midway = at(T0 + (T1 - T0) / 2)
check(near(midway[2], cruiseH, 1e-6), `it cruises ${DIVE_CRUISE_ABOVE_M} m above the higher end (${midway[2]})`)
check(Math.max(...Array.from({ length: 101 }, (_, k) => at(T0 + ((T1 - T0) * k) / 100)[2])) <= cruiseH + 1e-6, 'and no higher')
const speed = (t: number, p: FlightPath = dive, mm: Munition = d1) => metres(munitionAt(mm, t - 100, p), munitionAt(mm, t + 100, p)) / 0.2
const vCruise = speed(T0 + (T1 - T0) / 2)
check(Math.abs(vCruise - DIVE_CRUISE_MPS) < 0.1 * DIVE_CRUISE_MPS, `about the cruise speed (${vCruise.toFixed(1)} m/s)`)
check(speed(T0 + 500) < 0.7 * vCruise, 'slower out of the tube: it speeds up to the cruise')
// the dive: over the last few hundred metres, steepening and faster, onto the point
const lastS = (s: number) => at(T1 - s * 1000)
check(groundM(lastS(2), I0) < DIVE_MAX_M && lastS(2)[2] < cruiseH - 20, 'diving in the last seconds')
const grade = (s: number) => (lastS(s + 0.25)[2] - lastS(s)[2]) / Math.max(1e-6, groundM(lastS(s + 0.25), lastS(s)))
check(grade(0.3) > grade(3) && grade(3) > grade(8) && grade(0.3) > 0.8, `steepening onto the point (${grade(8).toFixed(2)}, ${grade(3).toFixed(2)}, ${grade(0.3).toFixed(2)})`)
const vEnd = speed(T1 - 300)
check(vEnd > 1.4 * vCruise && Math.abs(vEnd - DIVE_END_MPS) < 0.2 * DIVE_END_MPS, `faster in the dive (${vEnd.toFixed(1)} m/s)`)

// terrain: land 5 m up with a ridge 150 m high across the middle of the track, then the sea (the
// elevation tiles' seabed, 40 m down) out to the boat; the path keeps clear of the ridge and of
// the ground everywhere, the cruise above it
const ridge: GroundAt = (lon) => (lon < -122.49 ? -40 : Math.abs(lon - -122.486) < 0.002 ? 150 : 5)
const over = divePath(d1, ridge)
for (let k = 1; k < 1000; k++) {
  const p = over.point(k / 1000)
  const g = Math.max(0, ridge(p[0], p[1])!)
  const fromEnd = groundM(p, I0)
  check(p[2] >= g - 1e-6, `never under the ground (${k})`)
  if (fromEnd > DIVE_END_M && groundM(L0, p) > 1) check(p[2] >= g + DIVE_FLOOR_M - 1e-6, `kept above the ground away from the point (${k})`)
}
check(near(over.point(0.5)[2], 150 + DIVE_CLEAR_M, 1e-6), 'the cruise clears the ridge')
check(same(over.point(0), L0) && same(over.point(1), I0) && same(munitionAt(d1, T1, over), I0), 'the same ends over the ridge')
const vOver = speed(T0 + (T1 - T0) / 2, over)
check(Math.abs(vOver - DIVE_CRUISE_MPS) < 0.1 * DIVE_CRUISE_MPS, `about the cruise speed over it (${vOver.toFixed(1)} m/s)`)
// a mountain in the way makes the path far longer in the same time: the whole profile is flown
// faster, still arriving at the impact's time
const alp = divePath(d1, (lon) => (lon < -122.49 ? -40 : Math.abs(lon - -122.486) < 0.002 ? 700 : 5))
const vAlp = speed(T0 + (T1 - T0) / 2, alp)
check(alp.length_m > over.length_m + 500 && vAlp > 1.2 * vCruise && same(munitionAt(d1, T1, alp), I0), `a longer path in the same time, flown faster (${vAlp.toFixed(1)} m/s)`)
// a ground the console has nowhere (null) is no ground: the path is as without it
check(same(divePath(d1, () => null).point(0.5), dive.point(0.5)), 'no terrain, no change')
// bathymetry reads below the sea: the floor is the water
check(divePath(d1, () => -90).point(0.99)[2] >= 0, 'never under the water')

// a short shot (inside the band's 350 m) and a long one (10 km): the same ends, never back
for (const mm of [shot([-122.4768, 37.8064], 60, [-122.4806, 37.8064], 0), shot([-122.4768, 37.8064], 60, [-122.59, 37.8064], 0)]) {
  const p = divePath(mm)
  check(p.shaped && same(munitionAt(mm, mm.launch_t, p), [...mm.launch_point, mm.launch_alt_m]) && same(munitionAt(mm, mm.impact_t, p), [...mm.impact_point, mm.impact_alt_m]), 'ends, short and long')
  let u0 = 0
  for (let k = 0; k <= 400; k++) {
    const u = p.flown(k / 400)
    check(u >= u0 - 1e-12 && p.point(u)[2] >= 0, 'never back, never under, short and long')
    u0 = u
  }
}
// no length or no time: the straight line
check(!divePath({ ...d1, impact_point: d1.launch_point }).shaped && !divePath({ ...d1, impact_t: T0 }).shaped, 'degenerate: straight')

// its dashes follow the path
const tm = T1 - 4000
const ahead2 = dashesAhead(d1, T0, dive)
check(ahead2.length === DASHES && ahead2.every((d) => d.length > 2), 'a dash per period, each bending with the path')
check(same(ahead2[0][ahead2[0].length - 1], I0), 'the first dash ends at the impact point')
const later2 = dashesAhead(d1, tm, dive)
check(later2.length < ahead2.length && later2.length > 0 && same(later2[0][0], ahead2[0][0]), 'fewer dashes as it flies, the ones ahead fixed')
check(groundM(L0, later2[later2.length - 1][0]) >= groundM(L0, at(tm)) - 1e-6, 'none behind it')
// its heading: along the path (a straight one's on the line to the impact)
const hm = headingPoint(dive, dive.flown(0.5))
check(near(hm[2], midway[2], 1e-6) && groundM(hm, I0) < groundM(midway, I0), 'level at the cruise, towards the point')
const uDive = dive.flown(flightFraction(d1, T1 - 1500))
check(headingPoint(dive, uDive)[2] < dive.point(uDive)[2] - 50, 'nose down in the dive')
const hs = headingPoint(line, 0.25)
const onLine = pointAlong(m, 0.25)
check(Math.abs((hs[0] - onLine[0]) * (m.impact_point[1] - m.launch_point[1]) - (hs[1] - onLine[1]) * (m.impact_point[0] - m.launch_point[0])) < 1e-12, 'a straight path heads along its line')

// the profile the intent record does not carry: the launcher's catalog loadout's
const loadout = [{ name: 'M2 .50 HMG', kind: 'gun', profile: 'ballistic' }, { name: 'HIV3-class launcher', kind: 'missile', profile: 'dive' }]
check(loadoutProfile(loadout, 'HIV3-class launcher') === 'dive' && loadoutProfile(loadout, 'nothing') === undefined && loadoutProfile(undefined, 'x') === undefined, 'the loadout profile')

console.log('munition: ok')
