// Checks for vehicleException.ts (no test runner in web/): node web/src/vehicleException.check.ts, as
// make check does. The claims (bd murmur-iobd.1): one slot per vehicle, the most pressing first
// (confirmed lost, presumed lost, damaged, silent, link down, out of rounds, degraded); a damage
// report says what the vehicle can no longer do; silence keeps the words the map's badge always
// had ("LOST 12s" before rules 30, "SILENT 12s" under it); a platform is out of rounds only when
// every usable weapon is spent; a reporting vehicle with nothing wrong has no slot. Throws on the
// first failure, prints "vehicleException: ok" otherwise.
import { damageOf, damageText, exceptionOf, exceptionPriority, type ExceptionContext, type ExceptionFacts, LOSS_PCT, NO_EXCEPTIONS, outOfRounds, vehicleException } from './vehicleException.ts'
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

// the most pressing wins
check(kind({ standing: 'lost', stale: true, damage: { mobility: true, firepower: false } }) === 'lost', 'confirmed lost over all')
check(kind({ standing: 'presumed_lost', stale: true, damage: { mobility: true, firepower: false } }) === 'presumed', 'presumed over damaged')
check(kind({ stale: true, damage: { mobility: true, firepower: false } }) === 'damaged', 'damaged over silent (the fleet row counts it so too)')
check(kind({ stale: true, emu: { state: 'down', cause: '' } }) === 'silent', 'silent over link down')
check(kind({ emu: { state: 'down', cause: '' }, noRounds: true }) === 'link_down', 'link down over out of rounds')
check(kind({ noRounds: true, emu: { state: 'degraded', cause: '' } }) === 'no_rounds', 'out of rounds over degraded')

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

// a vehicle as the console has it
const veh = (link: Partial<Vehicle['link']>, uid = 7): Pick<Vehicle, 'uid' | 'link' | 'estimate'> => ({ uid, link: { since_last_ms: 300, loss_pct: 0, rtt_ms: 20, ...link }, estimate: null })
const ctx: ExceptionContext = { ...NO_EXCEPTIONS, damage: new Map([[7, { mobility: true, firepower: false }]]) }
check(exceptionOf(veh({}), ctx)?.text === 'DAMAGED: CANNOT MOVE', 'the damage map read by uid')
check(exceptionOf(veh({}, 8), ctx) === null, 'another vehicle: none')
check(exceptionOf(veh({ stale: true, since_last_ms: 9000 }, 8), NO_EXCEPTIONS)?.text === 'LOST 9s', 'a lite snapshot\'s stale verdict')
check(exceptionOf(veh({ emu: { state: 'down', cause: 'cut' } }, 8), NO_EXCEPTIONS, (c) => (c === 'cut' ? 'scripted cut' : ''))?.title.includes('scripted cut'), 'the cause in words')

console.log('vehicleException: ok')
