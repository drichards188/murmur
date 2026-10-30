// Checks for the map's weather layer (weather.ts, weatherField.ts, weatherNoise.ts, weatherGl.ts
// packEffects, windParticles.ts, rainParticles.ts; bd murmur-vr17). No browser required: make
// check runs it with Node 22.18 or later.
//   node web/src/weather.check.ts
import assert from 'node:assert/strict'
import { weatherValue, type WeatherGrid, type WeatherValues } from './weather.ts'
import { blendRegions, describeField, fieldRange, fieldSource, fogGroundBlend, hazeStrength, MAX_REGIONS, nearestRegion, rampColor, rampCss, rampTicks, regionsOf, valuesAt, MISSING, type WeatherRegions } from './weatherField.ts'
import { coveredShare, coverThreshold, fogAlpha, FOG_MAX, makeNoise, NOISE_SIZE, noiseQuantiles } from './weatherNoise.ts'
import { packEffects } from './weatherGl.ts'
import { rainSegments, rainWeight, stepRain, type Drop } from './rainParticles.ts'
import { particleAlpha, PX_PER_MPS, stepWind, TRAIL_POINTS, windArrows, windAt, windSegments, type Particle, type WindStep } from './windParticles.ts'

const NOISE = makeNoise()

const val = (visibility: number | null, east: number | null = 3, north: number | null = 4): WeatherValues => ({ visibility_m: visibility, wind_east_mps: east, wind_north_mps: north, cloud_cover_pct: null, precipitation_mm: 0 })
const NONE: WeatherValues = { visibility_m: null, wind_east_mps: null, wind_north_mps: null, cloud_cover_pct: null, precipitation_mm: null }
assert.equal(weatherValue(val(50), 'wind'), 5)
assert.equal(weatherValue({ ...val(50), wind_north_mps: null }, 'wind'), null)

// ---- the ramps: continuous, fog thicker than clear air, unknown is not clear --------------------
assert.ok(rampColor('visibility', 50)[3] > rampColor('visibility', 1000)[3])
assert.ok(rampColor('visibility', 1000)[3] > rampColor('visibility', 20000)[3])
assert.deepEqual(rampColor('visibility', 10), rampColor('visibility', 50), 'clamped below the ramp')
assert.deepEqual(rampColor('visibility', null), MISSING)
assert.notDeepEqual(rampColor('visibility', null), rampColor('visibility', 20000), 'unknown is not clear')
// continuous: a small step in value is a small step in colour (no swatch bands)
for (const [field, lo, hi] of [['visibility', 50, 20000], ['wind', 0, 20], ['cloud', 0, 100], ['precipitation', 0, 10]] as const) {
  let last = rampColor(field, lo)
  for (let i = 1; i <= 400; i++) {
    const v = field === 'visibility' ? lo * (hi / lo) ** (i / 400) : lo + ((hi - lo) * i) / 400
    const c = rampColor(field, v)
    assert.ok(c.slice(0, 3).every((x, k) => Math.abs(x - last[k]) < 6) && Math.abs(c[3] - last[3]) < 0.02, `${field} jumps at ${v}`)
    last = c
  }
  assert.match(rampCss(field), /^linear-gradient\(to right, rgba/)
  const ticks = rampTicks(field)
  assert.equal(ticks[0].at, 0)
  assert.equal(ticks[ticks.length - 1].at, 1)
  assert.ok(ticks.every((t, i) => i === 0 || t.at > ticks[i - 1].at), `${field} ticks in order`)
}
assert.equal(rampTicks('visibility').find((t) => t.value === 1000)!.label, '1k')
// visibility's ticks sit by the logarithm: 1 km about 0.5 of the way from 50 m to 20 km
assert.ok(Math.abs(rampTicks('visibility').find((t) => t.value === 1000)!.at - Math.log(20) / Math.log(400)) < 1e-9)

// ---- regions: the sampler's nearest-point rule, its bounds, and what holds outside them ----------
// fog-watch.json's grid: 5 by 4 points, 0.045 deg of longitude and 0.0367 of latitude apart
const lons = [-122.56, -122.515, -122.47, -122.425, -122.38], lats = [37.77, 37.80666666666667, 37.843333333333334, 37.88]
const pts = lats.flatMap((lat, i) => lons.map((lon, j) => ({ lat, lon, values: val(1000 * (i * 5 + j + 1), j - 2, i - 1) })))
const R: WeatherRegions = { bounds: [-122.56, 37.77, -122.38, 37.88], points: pts, outside: NONE }
// the sampler's own distance, written out as murmur-weather's Archive::at has it
const sampler = (lon: number, lat: number) => {
  const [w, s, e, n] = R.bounds!
  if (lat < s || lat > n || lon < w || lon > e) return -1
  const d = (p: { lat: number; lon: number }) => (p.lat - lat) ** 2 + ((p.lon - lon) * Math.cos(lat * (Math.PI / 180))) ** 2
  let best = 0
  for (let i = 1; i < pts.length; i++) if (d(pts[i]) < d(pts[best])) best = i
  return best
}
for (let i = 0; i <= 200; i++) for (let j = 0; j <= 200; j++) {
  const lon = -122.6 + (0.26 * j) / 200, lat = 37.74 + (0.17 * i) / 200
  assert.equal(nearestRegion(R, lon, lat), sampler(lon, lat), `${lon} ${lat}`)
}
// a point's own place is its own; just either side of the midway between two is each one's
assert.equal(nearestRegion(R, -122.47, 37.80666666666667), 7)
assert.equal(nearestRegion(R, -122.4926, 37.8), 6)
assert.equal(nearestRegion(R, -122.4924, 37.8), 7)
assert.deepEqual(valuesAt(R, -122.0, 37.8), NONE, 'outside the archive: unknown, not extrapolated')
assert.equal(valuesAt(R, -122.47, 37.80666666666667).visibility_m, 8000)
// scripted (no archive): the same everywhere
const fog: WeatherRegions = { bounds: null, points: [], outside: val(50) }
assert.equal(nearestRegion(fog, -122.47, 37.8), -1)
assert.equal(valuesAt(fog, 0, 0).visibility_m, 50)

// ---- what the shader is handed: each region's own values, nothing between them -------------------
{
  const sorted = noiseQuantiles(NOISE)
  const offsets = new Float32Array((MAX_REGIONS + 1) * 4).map((_, i) => i)
  const withCloud: WeatherRegions = { ...R, points: R.points.map((p, i) => ({ ...p, values: { ...p.values, cloud_cover_pct: i === 3 ? 100 : i === 4 ? 0 : 40 } })) }
  const packed = packEffects(withCloud, 'visibility', [-122.6, 37.9], offsets, sorted)
  assert.equal(packed.n, 20)
  // points relative to the mesh's origin, in degrees (the shader's nearest rule on them is the sampler's)
  assert.ok(Math.abs(packed.pts[0] - (-122.56 - -122.6)) < 1e-6 && Math.abs(packed.pts[1] - (37.77 - 37.9)) < 1e-6)
  // region k + 1 (0 is outside): fog from its own visibility, cloud from its own cover, its own drift
  withCloud.points.forEach((p, i) => {
    const k = i + 1
    assert.ok(Math.abs(packed.fx[4 * k] - fogAlpha(p.values.visibility_m)) < 1e-6)
    assert.ok(Math.abs(packed.fx[4 * k + 1] - coverThreshold(sorted, p.values.cloud_cover_pct! / 100)) < 1e-6)
    const c = rampColor('visibility', p.values.visibility_m)
    assert.ok(Math.abs(packed.col[4 * k + 3] - c[3]) < 1e-6)
    assert.deepEqual(Array.from(packed.off.subarray(4 * k, 4 * k + 4)), [4 * k, 4 * k + 1, 4 * k + 2, 4 * k + 3])
  })
  assert.equal(packed.fx[4 * 4 + 1], -1, 'full cover: every texel shaded')
  assert.equal(packed.fx[4 * 5 + 1], 2, 'clear: none')
  // outside the archive: nothing known, so no fog, no cloud and the field marked missing
  assert.equal(packed.fx[0], 0)
  assert.equal(packed.fx[1], 2)
  assert.equal(packed.fx[2], 1)
  // no colour field chosen: no colours
  assert.ok(packEffects(R, null, [0, 0], offsets, sorted).col.every((x) => x === 0))
}

// ---- the noise: cloud shadows cover each region's cloud cover ------------------------------------
{
  assert.equal(NOISE.length, NOISE_SIZE * NOISE_SIZE)
  assert.ok(NOISE.every((v) => v >= 0 && v <= 1))
  assert.deepEqual(Array.from(makeNoise().slice(0, 8)), Array.from(NOISE.slice(0, 8)), 'deterministic')
  const sorted = noiseQuantiles(NOISE)
  for (const cover of [0.1, 0.25, 0.4, 0.5, 0.75, 0.9]) {
    const share = coveredShare(NOISE, coverThreshold(sorted, cover))
    assert.ok(Math.abs(share - cover) < 0.01, `cover ${cover}: ${share}`)
  }
  assert.equal(coveredShare(NOISE, coverThreshold(sorted, 0)), 0)
  assert.equal(coveredShare(NOISE, coverThreshold(sorted, 1)), 1)
  assert.equal(coverThreshold(sorted, null), 2, 'unknown cover: no shadow')
  // tileable: the last column runs on into the first as any two neighbours do
  const n = NOISE_SIZE
  let edge = 0, inner = 0
  for (let y = 0; y < n; y++) {
    edge += Math.abs(NOISE[y * n + n - 1] - NOISE[y * n])
    inner += Math.abs(NOISE[y * n + n / 2] - NOISE[y * n + n / 2 - 1])
  }
  assert.ok(edge < inner * 3, `seamless (${edge} against ${inner})`)
  // fog: none in clear air or unknown, thickest at 50 m, monotonic between
  assert.equal(fogAlpha(null), 0)
  assert.equal(fogAlpha(20000), 0)
  assert.equal(fogAlpha(50), FOG_MAX)
  let last = 0
  for (const v of [15000, 5000, 1000, 200, 50]) {
    assert.ok(fogAlpha(v) > last)
    last = fogAlpha(v)
  }
  assert.ok(fogAlpha(12000) < 0.05, 'ordinary haze stays faint')
}

// ---- the legend's words: true per field ---------------------------------------------------------
{
  const scripted: WeatherRegions = { ...R, scripted: ['visibility_m', 'cloud_cover_pct'], points: R.points.map((p) => ({ ...p, values: { ...p.values, visibility_m: 50, cloud_cover_pct: 100 } })), outside: { ...NONE, visibility_m: 50, cloud_cover_pct: 100 } }
  assert.equal(fieldSource(scripted, 'visibility'), 'scripted')
  assert.equal(fieldSource(scripted, 'wind'), 'archived')
  assert.equal(fieldSource(scripted, 'precipitation'), 'archived', 'uniform zero but not scripted: archived')
  assert.equal(describeField(scripted, 'visibility'), '50 m, scripted, the same everywhere')
  assert.equal(describeField(scripted, 'cloud'), '100%, scripted, the same everywhere')
  assert.equal(describeField(scripted, 'precipitation'), 'none')
  assert.equal(describeField(R, 'wind'), '0.0 to 2.8 m/s')
  assert.equal(describeField({ ...R, points: [] }, 'wind'), 'no data')
  assert.equal(fieldRange(R, 'visibility')?.join(), '1000,20000')
}

// ---- grid answers: regions, stale or unavailable, too many to draw ------------------------------
{
  const grid: WeatherGrid = { available: true, bounds: [-123, 37, -122, 38], nlon: 1, nlat: 2,
    cells: [val(20000), val(50)], t: 0, age_s: 0, stale: false, source: 'fixture', model: null, synthetic: true, regions: R }
  assert.equal(regionsOf(grid), R)
  assert.equal(regionsOf({ ...grid, stale: true }), null, 'stale weather is hidden')
  assert.equal(regionsOf({ ...grid, available: false }), null)
  assert.equal(regionsOf({ ...grid, regions: undefined }), null, 'no regions: nothing drawn (never the raster\'s blocks)')
  assert.equal(regionsOf({ ...grid, regions: { ...R, points: Array.from({ length: MAX_REGIONS + 1 }, () => pts[0]) } }), null)
}

// ---- blending between two polls: values, not colours ---------------------------------------------
{
  const a: WeatherRegions = { ...fog, outside: val(20000, 0, 0) }
  const b: WeatherRegions = { ...fog, scripted: ['visibility_m'], outside: val(50, 4, 0) }
  assert.equal(blendRegions(a, b, 0.5).outside.visibility_m, 10025)
  assert.equal(blendRegions(a, b, 0.5).outside.wind_east_mps, 2)
  assert.deepEqual(blendRegions(a, b, 0.5).scripted, ['visibility_m'])
  assert.equal(blendRegions(a, b, 1), b)
  assert.equal(blendRegions(null, b, 0.2), b)
  assert.equal(blendRegions(R, b, 0.5), b, 'other points: no blend')
  assert.equal(blendRegions({ ...a, outside: val(null) }, b, 0.5).outside.visibility_m, 50, 'a missing value takes the new one')
}

// ---- rain: where and as hard as each region's precipitation ------------------------------------
{
  assert.equal(rainWeight(null), 0)
  assert.equal(rainWeight(0), 0)
  assert.equal(rainWeight(8), 1)
  assert.ok(rainWeight(1) > 0 && rainWeight(1) < rainWeight(4))
  // rain in one region only (index 7, 4 mm), the rest dry
  const wet: WeatherRegions = { ...R, points: R.points.map((p, i) => ({ ...p, values: { ...p.values, precipitation_mm: i === 7 ? 4 : 0 } })) }
  let seed = 1
  const rng = () => ((seed = (seed * 16807) % 2147483647) / 2147483647)
  const view = (lon: number, lat: number) => lon > -122.6 && lon < -122.3 && lat > 37.7 && lat < 37.9
  const spawn = (): [number, number] => [-122.58 + 0.22 * rng(), 37.77 + 0.11 * rng()]
  let ds: Drop[] = []
  for (let i = 0; i < 200; i++) ds = stepRain(ds, 0.05, { regions: wet, mPerPx: 10, max: 400, spawn, inView: view, zOf: () => 0, rng })
  assert.ok(ds.length > 0)
  assert.ok(ds.every((d) => nearestRegion(wet, d.lon, d.lat) === 7 || Math.abs(d.lon - -122.47) < 0.03), 'drops only where it rains (or drifted a little out of it)')
  // they fall: lower after a step, and land
  const h0 = ds.map((d) => d.above)
  const next = stepRain(ds.map((d) => ({ ...d })), 0.05, { regions: wet, mPerPx: 10, max: 0, spawn, inView: view, zOf: () => 0, rng })
  assert.ok(next.every((d) => d.above < Math.max(...h0)))
  const seg = rainSegments(ds, 10)
  assert.equal(seg.length, ds.length)
  assert.ok(seg.from[2] > seg.to[2], 'a drop\'s tail is above it')
  // dry everywhere: none
  assert.equal(stepRain([], 0.1, { regions: R, mPerPx: 10, max: 400, spawn, inView: view, zOf: () => 0, rng }).length, 0)
}

// ---- haze: 0 at 20 km or more (or unknown), 1 at 50 m or less, the fog closer as it falls --------
assert.equal(hazeStrength(null), 0)
assert.equal(hazeStrength(25000), 0)
assert.equal(hazeStrength(20000), 0)
assert.equal(hazeStrength(50), 1)
assert.equal(hazeStrength(10), 1)
assert.ok(hazeStrength(500) > hazeStrength(5000))
assert.ok(fogGroundBlend(50, 1000, 0.05) < fogGroundBlend(5000, 1000, 0.05))
assert.ok(fogGroundBlend(50, 1000, 0.05) < 0.01, 'thick fog starts at the view centre')
assert.ok(fogGroundBlend(1e9, 1000, 0.05) <= 0.98)
assert.equal(fogGroundBlend(500, 0, 0.05), 1, 'no camera distance: no fog')

// ---- wind: streaks carried by the region they are in ---------------------------------------------
{
  assert.equal(windAt(R, -122.47, 37.80666666666667), null, 'calm: none')
  assert.deepEqual(windAt(R, -122.425, 37.80666666666667), [1, 0])
  assert.equal(windAt(R, -122.0, 37.8), null, 'outside: no wind known')
  // one particle in a 1 m/s east region at 10 m a pixel: PX_PER_MPS px/s per m/s
  const p: Particle = { lon: -122.425, lat: 37.8067, z: 0, age: 0, life: 10, trail: [-122.425, 37.8067, 0], sinceTrail: 0 }
  const view = (lon: number, lat: number) => lon > -122.6 && lon < -122.3 && lat > 37.7 && lat < 37.9
  const step: WindStep = { regions: R, mPerPx: 10, count: 1, spawn: () => null, inView: view, zOf: () => 0, rng: () => 0.5 }
  let ps = [p]
  for (let i = 0; i < 20; i++) ps = stepWind(ps, 0.05, step)
  const moved = (ps[0].lon - -122.425) * 111_320 * Math.cos((37.8067 * Math.PI) / 180)
  assert.ok(Math.abs(moved - PX_PER_MPS * 10 * 1 * 1.0) < 0.5, `moved ${moved} m in a second`)
  assert.ok(Math.abs(ps[0].lat - 37.8067) < 1e-12, 'due east: no northing')
  assert.ok(ps[0].trail.length / 3 <= TRAIL_POINTS && ps[0].trail.length / 3 > 5)
  // a stalled frame does not fling it: dt is capped at 0.1 s
  const lon0 = ps[0].lon
  ps = stepWind(ps, 5, step)
  assert.ok((ps[0].lon - lon0) * 111_320 * Math.cos((37.8067 * Math.PI) / 180) < PX_PER_MPS * 10 * 0.1 + 0.01)
  // crossing into the calm region retires it (no wind known to carry it there)
  const calm: Particle = { lon: -122.47, lat: 37.80666666666667, z: 0, age: 0, life: 10, trail: [], sinceTrail: 0 }
  assert.equal(stepWind([calm], 0.05, step).length, 0)
  // spawns go only where there is wind, up to the count
  let k = 0
  const spots: [number, number][] = [[-122.0, 37.8], [-122.47, 37.80666666666667], [-122.425, 37.8067]]
  const filled = stepWind([], 0.05, { ...step, count: 5, spawn: () => spots[k++ % 3] })
  assert.ok(filled.length > 0 && filled.every((q) => windAt(R, q.lon, q.lat) != null))
  // segments: one per trail point, fading toward the tail
  const seg = windSegments(ps, [255, 255, 255], 1)
  assert.equal(seg.length, ps[0].trail.length / 3)
  assert.ok(seg.color[3] < seg.color[4 * (seg.length - 1) + 3], 'the tail is fainter than the head')
  assert.equal(seg.to[3 * (seg.length - 1)], ps[0].lon, 'the last segment ends at the head')
  assert.equal(particleAlpha({ ...p, age: 0 }), 0)
  assert.equal(particleAlpha({ ...p, age: 2 }), 1)
  assert.equal(particleAlpha({ ...p, age: 10 }), 0)
  // static arrows (reduced motion): one per region point in view with wind, pointing toward travel
  const arrows = windArrows(R, view, 10, [-122.47, 37.8])
  const windy = pts.filter((q) => windAt(R, q.lon, q.lat))
  assert.equal(arrows.length, windy.length * 2, 'a shaft and a head each')
  const [tail, head] = arrows[0]
  const w0 = windAt(R, windy[0].lon, windy[0].lat)!
  assert.ok(Math.sign(head[0] - tail[0]) === Math.sign(w0[0]) || w0[0] === 0)
  assert.ok(Math.sign(head[1] - tail[1]) === Math.sign(w0[1]) || w0[1] === 0)
}

console.log('weather: ok')
