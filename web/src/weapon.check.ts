// Checks for weapon.ts (no test runner in web/): node web/src/weapon.check.ts, as make check does.
// The claims: a seen launch gives an estimated flight from the launch fix to the vehicle it heads
// for at the class's speed, labelled as an estimate with a countdown, ending at the reported strike
// once there is one; no launch seen, no estimate (only fixes and the strike); the banner says
// INCOMING while a detected weapon flies, then the strike with its warning; the timeline names each
// cue and when. Throws on the first failure, prints "weapon: ok" otherwise.
import {
  clockText,
  countdownS,
  estimate,
  estimateMapLabel,
  estimateText,
  shownWeapons,
  SHOWN_MS,
  STRUCK_BANNER_MS,
  strikeFlash,
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
// HUNTER 1 1 km east of the boat
const hunter: [number, number] = [-122.495 + 1000 / (111_320 * Math.cos((37.812 * Math.PI) / 180)), 37.812]
const w: WeaponTrack = {
  id: 'W-1',
  report: 'A-1',
  class: 'ATGM-class missile',
  speed_mps: 250,
  aimed_at: '1',
  launch: { t: T0 + 80_400, point: boat, sigma_m: 20, sensors: ['1:eo_ir'] },
  fixes: [],
  status: 'in_flight',
  ended_t: null,
  struck: null,
  eta_t: T0 + 84_400,
  first_cue_t: T0 + 80_400,
  warning_s: null,
}

// ---- the estimate ----------------------------------------------------------------------------
const est = estimate(w, hunter)!
check(est != null && near(est.tof_s, 4, 0.01), `1 km at 250 m/s: ${est?.tof_s}`)
check(est.launch_point === boat && est.impact_point === hunter, 'from the launch fix to the vehicle it heads for')
check(near(est.impact_t, T0 + 84_400, 5), 'its expected strike')
check(countdownS(est, w, T0 + 82_400) === 2 && countdownS(est, w, T0 + 90_000) === 0, 'a countdown that stops at 0')
check(estimateMapLabel(est, w, T0 + 82_400) === 'EST. ATGM · 2.0 s', estimateMapLabel(est, w, T0 + 82_400))
check(estimateMapLabel(est, w, T0 + 85_000) === 'EST. ATGM · due', 'due once past')
check(estimateText(w, est, nameOf) === 'estimated from the launch seen by HUNTER 1 EO/IR: ATGM-class missile, about 4 s', estimateText(w, est, nameOf))
check(estimate({ ...w, launch: null }, hunter) === null, 'no launch seen: no estimate')
check(estimate(w, null) === null && estimate({ ...w, speed_mps: null }, hunter) === null, 'nor without the victim or a speed')
check(weaponShort('ATGM-class missile') === 'ATGM' && weaponShort(null) === 'WEAPON', 'short names')

// ---- the strike ------------------------------------------------------------------------------
const struck: WeaponTrack = {
  ...w,
  fixes: [{ t: T0 + 82_000, point: [-122.489, 37.812], sigma_m: 75, sensor: '2:radar' }],
  status: 'expended',
  ended_t: T0 + 84_500,
  struck: { uid: '1', t: T0 + 84_500 },
  warning_s: 4.1,
}
const est2 = estimate(struck, hunter)!
check(est2.impact_t === T0 + 84_500 && countdownS(est2, struck, T0 + 84_000) === null, 'struck: the estimate ends at the reported strike, no countdown')
check(strikeFlash(struck, T0 + 84_500) === 0 && strikeFlash(struck, T0 + 90_000) === null && strikeFlash(w, T0 + 84_500) === null, 'a flash at the strike')
check(shownWeapons([struck], T0 + 84_500 + SHOWN_MS - 1).length === 1 && shownWeapons([struck], T0 + 84_500 + SHOWN_MS).length === 0, 'shown a minute after it ended')
check(weaponReports([w, { ...w, id: 'W-2', report: null }]).has('A-1') && weaponReports([w]).size === 1, 'the acts that launched a weapon')

// ---- the banner ------------------------------------------------------------------------------
const at = () => hunter
const inc = weaponBanner([w], T0 + 82_400, nameOf, at)!
check(inc.tone === 'incoming' && inc.head === 'INCOMING', 'INCOMING while it flies')
check(inc.text === 'ATGM at HUNTER 1, strike expected in 2.0 s · launch seen by HUNTER 1 EO/IR · weapon not tracked', inc.text)
const tracked = weaponBanner([{ ...w, fixes: struck.fixes }], T0 + 82_400, nameOf, at)!
check(tracked.text.endsWith('weapon fixed by REAPER 1 radar'), tracked.text)
const hit = weaponBanner([struck], T0 + 86_000, nameOf, at)!
check(hit.tone === 'struck' && hit.head === 'HUNTER 1 STRUCK', hit.head)
check(hit.text === 'launch seen by HUNTER 1 EO/IR · 4.1 s of warning · no damage is modelled', hit.text)
check(weaponBanner([struck], T0 + 84_500 + STRUCK_BANNER_MS, nameOf, at) === null, 'the strike banner goes')
const blind: WeaponTrack = { ...struck, id: 'W-3', report: null, class: null, speed_mps: null, aimed_at: null, launch: null, fixes: [], first_cue_t: null, warning_s: null }
check(weaponBanner([blind], T0 + 86_000, nameOf, at)!.text === 'no sensor saw the launch or the weapon · no damage is modelled', 'an unseen strike')
check(weaponBanner([{ ...blind, status: 'in_flight', struck: null, ended_t: null }], T0 + 86_000, nameOf, at) === null, 'nothing known yet: no banner')

// ---- the timeline ----------------------------------------------------------------------------
check(clockText(T0 + 80_400, T0) === '1:20.4' && clockText(T0 + 9_000, T0) === '0:09.0', 'the run clock')
check(
  timeline(struck, nameOf, T0) === 'launch seen by HUNTER 1 EO/IR at 1:20.4; weapon fixed by REAPER 1 radar from 1:22.0; HUNTER 1 struck at 1:24.5; 4.1 s of warning',
  timeline(struck, nameOf, T0),
)
check(timeline(blind, nameOf, T0) === 'launch not seen; weapon not fixed in flight; HUNTER 1 struck at 1:24.5', timeline(blind, nameOf, T0))

console.log('weapon: ok')
