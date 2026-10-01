import type { LngLat } from './types'

export function circle(center: LngLat, radiusM: number, n = 64): LngLat[] {
  const [lon, lat] = center
  const pts: LngLat[] = []
  for (let k = 0; k <= n; k++) {
    const a = (2 * Math.PI * k) / n
    const dLat = (radiusM * Math.cos(a)) / 111_320
    const dLon = (radiusM * Math.sin(a)) / (111_320 * Math.cos((lat * Math.PI) / 180))
    pts.push([lon + dLon, lat + dLat])
  }
  return pts
}

export function distanceM(a: LngLat, b: LngLat) {
  const R = 6_371_000
  const toRad = Math.PI / 180
  const dLat = (b[1] - a[1]) * toRad
  const dLon = (b[0] - a[0]) * toRad
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a[1] * toRad) * Math.cos(b[1] * toRad) * Math.sin(dLon / 2) ** 2
  return 2 * R * Math.asin(Math.sqrt(h))
}
