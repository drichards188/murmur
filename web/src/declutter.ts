// Map label declutter (murmur-02g): milsymbol labels (callsign, type, heading) overlap when
// vehicles bunch up, e.g. planes on a runway. Greedy, in screen space: selected vehicles first,
// then by uid, each keeps its labels only if its label box is clear of every labelled vehicle
// before it. The rest are drawn as bare symbols and show their callsign on hover.

export interface ScreenPoint {
  uid: number
  x: number
  y: number
}

// label block of a 28 px symbol: type to the left, callsign to the right, about this big
const HALF_W = 70
const HALF_H = 22
/** below this zoom, symbols alone (a whole region is on screen; labels would be noise) */
export const LABEL_MIN_ZOOM = 9

export function declutter(points: ScreenPoint[], selected: Set<number>, zoom: number): Set<number> {
  const bare = new Set<number>()
  if (zoom < LABEL_MIN_ZOOM) {
    for (const p of points) if (!selected.has(p.uid)) bare.add(p.uid)
    return bare
  }
  const order = [...points].sort((a, b) => Number(selected.has(b.uid)) - Number(selected.has(a.uid)) || a.uid - b.uid)
  const kept: ScreenPoint[] = []
  for (const p of order) {
    const clash = kept.some((k) => Math.abs(k.x - p.x) < 2 * HALF_W && Math.abs(k.y - p.y) < 2 * HALF_H)
    if (clash && !selected.has(p.uid)) bare.add(p.uid)
    else kept.push(p)
  }
  return bare
}
