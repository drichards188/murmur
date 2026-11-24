// Observed hostile acts on the map (hostileAct.ts): for each act drawn, in the hostile red of the
// track symbols (trackLayers.ts), at its height (drawn twice over, as everything is: map3d.ts):
//
//   act-lines     an abstract act's dashed "fired on" arrow from the author's fix to the victim (a
//                 report: nothing modelled flew); none for an act that launched a modelled weapon,
//                 whose weapon is drawn instead (weaponLayers.ts)
//   act-sigma     the author's fix with its 1-sigma circle (metres): where our sensors placed it
//   act-flash     a burst where the author fired
//   act-victim    a ring where an abstract act's victim was, while the act is drawn
//   act-author    a dot at the author's fix
//   act-labels    "Act A-1: reported fire on HUNTER 1" (or "launch at") and who saw it, above the
//                 author's fix, fading after some seconds
//   act-tags      "A-1" under the author's fix once its label is gone (a track's own labels are
//                 left and right of its symbol)
//
// Both labels are placed by the one label budget (declutter.ts, actLabelCandidates) against our
// vehicles' and the tracks' labels: never drawn over them (bd murmur-iobd.3).
//
// MapView builds these on every animation frame while an act's burst or label fade is under way,
// and at the console's tick otherwise (and always under reduced motion, without the burst: a
// static arrow and markers).
import { PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { z } from './map3d'
import type { MapScreen } from './aloft'
import { type LabelCandidate, type LabelLevel, textBox } from './declutter'
import { type ActDrawing, arrowHead, dashes, headline, labelAlpha, labelledKeys, lingerAlpha, mapLabel, muzzleFlash, type Pt3 } from './hostileAct'

type RGBA = [number, number, number, number]
/** a hostile track's red (trackLayers.ts AFF_COLOR) */
const RED: [number, number, number] = [255, 93, 93]
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
}

/** where an act's label and its id are drawn from the author's fix, px */
const LABEL_AT: [number, number] = [0, -20]
const TAG_AT: [number, number] = [0, 14]
const LABEL_PAD: [number, number] = [6, 4]
/** px kept clear round an act's label: flush against a callsign it read as one sentence with it */
const LABEL_ROOM = 4

/** The acts' labels for the one label budget (declutter.ts, bd murmur-iobd.3), so that "Act A-2:
 *  launch at HUNTER 1" is placed or dropped against HUNTER 1's own label, never over it: an act's
 *  two-line label (one line short) while it is on, with what waits on the person; its id once the
 *  label has faded, with ours on alert. Anchored at the author's fix, as drawn. */
export function actLabelCandidates(visible: readonly ActDrawing[], nowMs: number, screen: MapScreen): LabelCandidate[] {
  const labelled = labelledKeys([...visible], nowMs)
  return visible.map((d) => {
    const at = screen.at(d.author[0], d.author[1], z(0))
    if (labelled.has(d.key)) {
      const lines = mapLabel(d).split('\n')
      return { key: `a:${d.key}`, at, full: textBox(lines, 12, 'middle', 'bottom', LABEL_AT, LABEL_PAD, LABEL_ROOM), short: textBox([headline(d)], 12, 'middle', 'bottom', LABEL_AT, LABEL_PAD, LABEL_ROOM), priority: 1 as const }
    }
    const tag = textBox([d.act], 11, 'middle', 'top', TAG_AT)
    return { key: `at:${d.key}`, at, full: tag, short: tag, priority: 3 as const }
  })
}

/** The acts `visible` (hostileAct.ts visibleActs, newest first) at `nowMs` (the bridge's clock),
 *  as deck.gl layers. `reduced`: prefers-reduced-motion, no burst. `level`: the label budget's
 *  level for a label's key, undefined for one the last plan did not weigh (an act newer than it:
 *  drawn in full until the next plan). */
export function buildActLayers(visible: ActDrawing[], nowMs: number, reduced: boolean, level: (key: string) => LabelLevel | undefined = () => undefined) {
  if (!visible.length) return []
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const drawn: Drawn[] = visible.map((d) => ({
    d,
    age: nowMs - d.start,
    from: [d.author[0], d.author[1], z(0)],
    // an act that launched a modelled weapon has no arrow: its weapon is drawn
    to: d.victim && !d.weapon ? [d.victim.at[0], d.victim.at[1], z(d.victim.alt)] : null,
    linger: lingerAlpha(nowMs - d.start),
  }))
  const labelled = labelledKeys(visible, nowMs)
  const lines: { path: Pt3[]; a: number }[] = []
  const flashes: FlashAt[] = []
  for (const x of drawn) {
    if (x.to) {
      // the report's arrow: brighter while its label is on, then faint
      const a = (0.35 + 0.45 * labelAlpha(x.age)) * x.linger
      for (const [p, q] of dashes(x.from, x.to)) lines.push({ path: [p, q], a })
      for (const [p, q] of arrowHead(x.from, x.to)) lines.push({ path: [p, q], a })
    }
    if (reduced) continue
    const m = muzzleFlash(x.age)
    if (m != null) flashes.push({ at: x.from, p: m, ring: true }, { at: x.from, p: m, ring: false })
  }
  // each label in the form the budget placed it (none: left out), its id where the budget placed it
  const labels = drawn
    .filter((x) => labelled.has(x.d.key) && level(`a:${x.d.key}`) !== 'none')
    .map((x) => ({ x, a: labelAlpha(x.age), text: level(`a:${x.d.key}`) === 'short' ? headline(x.d) : mapLabel(x.d) }))
  const tags = drawn.filter((x) => !labelled.has(x.d.key) && level(`at:${x.d.key}`) !== 'none')
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
    new ScatterplotLayer<FlashAt>({
      id: 'act-flash',
      data: flashes,
      getPosition: (f) => f.at,
      getRadius: (f) => (f.ring ? 6 + 18 * f.p : 3 + 6 * (1 - f.p)),
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
      data: drawn.filter((x) => x.to),
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
    new TextLayer<{ x: Drawn; a: number; text: string }>({
      id: 'act-labels',
      data: labels,
      getPosition: (l) => l.x.from,
      getText: (l) => l.text,
      getSize: 12,
      getColor: (l) => rgba([255, 226, 226], 255 * l.a),
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'bottom',
      getPixelOffset: LABEL_AT,
      fontFamily: FONT,
      fontWeight: 700,
      lineHeight: 1.25,
      background: true,
      getBackgroundColor: (l) => [52, 14, 16, Math.round(230 * l.a)],
      getBorderColor: (l) => rgba(RED, 230 * l.a),
      getBorderWidth: 1,
      backgroundPadding: [LABEL_PAD[0], LABEL_PAD[1], LABEL_PAD[0], LABEL_PAD[1]],
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
      getPixelOffset: TAG_AT,
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
