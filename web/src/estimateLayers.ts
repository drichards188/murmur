// Estimates for silent vehicles on the map (murmur-4y9), as deck.gl layers through MapView's
// `layers` prop, drawn under the vehicle symbols. Only vehicles past their stale threshold
// with a model from the bridge are in the data (estimate.ts `estimated`), so the cost follows
// the silent few, not the fleet. Attributes are rebuilt per clock tick (positions move); per
// silent vehicle:
//
//   estimate-regions   the region now: a circle, an ellipse along the course, a ring sector
//   estimate-bands     a patrol route the vehicle could be anywhere on (a wide path)
//   estimate-leaders   last known position to the estimate
//   estimate-symbols   the 2525 frame again, faded, at the estimate
//   estimate-labels    "EST on its orbit · silent 47 s"
//
// and, once the platform's lost-link timer has fired, the same in violet for the failsafe
// branch ("RTL if it lost us too · 12 s"): a second hypothesis, not a second vehicle.
import { IconLayer, LineLayer, PathLayer, PolygonLayer, TextLayer } from '@deck.gl/layers'
import { bandPath, BASIS_LABEL, estimateAt, FAILSAFE_LABEL, failsafeAt, regionRings, type Evaluated } from './estimate'
import { HEIGHT_EXAGGERATION } from './map3d'
import type { ViewMode } from './MapView'
import { frameIcon } from './symbols'
import type { LngLat, Vehicle } from './types'

export interface EstimateLayerInput {
  /** silent vehicles with an estimate (estimate.ts `estimated`) */
  vehicles: Vehicle[]
  /** bridge clock, ms */
  nowMs: number
  /** the vehicle whose details are open: its estimate drawn bolder */
  focus: number | null
}

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]
const OURS: RGBA = [127, 184, 255, 255]
const BRANCH: RGBA = [196, 155, 255, 255]
const TEXT: RGBA = [215, 221, 228, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }

function a(c: RGBA, k: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * k)]
}

/** one drawn hypothesis: the estimate itself, or the lost-link branch */
interface Drawn {
  v: Vehicle
  branch: boolean
  at: Evaluated
  /** where its leader starts: the last known position, or the branch's start */
  from: LngLat
  rings: LngLat[][]
  band: LngLat[] | null
  label: string
  focus: boolean
}

function fmtS(s: number) {
  const n = Math.max(0, Math.floor(s))
  return n < 60 ? `${n} s` : `${Math.floor(n / 60)} m ${String(n % 60).padStart(2, '0')} s`
}

export function buildEstimateLayers({ vehicles, nowMs, focus }: EstimateLayerInput, view: ViewMode) {
  const is3d = view === '3d'
  const drawn: Drawn[] = []
  for (const v of vehicles) {
    const e = v.estimate
    if (!e || !v.position) continue
    const silent = (v.link.since_last_ms ?? 0) / 1000
    const at = estimateAt(e, nowMs)
    drawn.push({
      v,
      branch: false,
      at,
      from: v.position,
      rings: regionRings(at.region),
      band: bandPath(at.region),
      label: `EST ${BASIS_LABEL[e.basis]} · silent ${fmtS(silent)}`,
      focus: v.uid === focus,
    })
    const fs = failsafeAt(e, nowMs)
    if (fs && e.failsafe) {
      const m = e.failsafe.motion
      const from: LngLat = m.kind === 'line' ? m.from : m.kind === 'point' ? m.at : at.position
      drawn.push({
        v,
        branch: true,
        at: fs,
        from,
        rings: regionRings(fs.region),
        band: bandPath(fs.region),
        label: `${FAILSAFE_LABEL[e.failsafe.action]} if it lost us too · ${fmtS(nowMs / 1000 - e.failsafe.fires_t)}`,
        focus: v.uid === focus,
      })
    }
  }
  const z = (d: Drawn) => (is3d ? (d.v.alt_msl ?? 0) * HEIGHT_EXAGGERATION : 0)
  const at = (d: Drawn): XYZ => [d.at.position[0], d.at.position[1], z(d)]
  const color = (d: Drawn) => (d.branch ? BRANCH : OURS)
  const dim = (d: Drawn) => (d.focus ? 1 : 0.7)
  const tick = Math.floor(nowMs / 500)
  const triggers = { getPosition: tick, getPolygon: tick, getPath: tick, getSourcePosition: tick, getTargetPosition: tick, getText: tick, getFillColor: focus, getLineColor: focus, getColor: focus, getWidth: focus, getLineWidth: focus }
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  return [
    new PolygonLayer<Drawn>({
      id: 'estimate-regions',
      data: drawn.filter((d) => d.rings.length > 0),
      getPolygon: (d) => d.rings,
      stroked: true,
      filled: true,
      getFillColor: (d) => a(color(d), d.branch ? 0.05 : 0.08),
      getLineColor: (d) => a(color(d), 0.6 * dim(d)),
      getLineWidth: (d) => (d.focus ? 2 : 1),
      lineWidthUnits: 'pixels',
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new PathLayer<Drawn>({
      id: 'estimate-bands',
      data: drawn.filter((d) => d.band),
      getPath: (d) => d.band!,
      getColor: (d) => a(color(d), 0.14),
      getWidth: (d) => (d.at.region.kind === 'band' ? d.at.region.width_m : 0),
      widthUnits: 'meters',
      widthMinPixels: 2,
      capRounded: true,
      jointRounded: true,
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new LineLayer<Drawn>({
      id: 'estimate-leaders',
      data: drawn,
      getSourcePosition: (d) => [d.from[0], d.from[1], z(d)],
      getTargetPosition: at,
      getColor: (d) => a(color(d), 0.75 * dim(d)),
      getWidth: (d) => (d.focus ? 2 : 1.5),
      widthUnits: 'pixels',
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new IconLayer<Drawn>({
      id: 'estimate-symbols',
      data: drawn,
      billboard: true,
      getPosition: at,
      getIcon: (d) => frameIcon(d.v, true),
      getSize: (d) => frameIcon(d.v, true).size * (d.branch ? 0.8 : 1),
      sizeUnits: 'pixels',
      getColor: (d) => (d.branch ? [220, 200, 255, 120] : [255, 255, 255, 150]),
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new TextLayer<Drawn>({
      id: 'estimate-labels',
      data: drawn,
      getPosition: at,
      getText: (d) => d.label,
      getSize: 10,
      getColor: (d) => a(TEXT, dim(d)),
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (d) => [0, frameIcon(d.v, true).halfH + 4],
      fontFamily: FONT,
      fontWeight: 600,
      background: true,
      getBackgroundColor: (d) => a(color(d), 0.28),
      backgroundPadding: [5, 3, 5, 3],
      outlineWidth: 2,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: triggers,
      parameters: onTop,
    }),
  ]
}
