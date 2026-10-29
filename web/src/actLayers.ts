// Observed hostile acts on the map (hostileAct.ts): for each act drawn, in the hostile red of the
// track symbols (trackLayers.ts), at its height (drawn twice over, as everything is: map3d.ts):
//
//   act-lines     the faint dashed line from the author's fix to the victim, once the streak is over
//   act-sigma     the author's fix with its 1-sigma circle (metres): where our sensors placed it
//   act-streak    the fire event: a bright streak from the author to the victim, a second long
//   act-flash     a flash where the author fired, and one on the victim as the streak arrives
//   act-victim    a ring where the victim was, while the act is drawn
//   act-author    a dot at the author's fix
//   act-labels    "Act A-1: fires on HUNTER 1" and who saw it, above the author's fix, fading
//                 after some seconds
//   act-tags      "A-1" under the author's fix once its label is gone (a track's own labels are
//                 left and right of its symbol)
//
// MapView builds these on every animation frame while an act's streak, flash or label fade is
// under way, and at the console's tick otherwise (and always under reduced motion, without the
// streak or the flashes: a static line and markers).
import { PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { z } from './map3d'
import {
  type ActDrawing,
  dashes,
  hitFlash,
  labelAlpha,
  labelledKeys,
  lingerAlpha,
  mapLabel,
  muzzleFlash,
  type Pt3,
  STREAK_MS,
  streakPoints,
} from './hostileAct'

type RGBA = [number, number, number, number]
/** a hostile track's red (trackLayers.ts AFF_COLOR) */
const RED: [number, number, number] = [255, 93, 93]
const HOT: [number, number, number] = [255, 236, 214]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }
const rgba = (c: [number, number, number], a: number): RGBA => [c[0], c[1], c[2], Math.max(0, Math.min(255, Math.round(a)))]

interface Drawn {
  d: ActDrawing
  age: number
  /** rendered metres (map3d.ts) */
  from: Pt3
  to: Pt3 | null
  /** opacity of what lingers, 0 to 1 */
  linger: number
}

interface FlashAt {
  at: Pt3
  p: number
  ring: boolean
  hit: boolean
}

/** The acts `visible` (hostileAct.ts visibleActs, newest first) at `nowMs` (the bridge's clock),
 *  as deck.gl layers. `reduced`: prefers-reduced-motion, no streak or flash. */
export function buildActLayers(visible: ActDrawing[], nowMs: number, reduced: boolean) {
  if (!visible.length) return []
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const drawn: Drawn[] = visible.map((d) => ({
    d,
    age: nowMs - d.start,
    from: [d.author[0], d.author[1], z(0)],
    to: d.victim ? [d.victim.at[0], d.victim.at[1], z(d.victim.alt)] : null,
    linger: lingerAlpha(nowMs - d.start),
  }))
  const labelled = labelledKeys(visible, nowMs)
  const lines: { path: Pt3[]; a: number }[] = []
  const streaks: Pt3[][] = []
  const flashes: FlashAt[] = []
  for (const x of drawn) {
    const streaking = !reduced && x.age < STREAK_MS
    if (x.to && !streaking) {
      // brighter while its label is on, then faint
      const a = (0.35 + 0.45 * labelAlpha(x.age)) * x.linger
      for (const [p, q] of dashes(x.from, x.to)) lines.push({ path: [p, q], a })
    }
    if (reduced) continue
    if (x.to) {
      const s = streakPoints(x.from, x.to, x.age)
      if (s.length > 1) streaks.push(s)
      const h = hitFlash(x.age)
      if (h != null) flashes.push({ at: x.to, p: h, ring: true, hit: true }, { at: x.to, p: h, ring: false, hit: true })
    }
    const m = muzzleFlash(x.age)
    if (m != null) flashes.push({ at: x.from, p: m, ring: true, hit: false }, { at: x.from, p: m, ring: false, hit: false })
  }
  const labels = drawn.filter((x) => labelled.has(x.d.key)).map((x) => ({ x, a: labelAlpha(x.age) }))
  const tags = drawn.filter((x) => !labelled.has(x.d.key))
  const n = (k: number, len: number) => k / Math.max(1, len - 1)
  return [
    new PathLayer<{ path: Pt3[]; a: number }>({
      id: 'act-lines',
      data: lines,
      getPath: (l) => l.path,
      getColor: (l) => rgba(RED, 255 * l.a),
      getWidth: 2,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<Drawn>({
      id: 'act-sigma',
      data: drawn,
      getPosition: (x) => x.from,
      getRadius: (x) => Math.max(1, x.d.sigma_m),
      radiusUnits: 'meters',
      radiusMinPixels: 6,
      stroked: true,
      filled: true,
      getFillColor: (x) => rgba(RED, 46 * x.linger),
      getLineColor: (x) => rgba(RED, 200 * x.linger),
      getLineWidth: 1.5,
      lineWidthUnits: 'pixels',
      updateTriggers: { getFillColor: nowMs, getLineColor: nowMs },
      parameters: onTop,
    }),
    new PathLayer<Pt3[]>({
      id: 'act-streak',
      data: streaks,
      getPath: (s) => s,
      // per point: red and thin at the tail, white-hot and wide at the head
      getColor: (s) => s.map((_, k) => (n(k, s.length) > 0.75 ? rgba(HOT, 255) : rgba(RED, 70 + 185 * n(k, s.length)))),
      getWidth: (s) => s.map((_, k) => 2 + 5 * n(k, s.length)),
      widthUnits: 'pixels',
      capRounded: true,
      jointRounded: true,
      parameters: onTop,
    }),
    new ScatterplotLayer<FlashAt>({
      id: 'act-flash',
      data: flashes,
      getPosition: (f) => f.at,
      getRadius: (f) => (f.ring ? (f.hit ? 10 + 42 * f.p : 6 + 18 * f.p) : (f.hit ? 6 + 11 * (1 - f.p) : 3 + 6 * (1 - f.p))),
      radiusUnits: 'pixels',
      stroked: true,
      filled: true,
      getFillColor: (f) => (f.ring ? [0, 0, 0, 0] : [255, 243, 220, Math.round(255 * (1 - f.p))]),
      getLineColor: (f) => rgba(RED, 245 * (1 - f.p) ** 1.5),
      getLineWidth: (f) => (f.ring ? 3 : 2),
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<Drawn>({
      id: 'act-victim',
      data: drawn.filter((x) => x.to && (reduced || x.age >= STREAK_MS - 300)),
      getPosition: (x) => x.to!,
      getRadius: 9,
      radiusUnits: 'pixels',
      stroked: true,
      filled: false,
      getLineColor: (x) => rgba(RED, 230 * x.linger),
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      updateTriggers: { getLineColor: nowMs },
      parameters: onTop,
    }),
    new ScatterplotLayer<Drawn>({
      id: 'act-author',
      data: drawn,
      getPosition: (x) => x.from,
      getRadius: 4.5,
      radiusUnits: 'pixels',
      stroked: true,
      filled: true,
      getFillColor: (x) => rgba(RED, 255 * x.linger),
      getLineColor: OUTLINE,
      getLineWidth: 1.5,
      lineWidthUnits: 'pixels',
      updateTriggers: { getFillColor: nowMs },
      parameters: onTop,
    }),
    new TextLayer<{ x: Drawn; a: number }>({
      id: 'act-labels',
      data: labels,
      getPosition: (l) => l.x.from,
      getText: (l) => mapLabel(l.x.d),
      getSize: 12,
      getColor: (l) => rgba([255, 226, 226], 255 * l.a),
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'bottom',
      getPixelOffset: [0, -20],
      fontFamily: FONT,
      fontWeight: 700,
      lineHeight: 1.25,
      background: true,
      getBackgroundColor: (l) => [52, 14, 16, Math.round(230 * l.a)],
      getBorderColor: (l) => rgba(RED, 230 * l.a),
      getBorderWidth: 1,
      backgroundPadding: [6, 4, 6, 4],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: { getColor: nowMs, getBackgroundColor: nowMs, getBorderColor: nowMs },
      parameters: onTop,
    }),
    new TextLayer<Drawn>({
      id: 'act-tags',
      data: tags,
      getPosition: (x) => x.from,
      getText: (x) => x.d.act,
      getSize: 11,
      getColor: (x) => rgba([255, 200, 200], 255 * x.linger),
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: [0, 14],
      fontFamily: FONT,
      fontWeight: 700,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: { getColor: nowMs },
      parameters: onTop,
    }),
  ]
}
