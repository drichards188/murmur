// An order's predicted link on the map (coverage.ts routeDrawing): its legs coloured up, degraded
// and down, the return leg (the way home, flown only at the end or by the failsafe) dashed, and
// marks where the link first goes down on the way and where the failsafe trips. Rules 22: the
// legs the decision engine's planner routed, thin and white through their waypoints (under the
// link's colours where there is a prediction), a mark saying what each added, and a mark where a
// refused order meets what refuses it. Drawn on the surface like the draft it is about
// (surfaceLayers.ts), over the scene so neither the terrain nor the seabed hides it.
import { PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { STATE_RGB, type LinkRouteDrawing, type LinkState, type RouteMark } from './coverage'
import { dashes, lift, type SurfaceScale } from './surfaceLayers'
import type { LngLat } from './types'

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]

const DARK: RGBA = [11, 15, 20, 230]
const WHITE: RGBA = [244, 241, 222, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
const rgba = (s: LinkState, a = 255): RGBA => [...STATE_RGB[s], a]

interface Line {
  path: XYZ[]
  color: RGBA
  width: number
}

interface Mark {
  at: XYZ
  mark: RouteMark
}

export function buildLinkRouteLayers(d: LinkRouteDrawing, { surfaceZ, mPerPx }: SurfaceScale) {
  const casing: Line[] = []
  const lines: Line[] = []
  for (const l of d.lines) {
    const lifted = lift(l.path, surfaceZ)
    if (l.part === 'return') {
      // the way home: dashed, thinner
      for (const p of dashes(lifted, 6 * mPerPx, 5 * mPerPx)) {
        casing.push({ path: p, color: DARK, width: 5 })
        lines.push({ path: p, color: rgba(l.state, 230), width: 2.5 })
      }
      continue
    }
    casing.push({ path: lifted, color: DARK, width: 8 })
    lines.push({ path: lifted, color: rgba(l.state), width: 4.5 })
  }
  const at = (p: LngLat): XYZ => [p[0], p[1], surfaceZ(p)]
  const marks: Mark[] = d.marks.map((m) => ({ at: at(m.at), mark: m }))
  const dots = d.dots.map((x) => ({ at: at(x.at), color: rgba(x.state) }))
  const routed = (d.routed ?? []).map((p) => lift(p, surfaceZ))
  const waypoints = (d.waypoints ?? []).map(at)
  const markFill = (m: Mark): RGBA => (m.mark.kind === 'trip' || m.mark.kind === 'refused' ? rgba('down') : WHITE)
  const markLine = (m: Mark): RGBA => (m.mark.kind === 'trip' || m.mark.kind === 'refused' ? WHITE : m.mark.kind === 'routed' ? DARK : rgba('down'))
  return [
    new PathLayer<XYZ[]>({
      id: 'link-route-planned',
      data: routed,
      getPath: (p) => p,
      getColor: WHITE,
      getWidth: 2,
      widthUnits: 'pixels',
      capRounded: true,
      jointRounded: true,
      parameters: onTop,
    }),
    new PathLayer<Line>({
      id: 'link-route-casing',
      data: casing,
      getPath: (l) => l.path,
      getColor: (l) => l.color,
      getWidth: (l) => l.width,
      widthUnits: 'pixels',
      capRounded: true,
      jointRounded: true,
      parameters: onTop,
    }),
    new PathLayer<Line>({
      id: 'link-route-lines',
      data: lines,
      getPath: (l) => l.path,
      getColor: (l) => l.color,
      getWidth: (l) => l.width,
      widthUnits: 'pixels',
      capRounded: true,
      jointRounded: true,
      parameters: onTop,
    }),
    new ScatterplotLayer<{ at: XYZ; color: RGBA }>({
      id: 'link-route-here',
      data: dots,
      getPosition: (x) => x.at,
      getRadius: 11,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: false,
      getLineColor: (x) => x.color,
      getLineWidth: 3,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<Mark>({
      id: 'link-route-marks',
      data: marks,
      getPosition: (m) => m.at,
      getRadius: (m) => (m.mark.kind === 'trip' || m.mark.kind === 'refused' ? 8 : 6),
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: true,
      getFillColor: markFill,
      getLineColor: markLine,
      getLineWidth: 2.5,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<XYZ>({
      id: 'link-route-waypoints',
      data: waypoints,
      getPosition: (x) => x,
      getRadius: 4,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: true,
      getFillColor: WHITE,
      getLineColor: DARK,
      getLineWidth: 1.5,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new TextLayer<Mark>({
      id: 'link-route-labels',
      data: marks,
      getPosition: (m) => m.at,
      getText: (m) => m.mark.text,
      getSize: 11,
      getColor: (m) => (m.mark.kind === 'trip' || m.mark.kind === 'refused' ? [255, 170, 170, 255] : WHITE),
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: [13, 0],
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: [11, 15, 20, 255],
      fontSettings: { sdf: true, fontSize: 48, buffer: 6 },
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}
