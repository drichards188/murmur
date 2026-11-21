// An order's predicted link on the map (coverage.ts routeDrawing): its legs coloured up, degraded
// and down, the return leg (the way home, flown only at the end or by the failsafe) dashed, and
// marks where the link first goes down on the way and where the failsafe trips. Rules 22: the
// legs the decision engine's planner routed, thin and white through their waypoints (under the
// link's colours where there is a prediction), a mark saying what each added, and a mark where a
// refused order meets what refuses it. Drawn on the surface like the draft it is about
// (surfaceLayers.ts), over the scene so neither the terrain nor the seabed hides it.
import { IconLayer, PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { STATE_RGB, type GroundStation, type LinkRouteDrawing, type LinkState, type RouteMark } from './coverage'
import { layerColor, type ExcludedMark, type SlotMark } from './groupOrders'
import { HEIGHT_EXAGGERATION } from './map3d'
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

/** The ground stations on the Links layer (the coverage grid's `sources.ground_stations`): a ring
 *  and its name at each radio site, so the coverage reads against where it is judged from. With
 *  several, a radio link goes through whichever gives it the best path. */
export function buildStationLayers(stations: GroundStation[]) {
  if (!stations.length) return []
  const at = (s: GroundStation): [number, number] => [s.at[0], s.at[1]]
  return [
    new ScatterplotLayer<GroundStation>({
      id: 'link-stations',
      data: stations,
      getPosition: at,
      getRadius: 7,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: true,
      getFillColor: DARK,
      getLineColor: WHITE,
      getLineWidth: 2.5,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new TextLayer<GroundStation>({
      id: 'link-station-names',
      data: stations,
      getPosition: at,
      getText: (s) => s.name,
      getSize: 12,
      getColor: WHITE,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: [12, 0],
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

// a slot's ghost: a white ring the IconLayer tints (mask), with a faint fill for a member the
// order sends now, dashed and open for one that joins if heard in time
const ghostSvg = (waiting: boolean) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48"><circle cx="24" cy="24" r="20" fill="#fff" fill-opacity="${waiting ? 0 : 0.35}" stroke="#fff" stroke-width="4.5"${waiting ? ' stroke-dasharray="7 5"' : ''}/></svg>`
const ghostIcon = (waiting: boolean) => ({ id: waiting ? 'slot-waiting' : 'slot-sent', url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(ghostSvg(waiting))}`, width: 48, height: 48, anchorX: 24, anchorY: 24, mask: true })
const GHOST_SENT = ghostIcon(false)
const GHOST_WAITING = ghostIcon(true)
const EXCLUDED: RGBA = [92, 102, 114, 235]

/** Rules 34: a group order's layout from the dry run, before it is sent (bd murmur-4nik): each
 *  member's slot a ghost at its planned height (its layer), in the group's colour when the members
 *  are one group's (else its layer's), faint and filled for a member sent now, dashed and open for
 *  one that waits to be heard, a stem down to the surface and a thin dashed line there from where
 *  the member is to its slot (through its own route's points, a block's transit); each member left
 *  out a grey cross where it is, its reason on hover; for a small group, names and altitudes. Gone
 *  once the order is sent or the form closed (the dry run with it). */
function slotLayers(d: LinkRouteDrawing, { surfaceZ, mPerPx }: SurfaceScale) {
  const slots = d.slots ?? []
  const excluded = d.excluded ?? []
  if (!slots.length && !excluded.length) return []
  const color = (s: SlotMark, a: number): RGBA => [...(d.slotColor ?? layerColor(s.layer)).slice(0, 3), a] as RGBA
  const ghostAt = (s: SlotMark): XYZ => [s.at[0], s.at[1], s.alt_m == null ? surfaceZ(s.at) : s.alt_m * HEIGHT_EXAGGERATION]
  const ways: Line[] = []
  for (const s of slots) {
    if (!s.from) continue
    for (const p of dashes(lift([s.from, ...s.via, s.at], surfaceZ), 4 * mPerPx, 4 * mPerPx)) ways.push({ path: p, color: color(s, 140), width: 1.5 })
  }
  const pts = slots.map((s) => ({ s, at: ghostAt(s), foot: [s.at[0], s.at[1], surfaceZ(s.at)] as XYZ }))
  const stems = pts.filter((p) => Math.abs(p.at[2] - p.foot[2]) > 2)
  const labelled = pts.filter((p) => p.s.label)
  return [
    new PathLayer<Line>({
      id: 'group-slot-ways',
      data: ways,
      getPath: (l) => l.path,
      getColor: (l) => l.color,
      getWidth: (l) => l.width,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new PathLayer<{ s: SlotMark; at: XYZ; foot: XYZ }>({
      id: 'group-slot-stems',
      data: stems,
      getPath: (p) => [p.foot, p.at],
      getColor: (p) => color(p.s, 90),
      getWidth: 1,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new IconLayer<{ s: SlotMark; at: XYZ }>({
      id: 'group-slots',
      data: pts,
      getPosition: (p) => p.at,
      getIcon: (p) => (p.s.class === 'sent' ? GHOST_SENT : GHOST_WAITING),
      getSize: 16,
      sizeUnits: 'pixels',
      getColor: (p) => color(p.s, p.s.class === 'sent' ? 210 : 240),
      parameters: onTop,
    }),
    new ScatterplotLayer<ExcludedMark>({
      id: 'group-slot-excluded',
      data: excluded,
      pickable: true,
      getPosition: (e) => e.at,
      getRadius: 9,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: true,
      getFillColor: EXCLUDED,
      getLineColor: DARK,
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new TextLayer<ExcludedMark>({
      id: 'group-slot-excluded-x',
      data: excluded,
      getPosition: (e) => e.at,
      getText: () => '✕',
      getSize: 12,
      getColor: WHITE,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'center',
      fontFamily: FONT,
      fontWeight: 600,
      fontSettings: { sdf: true, fontSize: 48, buffer: 6 },
      characterSet: 'auto',
      parameters: onTop,
    }),
    new TextLayer<{ s: SlotMark; at: XYZ }>({
      id: 'group-slot-labels',
      data: labelled,
      getPosition: (p) => p.at,
      getText: (p) => p.s.label,
      getSize: 11,
      getColor: WHITE,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: [10, 0],
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

/** the layer ids a member left out by a group order's dry run is picked on (its reason on hover) */
export const EXCLUDED_PICK_LAYER = 'group-slot-excluded'

export function buildLinkRouteLayers(d: LinkRouteDrawing, scale: SurfaceScale) {
  const { surfaceZ, mPerPx } = scale
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
    ...slotLayers(d, scale),
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
