// Opponent weapons on the map (weapon.ts, docs/OPPONENT-WEAPONS.md), in the hostile red of the
// track symbols, from the bridge's weapon picture only (never the simulator's truth):
//
//   weapon-estimate   the estimated flight, dashed, from the launch fix to the vehicle it heads for
//   weapon-flash      the launch flash at the launch fix, and the strike's at the victim
//   weapon-fix-line   the weapon's own fixes joined, solid
//   weapon-fixes      each fix with its 1-sigma circle, solid
//   weapon            the estimated weapon moving along the estimate (as our Hellfire's flight),
//                     with its countdown beside it
//   weapon-struck     a ring on the victim once it reports the strike, and its label
//
// MapView builds these on every animation frame while a weapon is in flight or a flash is on, and
// at the console's tick otherwise; under reduced motion nothing moves or flashes: the estimate's
// line, the fixes, the rings and the labels, static.
import { IconLayer, PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { MapScreen } from './aloft'
import { z } from './map3d'
import { dashesAhead, inFlight, launchFlash, munitionAt, screenAngle, type Munition, type Pt3 } from './munition'
import { estimate, estimateMapLabel, FLY_ALT_M, strikeFlash, weaponShort, type WeaponTrack } from './weapon'

type RGBA = [number, number, number, number]
const RED: [number, number, number] = [255, 93, 93]
const PALE: [number, number, number] = [255, 196, 196]
const TEXT: RGBA = [255, 226, 226, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }
const rgba = (c: [number, number, number], a: number): RGBA => [c[0], c[1], c[2], Math.max(0, Math.min(255, Math.round(a)))]

const ICON_PX = 40
const BODY = 'M20 6 C23.6 10 26 15 26 20 L26 64 L14 64 L14 20 C14 15 16.4 10 20 6 Z'
const FINS = 'M14 50 L7 61 L7 66 L14 64 Z M26 50 L33 61 L33 66 L26 64 Z'
/** the opponent's weapon: our munition's shape in a hostile red glow, its outline dashed (an estimate) */
const ICON_SVG = `<svg xmlns="http://www.w3.org/2000/svg" width="40" height="88" viewBox="0 0 40 88">
<defs><filter id="g" x="-60%" y="-30%" width="220%" height="160%"><feGaussianBlur stdDeviation="3.2"/></filter></defs>
<path d="M15.5 64 L20 80 L24.5 64 Z" fill="#ff5d5d"/>
<path d="${FINS}" fill="#ff8c8c" stroke="#0b0f14" stroke-width="2" stroke-linejoin="round"/>
<path d="${BODY} ${FINS}" fill="#ff5d5d" opacity="0.7" filter="url(#g)"/>
<path d="${BODY}" fill="#ffe4e4" stroke="#0b0f14" stroke-width="2.6" stroke-linejoin="round"/>
<path d="${BODY}" fill="none" stroke="#ff5d5d" stroke-width="2" stroke-dasharray="5 4"/>
</svg>`
const ICON = { id: 'weapon', url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(ICON_SVG)}`, width: 40, height: 88, anchorX: 20, anchorY: 38 }

const drawn = ([lon, lat, alt]: Pt3): Pt3 => [lon, lat, z(alt)]

interface Flying {
  at: Pt3
  angle: number
  label: string
}
interface Flash {
  at: Pt3
  p: number
  ring: boolean
}
interface FixAt {
  at: Pt3
  sigma: number
  a: number
}
interface Struck {
  at: Pt3
  label: string
  a: number
}

/** The weapons `ws` (weapon.ts shownWeapons) at `nowMs`. `victimOf`: where the vehicle a weapon
 *  heads for was when this console first had its launch (the estimate's end), and the struck
 *  vehicle's position; `nameOf`: a vehicle's callsign. */
export function buildWeaponLayers(
  ws: WeaponTrack[],
  nowMs: number,
  screen: MapScreen,
  reduced: boolean,
  victimOf: (w: WeaponTrack) => [number, number] | null,
  nameOf: (uid: string) => string,
) {
  if (!ws.length) return []
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const estimates: { path: Pt3[]; a: number }[] = []
  const flashes: Flash[] = []
  const flying: Flying[] = []
  const fixLines: { path: Pt3[]; a: number }[] = []
  const fixes: FixAt[] = []
  const struck: Struck[] = []
  for (const w of ws) {
    // ended ones fade over their last minute
    const a = w.ended_t == null ? 1 : Math.max(0.15, 1 - (nowMs - w.ended_t) / 60_000)
    const victim = victimOf(w)
    const est: Munition | null = estimate(w, victim)
    if (est) {
      // the whole estimate dashed while it flies and after; the part ahead only while it flies
      const from = drawn([...est.launch_point, FLY_ALT_M])
      const to = drawn([...est.impact_point, FLY_ALT_M])
      const n = 14
      for (let k = 0; k < n; k++) {
        const p = (f: number): Pt3 => [from[0] + (to[0] - from[0]) * f, from[1] + (to[1] - from[1]) * f, from[2] + (to[2] - from[2]) * f]
        estimates.push({ path: [p(k / n), p((k + 0.5) / n)], a: (inFlight(est, nowMs) ? 0.9 : 0.45) * a })
      }
      if (!reduced) {
        const p = launchFlash(est, nowMs)
        if (p != null) flashes.push({ at: from, p, ring: true }, { at: from, p, ring: false })
      }
      if (inFlight(est, nowMs) && !w.struck) {
        const at = drawn(munitionAt(est, reduced ? est.launch_t : nowMs))
        const sa = screen.at(at[0], at[1], at[2])
        const sb = screen.at(to[0], to[1], to[2])
        const angle = sa && sb && Math.hypot(sb[0] - sa[0], sb[1] - sa[1]) > 2 ? screenAngle(sb[0] - sa[0], sb[1] - sa[1]) : 180
        flying.push({ at: reduced ? from : at, angle, label: estimateMapLabel(est, w, nowMs) })
        if (!reduced) for (const [d0, d1] of dashesAhead(est, nowMs)) estimates.push({ path: [drawn(d0), drawn(d1)], a: a })
      }
    }
    // the weapon's own fixes, solid: what our radars and cameras actually had
    const pts = w.fixes.map((f): Pt3 => drawn([f.point[0], f.point[1], FLY_ALT_M]))
    if (w.launch && pts.length) pts.unshift(drawn([w.launch.point[0], w.launch.point[1], FLY_ALT_M]))
    if (pts.length > 1) fixLines.push({ path: pts, a })
    for (const f of w.fixes) fixes.push({ at: drawn([f.point[0], f.point[1], FLY_ALT_M]), sigma: f.sigma_m, a })
    if (w.launch) fixes.push({ at: drawn([w.launch.point[0], w.launch.point[1], FLY_ALT_M]), sigma: w.launch.sigma_m, a })
    if (w.struck) {
      const at = victim ? drawn([victim[0], victim[1], FLY_ALT_M]) : null
      if (at) {
        const p = strikeFlash(w, nowMs)
        if (p != null && !reduced) flashes.push({ at, p, ring: true }, { at, p, ring: false })
        struck.push({ at, label: `${weaponShort(w.class)} STRUCK ${nameOf(w.struck.uid)}`, a })
      }
    }
  }
  return [
    new PathLayer<{ path: Pt3[]; a: number }>({
      id: 'weapon-estimate',
      data: estimates,
      getPath: (d) => d.path,
      getColor: (d) => rgba(PALE, 230 * d.a),
      getWidth: 2,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new PathLayer<{ path: Pt3[]; a: number }>({
      id: 'weapon-fix-line',
      data: fixLines,
      getPath: (d) => d.path,
      getColor: (d) => rgba(RED, 255 * d.a),
      getWidth: 3,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<FixAt>({
      id: 'weapon-fixes',
      data: fixes,
      getPosition: (f) => f.at,
      getRadius: (f) => Math.max(1, f.sigma),
      radiusUnits: 'meters',
      radiusMinPixels: 4,
      stroked: true,
      filled: true,
      getFillColor: (f) => rgba(RED, 200 * f.a),
      getLineColor: OUTLINE,
      getLineWidth: 1.5,
      lineWidthUnits: 'pixels',
      updateTriggers: { getFillColor: nowMs },
      parameters: onTop,
    }),
    new ScatterplotLayer<Flash>({
      id: 'weapon-flash',
      data: flashes,
      getPosition: (f) => f.at,
      getRadius: (f) => (f.ring ? 10 + 44 * f.p : 4 + 14 * (1 - f.p)),
      radiusUnits: 'pixels',
      stroked: true,
      filled: true,
      getFillColor: (f) => (f.ring ? [0, 0, 0, 0] : [255, 243, 220, Math.round(255 * (1 - f.p))]),
      getLineColor: (f) => rgba(RED, 245 * (1 - f.p) ** 1.5),
      getLineWidth: (f) => (f.ring ? 3 : 2),
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new ScatterplotLayer<Struck>({
      id: 'weapon-struck',
      data: struck,
      getPosition: (s) => s.at,
      getRadius: 11,
      radiusUnits: 'pixels',
      stroked: true,
      filled: false,
      getLineColor: (s) => rgba(RED, 240 * s.a),
      getLineWidth: 2.5,
      lineWidthUnits: 'pixels',
      updateTriggers: { getLineColor: nowMs },
      parameters: onTop,
    }),
    new IconLayer<Flying>({
      id: 'weapon',
      data: flying,
      getPosition: (f) => f.at,
      getIcon: () => ICON,
      getSize: ICON_PX,
      sizeUnits: 'pixels',
      getAngle: (f) => f.angle,
      parameters: onTop,
    }),
    new TextLayer<Flying | Struck>({
      id: 'weapon-labels',
      data: [...flying, ...struck],
      getPosition: (f) => f.at,
      getText: (f) => f.label,
      getSize: 12,
      getColor: TEXT,
      // below and right of the weapon, clear of the act's label above the shooter
      getTextAnchor: 'start',
      getAlignmentBaseline: 'top',
      getPixelOffset: [ICON_PX / 3, ICON_PX / 2],
      fontFamily: FONT,
      fontWeight: 700,
      background: true,
      getBackgroundColor: [52, 14, 16, 225],
      getBorderColor: rgba(RED, 230),
      getBorderWidth: 1,
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}

/** Whether animation frames are wanted: a weapon flying on its estimate or a flash on. */
export function weaponsAnimating(ws: WeaponTrack[], nowMs: number, victimOf: (w: WeaponTrack) => [number, number] | null): boolean {
  return ws.some((w) => {
    const est = estimate(w, victimOf(w))
    return (est != null && (inFlight(est, nowMs) || launchFlash(est, nowMs) != null)) || strikeFlash(w, nowMs) != null
  })
}
