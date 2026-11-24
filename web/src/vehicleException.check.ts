// Checks for vehicleException.ts (no test runner in web/): node web/src/vehicleException.check.ts, as
// make check does. The claims (bd murmur-iobd.1): one slot per vehicle, the most pressing first
// (confirmed lost, presumed lost, damaged, silent, link down, no GNSS, low battery, out of rounds,
// degraded); a damage
// report says what the vehicle can no longer do; silence keeps the words the map's badge always
// had ("LOST 12s" before rules 30, "SILENT 12s" under it); a platform is out of rounds only when
// every usable weapon is spent; no GNSS is never a sub under water, nor a vehicle not yet under way
// before its first fix; a battery is low at its floor or short of the way home with a margin; a
// reporting vehicle with nothing wrong has no slot. Throws on the
// first failure, prints "vehicleException: ok" otherwise.
import { batteryLow, batteryOf, damageOf, damageText, exceptionOf, exceptionPriority, type ExceptionContext, type ExceptionFacts, HOME_MARGIN, LOSS_PCT, LOW_BATTERY_PCT, NO_EXCEPTIONS, noGnssOf, outOfRounds, vehicleException } from './vehicleException.ts'
import type { Vehicle } from './types'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`vehicleException: ${what}`)
}

const base: ExceptionFacts = { stale: false, silentS: 0.2, standing: null, damage: null, emu: null, lossPct: 0, noRounds: false, estimate: false }
const kind = (f: Partial<ExceptionFacts>) => vehicleException({ ...base, ...f })?.kind ?? null
const text = (f: Partial<ExceptionFacts>) => vehicleException({ ...base, ...f })?.text ?? null

// nothing wrong: no slot
check(kind({}) === null, 'a reporting vehicle with nothing wrong has no slot')

// each on its own
check(text({ damage: { mobility: true, firepower: false } }) === 'DAMAGED: CANNOT MOVE', 'propulsion lost: it cannot move')
check(text({ damage: { mobility: false, firepower: true } }) === 'DAMAGED: CANNOT FIRE', 'weapons out: it cannot fire')
check(damageText({ mobility: true, firepower: true }) === 'DAMAGED: CANNOT MOVE OR FIRE', 'both')
check(text({ stale: true, silentS: 12.7 }) === 'LOST 12s', 'silent before rules 30: the badge as it always was')
check(text({ stale: true, silentS: 12.7, standing: 'silent' }) === 'SILENT 12s', 'silent under rules 30')
check(text({ stale: true, silentS: 40, standing: 'presumed_lost' }) === 'PRESUMED LOST 40s', 'presumed lost')
check(text({ standing: 'lost' }) === 'LOST' && vehicleException({ ...base, standing: 'lost' })?.tone === 'settled', 'confirmed lost: settled, grey')
check(kind({ emu: { state: 'down', cause: 'scripted cut' } }) === 'link_down', 'link down before it goes silent')
check(kind({ noRounds: true }) === 'no_rounds' && text({ noRounds: true }) === 'OUT OF ROUNDS', 'out of rounds')
check(kind({ emu: { state: 'degraded', cause: 'out of range' } }) === 'degraded', 'degraded by the emulator')
check(text({ lossPct: LOSS_PCT + 2.4 }) === `LINK ${LOSS_PCT + 2}% LOSS` && kind({ lossPct: LOSS_PCT }) === null, 'measured loss past the threshold only')
check(text({ noGnss: 'dead_reckoning' }) === 'NO GNSS' && vehicleException({ ...base, noGnss: 'dead_reckoning' })?.tone === 'warn', 'no satellite fix: amber')
check(text({ battery: { pct: 18.4, leftS: null, homeS: null } }) === 'LOW BATTERY 18%', 'a battery at its floor')
check(kind({ battery: { pct: LOW_BATTERY_PCT + 1, leftS: null, homeS: null } }) === null, 'above the floor, nothing to judge the way home by: not low')
check(kind({ battery: { pct: 60, leftS: 600, homeS: 600 } }) === 'low_battery', 'ten minutes left, ten to get home: low')
check(kind({ battery: { pct: 60, leftS: 600 * HOME_MARGIN + 1, homeS: 600 } }) === null, 'home with its margin: not low')
check(vehicleException({ ...base, battery: { pct: 60, leftS: 600, homeS: 540 } })?.title === 'Battery 60%. About 10 min left at its endurance; 9 min to get home.', 'the battery\'s title says the time left and the way home')

// the most pressing wins
check(kind({ standing: 'lost', stale: true, damage: { mobility: true, firepower: false } }) === 'lost', 'confirmed lost over all')
check(kind({ standing: 'presumed_lost', stale: true, damage: { mobility: true, firepower: false } }) === 'presumed', 'presumed over damaged')
check(kind({ stale: true, damage: { mobility: true, firepower: false } }) === 'damaged', 'damaged over silent (the fleet row counts it so too)')
check(kind({ stale: true, emu: { state: 'down', cause: '' } }) === 'silent', 'silent over link down')
check(kind({ emu: { state: 'down', cause: '' }, noRounds: true }) === 'link_down', 'link down over out of rounds')
check(kind({ noRounds: true, emu: { state: 'degraded', cause: '' } }) === 'no_rounds', 'out of rounds over degraded')
check(kind({ emu: { state: 'down', cause: '' }, noGnss: 'dead_reckoning' }) === 'link_down', 'link down over no GNSS')
check(kind({ noGnss: 'dead_reckoning', battery: { pct: 5, leftS: null, homeS: null } }) === 'no_gnss', 'no GNSS over low battery')
check(kind({ battery: { pct: 5, leftS: null, homeS: null }, noRounds: true }) === 'low_battery', 'low battery over out of rounds')

// the map's budget: damage weighs with what waits on the person, silence with ours on alert, never below the vehicle's own
const x = (f: Partial<ExceptionFacts>) => vehicleException({ ...base, ...f })!
check(exceptionPriority(x({ damage: { mobility: true, firepower: false } }), 4) === 1, 'damage: 1, before an event\'s label')
check(exceptionPriority(x({ stale: true }), 4) === 3, 'silent: 3, as the LOST badge was')
check(exceptionPriority(x({ noRounds: true }), 4) === 4 && exceptionPriority(x({ noRounds: true }), 1) === 1, 'the rest: the vehicle\'s own')

// from the bridge's words and answers
check(JSON.stringify(damageOf('reports propulsion lost since t=1795533627.2: it cannot move, so an order will not move it')) === '{"mobility":true,"firepower":false}', 'a damage alert read')
check(JSON.stringify(damageOf('reports propulsion lost and weapons out of action since t=1.0')) === '{"mobility":true,"firepower":true}', 'both read')
check(outOfRounds([{ kind: 'missile', left: 0 }, { kind: 'bomb', left: 0 }]), 'every round spent')
check(!outOfRounds([{ kind: 'missile', left: 0 }, { kind: 'bomb', left: 1 }]), 'one left: not out')
check(!outOfRounds([{ kind: 'self', left: 1 }]) && !outOfRounds([]), 'the platform itself, or no loadout: never out')
check(outOfRounds([{ kind: 'missile', left: 0 }, { kind: 'mine', left: 4, inert: true }]), 'an inert weapon does not count')

// the battery and the fix as a report carries them
check(batteryLow({ pct: LOW_BATTERY_PCT, leftS: null, homeS: null }) && !batteryLow({ pct: 90, leftS: null, homeS: 3000 }), 'the floor, and only with both times the way home')
const id = (platform: string) => ({ callsign: 'X', platform, name: '', short: '', role: '', domain: 'air' as const, link: '', sidc: '', sim: 'standin' as const })
const home: [number, number] = [-122.48, 37.82]
const away: [number, number] = [-122.48, 37.82 + 9000 / 111_320] // about 9 km north
const cat = new Map([['puma', { endurance_h: 2, cruise_mps: 15 }]])
const b = batteryOf({ battery_pct: 10, identity: id('puma'), position: away, home }, cat)!
check(Math.abs(b.leftS! - 720) < 1 && Math.abs(b.homeS! - 600) < 10, 'what is left of two hours at 10 %, and nine km at 15 m/s')
check(batteryOf({ battery_pct: null }, cat) === null, 'no battery reported: nothing')
check(batteryOf({ battery_pct: 50, identity: id('magura') }, cat)?.leftS === null, 'a platform the catalog does not time: the floor only')
check(noGnssOf({ nav_source: 'dead_reckoning', kind: 'plane' }) === 'dead_reckoning', 'a plane on dead reckoning')
check(noGnssOf({ nav_source: 'dead_reckoning', kind: 'sub', alt_msl: -30 }) === null, 'never a sub under water')
check(noGnssOf({ nav_source: 'dead_reckoning', kind: 'sub', alt_msl: 0 }) === 'dead_reckoning', 'a sub at the surface without a fix')
check(noGnssOf({ nav_source: 'none', kind: 'copter', armed: false }) === null, 'before its first fix, not under way: every vehicle at start')
check(noGnssOf({ nav_source: 'none', kind: 'copter', armed: true }) === 'none', 'under way with no position it trusts')
check(noGnssOf({ nav_source: 'gnss', kind: 'boat' }) === null && noGnssOf({}) === null, 'a fix, or a report without the field: nothing')

// a vehicle as the console has it
const veh = (link: Partial<Vehicle['link']>, uid = 7): Pick<Vehicle, 'uid' | 'link' | 'estimate'> => ({ uid, link: { since_last_ms: 300, loss_pct: 0, rtt_ms: 20, ...link }, estimate: null })
const ctx: ExceptionContext = { ...NO_EXCEPTIONS, damage: new Map([[7, { mobility: true, firepower: false }]]) }
check(exceptionOf(veh({}), ctx)?.text === 'DAMAGED: CANNOT MOVE', 'the damage map read by uid')
check(exceptionOf(veh({}, 8), ctx) === null, 'another vehicle: none')
check(exceptionOf(veh({ stale: true, since_last_ms: 9000 }, 8), NO_EXCEPTIONS)?.text === 'LOST 9s', 'a lite snapshot\'s stale verdict')
check(exceptionOf(veh({ emu: { state: 'down', cause: 'cut' } }, 8), NO_EXCEPTIONS, (c) => (c === 'cut' ? 'scripted cut' : ''))?.title.includes('scripted cut'), 'the cause in words')
check(exceptionOf({ ...veh({}, 8), battery_pct: 60, identity: id('puma'), position: away, home }, { ...NO_EXCEPTIONS, platforms: new Map([['puma', { endurance_h: 0.2, cruise_mps: 15 }]]) })?.kind === 'low_battery', 'the catalog read by platform')
check(exceptionOf({ ...veh({}, 8), battery_pct: 60, identity: id('puma'), position: away, home }, NO_EXCEPTIONS) === null, 'without the catalog: the floor only')

console.log('vehicleException: ok')
