// Checks for stableFleet.ts: node web/src/stableFleet.check.ts (prints "stableFleet: ok").
import assert from 'node:assert/strict'
import { sameFleet } from './stableFleet.ts'
import type { Vehicle } from './types.ts'

const v = (uid: number, platform = 'puma', position: [number, number] = [0, 0]) =>
  ({ uid, sysid: uid, kind: 'air', identity: { callsign: `V${uid}`, platform }, position }) as unknown as Vehicle
const a = [v(1), v(2), v(3)]
assert.ok(sameFleet(a, a), 'the same array')
assert.ok(sameFleet(a, [v(1, 'puma', [5, 5]), v(2), v(3)]), 'moved: the same fleet for a reader of names')
assert.ok(!sameFleet(a, [v(1), v(2)]), 'one left')
assert.ok(!sameFleet(a, [v(1), v(2), v(4)]), 'another vehicle')
assert.ok(!sameFleet(a, [v(1), v(2), v(3, 'magura')]), 'another platform under the uid')
assert.ok(!sameFleet(a, [v(1), v(2), { ...v(3), identity: null }]), 'identity lost: callsign differs')
console.log('stableFleet: ok')
