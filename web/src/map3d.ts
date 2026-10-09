import { LineLayer, PathLayer, ScatterplotLayer, SolidPolygonLayer } from '@deck.gl/layers'
import type * as maplibregl from 'maplibre-gl'
import type { LiftedBridge } from './bridges'
import { drawnTick, drawnXYZ, smoothing } from './drawn'
import { assembleSea, outermost, polygonOffset, polygonsOf, tileKey, type SeaPolygon, type TileWater } from './sea'
import type { Vehicle } from './types'
import { domainOf } from './types'

// Heights are exaggerated in 3D so 80 m altitudes and 5 m depths read at bay scale.
// Terrain uses the same factor, so vehicles stay in the right place relative to hills and seabed.
export const HEIGHT_EXAGGERATION = 2

// Terrarium-encoded elevation tiles (AWS open data, Mapzen). Includes bathymetry:
// the Golden Gate channel reads -96 m, matching NOAA's DEM.
export const DEM_TILES = 'https://s3.amazonaws.com/elevation-tiles-prod/terrarium/{z}/{x}/{y}.png'

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]
const AIR: RGBA = [127, 184, 255, 255]
const SEA: RGBA = [111, 224, 196, 255]
const GROUND: RGBA = [214, 196, 140, 255]

export function colour(v: Vehicle, alpha = 255): RGBA {
  const c = domainOf(v) === 'air' ? AIR : domainOf(v) === 'ground' ? GROUND : SEA
  return [c[0], c[1], c[2], alpha]
}

export function z(metresMsl: number | null) {
  return (metresMsl ?? 0) * HEIGHT_EXAGGERATION
}

/** Where a vehicle is drawn in 3D, in rendered metres (the camera aims here to follow it). */
export function drawnHeight(v: Vehicle) {
  return z(v.alt_msl)
}

/** a number from the page's URL (read once at load), else the default */
function urlNumber(name: string, fallback: number) {
  try {
    const q = new URLSearchParams(location.search).get(name)
    if (q != null && q.trim() !== '' && Number.isFinite(Number(q))) return Number(q)
  } catch {
    // no location (not a browser): the default
  }
  return fallback
}

/** Depth offset of the sea surface (murmur-5xl), as gl.polygonOffset(slope, bias).
 *
 *  The plane sits at 0 m over MapLibre terrain whose bathymetry is exaggerated. Over shallow
 *  water the terrain's surface comes within depth-buffer precision of the plane, and closer
 *  still at mid zooms, where a terrain mesh cell spans hundreds of metres and takes in the
 *  shore; which of the two wins then flips from frame to frame (the blue flashing), or the
 *  terrain simply wins and leaves shallow water untinted. So the plane is pulled towards the
 *  camera.
 *
 *  `bias` is the constant part, in units of the depth buffer's resolution (`?seabias=<n>`).
 *  `slope` (`?seaslope=<n>`) scales with the plane's own depth slope, about one pixel's worth of
 *  how far the plane recedes per pixel: at a far, oblique view a hundred metres or more. That
 *  term is what let the first fix (both -1) paint hills, cities and bridge decks tens of metres
 *  up when the plane was a rectangle over land. Now that the plane is cut to the basemap's water
 *  (sea.ts, refreshSea) and the bridge decks draw after it, it can only reach what lies inside
 *  the water polygons: shores, tidal flats and salt pond levees. And a constant alone is not
 *  enough: in software rendering (the Playwright browser, 24-bit depth) at zoom 10.6 and pitch
 *  55 over the bay off SFO, a bias of -4 tinted 41,000 pixels with no slope, 95,000 with
 *  -0.25, 155,000 with -0.5, 179,000 with -1, 181,000 with -2 and 187,000 with an offset nothing
 *  can hide (that last one also paints the shore), and which tiles came out untinted with no
 *  slope changed with the camera's history (the terrain mesh's level of detail). So the slope
 *  stays at -1, the value the first fix confirmed on a real GPU against the flashing, and the
 *  constant is -4 (the first fix had -1), for a top-down view where the slope term is nearly 0;
 *  -4 tints 0.1 % more pixels than no constant at all. Confirm on a real GPU: no flashing over
 *  shallow water at a grazing angle, no shallow water left untinted, no shore painted far inland.
 *  Both are read once at load; in a dev build `murmurSeaOffset({ bias, slope })` in the browser
 *  console changes them live (from the next update), to compare values without losing the view. */
const seaOffset = { bias: urlNumber('seabias', -4), slope: urlNumber('seaslope', -1) }
if (import.meta.env.DEV && typeof window !== 'undefined') {
  ;(window as unknown as { murmurSeaOffset: (o: Partial<typeof seaOffset>) => typeof seaOffset }).murmurSeaOffset = (o) => Object.assign(seaOffset, o)
}

// the basemap's water (OpenMapTiles `water`), as the style's own water layer draws it; swimming
// pools left out (on land, under the terrain anyway, and many)
const WATER_FILTER = [
  'all',
  ['match', ['geometry-type'], ['Polygon', 'MultiPolygon'], true, false],
  ['!=', ['coalesce', ['get', 'brunnel'], ''], 'tunnel'],
  ['!=', ['coalesce', ['get', 'class'], ''], 'swimming_pool'],
] as maplibregl.FilterSpecification

// the water tiles the sea surface was last built from, and the polygons
let seaKey = ''
let seaData: SeaPolygon[] = []

/** Re-reads the water of the vector tiles MapLibre has for the view and, if that set of tiles
 *  changed, rebuilds the sea surface from it; true when it did (the layers need pushing again).
 *  Cheap when nothing changed: the query reads tile ids, not geometry (MapLibre's features
 *  convert their geometry lazily). The same data object is kept until the tile set changes, so
 *  deck.gl does not tessellate the sea again on every update, and the depth results do not
 *  reshuffle with a still camera (a polygon rebuilt per update was part of the flashing).
 *  Until water tiles have loaded there is no sea at all: a stand-in rectangle would paint the
 *  land under it, which is the bug this replaced. Inland water above sea level is drawn too, at
 *  0 m, where the terrain over it hides it: reservoirs tens of metres up stayed clear at every
 *  view tried, though a lake only a few metres up (Lake Merced) can show tinted from far off at
 *  a grazing angle, where the slope offset reaches that high. */
export function refreshSea(m: maplibregl.Map): boolean {
  const feats = m.querySourceFeatures('openmaptiles', { sourceLayer: 'water', filter: WATER_FILTER })
  const byTile = new Map<string, { z: number; x: number; y: number; feats: typeof feats }>()
  for (const f of feats) {
    const id = { z: f._z, x: f._x, y: f._y }
    const k = tileKey(id)
    const t = byTile.get(k)
    if (t) t.feats.push(f)
    else byTile.set(k, { ...id, feats: [f] })
  }
  const key = [...byTile.entries()]
    .map(([k, t]) => `${k}:${t.feats.length}`)
    .sort()
    .join(' ')
  if (key === seaKey) return false
  seaKey = key
  // geometry only for the tiles drawn (not a child under a kept parent)
  const tiles: TileWater[] = outermost([...byTile.values()]).map((t) => ({ z: t.z, x: t.x, y: t.y, polygons: t.feats.flatMap((f) => polygonsOf(f.geometry)) }))
  seaData = assembleSea(tiles)
  return true
}

/** The 3D scene around the vehicles is built in two parts: build3dStatic (trails, sea surface,
 *  bridge decks) and build3dGlide (height stems and sea-level shadows). The vehicles themselves
 *  (both views) are in vehicleLayers.ts. visible=false keeps the layers (and their GPU
 *  resources) alive while the 2D view is shown.
 *
 *  Draw order around the sea surface (MapView.deckLayers): the sea tests depth but never writes
 *  it, so whatever is drawn before it and lies below 0 m is seen through the water (tinted),
 *  and whatever is drawn after it is never tinted. Before it: the terrain and seabed (MapLibre),
 *  trails and stems (a submarine's are under water). After it: bridge decks (above the water by
 *  construction, so no depth offset of the sea can reach them), sea-level shadows (on the water
 *  surface, which they would otherwise fight), and the screen-space overlays. */

/** Trails, sea surface and bridge decks: nothing here moves with a glide (display smoothing,
 *  drawn.ts), so MapView builds them when the data changes and reuses them between frames.
 *  `under` goes before the stems and the sea surface, `surface` after the stems. */
export function build3dStatic(vehicles: Vehicle[], visible = true, bridges: LiftedBridge[] = []) {
  // nothing to compute while 2D is shown: the layers stay (hidden), their data is empty
  const placed = visible ? vehicles.filter((v) => v.position && v.alt_msl != null) : []
  const at = (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  return {
    under: [
      new PathLayer<Vehicle>({
        id: 'trails-3d',
        visible,
        // with smoothing on, trailLayers.ts draws the trails (they end at the drawn position)
        data: smoothing() ? [] : placed.filter((v) => v.trail.length > 1),
        getPath: (v) => [...v.trail.map(([lon, lat, alt]): XYZ => [lon, lat, z(alt)]), at(v)],
        getColor: (v) => colour(v, 140),
        getWidth: 2,
        widthUnits: 'pixels',
        updateTriggers: { getPath: placed.map((v) => v.trail.length).join() },
      }),
    ],
    // The sea is drawn in two passes so that each pixel is tinted once, however many water
    // polygons cover it: OSM's water areas overlap (a salt pond over its pond complex, a lagoon
    // inside the coastline's water), and a translucent fill blends twice where they do. Bit 0
    // of the stencil marks a tinted pixel: the first pass clears it under the sea's outline
    // (MapLibre's tile clipping masks leave tile ids in the stencil) without drawing anything;
    // the second tints where it is clear and sets it, so a second polygon over the same pixel
    // fails. Only bit 0, because luma.gl 9.4's WebGL render pass sets the stencil write mask to
    // 1 before each draw (a wider mask would work the same) and compares against reference 0;
    // luma.gl restores the rest of the state after each draw, and nothing draws between the two
    // passes (consecutive layers of one deck.gl group). MapLibre clears the stencil at the
    // start of every frame.
    surface: [
      new SolidPolygonLayer<SeaPolygon>({
        id: 'sea-stencil',
        visible,
        data: seaData,
        getPolygon: (d) => d.polygon,
        positionFormat: 'XY',
        getFillColor: [0, 0, 0, 255],
        parameters: {
          // everywhere under the sea's outline, hidden or not, and no colour (blend keeps dst)
          depthCompare: 'always',
          depthWriteEnabled: false,
          blend: true,
          blendColorOperation: 'add',
          blendColorSrcFactor: 'zero',
          blendColorDstFactor: 'one',
          blendAlphaOperation: 'add',
          blendAlphaSrcFactor: 'zero',
          blendAlphaDstFactor: 'one',
          // 'always' would switch the stencil test (and its writes) off: where set, clear it
          stencilReadMask: 1,
          stencilWriteMask: 1,
          stencilCompare: 'not-equal',
          stencilPassOperation: 'zero',
          stencilFailOperation: 'keep',
          stencilDepthFailOperation: 'zero',
        },
      }),
      new SolidPolygonLayer<SeaPolygon>({
        id: 'sea-surface',
        visible,
        // the basemap's water, at 0 m (refreshSea); the same array until the water tiles change
        data: seaData,
        getPolygon: (d) => d.polygon,
        positionFormat: 'XY',
        getFillColor: [40, 110, 160, 70],
        parameters: {
          depthWriteEnabled: false,
          ...polygonOffset(seaOffset.slope, seaOffset.bias),
          // tint where bit 0 is clear, and set it
          stencilReadMask: 1,
          stencilWriteMask: 1,
          stencilCompare: 'equal',
          stencilPassOperation: 'invert',
          stencilFailOperation: 'keep',
          stencilDepthFailOperation: 'keep',
        },
      }),
      // bridge decks at estimated height (see bridges.ts), after the sea surface: never tinted
      new PathLayer<LiftedBridge>({
        id: 'bridge-decks',
        visible,
        data: bridges,
        getPath: (b) => b.path,
        getColor: (b) => (b.major ? [58, 64, 74, 255] : [44, 49, 57, 255]),
        getWidth: (b) => (b.major ? 14 : 8),
        widthUnits: 'meters',
        widthMinPixels: 2,
        capRounded: true,
        jointRounded: true,
      }),
    ],
  }
}

// the vehicles placed in 3D, kept while the vehicles are the same (animation frames of a glide)
let memo: { vehicles: Vehicle[]; visible: boolean; placed: Vehicle[] } | null = null

/** Stems and sea-level shadows: they follow a vehicle's drawn position (display smoothing,
 *  drawn.ts), so MapView rebuilds just these on an animation frame. The stems go before the sea
 *  surface (a submerged one is seen through the water), the shadows after it (they mark the
 *  surface). */
export function build3dGlide(vehicles: Vehicle[], visible = true) {
  if (!memo || memo.vehicles !== vehicles || memo.visible !== visible) {
    memo = { vehicles, visible, placed: visible ? vehicles.filter((v) => v.position && v.alt_msl != null) : [] }
  }
  const { placed } = memo
  const smooth = smoothing()
  const at = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  const ground = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, 0) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], 0]
  const triggers = smooth ? { getSourcePosition: drawnTick(), getTargetPosition: drawnTick(), getPosition: drawnTick() } : {}
  return {
    // stem from sea level to the vehicle: shows height (or depth) at a glance
    stems: new LineLayer<Vehicle>({
      id: 'stems',
      visible,
      data: placed,
      getSourcePosition: ground,
      getTargetPosition: at,
      getColor: (v) => colour(v, 160),
      getWidth: 1.5,
      widthUnits: 'pixels',
      updateTriggers: triggers,
    }),
    shadows: new ScatterplotLayer<Vehicle>({
      id: 'sea-level-shadow',
      visible,
      data: placed,
      getPosition: ground,
      getRadius: 4,
      radiusUnits: 'pixels',
      getFillColor: [0, 0, 0, 110],
      updateTriggers: triggers,
    }),
  }
}
