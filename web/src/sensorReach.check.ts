// Checks for sensorReach.ts (no browser required; make check runs it with Node 22.18 or later,
// which strips the types itself):
//   node web/src/sensorReach.check.ts
// Throws on the first failure, prints "sensorReach: ok" otherwise. Not part of the bundle.
import assert from 'node:assert/strict'
import { circle, conditionsText, lightState, pickDomain, reachFeatures, sensorLines, type ReachAnswer, type VehicleReach } from './sensorReach.ts'
import { distanceM } from './geo.ts'
import { RingCache, ringSet, shiftLimitM } from './reachCache.ts'
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
assert.equal(pickDomain(s)?.domain, 'surface', 'each sensor shows the bridge\'s primary domain')

// --- the details' Sensors lines: each sensor in its own domain, now against the clear day, what limits it
assert.deepEqual(sensorLines(puma(2600, 'fog')), [{ name: 'EO/IR', text: "surface contact: detects to 2.6 km of a clear day's 14 km, identifies to 650 m; limited by fog", low: true }])
assert.equal(sensorLines(puma(9000, null))[0].low, false, 'at 62% it is not a loss')
assert.equal(sensorLines(puma(0, null))[0].text, 'surface contact: cannot detect one now', 'no reach said in words, not "detects to 0 m"')
assert.equal(sensorLines(puma(9000, null))[0].text, "surface contact: detects to 9.0 km of a clear day's 14 km, identifies to 2.3 km", 'no reason given, none shown')
assert.equal(conditionsText(puma(2600, 'fog').conditions), 'visibility 50 m, sea state 2', 'day and clear air say nothing')
assert.equal(conditionsText({ ...puma(2600, 'fog').conditions, visibility_m: 20_000, light: 'night' }), 'night, sea state 2')

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
const fc = reachFeatures(answer, positions, new Set([7]))
assert.deepEqual(fc.features.map((f) => f.properties?.kind), ['clear', 'detect', 'identify'])
assert.ok(fc.features.every((f) => f.properties?.uid === 7), 'only the selected vehicle')
const detect = fc.features.find((f) => f.properties?.kind === 'detect')!.geometry as GeoJSON.LineString
assert.ok(Math.abs(distanceM(at, detect.coordinates[10] as LngLat) - 2600) < 30, 'the detection ring at the reach now')
assert.equal(reachFeatures(answer, positions, null).features.length, 6, 'every vehicle')
assert.equal(reachFeatures({ ...answer, stale: true }, positions, null).features.length, 0, 'stale answers draw nothing')
assert.equal(reachFeatures({ available: true, model: 'legacy', vehicles: [], note: '' }, positions, null).features.length, 0, 'legacy: no rings')
assert.equal(reachFeatures(answer, new Map(), null).features.length, 0, 'no position, no ring')

// --- a ring is lifted once and moved with its vehicle (reachCache.ts; bd murmur-yfp6)
{
  const cache = new RingCache<{ at: LngLat; n: number }>()
  let lifts = 0
  const build = (c: LngLat) => () => ({ at: c, n: ++lifts })
  const shift = (b: { at: LngLat; n: number }, dLon: number, dLat: number) => ({ ...b, at: [b.at[0] + dLon, b.at[1] + dLat] as LngLat })
  const a1 = cache.get('7|eo_ir|detect', at, 2600, 'g0|12', build(at), shift)
  assert.equal(lifts, 1)
  const near: LngLat = [at[0] + 0.0002, at[1]] // about 20 m
  const a2 = cache.get('7|eo_ir|detect', near, 2600, 'g0|12', build(near), shift)
  assert.equal(lifts, 1, 'a few metres: the heights are reused')
  assert.deepEqual(a2.at, near, 'and the ring moved to its vehicle')
  assert.equal(cache.get('7|eo_ir|detect', at, 2600, 'g0|12', build(at), shift), a1, 'unmoved: the very same ring')
  assert.ok(shiftLimitM(2600) <= 150 && shiftLimitM(100) === 3, 'the limit: 3 % of the radius, at most 150 m')
  const far: LngLat = [at[0] + 0.01, at[1]] // about 880 m
  cache.get('7|eo_ir|detect', far, 2600, 'g0|12', build(far), shift)
  assert.equal(lifts, 2, 'past the limit: lifted again')
  cache.get('7|eo_ir|detect', far, 2700, 'g0|12', build(far), shift)
  assert.equal(lifts, 3, 'another radius: lifted again')
  cache.get('7|eo_ir|detect', far, 2700, 'g1|12', build(far), shift)
  assert.equal(lifts, 4, 'new terrain or zoom: lifted again')
  cache.sweep() // seen since the last sweep? the ring above was
  assert.equal(cache.size, 1)
  cache.sweep()
  assert.equal(cache.size, 0, 'a ring not asked for since the last sweep is forgotten')
}
// --- the ring set is the same while the vehicles only move
{
  const a = reachFeatures(answer, positions, null)
  const b = reachFeatures(answer, new Map<number, LngLat>([[7, [at[0] + 0.001, at[1]]], [8, [-122.5, 37.8]]]), null)
  assert.equal(ringSet(a), ringSet(b), 'moved a little: the same set')
  assert.notEqual(ringSet(a), ringSet(reachFeatures(answer, positions, new Set([7]))), 'another selection: another set')
  assert.ok(a.features.every((f) => f.properties?.id && f.properties?.at && f.properties?.m > 0), 'each ring says its id, centre and radius')
}

console.log('sensorReach: ok')
