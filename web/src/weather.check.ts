// Unit/encoding checks for the map's environmental raster (no browser required).
import assert from 'node:assert/strict'
import { weatherColor, weatherValue, weatherImage, type WeatherGrid, type WeatherValues } from './weather.ts'
const value = (visibility: number | null): WeatherValues => ({ visibility_m: visibility, wind_east_mps: 3, wind_north_mps: 4, cloud_cover_pct: null, precipitation_mm: 0 })
assert.equal(weatherValue(value(50), 'wind'), 5)
assert.equal(weatherValue({ ...value(50), wind_north_mps: null }, 'wind'), null)
assert.ok(weatherColor(50, 'visibility')[3] > weatherColor(20000, 'visibility')[3])
assert.notDeepEqual(weatherColor(null, 'visibility'), weatherColor(20000, 'visibility'), 'unknown is not clear')
const fills: { color: string; x: number; y: number }[] = []
const ctx = { fillStyle: '', strokeStyle: '', lineWidth: 1,
  fillRect(x: number, y: number) { fills.push({ color: this.fillStyle, x, y }) },
  beginPath() {}, moveTo() {}, lineTo() {}, stroke() {},
}
Object.defineProperty(globalThis, 'document', { value: { createElement: () => ({ width: 0, height: 0, getContext: () => ctx, toDataURL: () => 'image' }) } })
const grid: WeatherGrid = { available: true, bounds: [-123, 37, -122, 38], nlon: 1, nlat: 2,
  cells: [value(20000), value(50)], t: 0, age_s: 0, stale: false, source: 'fixture', model: null, synthetic: true }
const image = weatherImage(grid, 'visibility')!
assert.deepEqual(image.coordinates, [[-123, 38], [-122, 38], [-122, 37], [-123, 37]])
assert.equal(fills[0].y, 24, 'south row appears at the bottom')
assert.equal(fills[1].y, 0, 'north row appears at the top')
assert.equal(weatherImage({ ...grid, stale: true }, 'visibility'), null, 'stale weather is hidden')
assert.equal(weatherImage({ ...grid, available: false }, 'visibility'), null)
console.log('weather: ok')
