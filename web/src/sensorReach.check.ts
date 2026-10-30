// Checks for sensorReach.ts (no browser required; make check runs it with Node 22.18 or later,
// which strips the types itself):
//   node web/src/sensorReach.check.ts
// Throws on the first failure, prints "sensorReach: ok" otherwise. Not part of the bundle.
import assert from 'node:assert/strict'
import { circle, lightState, pickDomain, reachBadge, reachFeatures, type ReachAnswer, type VehicleReach } from './sensorReach.ts'
import { distanceM } from './geo.ts'
import type { LngLat } from './types.ts'

const puma = (detect: number, limited: string | null): VehicleReach => ({
  uid: 7, callsign: 'PUMA 1', platform: 'puma', alt_msl_m: 120, submerged: false, age_s: 0,
  conditions: { visibility_m: 50, sun_elevation_deg: 60, light: 'day', wind_mps: 4, sea_state: 2, synthetic: true, source: 'fixture', model: null },
  sensors: [{
    type: 'eo_ir', primary_domain: 'surface',
    domains: [
      { domain: 'air', detect_m: 16_000, recognise_m: 6000, identify_m: 3000, clear_day_m: 16_210, ratio: 0.99, limited_by: null, factors: [], horizon_limited: false },
      { domain: 'surface', detect_m: detect, recognise_m: detect / 2, identify_m: detect / 4, clear_day_m: 14_470, ratio: detect / 14_470, limited_by: limited, factors: limited ? [{ factor: limited, detect_m: 14_470 }] : [], horizon_limited: false },
    ],
  }],
})

// --- the domain each sensor shows
const s = puma(2600, 'fog').sensors[0]
assert.equal(pickDomain(s, 'auto')?.domain, 'surface', 'auto is the bridge\'s primary domain')
assert.equal(pickDomain(s, 'air')?.domain, 'air')
assert.equal(pickDomain(s, 'subsurface'), null, 'a camera draws nothing for under water')

// --- badges: only under half the clear-day reach, the worst sensor, its reason
assert.deepEqual(reachBadge(puma(2600, 'fog'), 'auto')?.text, 'EO 18% · fog')
assert.equal(reachBadge(puma(9000, null), 'auto'), null, 'at 62% there is no badge')
assert.equal(reachBadge(puma(2600, 'fog'), 'air'), null, 'the air reach is not degraded')
assert.equal(reachBadge(puma(2600, null), 'auto')?.text, 'EO 18%', 'no reason given, none shown')

// --- the sun
assert.equal(lightState(10), 'day')
assert.equal(lightState(-3), 'twilight')
assert.equal(lightState(-12), 'night')

// --- rings: centred on the vehicle at their radii, one per kind, only for the vehicles shown
const at: LngLat = [-122.46, 37.82]
const ring = circle(at, 1000)
assert.ok(ring.every((p) => Math.abs(distanceM(at, p) - 1000) < 10), 'a ring is its radius from the centre')
assert.deepEqual(ring[0], ring[ring.length - 1], 'closed')
const answer: ReachAnswer = { available: true, model: 'physical', vehicles: [puma(2600, 'fog'), { ...puma(14_000, null), uid: 8 }], note: '' }
const positions = new Map<number, LngLat>([[7, at], [8, [-122.5, 37.8]]])
const fc = reachFeatures(answer, positions, new Set([7]), 'auto')
assert.deepEqual(fc.features.map((f) => f.properties?.kind), ['clear', 'detect', 'identify'])
assert.ok(fc.features.every((f) => f.properties?.uid === 7), 'only the selected vehicle')
const detect = fc.features.find((f) => f.properties?.kind === 'detect')!.geometry as GeoJSON.LineString
assert.ok(Math.abs(distanceM(at, detect.coordinates[10] as LngLat) - 2600) < 30, 'the detection ring at the reach now')
assert.equal(reachFeatures(answer, positions, null, 'auto').features.length, 6, 'every vehicle')
assert.equal(reachFeatures({ ...answer, stale: true }, positions, null, 'auto').features.length, 0, 'stale answers draw nothing')
assert.equal(reachFeatures({ available: true, model: 'legacy', vehicles: [], note: '' }, positions, null, 'auto').features.length, 0, 'legacy: no rings')
assert.equal(reachFeatures(answer, new Map(), null, 'auto').features.length, 0, 'no position, no ring')

console.log('sensorReach: ok')
