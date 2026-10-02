import type * as maplibregl from 'maplibre-gl'

/*
  With terrain on, MapLibre drapes every road onto the ground. Our elevation tiles include
  the seabed, so a bridge's roadway dives to the bottom of the channel it crosses (the Golden
  Gate Bridge dropped ~96 m). In 3D we hide the draped bridges and redraw them at deck height.

  OSM has no deck heights, so they're estimated per vertex: the deck sits at the highest land
  the bridge touches along its own line (its higher abutment), and never below the ground
  under it. So the Golden Gate carries its ~60 m approaches straight across, including over
  Fort Point (9 m), which its viaduct passes above. (A "lower of the two sides" rule failed
  exactly there: viaducts run over low land, which isn't where the deck is.) Sloped bridges
  come out flat at the higher end: fine for a display. Working per vertex means tile clipping,
  which cuts bridges into pieces, doesn't matter.
*/

type XY = [number, number]
type XYZ = [number, number, number]

export interface LiftedBridge {
  path: XYZ[]
  major: boolean
}

const MAJOR = new Set(['motorway', 'trunk', 'primary', 'secondary'])
const SEARCH_M = 4000 // how far along a bridge to look for its abutments
const AXIS_M = 40 // land points within this distance of the bridge's line count as its own
const CLEARANCE_M = 1 // keep the deck just above draped ground to avoid z-fighting
const MIN_OVER_WATER_M = 6

export const BRIDGE_FILTER: maplibregl.FilterSpecification = ['!=', ['coalesce', ['get', 'brunnel'], ''], 'bridge']

export function liftBridges(m: maplibregl.Map): LiftedBridge[] {
  const terrain = m.getTerrain()
  if (!terrain) return []
  // queryTerrainElevation returns heights with the terrain exaggeration applied, which is
  // exactly the scale the 3D layers draw at
  const exag = terrain.exaggeration ?? 1
  const feats = m.querySourceFeatures('openmaptiles', {
    sourceLayer: 'transportation',
    filter: ['==', ['get', 'brunnel'], 'bridge'],
  })

  const seen = new Set<string>()
  const pieces: { coords: XY[]; major: boolean }[] = []
  for (const f of feats) {
    const g = f.geometry
    const lines = g.type === 'LineString' ? [g.coordinates] : g.type === 'MultiLineString' ? g.coordinates : []
    for (const line of lines) {
      const coords = line.map(([x, y]) => [x, y] as XY)
      const key = coords.map(([x, y]) => `${x.toFixed(6)},${y.toFixed(6)}`).join(';')
      if (coords.length < 2 || seen.has(key)) continue
      seen.add(key)
      pieces.push({ coords, major: MAJOR.has(String(f.properties?.class)) })
    }
  }
  if (!pieces.length) return []

  const lat0 = pieces[0].coords[0][1]
  const kx = 111_320 * Math.cos((lat0 * Math.PI) / 180)
  const toM = (c: XY): XY => [c[0] * kx, c[1] * 111_320]

  // every bridge vertex that stands on land, with its ground height
  // null where terrain tiles aren't loaded (off screen): unknown, not sea level
  const cache = new Map<string, number | null>()
  const ground = (c: XY): number | null => {
    const k = `${c[0]},${c[1]}`
    if (!cache.has(k)) cache.set(k, m.queryTerrainElevation(c))
    return cache.get(k)!
  }
  const land: { p: XY; z: number }[] = []
  for (const piece of pieces) {
    for (const c of piece.coords) {
      const z = ground(c)
      if (z != null && z >= 0) land.push({ p: toM(c), z })
    }
  }

  const out: LiftedBridge[] = []
  for (const piece of pieces) {
    const pts = piece.coords.map(toM)
    // the piece's overall direction: local directions are noisy where OSM has tiny segments
    // (e.g. around bridge towers)
    const first = pts[0]
    const last = pts[pts.length - 1]
    const len = Math.hypot(last[0] - first[0], last[1] - first[1]) || 1
    const ux = (last[0] - first[0]) / len
    const uy = (last[1] - first[1]) / len
    out.push({
      major: piece.major,
      path: piece.coords.map((c, i) => {
        // highest land this bridge touches along its own line
        let abutment = -Infinity
        for (const l of land) {
          const dx = l.p[0] - pts[i][0]
          const dy = l.p[1] - pts[i][1]
          if (Math.abs(dx * ux + dy * uy) > SEARCH_M || Math.abs(dx * uy - dy * ux) > AXIS_M) continue
          abutment = Math.max(abutment, l.z)
        }
        const z0 = ground(c)
        const floor = z0 != null && z0 >= 0 ? z0 + CLEARANCE_M * exag : MIN_OVER_WATER_M * exag
        return [c[0], c[1], Math.max(abutment, floor)] as XYZ
      }),
    })
  }
  return out
}
