import type { UpdateParameters } from '@deck.gl/core'
import { LineLayer, PathLayer, ScatterplotLayer, SolidPolygonLayer } from '@deck.gl/layers'
import type * as maplibregl from 'maplibre-gl'
import type { LiftedBridge } from './bridges'
import { drawnTick, drawnXYZ, smoothing } from './drawn'
import { perfSeaTess } from './perf'
import { assembleSea, byTile, outermost, polygonOffset, polygonsOf, SEA_TINT, tilesKey, underWater, type SeaPolygon, type TileWater } from './sea'
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
 *  up when the plane was a rectangle over land. Now that the plane is cut to the basemap's sea
 *  (sea.ts, refreshSea) and the bridge decks draw after it, it can only reach what lies inside
 *  the sea's polygons: shores, tidal flats and marsh. And a constant alone is not
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

// the water tiles the sea surface was last built from, and the polygons
let seaKey = ''
let seaData: SeaPolygon[] = []
let warnedIds = false

/** Re-reads the sea of the vector tiles MapLibre has for the view and, if that set of tiles
 *  changed, rebuilds the sea surface from it; true when it did (the layers need pushing again).
 *  A check that finds nothing changed costs one unfiltered querySourceFeatures over the water
 *  layer of the renderable tiles: MapLibre builds a light feature object for each (lakes and
 *  pools included) and converts no geometry until it is read; the sea is filtered here, and
 *  only on a rebuild (?perf; over the probes of murmur-5xl's class split, a check 0.2 ms, a
 *  rebuild 3.3 ms here plus 3.6 ms of deck.gl tessellation, about half what it cost while every
 *  water polygon was part of the sea). The same data object is kept until the tile set
 *  changes, so deck.gl does not tessellate the sea again on every update, and the depth results
 *  do not reshuffle with a still camera (a polygon rebuilt per update was part of the flashing).
 *  Until water tiles have loaded there is no sea at all: a stand-in rectangle would paint the
 *  land under it, which is the bug this replaced. Only the sea (sea.ts isSeaWater) is drawn:
 *  lakes, reservoirs, ponds and rivers lie at their own height, mostly above 0 m, where a plane
 *  at 0 m was hidden by the terrain seen from above and showed once tilted. The style's water
 *  fill tints them in 3D instead, draped on the terrain (sea.ts drapedWater, MapView). */
export function refreshSea(m: maplibregl.Map): boolean {
  const { groups, missing } = byTile(m.querySourceFeatures('openmaptiles', { sourceLayer: 'water' }))
  if (missing && !warnedIds) {
    warnedIds = true
    console.warn(`[murmur] the sea: ${missing} water features came without their tile (_z, _x, _y): a MapLibre change? They are left out of the 3D sea surface.`)
  }
  const key = tilesKey(groups)
  if (key === seaKey) return false
  seaKey = key
  // geometry only for the water of the tiles drawn (not a child under a kept parent)
  const tiles: TileWater[] = outermost([...groups.values()]).map((t) => ({
    z: t.z,
    x: t.x,
    y: t.y,
    polygons: t.feats.flatMap((f) => polygonsOf(f.geometry)),
  }))
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
 *  and whatever is drawn after it is never tinted, whatever the sea's depth offset. Before it:
 *  the terrain and seabed (MapLibre), and the trails and stems of what is under water
 *  (underWater). After it: bridge decks (above the water by construction), the trails and stems
 *  of everything else (surface vessels at 0 m, aircraft), sea-level shadows (on the surface,
 *  which they would otherwise fight), and the screen-space overlays. A stem runs from sea level
 *  to its vehicle, so it lies wholly on one side of the surface. */

// The sea in two passes of one layer, so that each pixel is tinted once however many sea polygons
// cover it: neighbouring tiles' water overlaps by the tiles' buffers (sea.ts), and a translucent
// fill blends twice where it does. Bit 0 of the stencil marks a tinted pixel. The first pass
// clears it under the sea's outline and draws nothing: with terrain on, MapLibre renders fill and
// line layers to textures, but it still writes tile clipping masks (tile ids) into the screen's
// stencil for those of them hidden at the current zoom, so the stencil is not 0 when deck.gl draws
// (left uncleared, only about a third of the sea came out tinted). The second pass tints where the
// bit is clear and sets it, so a second polygon over the same pixel fails. Only bit 0, because
// luma.gl 9.4's WebGL render pass forces the stencil write mask to 1 before each draw (a wider
// mask would work the same); its stencil reference is always 0, and a compare of 'always' turns
// the test off, so the first pass uses not-equal (sea.check.ts pins these three). Each pass is its
// own model with fixed parameters, both drawn from the layer's one tessellation, back to back in
// one draw() with nothing between them, and luma.gl pushes and pops the GL state around each model
// draw. Nothing of MapLibre's draws after the sea: in 3D the deck.gl layers have no beforeId, so
// they share the one group deck.gl keeps as the style's last layer (it moves it back to the end
// whenever its layers are set; MapView warns in a dev build if a frame is ever drawn otherwise).
// MapLibre clears the stencil at the start of every frame.
const SEA_CLEAR = {
  // everywhere under the sea's outline, hidden or not, and no colour (blending keeps dst)
  depthCompare: 'always',
  depthWriteEnabled: false,
  blend: true,
  blendColorOperation: 'add',
  blendColorSrcFactor: 'zero',
  blendColorDstFactor: 'one',
  blendAlphaOperation: 'add',
  blendAlphaSrcFactor: 'zero',
  blendAlphaDstFactor: 'one',
  stencilReadMask: 1,
  stencilWriteMask: 1,
  stencilCompare: 'not-equal',
  stencilPassOperation: 'zero',
  stencilFailOperation: 'keep',
  stencilDepthFailOperation: 'zero',
} as const
const seaTint = () =>
  ({
    depthWriteEnabled: false,
    ...polygonOffset(seaOffset.slope, seaOffset.bias),
    // tint where bit 0 is clear, and set it
    stencilReadMask: 1,
    stencilWriteMask: 1,
    stencilCompare: 'equal',
    stencilPassOperation: 'invert',
    stencilFailOperation: 'keep',
    stencilDepthFailOperation: 'keep',
  }) as const

type ModelLike = {
  setParameters(p: object): void
  setVertexCount(n: number): void
  shaderInputs: { setProps(p: object): void }
  draw(renderPass: unknown): boolean
}

/** Keeps a model's parameters its pass's whatever deck.gl hands it: deck.gl sets the layer's
 *  parameters on every model before each draw, and luma.gl rebuilds a model's pipeline when its
 *  parameters change (taking a new reference to the cached one each time, never releasing the
 *  old). Merged, they come out equal every frame and nothing is rebuilt. */
function pinPass(model: ModelLike, pass: () => object) {
  const set = model.setParameters.bind(model)
  model.setParameters = (p) => set({ ...p, ...pass() })
  model.setParameters({})
}

/** One SolidPolygonLayer, one tessellation, two top models: the stencil-clearing pass and the
 *  tint (see above), each with its parameters fixed. deck.gl gives the attribute buffers to every
 *  model the layer returns, so both draw the same triangles. Also times deck.gl's tessellation of
 *  new data for ?perf. */
class SeaLayer extends SolidPolygonLayer<SeaPolygon> {
  static layerName = 'SeaLayer'
  private tessStart = 0

  protected _getModels() {
    const clear = super._getModels()
    // a second top model: the same shaders and buffer layout
    const tint = super._getModels().topModel as ModelLike
    pinPass(clear.topModel as ModelLike, () => SEA_CLEAR)
    pinPass(tint, seaTint)
    return { ...clear, models: [...clear.models, tint], tintModel: tint }
  }

  draw(opts: { uniforms: unknown }) {
    // the clearing pass (SolidPolygonLayer draws its top model), then at once the tint
    super.draw(opts)
    const { tintModel, polygonTesselator } = this.state as { tintModel?: ModelLike; polygonTesselator: { vertexCount: number } }
    if (!tintModel || !this.props.filled) return
    tintModel.setVertexCount(polygonTesselator.vertexCount)
    tintModel.shaderInputs.setProps({ solidPolygon: { extruded: false, elevationScale: this.props.elevationScale, isWireframe: false } })
    tintModel.draw(this.context.renderPass)
  }

  updateState(params: UpdateParameters<this>) {
    if (params.changeFlags.dataChanged) this.tessStart = performance.now()
    super.updateState(params)
  }

  protected _updateAttributes() {
    super._updateAttributes()
    if (this.tessStart) {
      perfSeaTess(performance.now() - this.tessStart)
      this.tessStart = 0
    }
  }
}

/** Trails, sea surface and bridge decks: nothing here moves with a glide (display smoothing,
 *  drawn.ts), so MapView builds them when the data changes and reuses them between frames.
 *  `under` goes before the stems under water and the sea, `surface` after them, `over` after
 *  that. While 2D is shown the layers stay (hidden): the trails' data is then empty, and the sea
 *  keeps the polygons of the last 3D view (refreshSea runs only in 3D), so it is not tessellated
 *  again on the way back. */
export function build3dStatic(vehicles: Vehicle[], visible = true, bridges: LiftedBridge[] = []) {
  const placed = visible ? vehicles.filter((v) => v.position && v.alt_msl != null) : []
  const at = (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  // with smoothing on, trailLayers.ts draws the trails (they end at the drawn position)
  const trailed = smoothing() ? [] : placed.filter((v) => v.trail.length > 1)
  const trails = (id: string, data: Vehicle[]) =>
    new PathLayer<Vehicle>({
      id,
      visible,
      data,
      getPath: (v) => [...v.trail.map(([lon, lat, alt]): XYZ => [lon, lat, z(alt)]), at(v)],
      getColor: (v) => colour(v, 140),
      getWidth: 2,
      widthUnits: 'pixels',
      updateTriggers: { getPath: data.map((v) => v.trail.length).join() },
    })
  return {
    under: [trails('trails-3d-under', trailed.filter(underWater))],
    surface: [
      new SeaLayer({
        id: 'sea-surface',
        visible,
        // the basemap's sea, at 0 m (refreshSea); the same array until the sea's tiles change
        data: seaData,
        getPolygon: (d) => d.polygon,
        positionFormat: 'XY',
        getFillColor: SEA_TINT,
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
    over: [trails('trails-3d', trailed.filter((v) => !underWater(v)))],
  }
}

// the vehicles placed in 3D, kept while the vehicles are the same (animation frames of a glide)
let memo: { vehicles: Vehicle[]; visible: boolean; placed: Vehicle[]; under: Vehicle[]; over: Vehicle[] } | null = null

/** Stems and sea-level shadows: they follow a vehicle's drawn position (display smoothing,
 *  drawn.ts), so MapView rebuilds just these on an animation frame. The stems of what is under
 *  water go before the sea surface (seen through it), the other stems and the shadows after it. */
export function build3dGlide(vehicles: Vehicle[], visible = true) {
  if (!memo || memo.vehicles !== vehicles || memo.visible !== visible) {
    const placed = visible ? vehicles.filter((v) => v.position && v.alt_msl != null) : []
    memo = { vehicles, visible, placed, under: placed.filter(underWater), over: placed.filter((v) => !underWater(v)) }
  }
  const { placed, under, over } = memo
  const smooth = smoothing()
  const at = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], z(v.alt_msl)]
  const ground = smooth
    ? (v: Vehicle, info?: { target?: number[] }) => drawnXYZ(v, info?.target, 0) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], 0]
  const triggers = smooth ? { getSourcePosition: drawnTick(), getTargetPosition: drawnTick(), getPosition: drawnTick() } : {}
  // stem from sea level to the vehicle: shows height (or depth) at a glance
  const stems = (id: string, data: Vehicle[]) =>
    new LineLayer<Vehicle>({
      id,
      visible,
      data,
      getSourcePosition: ground,
      getTargetPosition: at,
      getColor: (v) => colour(v, 160),
      getWidth: 1.5,
      widthUnits: 'pixels',
      updateTriggers: triggers,
    })
  return {
    stemsUnder: stems('stems-under', under),
    stems: stems('stems', over),
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
