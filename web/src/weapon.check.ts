// Checks for weapon.ts (no test runner in web/): node web/src/weapon.check.ts, as make check does.
// The claims (bd murmur-ju1h): a weapon is shown only as far as its identity has come; a launch is a
// launch (its bearing, no class, speed, target or estimate); a weapon is "type unknown" with its
// measured speed, and with a measured line but no target only its heading is drawn; a class is
// shown with its evidence; the estimated flight needs a derived target and strike time, ends at the
// reported strike once there is one, and counts down; the map's one line says only what the stage
// knows. Throws on the first failure, prints "weapon: ok".
import {
  countdownS,
  estimate,
  heading,
  shownWeapons,
  SHOWN_MS,
  strikeFlash,
  weaponReports,
  weaponLine,
  weaponShort,
  type WeaponTrack,
} from './weapon.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`weapon check failed: ${what}`)
}
const near = (a: number, b: number, eps = 1e-6) => Math.abs(a - b) <= eps

const names: Record<string, string> = { '1': 'HUNTER 1', '2': 'REAPER 1' }
const nameOf = (uid: string) => names[uid] ?? `vehicle ${uid}`
const T0 = 1_781_431_200_000
const boat: [number, number] = [-122.495, 37.812]
const mPerLon = 111_320 * Math.cos((37.812 * Math.PI) / 180)
// HUNTER 1 1 km east of the boat
const hunter: [number, number] = [-122.495 + 1000 / mPerLon, 37.812]

// ---- the launch stage: where and the bearing, nothing more ----------------------------------------
const seen: WeaponTrack = {
  id: 'W-1',
  report: 'A-1',
  stage: 'launch',
  class: null,
  class_symbol: null,
  evidence: [],
  weapon_by: null,
  speed_mps: null,
  speed_sigma_mps: null,
  velocity_mps: null,
  kin_t: null,
  kin_point: null,
  track: null,
  shooter: 'T-0004',
  aimed_at: null,
  aim_basis: null,
  eta_t: null,
  nominal_mps: null,
  launch: { t: T0 + 80_400, flash_t: T0 + 80_000, point: boat, sigma_m: 20, sensors: ['1:eo_ir'], observer: { uid: '1', point: hunter }, bearing_deg: 270 },
  fixes: [],
  status: 'in_flight',
  ended_t: null,
  struck: null,
  first_cue_t: T0 + 80_400,
  warning_s: null,
}
check(estimate(seen, hunter) === null && heading(seen, T0 + 81_000) === null, 'a launch: no estimate or heading')

// ---- the weapon stage: type unknown, its measured speed, its heading with no target -------------
const fix = { t: T0 + 82_000, point: [-122.495 + 500 / mPerLon, 37.812] as [number, number], sigma_m: 150, sensor: '2:radar' }
const weapon: WeaponTrack = {
  ...seen,
  stage: 'weapon',
  weapon_by: { speed_mps: 243, over_mps: 60, from_launch: true, closing_on: null },
  speed_mps: 243,
  speed_sigma_mps: 84,
  velocity_mps: [243, 0],
  kin_t: fix.t,
  kin_point: fix.point,
  fixes: [fix],
}
const h = heading(weapon, T0 + 83_000)!
check(h != null && near((h.at[0] - fix.point[0]) * mPerLon, 243, 0.5), `its heading a second on: ${h && (h.at[0] - fix.point[0]) * mPerLon}`)
check(near((heading(weapon, T0 + 99_000)!.at[0] - fix.point[0]) * mPerLon, 729, 1), 'no further than 3 s ahead')
check(estimate(weapon, hunter) === null, 'no target derived: no estimated flight')

// ---- the class stage: the class with its evidence, the estimate on a derived target ----------------
const classed: WeaponTrack = {
  ...weapon,
  stage: 'class',
  class: 'ATGM-class',
  class_symbol: 'missile',
  evidence: [
    { basis: 'speed', short: '250 m/s', text: 'measured 250 m/s' },
    { basis: 'shooter', short: 'from T-0004', text: 'launched from T-0004' },
  ],
  speed_mps: 250,
  aimed_at: '1',
  aim_basis: 'track',
  eta_t: T0 + 84_000,
  nominal_mps: 250,
}
const est = estimate(classed, hunter)!
check(est != null && near(est.tof_s, 4, 0.01) && near(est.speed_mps, 250, 0.5), `from the flash to the strike time: ${est?.tof_s} s at ${est?.speed_mps}`)
check(est.launch_point === boat && est.impact_point === hunter && est.launch_t === T0 + 80_000, 'from the launch fix at the flash to the vehicle it heads for')
check(heading(classed, T0 + 82_500) === null, 'a target: the estimate, not a heading')
check(countdownS(est, classed, T0 + 82_000) === 2 && countdownS(est, classed, T0 + 90_000) === 0, 'a countdown that stops at 0')
// the map's one line: what it is as far as its stage has come, whom it heads for, the seconds left
check(weaponLine(classed, est, T0 + 82_000, nameOf) === 'ATGM at HUNTER 1, 2 s', weaponLine(classed, est, T0 + 82_000, nameOf))
check(weaponLine({ ...classed, stage: 'weapon', class: null }, est, T0 + 85_000, nameOf) === 'weapon at HUNTER 1, due', 'due once past; weapon below the class stage')
check(estimate(classed, null) === null && estimate({ ...classed, eta_t: null }, hunter) === null, 'nor without the victim or a strike time')
check(weaponShort('ATGM-class') === 'ATGM' && weaponShort(null) === 'WEAPON', 'short names')
// every class of the threat library (section 15): its short name, and a gun's burst told by its
// muzzle flash, classed at its launch, aimed by its reach
check(
  weaponShort('Air missile-class') === 'AIR MISSILE' && weaponShort('Rocket-class') === 'ROCKET' && weaponShort('Gun-class') === 'GUN' && weaponShort('Torpedo-class') === 'TORPEDO',
  'every class short',
)
const burst: WeaponTrack = {
  ...seen,
  id: 'W-5',
  stage: 'class',
  class: 'Gun-class',
  class_symbol: 'gun',
  evidence: [
    { basis: 'launch', short: 'muzzle flash', text: '1:eo_ir told it as gunfire, a muzzle flash and no plume' },
    { basis: 'shooter', short: 'from T-0004', text: 'launched from T-0004' },
  ],
  launch: { ...seen.launch!, look: 'muzzle' },
  aimed_at: '1',
  aim_basis: 'reach',
  eta_t: T0 + 80_000 + 1250,
  nominal_mps: 800,
}
check(estimate(burst, hunter) != null && weaponLine(burst, estimate(burst, hunter), T0 + 80_000, nameOf) === 'gun at HUNTER 1, 1 s', weaponLine(burst, estimate(burst, hunter), T0 + 80_000, nameOf))
check(weaponLine({ ...classed, class: 'Air missile-class' }, est, T0 + 82_000, nameOf) === 'air missile at HUNTER 1, 2 s', 'a class in words')
// below the class stage: a launch says what it was told as, a weapon with no target its speed
check(weaponLine(seen, null, T0 + 81_000, nameOf) === 'launch' && weaponLine({ ...seen, launch: { ...seen.launch!, look: 'missile' } }, null, T0 + 81_000, nameOf) === 'missile launch', 'a launch')
check(weaponLine(weapon, null, T0 + 82_400, nameOf) === 'weapon, 243 m/s', weaponLine(weapon, null, T0 + 82_400, nameOf))

// ---- the strike ------------------------------------------------------------------------------
const struck: WeaponTrack = { ...classed, status: 'expended', ended_t: T0 + 84_500, struck: { uid: '1', t: T0 + 84_500 }, warning_s: 4.1 }
const est2 = estimate(struck, hunter)!
check(est2.impact_t === T0 + 84_500 && countdownS(est2, struck, T0 + 84_000) === null, 'struck: the estimate ends at the reported strike, no countdown')
check(strikeFlash(struck, T0 + 84_500) === 0 && strikeFlash(struck, T0 + 90_000) === null && strikeFlash(seen, T0 + 84_500) === null, 'a flash at the strike')
check(shownWeapons([struck], T0 + 84_500 + SHOWN_MS - 1).length === 1 && shownWeapons([struck], T0 + 84_500 + SHOWN_MS).length === 0, 'shown a minute after it ended')
check(weaponReports([seen, { ...seen, id: 'W-2', report: null }]).has('A-1') && weaponReports([seen]).size === 1, 'the acts that launched a weapon')

console.log('weapon: ok')
