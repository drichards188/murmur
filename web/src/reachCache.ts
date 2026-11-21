// A ring's lifted path is the expensive part of drawing the sensor reach rings: every vertex asks
// the terrain for its height (surfaceLayers.ts `lift`, a ring of 5 km is 780 vertices, a hundred
// selected vehicles' six rings each 470,000 lookups an update; bd murmur-yfp6 measured it at 5.5 of
// every 15 s). A ring follows its vehicle, but the ground under a ring that moved a few metres is
// the ground it was lifted from. So a ring is lifted once and then only translated while its
// centre stays within `shiftLimitM` of where it was lifted; past that, or when its radius, the
// terrain or the zoom changed, it is lifted again. Pure (no deck.gl, no map), so it has a check.
import type { LngLat } from './types'

const M_PER_DEG_LAT = 111_320

/** how far a ring's centre may move from where its path was lifted and still reuse the heights:
 *  3 % of its radius, at most 150 m (a ring is km across; the surface it was lifted over is
 *  the same to a few metres there) */
export function shiftLimitM(radiusM: number): number {
  return Math.min(150, 0.03 * radiusM)
}

interface Entry<B> {
  at: LngLat
  m: number
  sig: string
  built: B
  seen: boolean
}

export class RingCache<B> {
  private entries = new Map<string, Entry<B>>()
  /** how many rings were lifted (built) and how many reused, since `reset` (for the check and ?perf) */
  built = 0
  reused = 0

  /** The ring `key` centred `at` of radius `m`: built by `build` (centred `at`), or the one built
   *  earlier moved by `shift` to `at`. `sig` is whatever else the build rests on (terrain, zoom). */
  get(key: string, at: LngLat, m: number, sig: string, build: () => B, shift: (b: B, dLon: number, dLat: number) => B): B {
    const e = this.entries.get(key)
    if (e && e.m === m && e.sig === sig) {
      const dLat = (at[1] - e.at[1]) * M_PER_DEG_LAT
      const dLon = (at[0] - e.at[0]) * M_PER_DEG_LAT * Math.cos((at[1] * Math.PI) / 180)
      if (Math.hypot(dLat, dLon) <= shiftLimitM(m)) {
        e.seen = true
        this.reused++
        return dLat === 0 && dLon === 0 ? e.built : shift(e.built, at[0] - e.at[0], at[1] - e.at[1])
      }
    }
    const built = build()
    this.entries.set(key, { at, m, sig, built, seen: true })
    this.built++
    return built
  }

  /** Forget the rings not asked for since the last sweep (a vehicle deselected, a sensor gone). */
  sweep() {
    for (const [k, e] of this.entries) {
      if (e.seen) e.seen = false
      else this.entries.delete(k)
    }
  }

  get size() {
    return this.entries.size
  }
}

/** A ring set's identity apart from where its rings stand: each sensor ring's id and radius (a
 *  minefield's feature by its kind and size). Equal sets are the same drawing, moved a little. */
export function ringSet(fc: GeoJSON.FeatureCollection): string {
  let out = ''
  for (const f of fc.features) {
    const p = f.properties ?? {}
    out += p.id != null ? `${p.id}:${p.m};` : `${p.kind ?? ''}${f.geometry.type === 'LineString' ? f.geometry.coordinates.length : ''}${JSON.stringify(f.geometry).length};`
  }
  return out
}

/** The same rings are redrawn at most this often as their vehicles move (a ring is km across: a
 *  second of a vehicle's motion is a few metres of it). A change of the set, the terrain or the
 *  zoom redraws at once. */
export const REACH_REDRAW_MS = 1000
