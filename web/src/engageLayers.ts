import { PathLayer, PolygonLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { ellipseRing, fmtDist, fmtRel, KEEP_OUT_KIND_LABEL, type EngagePreview, type KeepOutExplanation } from './engage'
import { circle, distanceM } from './geo'
import type { LngLat } from './types'

/** Engagement previews on the map (Phase E), through MapView's `layers` prop: for each
 *  engagement in play (and the nomination form's draft) the platform's path forward, the
 *  track's projected motion, the predicted effect point with its radius, the keep-out circle
 *  and the track's uncertainty ellipse at the effect time. At sea level, drawn over the
 *  terrain and the water (never hidden by either). */

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

export function buildEngageLayers(drawings: EngageDrawing[], nowMs: number) {
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

// ---- the keep-out explanation (murmur-12e) --------------------------------------------------

const NEAR: RGBA = [154, 166, 178, 200]

/** A ring as dashes (deck.gl's PathLayer draws no dashes without an extension). */
function dashedRing(center: LngLat, radiusM: number, dashes = 48): LngLat[][] {
  const ring = circle(center, radiusM, dashes * 2)
  const out: LngLat[][] = []
  for (let k = 0; k + 1 < ring.length; k += 2) out.push([ring[k], ring[k + 1]])
  return out
}

interface Label {
  at: LngLat
  text: string
  color: RGBA
  /** the platform's labels sit centred below their ring, clear of the nomination's label
   *  (above right of the effect point) and the tracks' (right of each track) */
  below?: boolean
}

/** The nomination form's keep-out explanation on the map: for each track or vehicle of ours
 *  that blocks (amber) or nearly blocks (grey), the region the check reads at the effect time
 *  (dashed, faintly filled when it blocks) with its other regions that reach the clearance, a
 *  line from where it is now to where that region is centred, a ring on it now and a short
 *  label; and the platform's move-within ring (dashed blue) around the point it would need to
 *  be within. Read-only, as the explanation is. */
export function buildKeepOutLayers(ex: KeepOutExplanation, platformName: string) {
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const paths: Path[] = []
  const fills: { ring: LngLat[]; color: RGBA }[] = []
  const marks: { at: LngLat; color: RGBA }[] = []
  const labels: Label[] = []
  for (const i of ex.items) {
    const color = i.blocks ? KEEP : NEAR
    const r = i.region
    if (r) {
      for (const d of dashedRing(r.center, r.radius_m)) paths.push({ path: d, color, width: i.blocks ? 2 : 1.2 })
      if (i.blocks) fills.push({ ring: circle(r.center, r.radius_m), color: a(KEEP, 0.05) })
      // its other regions that reach into the clearance too, fainter
      for (const o of i.regions) {
        if (o === r || !o.inside || (o.center[0] === r.center[0] && o.center[1] === r.center[1] && o.radius_m === r.radius_m)) continue
        for (const d of dashedRing(o.center, o.radius_m, 36)) paths.push({ path: d, color: a(color, 0.45), width: 1 })
      }
      if (i.now && distanceM(i.now, r.center) > 30) paths.push({ path: [i.now, r.center], color: a(color, 0.7), width: 1 })
    }
    if (i.now) {
      marks.push({ at: i.now, color })
      const kind = r ? KEEP_OUT_KIND_LABEL[r.kind] : 'cannot be placed'
      const num = i.short_m == null ? '' : i.blocks ? ` · short ${fmtDist(i.short_m)}` : ` · clear by ${fmtDist(-i.short_m)}`
      const who = i.subject === 'vehicle' ? i.label : i.subject === 'lost_contact' ? `${i.id} (lost)` : i.id
      labels.push({ at: i.now, text: `${who} ${kind}${num}`, color: i.blocks ? [255, 211, 154, 255] : [200, 208, 216, 255] })
    }
  }
  const mw = ex.blocked ? ex.move_within : null
  const blue: RGBA = [170, 205, 255, 255]
  if (mw?.possible && mw.point && mw.within_m != null) {
    for (const d of dashedRing(mw.point, mw.within_m, 40)) paths.push({ path: d, color: a(OURS, 0.95), width: 2 })
    labels.push({ at: circle(mw.point, mw.within_m, 4)[2], text: `${platformName} within ${fmtDist(mw.within_m)}`, color: blue, below: true })
  }
  // a holding target and a band farther out (no closer position): its two rings
  const band = mw?.possible && mw.holding && mw.within_m == null ? mw.windows.find((w) => w.from_m != null && w.until_m != null) : undefined
  if (band && band.from_m != null && band.until_m != null) {
    for (const r of [band.from_m, band.until_m]) for (const d of dashedRing(ex.effect_point, r, 40)) paths.push({ path: d, color: a(OURS, 0.7), width: 1.5 })
    labels.push({
      at: circle(ex.effect_point, band.until_m, 4)[2],
      text: `${platformName} ${fmtDist(band.from_m)} to ${fmtDist(band.until_m)} out`,
      color: blue,
      below: true,
    })
  }
  return [
    new PolygonLayer<{ ring: LngLat[]; color: RGBA }>({
      id: 'keepout-fill',
      data: fills,
      getPolygon: (f) => f.ring,
      stroked: false,
      filled: true,
      getFillColor: (f) => f.color,
      parameters: onTop,
    }),
    new PathLayer<Path>({
      id: 'keepout-regions',
      data: paths,
      getPath: (p) => p.path,
      getColor: (p) => p.color,
      getWidth: (p) => p.width,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<{ at: LngLat; color: RGBA }>({
      id: 'keepout-marks',
      data: marks,
      getPosition: (m) => m.at,
      getRadius: 9,
      radiusUnits: 'pixels',
      stroked: true,
      filled: false,
      getLineColor: (m) => m.color,
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new TextLayer<Label>({
      id: 'keepout-labels',
      data: labels,
      getPosition: (l) => l.at,
      getText: (l) => l.text,
      getSize: 11,
      getColor: (l) => l.color,
      getTextAnchor: (l) => (l.below ? 'middle' : 'start'),
      getAlignmentBaseline: 'top',
      getPixelOffset: (l) => (l.below ? [0, 10] : [12, 6]),
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: { sdf: true, fontSize: 48, buffer: 6 },
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}
