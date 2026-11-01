import type * as maplibregl from 'maplibre-gl'
import { LngLat } from 'maplibre-gl'

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

/** The ground height under each point, as `queryTerrainElevation` gives it (with the terrain
 *  exaggeration), for one camera: null where no terrain tiles are loaded. `queryTerrainElevation`
 *  works out which elevation zoom to read from the tiles covering the whole view on every call
 *  (MapLibre's `Terrain.getElevationForLngLat`), which was nearly all of a lift's cost (some 0.8 s
 *  of every 10 s of a camera move); that zoom is the same for every point of one camera, so this
 *  works it out once, the same way, and reads each point at it. */
function groundAt(m: maplibregl.Map): ((c: XY) => number | null) | null {
  const terrain = m.terrain
  if (!terrain) return null
  const tm = terrain.tileManager
  // MapLibre's own query: the covering tiles with the terrain's zoom range and the terrain itself
  const covering = m.coveringTiles({ tileSize: 512, minzoom: tm.minzoom, maxzoom: tm.maxzoom, terrain } as maplibregl.CoveringTilesOptions)
  let zoom = 0
  for (const t of covering) if (t.canonical.z > zoom) zoom = Math.min(t.canonical.z, tm.maxzoom)
  return (c) => terrain.getElevationForLngLatZoom(new LngLat(c[0], c[1]), zoom)
}

/** What a lift reads: the bridge pieces in the loaded tiles and the ground under each of their
 *  vertices. Two lifts that read the same give the same decks. */
interface LiftInput {
  pieces: { coords: XY[]; major: boolean }[]
  ground: Map<string, number | null>
  exag: number
  /** all of the above, as one string */
  key: string
}

function readInput(m: maplibregl.Map): LiftInput | null {
  const terrain = m.getTerrain()
  const at = groundAt(m)
  if (!terrain || !at) return null
  // queryTerrainElevation returns heights with the terrain exaggeration applied, which is
  // exactly the scale the 3D layers draw at
  const exag = terrain.exaggeration ?? 1
  const feats = m.querySourceFeatures('openmaptiles', {
    sourceLayer: 'transportation',
    filter: ['==', ['get', 'brunnel'], 'bridge'],
  })

  const seen = new Set<string>()
  const pieces: { coords: XY[]; major: boolean }[] = []
  const keys: string[] = []
  for (const f of feats) {
    const g = f.geometry
    const lines = g.type === 'LineString' ? [g.coordinates] : g.type === 'MultiLineString' ? g.coordinates : []
    for (const line of lines) {
      const coords = line.map(([x, y]) => [x, y] as XY)
      const key = coords.map(([x, y]) => `${x.toFixed(6)},${y.toFixed(6)}`).join(';')
      if (coords.length < 2 || seen.has(key)) continue
      seen.add(key)
      const major = MAJOR.has(String(f.properties?.class))
      pieces.push({ coords, major })
      keys.push(`${major ? 'M' : 'm'}${key}`)
    }
  }
  // null where terrain tiles aren't loaded (off screen): unknown, not sea level
  const ground = new Map<string, number | null>()
  const heights: string[] = []
  for (const piece of pieces) {
    for (const c of piece.coords) {
      const k = `${c[0]},${c[1]}`
      if (ground.has(k)) continue
      const z = at(c)
      ground.set(k, z)
      heights.push(`${k}=${z}`)
    }
  }
  return { pieces, ground, exag, key: `${exag}|${keys.join('|')}|${heights.join(';')}` }
}

/** Lifts the bridges, keeping the last decks while what they are made from is the same: returns
 *  the very array it returned last then, so the caller can tell nothing changed. */
export function bridgeLifter(): (m: maplibregl.Map) => LiftedBridge[] {
  let last: { key: string; out: LiftedBridge[] } | null = null
  return (m) => {
    const input = readInput(m)
    if (!input) {
      last = null
      return NONE
    }
    if (last && last.key === input.key) return last.out
    const out = lift(input)
    last = { key: input.key, out }
    return out
  }
}

const NONE: LiftedBridge[] = []

function lift({ pieces, ground: grounds, exag }: LiftInput): LiftedBridge[] {
  if (!pieces.length) return NONE

  const lat0 = pieces[0].coords[0][1]
  const kx = 111_320 * Math.cos((lat0 * Math.PI) / 180)
  const toM = (c: XY): XY => [c[0] * kx, c[1] * 111_320]
  const ground = (c: XY): number | null => grounds.get(`${c[0]},${c[1]}`) ?? null

  // every bridge vertex that stands on land, with its ground height
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
    // only the land within reach of some vertex of this piece can be its abutment: its corridor
    // (the vertices' extent along and across its line, widened by the search), a metre wider
    // still so rounding never drops a point the test below would keep
    let a0 = Infinity
    let a1 = -Infinity
    let c0 = Infinity
    let c1 = -Infinity
    for (const p of pts) {
      const a = p[0] * ux + p[1] * uy
      const c = p[0] * uy - p[1] * ux
      a0 = Math.min(a0, a)
      a1 = Math.max(a1, a)
      c0 = Math.min(c0, c)
      c1 = Math.max(c1, c)
    }
    const near = land.filter(({ p }) => {
      const a = p[0] * ux + p[1] * uy
      const c = p[0] * uy - p[1] * ux
      return a >= a0 - SEARCH_M - 1 && a <= a1 + SEARCH_M + 1 && c >= c0 - AXIS_M - 1 && c <= c1 + AXIS_M + 1
    })
    out.push({
      major: piece.major,
      path: piece.coords.map((c, i) => {
        // highest land this bridge touches along its own line
        let abutment = -Infinity
        for (const l of near) {
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
