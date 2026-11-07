// Checks for weapon.ts (no test runner in web/): node web/src/weapon.check.ts, as make check does.
// The claims (bd murmur-ju1h): a weapon is shown only as far as its identity has come; a launch is a
// launch (its bearing, no class, speed, target or estimate); a weapon is "type unknown" with its
// measured speed, and with a measured line but no target only its heading is drawn; a class is
// shown with its evidence; the estimated flight needs a derived target and strike time, ends at the
// reported strike once there is one, and counts down; the banner and the timeline say only what the
// stage knows; a lone fix is not called a weapon. Throws on the first failure, prints "weapon: ok".
import {
  classText,
  clockText,
  countdownS,
  estimate,
  estimateMapLabel,
  estimateText,
  heading,
  shownWeapons,
  SHOWN_MS,
  stageText,
  STRUCK_BANNER_MS,
  strikeFlash,
  struckGroups,
  timeline,
  weaponBanner,
  weaponReports,
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
const at = () => hunter

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
check(stageText(seen, nameOf) === 'launch, bearing 270° from HUNTER 1', stageText(seen, nameOf))
check(estimate(seen, hunter) === null && heading(seen, T0 + 81_000) === null && classText(seen) === null, 'a launch: no estimate, heading or class')
const launchBanner = weaponBanner([seen], T0 + 81_000, nameOf, at)!
check(launchBanner.head === 'LAUNCH', launchBanner.head)
check(
  launchBanner.text === 'launch, bearing 270° from HUNTER 1 from T-0004 · seen by HUNTER 1 EO/IR · weapon not yet tracked: type, speed and target unknown',
  launchBanner.text,
)

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
check(stageText(weapon, nameOf) === 'weapon, type unknown, 243 m/s', stageText(weapon, nameOf))
const h = heading(weapon, T0 + 83_000)!
check(h != null && near((h.at[0] - fix.point[0]) * mPerLon, 243, 0.5), `its heading a second on: ${h && (h.at[0] - fix.point[0]) * mPerLon}`)
check(near((heading(weapon, T0 + 99_000)!.at[0] - fix.point[0]) * mPerLon, 729, 1), 'no further than 3 s ahead')
check(estimate(weapon, hunter) === null, 'no target derived: no estimated flight')
const wb = weaponBanner([weapon], T0 + 82_400, nameOf, at)!
check(wb.head === 'INCOMING' && wb.text === 'weapon, type unknown, 243 m/s · seen by HUNTER 1 EO/IR · fixed by REAPER 1 radar', wb.text)
const jet: WeaponTrack = { ...weapon, id: 'W-2', report: null, launch: null, shooter: null, track: 'T-0009', speed_mps: 220, weapon_by: { speed_mps: 220, over_mps: 150, from_launch: false, closing_on: '1' } }
check(stageText(jet, nameOf) === 'weapon, type unknown, 220 m/s (track T-0009)', 'a contact track taken for a weapon names its track')

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
check(classText(classed) === 'ATGM-class: 250 m/s, from T-0004', classText(classed)!)
check(stageText(classed, nameOf) === 'ATGM-class: 250 m/s, from T-0004', 'the class stage says its evidence')
const est = estimate(classed, hunter)!
check(est != null && near(est.tof_s, 4, 0.01) && near(est.speed_mps, 250, 0.5), `from the flash to the strike time: ${est?.tof_s} s at ${est?.speed_mps}`)
check(est.launch_point === boat && est.impact_point === hunter && est.launch_t === T0 + 80_000, 'from the launch fix at the flash to the vehicle it heads for')
check(heading(classed, T0 + 82_500) === null, 'a target: the estimate, not a heading')
check(countdownS(est, classed, T0 + 82_000) === 2 && countdownS(est, classed, T0 + 90_000) === 0, 'a countdown that stops at 0')
check(estimateMapLabel(est, classed, T0 + 82_000) === 'EST. ATGM · 2.0 s', estimateMapLabel(est, classed, T0 + 82_000))
check(estimateMapLabel(est, { ...classed, class: null }, T0 + 85_000) === 'EST. WEAPON · due', 'due once past; WEAPON without a class')
check(estimateText(classed, est, nameOf).startsWith('heading for HUNTER 1 on its measured line'), estimateText(classed, est, nameOf))
const reach = { ...classed, aim_basis: 'reach' as const }
check(
  estimateText(reach, estimate(reach, hunter), nameOf) === "heading for HUNTER 1, the only one of ours within the ATGM-class's reach of the launch, at its nominal 250 m/s",
  estimateText(reach, estimate(reach, hunter), nameOf),
)
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
check(classText(burst) === 'Gun-class: muzzle flash, from T-0004', classText(burst)!)
check(estimate(burst, hunter) != null && estimateMapLabel(estimate(burst, hunter)!, burst, T0 + 80_000) === 'EST. GUN · 1.3 s', estimateMapLabel(estimate(burst, hunter)!, burst, T0 + 80_000))
const inc = weaponBanner([classed], T0 + 82_000, nameOf, at)!
check(
  inc.text === 'ATGM-class: 250 m/s, from T-0004 at HUNTER 1, strike expected in 2.0 s · seen by HUNTER 1 EO/IR · fixed by REAPER 1 radar',
  inc.text,
)

// ---- a lone fix is not a weapon -------------------------------------------------------------------
const plot: WeaponTrack = { ...weapon, id: 'W-3', report: null, stage: 'plot', launch: null, shooter: null, weapon_by: null, speed_mps: null, velocity_mps: null, kin_point: null, kin_t: null }
check(weaponBanner([plot], T0 + 82_400, nameOf, at) === null, 'a lone fix: no banner')

// ---- the strike ------------------------------------------------------------------------------
const struck: WeaponTrack = { ...classed, status: 'expended', ended_t: T0 + 84_500, struck: { uid: '1', t: T0 + 84_500 }, warning_s: 4.1 }
const est2 = estimate(struck, hunter)!
check(est2.impact_t === T0 + 84_500 && countdownS(est2, struck, T0 + 84_000) === null, 'struck: the estimate ends at the reported strike, no countdown')
check(strikeFlash(struck, T0 + 84_500) === 0 && strikeFlash(struck, T0 + 90_000) === null && strikeFlash(seen, T0 + 84_500) === null, 'a flash at the strike')
check(shownWeapons([struck], T0 + 84_500 + SHOWN_MS - 1).length === 1 && shownWeapons([struck], T0 + 84_500 + SHOWN_MS).length === 0, 'shown a minute after it ended')
check(weaponReports([seen, { ...seen, id: 'W-2', report: null }]).has('A-1') && weaponReports([seen]).size === 1, 'the acts that launched a weapon')
const hit = weaponBanner([struck], T0 + 86_000, nameOf, at)!
check(hit.tone === 'struck' && hit.head === 'HUNTER 1 STRUCK', hit.head)
check(hit.text === 'launch seen by HUNTER 1 EO/IR · ATGM-class: 250 m/s, from T-0004 · 4.1 s of warning · any damage is its own report (Inbox)', hit.text)
check(weaponBanner([struck], T0 + 84_500 + STRUCK_BANNER_MS, nameOf, at) === null, 'the strike banner goes')
const blind: WeaponTrack = { ...plot, id: 'W-4', fixes: [], status: 'expended', ended_t: T0 + 84_500, struck: { uid: '1', t: T0 + 84_500 }, first_cue_t: null, warning_s: null }
check(weaponBanner([blind], T0 + 86_000, nameOf, at)!.text === 'no sensor saw the launch or the weapon · any damage is its own report (Inbox)', 'an unseen strike')

// ---- the strikes on one vehicle are labelled once ------------------------------------------------
const hmg = (k: number): WeaponTrack => ({ ...struck, id: `W-${10 + k}`, class: 'Gun-class', class_symbol: 'gun', struck: { uid: '1', t: T0 + 90_000 + k * 3000 } })
const gun = struckGroups([hmg(2), hmg(0), hmg(1)], nameOf)
check(gun.length === 1 && gun[0].n === 3 && gun[0].newest.id === 'W-12', 'three bursts on HUNTER 1: one group, the newest last')
check(gun[0].label === 'GUN STRUCK HUNTER 1 ×3', gun[0].label)
check(struckGroups([struck], nameOf)[0].label === 'ATGM STRUCK HUNTER 1', 'one strike: no count')
const mixed = struckGroups([hmg(0), struck, { ...struck, id: 'W-20', struck: { uid: '2', t: T0 + 85_000 } }], nameOf)
check(mixed.length === 2 && mixed.find((g) => g.uid === '1')!.label === 'ATGM + GUN STRUCK HUNTER 1 ×2', 'classes in the order they struck, a group a vehicle')
check(struckGroups([seen, weapon], nameOf).length === 0, 'nothing struck: no label')

// ---- the timeline ----------------------------------------------------------------------------
check(clockText(T0 + 80_400, T0) === '1:20.4' && clockText(T0 + 9_000, T0) === '0:09.0', 'the run clock')
check(
  timeline(struck, nameOf, T0) ===
    'launch seen by HUNTER 1 EO/IR at 1:20.4; weapon fixed by REAPER 1 radar from 1:22.0; assessed ATGM-class: 250 m/s, from T-0004; HUNTER 1 struck at 1:24.5; 4.1 s of warning',
  timeline(struck, nameOf, T0),
)
check(timeline(seen, nameOf, T0) === 'launch seen by HUNTER 1 EO/IR at 1:20.4; weapon not fixed in flight; strike to come', timeline(seen, nameOf, T0))
check(timeline(blind, nameOf, T0) === 'launch not seen; weapon not fixed in flight; HUNTER 1 struck at 1:24.5', timeline(blind, nameOf, T0))

console.log('weapon: ok')
