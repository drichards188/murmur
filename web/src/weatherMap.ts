// The weather layer on the tactical map (bd murmur-vr17; docs/WEATHER.md, first paragraph). By
// default every animated effect at once, each with its own toggle, and an optional colour field:
//
//  - on the surface (weatherGl.ts, one shader under the labels): the fog, a drifting haze whose
//    mean opacity is each region's visibility; the cloud shadows, covering each region's cloud
//    cover; the colour field, if one is chosen; the archive's outline. They lie on the surface
//    (sea level over the water, the ground over land) as the tracks and reach rings do.
//  - particles (deck.gl, over the surface effects and under the labels and vehicles): the wind's
//    streaks (windParticles.ts) and the rain (rainParticles.ts).
//  - the sky: with fog shown, MapLibre's sky and terrain fog follow the visibility at the view's
//    centre (from a pitch of 60 degrees), and the map's labels take a pale halo while the fog
//    there is thick (a dark halo reads as a smudge on it). Both are put back after.
//
// Everything takes each model point's region's values, blended over FADE_MS when a poll changes
// them (the sampler interpolates in time; nothing jumps). Shapes and motion are drawn to be seen:
// the fog's and clouds' shapes are a noise texture, and every drift is exaggerated, slower and
// fainter than the symbols. No frames while nothing animates, under prefers-reduced-motion (a
// still frame and static arrows instead), or while the tab is hidden.
import type * as maplibregl from 'maplibre-gl'
import { LineLayer, PathLayer } from '@deck.gl/layers'
import type { LayersList } from '@deck.gl/core'
import type { LngLat } from './types'
import type { WeatherField } from './weather'
import { perfPush } from './perf'
import { blendRegions, fieldSource, fogGroundBlend, hazeStrength, MAX_REGIONS, mixCss, valuesAt, type RGBA, type WeatherRegions } from './weatherField'
import { EffectsLayer, CLOUD_TILE_M, FOG_TILE_M, packEffects, type Effects } from './weatherGl'
import { makeNoise, noiseQuantiles } from './weatherNoise'
import { particleCount, stepWind, windArrows, windSegments, type Particle } from './windParticles'
import { rainSegments, stepRain, type Drop } from './rainParticles'

export type { Effects } from './weatherGl'

/** what App hands the map: the regions now, the effects on, and the colour field if any */
export interface WeatherView {
  regions: WeatherRegions
  effects: Effects
  field: WeatherField | null
}

/** where a drawing on the surface goes, as MapView's surfaceScale */
export interface Surface {
  surfaceZ: (p: LngLat) => number
  mPerPx: number
}

const LAYER = 'weather-effects'
/** a poll's change blends in over this long (ms) */
const FADE_MS = 1500
/** frames a second while anything animates */
const HZ = 30
/** screen pixels a second per m/s for the fog's and clouds' drift: slower than the streaks */
const FOG_PX_PER_MPS = 1.6
const CLOUD_PX_PER_MPS = 1.0
const STREAK_RGB: [number, number, number] = [214, 232, 246]
const RAIN_RGB: [number, number, number] = [176, 204, 236]
const FOG: RGBA = [214, 222, 231, 1]
const NIGHT_SKY: RGBA = [10, 19, 29, 1]
/** fog this thick at the view's centre gives the labels a pale halo */
const LABEL_FOG = 0.45
const NOISE = makeNoise()
const SORTED = noiseQuantiles(NOISE)

export class WeatherMap {
  private m: maplibregl.Map | null = null
  private before: string | undefined
  private layer: EffectsLayer | null = null
  private view: WeatherView | null = null
  private prev: WeatherRegions | null = null
  private changedAt = 0
  /** each region's drift so far: fog east, north, cloud east, north (m), index 0 outside */
  private offsets = new Float32Array((MAX_REGIONS + 1) * 4)
  /** the drift of a scripted (uniform) field's one sheet: fog east, north, cloud east, north */
  private shared = new Float32Array(4)
  private raf = 0
  private last = 0
  private particles: Particle[] = []
  private drops: Drop[] = []
  private deck: LayersList = []
  private reduced = false
  private savedSky: maplibregl.SkySpecification | undefined
  private hazing = false
  /** the labels' paint before the fog changed it, by layer */
  private savedLabels: Map<string, Record<string, unknown>> | null = null
  private readonly push: () => void
  private readonly surface: () => Surface | null

  /** `push`: hand deck.gl the layers again (MapView), `surface`: where the surface is */
  constructor(push: () => void, surface: () => Surface | null) {
    this.push = push
    this.surface = surface
  }

  private onMotion = (e: MediaQueryListEvent) => {
    this.reduced = e.matches
    this.particles = []
    this.drops = []
    this.sync()
  }
  private onVisibility = () => this.sync()
  private onMoveEnd = () => {
    // a still view's rain is spread again for the new view
    if (this.reduced) this.drops = []
    this.mesh()
    this.sync()
  }
  private motionQuery(): MediaQueryList | null {
    try {
      return window.matchMedia('(prefers-reduced-motion: reduce)')
    } catch {
      return null
    }
  }

  /** Adds the surface effects under `before` (the first label layer) on a loaded style. */
  attach(m: maplibregl.Map, before: string | undefined) {
    if (this.m) this.detach()
    const q = this.motionQuery()
    this.reduced = q?.matches ?? false
    q?.addEventListener('change', this.onMotion)
    document.addEventListener('visibilitychange', this.onVisibility)
    this.m = m
    this.before = before
    this.layer = new EffectsLayer(LAYER, NOISE)
    m.addLayer(this.layer, before)
    this.savedSky = m.getSky()
    this.hazing = false
    this.savedLabels = null
    m.on('moveend', this.onMoveEnd)
    this.mesh()
    this.sync()
  }

  /** the map is going away (context loss, a rebuild) */
  detach() {
    this.motionQuery()?.removeEventListener('change', this.onMotion)
    document.removeEventListener('visibilitychange', this.onVisibility)
    this.stop()
    this.m?.off('moveend', this.onMoveEnd)
    this.deck = []
    this.particles = []
    this.drops = []
    this.layer = null
    this.m = null
  }

  /** new regions, other effects, or none (the layer off, or the Links layer shown) */
  update(view: WeatherView | null) {
    const now = performance.now()
    this.prev = this.view && view ? this.blended(now) : null
    this.changedAt = view && this.prev ? now : 0
    if (!view?.effects.wind) this.particles = []
    if (!view?.effects.rain) this.drops = []
    this.view = view
    // the terrain may have come in since: the mesh follows it
    this.mesh()
    this.sync()
  }

  /** the deck.gl layers: streaks and rain (or the still versions) */
  layers(): LayersList {
    return this.view && this.m ? this.deck : []
  }

  private blended(now: number): WeatherRegions | null {
    if (!this.view) return null
    return blendRegions(this.prev, this.view.regions, this.changedAt ? (now - this.changedAt) / FADE_MS : 1)
  }

  private animates(): boolean {
    const v = this.view
    if (!v || !this.m || this.reduced || document.visibilityState !== 'visible') return false
    const fx = v.effects
    return fx.wind || fx.fog || fx.rain || fx.cloud || (this.changedAt > 0 && performance.now() - this.changedAt < FADE_MS)
  }

  private stop() {
    if (this.raf) cancelAnimationFrame(this.raf)
    this.raf = 0
  }

  /** Draws the current state once, and runs frames if anything animates. */
  private sync() {
    if (!this.m) return
    this.frame(performance.now(), 0)
    this.syncSky()
    this.syncLabels()
    if (this.animates()) {
      if (!this.raf) {
        this.last = performance.now()
        this.raf = requestAnimationFrame(this.tick)
      }
    } else this.stop()
  }

  private tick = (t: number) => {
    this.raf = 0
    if (!this.animates()) return
    this.raf = requestAnimationFrame(this.tick)
    if (t - this.last < 1000 / HZ - 2) return
    const dt = Math.min(0.1, (t - this.last) / 1000)
    this.last = t
    this.frame(t, dt)
  }

  /** The view's bounds with a margin, for the mesh. */
  private mesh() {
    const m = this.m, s = this.surface()
    if (!m || !this.layer) return
    const b = m.getBounds()
    const k = m.getPitch() < 30 ? 0.25 : 0.1
    const dx = (b.getEast() - b.getWest()) * k, dy = (b.getNorth() - b.getSouth()) * k
    this.layer.setMesh([Math.max(-180, b.getWest() - dx), Math.max(-85, b.getSouth() - dy), Math.min(180, b.getEast() + dx), Math.min(85, b.getNorth() + dy)],
      s ? (lon, lat) => s.surfaceZ([lon, lat]) : () => 0)
  }

  /** One frame: drift, pack, repaint the surface effects, step and hand over the particles.
   *  `dt` 0: draw as things are (a still frame, or the first). */
  private frame(now: number, dt: number) {
    const m = this.m, layer = this.layer, view = this.view, s = this.surface()
    if (!m || !layer) return
    const t0 = performance.now()
    const regions = this.blended(now)
    if (!view || !regions || !s) {
      layer.state = { packed: null, on: [0, 0, 0, 0], bounds: null }
      m.triggerRepaint()
      if (this.deck.length) {
        this.deck = []
        this.push()
      }
      return
    }
    const fx = view.effects
    // each region's shapes drift with its own wind
    if (dt > 0) {
      const values = [regions.outside, ...regions.points.slice(0, MAX_REGIONS).map((p) => p.values)]
      values.forEach((v, k) => {
        const e = v.wind_east_mps ?? 0, n = v.wind_north_mps ?? 0
        const fog = FOG_PX_PER_MPS * s.mPerPx * dt, cloud = CLOUD_PX_PER_MPS * s.mPerPx * dt
        const o = this.offsets
        o[4 * k] = (o[4 * k] - e * fog) % FOG_TILE_M
        o[4 * k + 1] = (o[4 * k + 1] - n * fog) % FOG_TILE_M
        o[4 * k + 2] = (o[4 * k + 2] - e * cloud) % CLOUD_TILE_M
        o[4 * k + 3] = (o[4 * k + 3] - n * cloud) % CLOUD_TILE_M
      })
      // a scripted field is one sheet (the same everywhere): it drifts as one, with the wind at
      // the view's centre, and shows no seam where two regions' winds differ
      const c = m.getCenter()
      const w = valuesAt(regions, c.lng, c.lat)
      const e = w.wind_east_mps ?? 0, n = w.wind_north_mps ?? 0
      const sh = this.shared
      sh[0] = (sh[0] - e * FOG_PX_PER_MPS * s.mPerPx * dt) % FOG_TILE_M
      sh[1] = (sh[1] - n * FOG_PX_PER_MPS * s.mPerPx * dt) % FOG_TILE_M
      sh[2] = (sh[2] - e * CLOUD_PX_PER_MPS * s.mPerPx * dt) % CLOUD_TILE_M
      sh[3] = (sh[3] - n * CLOUD_PX_PER_MPS * s.mPerPx * dt) % CLOUD_TILE_M
    }
    let offsets = this.offsets
    const fogSheet = fieldSource(regions, 'visibility') === 'scripted', cloudSheet = fieldSource(regions, 'cloud') === 'scripted'
    if (fogSheet || cloudSheet) {
      offsets = Float32Array.from(this.offsets)
      for (let k = 0; k <= MAX_REGIONS; k++) {
        if (fogSheet) offsets.set(this.shared.subarray(0, 2), 4 * k)
        if (cloudSheet) offsets.set(this.shared.subarray(2, 4), 4 * k + 2)
      }
    }
    layer.state = {
      packed: packEffects(regions, view.field, layer.origin, offsets, SORTED),
      on: [view.field ? 1 : 0, fx.fog ? 1 : 0, fx.cloud ? 1 : 0, fx.wind || fx.fog || fx.rain || fx.cloud || view.field ? 1 : 0],
      bounds: regions.bounds,
    }
    m.triggerRepaint()
    this.particlesFrame(m, s, regions, dt)
    perfPush('weatherFrame', performance.now() - t0)
  }

  private particlesFrame(m: maplibregl.Map, s: Surface, regions: WeatherRegions, dt: number) {
    const fx = this.view!.effects
    const b = m.getBounds()
    const w = b.getWest(), e = b.getEast(), so = b.getSouth(), n = b.getNorth()
    const inView = (lon: number, lat: number) => lon >= w && lon <= e && lat >= so && lat <= n
    const rect = m.getCanvas().getBoundingClientRect()
    const tr = m.transform as unknown as { elevation: number; screenPointToMercatorCoordinateAtZ?: (p: { x: number; y: number }, z: number) => { toLngLat(): { lng: number; lat: number } } }
    // spread over the screen, not the ground (a tilted view's far side would crowd otherwise),
    // unprojected onto sea level by the flat camera: map.unproject reads the terrain's depth
    // back from the GPU, a stall per call
    const spawn = (): LngLat | null => {
      const p = { x: Math.random() * rect.width, y: Math.random() * rect.height } as unknown as maplibregl.Point
      const at = tr.screenPointToMercatorCoordinateAtZ?.(p, -tr.elevation)?.toLngLat()
      return at && Number.isFinite(at.lng) && Number.isFinite(at.lat) ? [at.lng, at.lat] : null
    }
    const zOf = (lon: number, lat: number) => s.surfaceZ([lon, lat])
    const still = this.reduced || dt === 0
    const layers: LayersList = []
    const common = { beforeId: this.before, parameters: { depthCompare: 'always' as const, depthWriteEnabled: false } }
    if (fx.wind && this.reduced) {
      const c = m.getCenter()
      const paths = windArrows(regions, inView, s.mPerPx, [c.lng, c.lat]).map((p) => p.map(([lon, lat]) => [lon, lat, zOf(lon, lat)]))
      const arrows = { data: paths, getPath: (p: number[][]) => p, widthUnits: 'pixels' as const, capRounded: true, jointRounded: true, ...common }
      layers.push(
        new PathLayer<number[][]>({ id: 'weather-wind-casing', ...arrows, getColor: [19, 39, 58, 170], getWidth: 4 } as never),
        new PathLayer<number[][]>({ id: 'weather-wind-arrows', ...arrows, getColor: [216, 236, 250, 200], getWidth: 1.8 } as never),
      )
    } else if (fx.wind && !still) {
      this.particles = stepWind(this.particles, dt, { regions, mPerPx: s.mPerPx, count: particleCount(rect.width, rect.height), spawn, inView, zOf, rng: Math.random })
    }
    if (fx.wind && !this.reduced && this.particles.length) {
      const seg = windSegments(this.particles, STREAK_RGB, 0.6)
      layers.push(segmentsLayer('weather-wind', seg.length, seg.from, seg.to, (i, out) => {
        out[0] = seg.color[4 * i]; out[1] = seg.color[4 * i + 1]; out[2] = seg.color[4 * i + 2]; out[3] = seg.color[4 * i + 3]
      }, 1.4, common))
    }
    if (fx.rain) {
      const step = { regions, mPerPx: s.mPerPx, max: Math.min(700, particleCount(rect.width, rect.height) * 1.5), spawn, inView, zOf, rng: Math.random }
      if (!still) this.drops = stepRain(this.drops, dt, step)
      // still: one frozen spread of drops (made once per sync)
      else if (!this.drops.length) {
        let ds: Drop[] = []
        for (let i = 0; i < 12; i++) ds = stepRain(ds, 0.1, step)
        this.drops = ds
      }
      if (this.drops.length) {
        const seg = rainSegments(this.drops, s.mPerPx)
        layers.push(segmentsLayer('weather-rain', seg.length, seg.from, seg.to, (_, out) => {
          out[0] = RAIN_RGB[0]; out[1] = RAIN_RGB[1]; out[2] = RAIN_RGB[2]; out[3] = 120
        }, 1.2, common))
      }
    }
    if (layers.length || this.deck.length) {
      this.deck = layers
      this.push()
    }
  }

  // ---- the haze and the labels -------------------------------------------------------------------

  /** how foggy the view's centre is (0 to 1), with the fog shown */
  private centreFog(): { strength: number; visibility: number } {
    const m = this.m, v = this.view
    if (!m || !v?.effects.fog) return { strength: 0, visibility: 20_000 }
    const c = m.getCenter()
    const vis = valuesAt(v.regions, c.lng, c.lat).visibility_m
    return { strength: hazeStrength(vis), visibility: vis ?? 20_000 }
  }

  private syncSky() {
    const m = this.m
    if (!m) return
    const { strength, visibility } = this.centreFog()
    if (strength < 0.02) {
      if (this.hazing) {
        m.setSky(this.savedSky as maplibregl.SkySpecification)
        this.hazing = false
      }
      return
    }
    const s = this.surface()
    const tr = m.transform as unknown as { cameraToCenterDistance?: number }
    m.setSky({
      'sky-color': mixCss(NIGHT_SKY, FOG, strength * 0.8),
      'horizon-color': mixCss(NIGHT_SKY, FOG, strength),
      'fog-color': mixCss(NIGHT_SKY, FOG, 0.35 + 0.65 * strength),
      'fog-ground-blend': fogGroundBlend(visibility, tr.cameraToCenterDistance ?? 0, s ? 1 / s.mPerPx : 0),
      'horizon-fog-blend': 0.3,
      'sky-horizon-blend': 0.5 + 0.5 * strength,
      'atmosphere-blend': 0,
    })
    this.hazing = true
  }

  /** In thick fog the style's labels (light text, dark halo) read as dark smudges on the pale
   *  haze: give them dark text on a pale halo while it lasts, and their own paint back after. */
  private syncLabels() {
    const m = this.m
    if (!m) return
    const foggy = this.centreFog().strength >= LABEL_FOG
    if (foggy && !this.savedLabels) {
      const saved = new Map<string, Record<string, unknown>>()
      for (const l of m.getStyle().layers) {
        if (l.type !== 'symbol' || !('layout' in l) || !l.layout || !('text-field' in l.layout)) continue
        saved.set(l.id, { 'text-color': m.getPaintProperty(l.id, 'text-color'), 'text-halo-color': m.getPaintProperty(l.id, 'text-halo-color'), 'text-halo-width': m.getPaintProperty(l.id, 'text-halo-width') })
        m.setPaintProperty(l.id, 'text-color', '#1e2a36')
        m.setPaintProperty(l.id, 'text-halo-color', 'rgba(228,234,241,0.85)')
        m.setPaintProperty(l.id, 'text-halo-width', 1.2)
      }
      this.savedLabels = saved
    } else if (!foggy && this.savedLabels) {
      for (const [id, paint] of this.savedLabels) {
        if (!m.getLayer(id)) continue
        for (const [k, v] of Object.entries(paint)) m.setPaintProperty(id, k, v as never)
      }
      this.savedLabels = null
    }
  }
}

/** Line segments from flat arrays (lon, lat, z for each end), coloured by `color(i, out)`. */
function segmentsLayer(id: string, n: number, from: Float64Array, to: Float64Array, color: (i: number, out: number[]) => void, width: number, common: object) {
  const tick = performance.now()
  return new LineLayer<number>({
    id,
    data: { length: n } as unknown as number[],
    getSourcePosition: (_: number, { index, target }: { index: number; target: number[] }) => {
      target[0] = from[3 * index]; target[1] = from[3 * index + 1]; target[2] = from[3 * index + 2]
      return target as unknown as [number, number, number]
    },
    getTargetPosition: (_: number, { index, target }: { index: number; target: number[] }) => {
      target[0] = to[3 * index]; target[1] = to[3 * index + 1]; target[2] = to[3 * index + 2]
      return target as unknown as [number, number, number]
    },
    getColor: (_: number, { index, target }: { index: number; target: number[] }) => {
      color(index, target)
      return target as unknown as [number, number, number, number]
    },
    getWidth: width,
    widthUnits: 'pixels',
    updateTriggers: { getSourcePosition: tick, getTargetPosition: tick, getColor: tick },
    ...common,
  } as never)
}
