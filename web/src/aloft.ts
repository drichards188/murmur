import type { Viewport } from '@deck.gl/core'

/** What a camera a few kilometres up cannot show (murmur-1e4): heights are drawn twice over
 *  (map3d.ts), so a HALE aircraft at 18,000 m is drawn at 36 km, a MALE at 7,600 m at 15 km,
 *  and at the zooms an operator works at around the bay the camera is lower than that: the
 *  aircraft is above the view (or behind the camera) while the foot of its stem, on the water
 *  under it, is in it. Such a vehicle or track gets its label at that foot, with an arrow up, so
 *  it is never missing from the map; the stem rises from there. */
export interface MapScreen {
  /** drawn out of the view at its height (rendered metres), while its foot at sea level is in it */
  aloft(lon: number, lat: number, z: number): boolean
  /** where deck.gl draws a point, CSS pixels in the map; null behind the camera */
  at(lon: number, lat: number, z: number): [number, number] | null
}

/** Nothing is aloft: before the map has a camera. */
export const NO_SCREEN: MapScreen = { aloft: () => false, at: () => null }

/** The test for this camera: deck.gl's viewport (what it draws with) and the map's size. */
export function mapScreen(vp: Viewport, width: number, height: number): MapScreen {
  const m = vp.pixelProjectionMatrix
  const at = (lon: number, lat: number, z: number): [number, number] | null => {
    const [x, y, zz] = vp.projectPosition([lon, lat, z])
    const w = m[3] * x + m[7] * y + m[11] * zz + m[15]
    if (w <= 0) return null
    return [(m[0] * x + m[4] * y + m[8] * zz + m[12]) / w, (m[1] * x + m[5] * y + m[9] * zz + m[13]) / w]
  }
  const inside = (p: [number, number] | null) => p != null && p[0] >= 0 && p[0] <= width && p[1] >= 0 && p[1] <= height
  return { at, aloft: (lon, lat, z) => z > 0 && !inside(at(lon, lat, z)) && inside(at(lon, lat, 0)) }
}
