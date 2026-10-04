// What the map shows (murmur-and): the bounds and zoom the console sends the bridge as its
// view subscription, and the same margin the bridge applies, so the symbols the console
// draws are the vehicles the bridge sends in full (plus lite ones just revealed by a pan,
// until their full detail arrives).
import type { Vehicle } from './types'

export type Bounds = [number, number, number, number] // w, s, e, n

export interface MapViewport {
  bounds: Bounds
  zoom: number
}

/** The bridge's margin around the view (push.rs VIEW_MARGIN) */
export const VIEW_MARGIN = 0.25

export function expand(b: Bounds, margin = VIEW_MARGIN): Bounds {
  const [w, s, e, n] = b
  const dx = (e - w) * margin
  const dy = (n - s) * margin
  return [w - dx, s - dy, e + dx, n + dy]
}

export function inBounds([lon, lat]: [number, number], [w, s, e, n]: Bounds) {
  if (lat < s || lat > n) return false
  if (w > e) return lon >= w || lon <= e // across the antimeridian
  return lon >= w && lon <= e
}

/** The vehicles to draw as symbols: those at full detail, and the lite ones in the (grown)
 *  view (a pan reveals them before their full detail arrives). In cluster mode the markers
 *  stand for the lite ones, so only the full ones (pinned) are drawn. All of them when there
 *  is no view yet. */
export function visibleVehicles(vehicles: Vehicle[], view: MapViewport | null, clustered: boolean): Vehicle[] {
  if (clustered) return vehicles.filter((v) => v.lod !== 'lite')
  if (!view) return vehicles
  const b = expand(view.bounds)
  return vehicles.filter((v) => v.lod !== 'lite' || (v.position && inBounds(v.position, b)))
}
