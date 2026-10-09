// The water in 3D (murmur-5xl): every water polygon of the basemap is tinted by the style's own
// water fill, draped on the terrain, and what is under water is drawn in the colour it has seen
// through that tint. There is no sea surface at 0 m any more. A plane at 0 m, cut to the water
// and pulled towards the camera by a depth offset, tinted the water only where it won the depth
// test against the terrain under it, and the water is not at 0 m: measured against the console's
// own elevation tiles (Terrarium, the 3D terrain) under the basemap's water around the bay
// (2026-10-09), lakes and reservoirs sit at a median +1.8 m (reservoirs to +150 m), ponds and
// salt ponds +1.1 m, rivers anywhere from -6 m (the North Bay's tidal rivers) to +1 m (the South
// Bay's creeks), and even the `ocean` class reads above 0 m over 53 % of it in the Mountain View
// tiles (tidal flats, marsh and sloughs to +1.3 m). So the plane lost seen from above and won
// once tilted, and a pond's colour depended on the view angle. Draped, the tint follows the
// terrain at any height and any view angle, has no depth to fight (no flashing, no shore or
// bridge painted), and costs nothing per frame. Pure functions (no MapLibre or deck.gl at run
// time): checked by sea.check.ts.

import { domainOf, type Vehicle } from './types.ts'

export type RGBA = [number, number, number, number]

/** The water's tint in 3D: the colour of the translucent sea surface this replaced. */
export const SEA_TINT: RGBA = [40, 110, 160, 70]

/** Water the draped fill leaves at the style's own colour in 3D: swimming pools (many, in
 *  cities, and never tinted before). */
export const UNTINTED_CLASSES = ['swimming_pool']

/** The style's water fill in 3D, for a layer whose colour is `base` (2D keeps the style's own):
 *  every water polygon but pools gets SEA_TINT composited over `base`, which is what the sea
 *  surface made of the same fill where it drew (it blended over the draped water as a
 *  translucent fill does), so the water looks as it did where the surface won. The composite is
 *  left to MapLibre (`to-rgba`), so it holds for whatever colour the style gives its water; a
 *  `base` that is not a plain colour string (an expression or a zoom function, none in the style
 *  now) cannot be nested in a match, and gives null: the caller leaves the layer as it is. */
export function drapedWater(base: unknown, tint: RGBA = SEA_TINT) {
  if (typeof base !== 'string') return null
  const a = tint[3] / 255
  const over = [
    'let',
    'water',
    ['to-rgba', ['to-color', base]],
    ['rgba', ...[0, 1, 2].map((k) => ['+', ['*', ['at', k, ['var', 'water']], 1 - a], tint[k] * a]), ['at', 3, ['var', 'water']]],
  ]
  return { color: ['match', ['get', 'class'], UNTINTED_CLASSES, base, over] as unknown[] }
}

/** The colour something under water is drawn in: as the sea surface showed it, SEA_TINT over
 *  its own colour, its alpha kept. Drawn with alpha `a` over the draped (tinted) water, that
 *  gives exactly what the surface gave drawn over it: tint over (colour over water) and (tint
 *  over colour) over (tint over water) come out the same (sea.check.ts). */
export function seenThroughWater(c: RGBA, tint: RGBA = SEA_TINT): RGBA {
  const a = tint[3] / 255
  return [c[0] * (1 - a) + tint[0] * a, c[1] * (1 - a) + tint[1] * a, c[2] * (1 - a) + tint[2] * a, c[3]]
}

// --- what is drawn as under water (map3d.ts, trailLayers.ts) -----------------------------------

/** Seen through the water: a submarine's trail and stem (the subsurface domain), and anything
 *  more than a metre under the surface. A surface vessel's GPS altitude wanders around 0, and
 *  the margin keeps its trail from flipping between tinted and not. A surfaced submarine counts
 *  as under water on purpose: its trail (mostly a dive) does not change colour each time it
 *  comes up. */
export function underWater(v: Pick<Vehicle, 'kind' | 'identity' | 'alt_msl'>) {
  return domainOf(v) === 'subsurface' || (v.alt_msl != null && v.alt_msl < -1)
}

/** The uids of the vehicles under water, and a signature that changes only when that set does. */
export function belowOf(vehicles: Pick<Vehicle, 'uid' | 'kind' | 'identity' | 'alt_msl'>[]) {
  const below = new Set(vehicles.filter(underWater).map((v) => v.uid))
  return { below, sig: [...below].join() }
}

export interface UnderSplit<T> {
  list: T[]
  sig: string
  under: T[]
  over: T[]
}

/** A list split into what is under water (`below`) and the rest. The previous split comes back
 *  as it was while the list and the signature are the same, so deck.gl gets the same data arrays
 *  and does not redo the layers; with nothing under water `over` is the list itself. */
export function splitUnder<T extends { uid: number }>(prev: UnderSplit<T> | null, list: T[], below: Set<number>, sig: string): UnderSplit<T> {
  if (prev && prev.list === list && prev.sig === sig) return prev
  if (!below.size) return { list, sig, under: [], over: list }
  return { list, sig, under: list.filter((v) => below.has(v.uid)), over: list.filter((v) => !below.has(v.uid)) }
}
