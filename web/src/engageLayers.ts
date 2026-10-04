import { PathLayer, PolygonLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { ViewMode } from './MapView'
import { ellipseRing, fmtRel, type EngagePreview } from './engage'
import type { LngLat } from './types'

/** Engagement previews on the map (Phase E), through MapView's `layers` prop: for each
 *  engagement in play (and the nomination form's draft) the platform's path forward, the
 *  track's projected motion, the predicted effect point with its radius, the keep-out circle
 *  and the track's uncertainty ellipse at the effect time. At ground level in both views,
 *  drawn over terrain. */

export interface EngageDrawing {
  key: string
  label: string
  preview: EngagePreview
  /** pending, running, effected, or the form's draft */
  phase: 'draft' | 'pending' | 'running' | 'effected' | 'ended'
  selected: boolean
  /** an engagement whose picture no longer holds */
  warn?: boolean
}

type RGBA = [number, number, number, number]
const OURS: RGBA = [127, 184, 255, 230]
const THEIRS: RGBA = [255, 93, 93, 220]
const KEEP: RGBA = [255, 179, 71, 230]
const EFFECT: RGBA = [255, 93, 93, 255]
const TEXT: RGBA = [235, 239, 244, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'

function a(c: RGBA, k: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * k)]
}

interface Path {
  path: LngLat[]
  color: RGBA
  width: number
}

export function buildEngageLayers(drawings: EngageDrawing[], nowMs: number, _view: ViewMode) {
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const dim = (d: EngageDrawing) => (d.selected || d.phase === 'draft' ? 1 : d.phase === 'ended' ? 0.35 : 0.65)
  const paths: Path[] = []
  for (const d of drawings) {
    const k = dim(d)
    if (d.phase !== 'effected' && d.phase !== 'ended') paths.push({ path: d.preview.path, color: a(OURS, k), width: d.selected ? 3 : 2 })
    paths.push({ path: d.preview.track_path, color: a(THEIRS, k * 0.8), width: 1.5 })
  }
  const tick = Math.floor(nowMs / 1000)
  return [
    new ScatterplotLayer<EngageDrawing>({
      id: 'engage-keepout',
      data: drawings.filter((d) => d.phase !== 'ended'),
      getPosition: (d) => d.preview.effect_point,
      getRadius: (d) => d.preview.keep_out_m,
      radiusUnits: 'meters',
      stroked: true,
      filled: true,
      getFillColor: (d) => a(KEEP, d.warn ? 0.14 : 0.05),
      getLineColor: (d) => a(KEEP, dim(d) * (d.warn ? 1 : 0.7)),
      getLineWidth: (d) => (d.warn ? 2.5 : 1.5),
      lineWidthUnits: 'pixels',
      updateTriggers: { getFillColor: drawings.map((d) => d.warn).join(), getLineWidth: drawings.map((d) => d.warn).join() },
      parameters: onTop,
    }),
    new PolygonLayer<EngageDrawing>({
      id: 'engage-ellipse',
      data: drawings.filter((d) => d.phase !== 'ended'),
      getPolygon: (d) => ellipseRing(d.preview.ellipse),
      stroked: true,
      filled: true,
      getFillColor: (d) => a(THEIRS, 0.06 * dim(d)),
      getLineColor: (d) => a(THEIRS, 0.7 * dim(d)),
      getLineWidth: 1,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<EngageDrawing>({
      id: 'engage-effect',
      data: drawings,
      getPosition: (d) => d.preview.effect_point,
      getRadius: (d) => d.preview.effect_radius_m,
      radiusUnits: 'meters',
      radiusMinPixels: 4,
      stroked: true,
      filled: true,
      getFillColor: (d) => a(EFFECT, d.phase === 'effected' ? 0.45 : d.phase === 'ended' ? 0.1 : 0.16),
      getLineColor: (d) => a(EFFECT, dim(d)),
      getLineWidth: (d) => (d.selected ? 2.5 : 1.5),
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new PathLayer<Path>({
      id: 'engage-paths',
      data: paths,
      getPath: (p) => p.path,
      getColor: (p) => p.color,
      getWidth: (p) => p.width,
      widthUnits: 'pixels',
      capRounded: true,
      parameters: onTop,
    }),
    new TextLayer<EngageDrawing>({
      id: 'engage-labels',
      data: drawings,
      getPosition: (d) => d.preview.effect_point,
      getText: (d) =>
        d.phase === 'effected' || d.phase === 'ended'
          ? d.label
          : `${d.label} effect ${fmtRel(d.preview.effect_t - nowMs)}`,
      getSize: 11,
      getColor: TEXT,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'bottom',
      getPixelOffset: [10, -10],
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: { sdf: true, fontSize: 48, buffer: 6 },
      characterSet: 'auto',
      updateTriggers: { getText: tick },
      parameters: onTop,
    }),
  ]
}
