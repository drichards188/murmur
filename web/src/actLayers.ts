// Observed hostile acts on the map (hostileAct.ts): for each act drawn, in the hostile red of the
// track symbols (trackLayers.ts), at its height (drawn twice over, as everything is: map3d.ts):
//
//   act-lines     an abstract act's dashed "fired on" arrow from the author's fix to the victim (a
//                 report: nothing modelled flew); none for an act that launched a modelled weapon,
//                 whose weapon is drawn instead (weaponLayers.ts)
//   act-flash     a burst where the author fired, once
//   act-victim    a ring where an abstract act's victim was, while the act is drawn
//   act-author    a dot at the author's fix
//
// No words on the map: the banner says who did what to whom (ledger 1, the map's acts), and the
// author's track draws its own uncertainty.
//
// MapView builds these on every animation frame while an act's burst or its line's fade is under way,
// and at the console's tick otherwise (and always under reduced motion, without the burst: a
// static arrow and markers).
import { PathLayer, ScatterplotLayer } from '@deck.gl/layers'
import { z } from './map3d'
import { type ActDrawing, arrowHead, dashes, labelAlpha, lingerAlpha, muzzleFlash, type Pt3 } from './hostileAct'

type RGBA = [number, number, number, number]
/** a hostile track's red (trackLayers.ts AFF_COLOR) */
const RED: [number, number, number] = [255, 93, 93]
const OUTLINE: RGBA = [11, 15, 20, 255]
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

/** The acts `visible` (hostileAct.ts visibleActs, newest first) at `nowMs` (the bridge's clock),
 *  as deck.gl layers. `reduced`: prefers-reduced-motion, no burst. */
export function buildActLayers(visible: ActDrawing[], nowMs: number, reduced: boolean) {
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
  const lines: { path: Pt3[]; a: number }[] = []
  const flashes: FlashAt[] = []
  for (const x of drawn) {
    if (x.to) {
      // the report's arrow: brighter its first seconds, then faint
      const a = (0.35 + 0.45 * labelAlpha(x.age)) * x.linger
      for (const [p, q] of dashes(x.from, x.to)) lines.push({ path: [p, q], a })
      for (const [p, q] of arrowHead(x.from, x.to)) lines.push({ path: [p, q], a })
    }
    if (reduced) continue
    const m = muzzleFlash(x.age)
    if (m != null) flashes.push({ at: x.from, p: m, ring: true }, { at: x.from, p: m, ring: false })
  }
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
  ]
}
