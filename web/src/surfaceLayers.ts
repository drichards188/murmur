/// <reference types="geojson" />
// What the operator draws and orders on the surface (murmur-1e4): the taskings' orbits and patrol
// routes (dashed until a vehicle is seen flying them), the draft being drawn, the inbox's
// previews of pending intents, and the space segment's drawn box, selected scene and tasked
// collections. Drawn by deck.gl at the surface: the water's over the sea (sea level, where our
// boats are drawn and where a click there lands, MapView), the ground's over land, sampled along
// each line so a route follows the hills; over the scene, so neither the terrain nor the seabed
// hides them. MapLibre would drape them on the terrain, which under water is the seabed (the
// elevation tiles include it): in a tilted view an orbit drawn there sits up to a few hundred
// metres from the boats circling it, the Gate's channel being 100 m deep and heights doubled.
import { PathLayer, PolygonLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { circle, distanceM } from './geo'
import { topLeft } from './spaceLayers'
import type { LngLat, Tasking } from './types'

type FC = GeoJSON.FeatureCollection
type RGBA = [number, number, number, number]
type XYZ = [number, number, number]

/** The surface's height at a point, rendered metres (the terrain's exaggeration applied): 0 over
 *  the water, the ground over land. */
export type SurfaceZ = (p: LngLat) => number

/** Where to draw: the surface's height, and metres per screen pixel at the view's centre
 *  (dashes are a few pixels long, as MapLibre's line-dasharray). */
export interface SurfaceScale {
  surfaceZ: SurfaceZ
  mPerPx: number
}

/** A drafted orbit or patrol (MapView's Draft) */
export interface DraftShape {
  mode: 'orbit' | 'patrol' | null
  center: LngLat | null
  radius: number
  route: LngLat[]
}

export interface SurfaceInput {
  taskings: Tasking[]
  draft: DraftShape
  /** the space segment's: the box being drawn, the selected scene, the tasked collections */
  box: LngLat[] | null
  scene: { ring: LngLat[]; label: string } | null
  tasked: { ring: LngLat[]; label: string }[]
}

const ORBIT: RGBA = [255, 179, 71, 255]
const PATROL: RGBA = [196, 155, 255, 255]
const OTHER: RGBA = [215, 221, 228, 255]
const DRAFT: RGBA = [244, 241, 222, 255]
const PREVIEW: RGBA = [196, 155, 255, 255]
const SPACE: RGBA = [244, 241, 222, 255]
const SCENE: RGBA = [255, 179, 71, 255]
const DARK: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }
// over the scene: a drawing on the surface is never hidden by the terrain or the seabed
const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }

function a(c: RGBA, k: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * k)]
}

/** Points at most this far apart along a lifted line, metres: it follows the ground over land. */
const STEP_M = 40

/** A path on the surface: its points, and more between them where they are far apart, each at
 *  the surface's height. */
export function lift(path: LngLat[], zOf: SurfaceZ): XYZ[] {
  const out: XYZ[] = []
  for (let k = 0; k < path.length; k++) {
    const b = path[k]
    if (k > 0) {
      const p0 = path[k - 1]
      const n = Math.min(200, Math.floor(distanceM(p0, b) / STEP_M))
      for (let j = 1; j < n; j++) {
        const f = j / n
        const p: LngLat = [p0[0] + (b[0] - p0[0]) * f, p0[1] + (b[1] - p0[1]) * f]
        out.push([p[0], p[1], zOf(p)])
      }
    }
    out.push([b[0], b[1], zOf(b)])
  }
  return out
}

/** A lifted path cut into dashes `dashM` long with `gapM` between them (deck.gl's PathLayer draws
 *  solid lines without the extensions package). */
export function dashes(path: XYZ[], dashM: number, gapM: number): XYZ[][] {
  const out: XYZ[][] = []
  if (path.length < 2 || dashM <= 0) return path.length < 2 ? out : [path]
  let cur: XYZ[] = [path[0]]
  let draw = true
  let left = dashM
  for (let k = 1; k < path.length; k++) {
    let p0 = path[k - 1]
    const p1 = path[k]
    let d = distanceM([p0[0], p0[1]], [p1[0], p1[1]])
    while (d > left) {
      const f = left / d
      const m: XYZ = [p0[0] + (p1[0] - p0[0]) * f, p0[1] + (p1[1] - p0[1]) * f, p0[2] + (p1[2] - p0[2]) * f]
      if (draw) {
        cur.push(m)
        out.push(cur)
      }
      cur = [m]
      draw = !draw
      d -= left
      p0 = m
      left = draw ? dashM : gapM
    }
    left -= d
    if (draw) cur.push(p1)
  }
  if (draw && cur.length > 1) out.push(cur)
  return out
}

interface Line {
  path: XYZ[]
  color: RGBA
  width: number
}

interface Fill {
  ring: XYZ[]
  color: RGBA
}

interface Mark {
  at: XYZ
  color: RGBA
  radius: number
  stroke: number
  text?: string
}

interface Label {
  at: XYZ
  text: string
  color: RGBA
}

/** What one builder draws, and how it adds to it. */
function drawing({ surfaceZ: zOf, mPerPx }: SurfaceScale) {
  const d = { lines: [] as Line[], fills: [] as Fill[], marks: [] as Mark[], labels: [] as Label[] }
  const at = (p: LngLat): XYZ => [p[0], p[1], zOf(p)]
  /** a line, dashed `[dash, gap]` in line widths (as MapLibre's line-dasharray) */
  const line = (path: LngLat[], color: RGBA, width: number, dash?: [number, number]) => {
    if (path.length < 2) return
    const lifted = lift(path, zOf)
    if (!dash) return void d.lines.push({ path: lifted, color, width })
    // a dash at least 1.5 px, or a dotted line vanishes
    for (const p of dashes(lifted, Math.max(1.5, dash[0] * width) * mPerPx, dash[1] * width * mPerPx)) d.lines.push({ path: p, color, width })
  }
  return { d, at, line, lifted: (ring: LngLat[]) => lift(ring, zOf) }
}

function layersOf(prefix: string, { lines, fills, marks, labels }: ReturnType<typeof drawing>['d']) {
  return [
    new PolygonLayer<Fill>({
      id: `${prefix}-fills`,
      data: fills,
      getPolygon: (f) => f.ring,
      stroked: false,
      filled: true,
      getFillColor: (f) => f.color,
      parameters: onTop,
    }),
    new PathLayer<Line>({
      id: `${prefix}-lines`,
      data: lines,
      getPath: (l) => l.path,
      getColor: (l) => l.color,
      getWidth: (l) => l.width,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<Mark>({
      id: `${prefix}-marks`,
      data: marks,
      getPosition: (m) => m.at,
      getRadius: (m) => m.radius,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: true,
      getFillColor: (m) => m.color,
      getLineColor: DARK,
      getLineWidth: (m) => m.stroke,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new TextLayer<Mark>({
      id: `${prefix}-numbers`,
      data: marks.filter((m) => m.text),
      getPosition: (m) => m.at,
      getText: (m) => m.text!,
      getSize: 11,
      getColor: DARK,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'center',
      fontFamily: FONT,
      fontWeight: 700,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
    new TextLayer<Label>({
      id: `${prefix}-labels`,
      data: labels,
      getPosition: (l) => l.at,
      getText: (l) => l.text,
      getSize: 11,
      getColor: (l) => l.color,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'bottom',
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: DARK,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}

/** The taskings, the draft and the space segment's drawings, under the vehicles and tracks. */
export function buildSurfaceLayers(input: SurfaceInput, scale: SurfaceScale) {
  const { d, at, line, lifted } = drawing(scale)

  // the space segment's: the selected scene (filled faintly), the tasked collections still
  // running, the box being drawn
  for (const t of input.tasked) {
    line([...t.ring, t.ring[0]], PATROL, 2, [4, 2])
    d.labels.push({ at: at(topLeft(t.ring)), text: t.label, color: PATROL })
  }
  if (input.scene) {
    const ring = [...input.scene.ring, input.scene.ring[0]]
    d.fills.push({ ring: lifted(ring), color: a(SCENE, 0.12) })
    line(ring, SCENE, 2)
    d.labels.push({ at: at(topLeft(input.scene.ring)), text: input.scene.label, color: SCENE })
  }
  if (input.box) line([...input.box, input.box[0]], SPACE, 1.5, [2, 2])

  // taskings: dashed until a vehicle's own telemetry shows it executing (an ACK says it accepted
  // the order, not that it flies it)
  for (const t of input.taskings) {
    const live = t.ops.filter((o) => o.state !== 'superseded' && o.state !== 'failed' && o.state !== 'requeued')
    if (live.length === 0) continue
    const confirmed = live.some((o) => ['active', 'done'].includes(o.state))
    const color = t.task === 'orbit' ? ORBIT : t.task === 'patrol' ? PATROL : OTHER
    const lineColor = a(color, confirmed ? 0.75 : 0.9)
    const dash: [number, number] | undefined = confirmed ? undefined : [2, 2]
    const pts: LngLat[] = []
    if (t.task === 'orbit' && t.params.center) {
      line(circle(t.params.center, t.params.radius ?? 200, 72), lineColor, 2, dash)
      pts.push(t.params.center)
    } else if (t.task === 'patrol' && t.params.route) {
      const r = t.params.route
      line([...r, r[0]], lineColor, 2, dash)
      pts.push(...r)
    }
    for (const p of pts) d.marks.push({ at: at(p), color, radius: 4, stroke: 1.5 })
  }

  // the draft: its line, and its points numbered (an orbit's centre unnumbered)
  const dr = input.draft
  if (dr.mode === 'orbit' && dr.center) {
    line(circle(dr.center, dr.radius), DRAFT, 2, [1, 1.5])
    d.marks.push({ at: at(dr.center), color: DRAFT, radius: 9, stroke: 2 })
  }
  if (dr.mode === 'patrol' && dr.route.length) {
    line(dr.route.length > 2 ? [...dr.route, dr.route[0]] : dr.route, DRAFT, 2, [1, 1.5])
    dr.route.forEach((p, i) => d.marks.push({ at: at(p), color: DRAFT, radius: 9, stroke: 2, text: String(i + 1) }))
  }
  return layersOf('surface', d)
}

/** The inbox's previews (inboxPreview.ts): where pending intents would send their vehicles, the
 *  one being read in bold. Lines `part` transit (from the vehicle) or path, and points. */
export function buildPreviewLayers(preview: FC, scale: SurfaceScale) {
  const { d, at, line } = drawing(scale)
  for (const f of preview.features) {
    const sel = !!f.properties?.sel
    const g = f.geometry
    if (g.type === 'LineString') {
      const transit = f.properties?.part === 'transit'
      line(g.coordinates as LngLat[], a(PREVIEW, transit ? 0.8 : 0.95), sel ? 3 : 1.5, transit ? [0.5, 2] : [3, 2])
    } else if (g.type === 'Point') d.marks.push({ at: at(g.coordinates as LngLat), color: PREVIEW, radius: sel ? 7 : 4, stroke: 1.5 })
  }
  return layersOf('inbox-preview', d)
}
