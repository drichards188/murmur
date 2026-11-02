// Rules 27 (bd murmur-lbw8.4): our minefields on the map. GET /api/minefields serves every field
// of ours not yet inert (the engine's record: each mine's place, when it arms and goes inert, the
// trigger reach, the danger area round its mines, the domains that set it off). Each live field is
// drawn as its danger area's outline (a capsule round each run of mines, the no-go area the
// planner and the geofence keep our vehicles of those domains out of) and a small ring on each
// mine, on the surface with the sensor reach rings (reachLayers.ts, kinds `minefield` and `mine`).
import type { LngLat } from './types'

export interface MineView {
  n: number
  at: LngLat
  laid_t: number
  arms_t: number
  inert_t: number
}

export interface MinefieldView {
  id: string
  intent: number
  from: string | null
  layer: number | null
  weapon: string
  trigger: string
  reach_m: number
  danger_m: number
  targets: string[]
  live: boolean
  inert_t: number
  mines: MineView[]
}

export interface MinefieldsAnswer {
  minefields: MinefieldView[]
}

const M_PER_DEG = 111_320

/** `p` moved `e` metres east and `n` north (a flat earth over a field's few hundred metres). */
function offset(p: LngLat, e: number, n: number): LngLat {
  return [p[0] + e / (M_PER_DEG * Math.cos((p[1] * Math.PI) / 180)), p[1] + n / M_PER_DEG]
}

/** east and north metres from `a` to `b` */
function enu(a: LngLat, b: LngLat): [number, number] {
  return [(b[0] - a[0]) * M_PER_DEG * Math.cos((a[1] * Math.PI) / 180), (b[1] - a[1]) * M_PER_DEG]
}

/** A closed ring of radius `r` round `c`. */
function ring(c: LngLat, r: number, n = 24): LngLat[] {
  const out: LngLat[] = []
  for (let i = 0; i <= n; i++) {
    const t = (2 * Math.PI * i) / n
    out.push(offset(c, r * Math.cos(t), r * Math.sin(t)))
  }
  return out
}

/** The outline of the capsule of half-width `r` round the segment `a`-`b` (a ring for one point). */
export function capsule(a: LngLat, b: LngLat, r: number, arc = 12): LngLat[] {
  const [e, n] = enu(a, b)
  if (Math.hypot(e, n) < 0.5) return ring(a, r)
  const h = Math.atan2(n, e)
  const out: LngLat[] = []
  for (const [c, from] of [[b, h - Math.PI / 2], [a, h + Math.PI / 2]] as [LngLat, number][]) {
    for (let i = 0; i <= arc; i++) {
      const t = from + (Math.PI * i) / arc
      out.push(offset(c, r * Math.cos(t), r * Math.sin(t)))
    }
  }
  out.push(out[0])
  return out
}

/** The live fields as LineString features: each run's danger outline (`minefield`) and a ring on
 *  each mine (`mine`), each with the field's words for a tooltip. */
export function minefieldFeatures(answer: MinefieldsAnswer | null): GeoJSON.Feature[] {
  const out: GeoJSON.Feature[] = []
  for (const f of answer?.minefields ?? []) {
    if (!f.live || !f.mines.length) continue
    const words = `${f.id}: ${f.mines.length} ${f.weapon}, set off by ${f.targets.join(' and ')} within ${Math.round(f.reach_m)} m`
    const pts = f.mines.map((m) => m.at)
    const runs = pts.length === 1 ? [[pts[0], pts[0]]] : pts.slice(1).map((p, i) => [pts[i], p])
    for (const [a, b] of runs) {
      out.push({ type: 'Feature', properties: { kind: 'minefield', sensor: 'minefield', field: f.id, words }, geometry: { type: 'LineString', coordinates: capsule(a, b, f.danger_m) } })
    }
    for (const m of f.mines) {
      out.push({ type: 'Feature', properties: { kind: 'mine', sensor: 'minefield', field: f.id, words }, geometry: { type: 'LineString', coordinates: ring(m.at, Math.max(f.reach_m, 3), 12) } })
    }
  }
  return out
}
