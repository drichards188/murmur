// Munitions in flight on the map (bd murmur-64z): from its launch to its impact, each munition at
// its place on the flight (munition.ts) and its height (drawn twice over, as everything is:
// map3d.ts), as a small missile pointing along its flight on the screen, a short fading trail
// behind it, a thin dashed line on to the impact point, and "HELLFIRE · 9 s" beside it; a flash
// at the launch point as it leaves. MapView builds these on every animation frame while one is
// in flight (and at the console's tick under reduced motion, without the flash), so they move as
// smoothly as the vehicles' glides; the strike itself is the impact marker (impact.ts).
//
//   munition-ahead    the dashes from the munition on to the impact point
//   munition-trail    the trail, fading out behind it
//   munition-flash    the launch flash: a core that fades and a ring that spreads
//   munition          the missile, turned along its flight as the camera sees it
//   munition-labels   name and seconds to impact, right of it
//   munition-aloft    "▲ HELLFIRE · 9 s" at the surface under it while it is above the view
//                     (aloft.ts), as a vehicle there is labelled
import { IconLayer, PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { MapScreen } from './aloft'
import { z } from './map3d'
import { dashesAhead, inFlight, launchFlash, munitionAt, munitionLabel, screenAngle, trailPoints, type Munition, type Pt3 } from './munition'

type RGBA = [number, number, number, number]
/** the impact marker's orange (impact.css), so the flight and its strike read as one */
const HOT: RGBA = [255, 140, 49, 255]
const PALE: RGBA = [255, 211, 152, 255]
const TEXT: RGBA = [255, 236, 214, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }

/** on-screen height of the missile's icon (its glow included), CSS pixels */
const ICON_PX = 40
// the fins and canards, and the whole outline (for the glow)
const FINS = 'M14 50 L7 61 L7 66 L14 64 Z M26 50 L33 61 L33 66 L26 64 Z M14 23 L10 30 L14 30 Z M26 23 L30 30 L26 30 Z'
const BODY = 'M20 6 C23.6 10 26 15 26 20 L26 64 L14 64 L14 20 C14 15 16.4 10 20 6 Z'
/** A missile seen from the side, nose up: a light body with a dark outline (it reads on the dark
 *  map, the water and the terrain alike) in an orange glow, orange fins and a flame at the tail.
 *  Drawn at twice its on-screen size, so it stays sharp on HiDPI (as symbols.ts does). */
const ICON_SVG = `<svg xmlns="http://www.w3.org/2000/svg" width="40" height="88" viewBox="0 0 40 88">
<defs><filter id="g" x="-60%" y="-30%" width="220%" height="160%"><feGaussianBlur stdDeviation="3.2"/></filter></defs>
<path d="${BODY} ${FINS}" fill="#ff8c31" opacity="0.8" filter="url(#g)"/>
<path d="M15.5 64 L20 83 L24.5 64 Z" fill="#ff8c31"/><path d="M17.6 64 L20 75 L22.4 64 Z" fill="#fff3c1"/>
<path d="${FINS}" fill="#ffb347" stroke="#0b0f14" stroke-width="2" stroke-linejoin="round"/>
<path d="${BODY}" fill="#f4f6f8" stroke="#0b0f14" stroke-width="2.6" stroke-linejoin="round"/>
</svg>`
const ICON = {
  id: 'munition',
  url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(ICON_SVG)}`,
  width: 40,
  height: 88,
  // turned about the middle of its body
  anchorX: 20,
  anchorY: 38,
}

/** metres above mean sea level to rendered metres (map3d.ts) */
const drawn = ([lon, lat, alt]: Pt3): Pt3 => [lon, lat, z(alt)]

interface Flying {
  m: Munition
  /** where it is drawn, rendered metres */
  at: Pt3
  angle: number
  label: string
  /** above the view: labelled at the surface under it */
  aloft: boolean
}

/** a launch flash's two parts: a ring that spreads and fades, a core that shrinks and fades */
interface Flash {
  at: Pt3
  /** 0 at the launch to 1 as the flash ends */
  p: number
  ring: boolean
}

/** The munitions in flight at `nowMs` (engine ms, the bridge's clock) and the launch flashes then,
 *  as deck.gl layers. `screen` says where the camera puts a point (for the missile's turn and
 *  what is above the view); `reduced`: prefers-reduced-motion, no flash. */
export function buildMunitionLayers(munitions: Munition[], nowMs: number, screen: MapScreen, reduced: boolean) {
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const flying: Flying[] = []
  const flashes: Flash[] = []
  const trails: Pt3[][] = []
  const dashes: Pt3[][] = []
  for (const m of munitions) {
    const p = launchFlash(m, nowMs)
    if (p != null && !reduced) {
      const at = drawn([...m.launch_point, m.launch_alt_m])
      flashes.push({ at, p, ring: true }, { at, p, ring: false })
    }
    if (!inFlight(m, nowMs)) continue
    const at = drawn(munitionAt(m, nowMs))
    const to = drawn([...m.impact_point, m.impact_alt_m])
    // its line is straight on the screen too: towards the impact point, or on from the launch
    // point when it is nearly there
    const a = screen.at(at[0], at[1], at[2])
    const b = screen.at(to[0], to[1], to[2])
    const from = drawn([...m.launch_point, m.launch_alt_m])
    const c = screen.at(from[0], from[1], from[2])
    const angle = a && b && Math.hypot(b[0] - a[0], b[1] - a[1]) > 2 ? screenAngle(b[0] - a[0], b[1] - a[1]) : a && c ? screenAngle(a[0] - c[0], a[1] - c[1]) : 180
    flying.push({ m, at, angle, label: munitionLabel(m, nowMs), aloft: screen.aloft(at[0], at[1], at[2]) })
    const trail = trailPoints(m, nowMs).map(drawn)
    if (trail.length > 1) trails.push(trail)
    for (const [d0, d1] of dashesAhead(m, nowMs)) dashes.push([drawn(d0), drawn(d1)])
  }
  if (!flying.length && !flashes.length) return []
  // smoke at the tail, hot at the munition
  const fade = (k: number, n: number): RGBA => [255, 190 + Math.round(50 * (k / n)), 150 + Math.round(70 * (k / n)), Math.round(235 * (k / n))]
  return [
    new PathLayer<Pt3[]>({
      id: 'munition-ahead',
      data: dashes,
      getPath: (d) => d,
      getColor: [PALE[0], PALE[1], PALE[2], 215],
      getWidth: 2,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
    new PathLayer<Pt3[]>({
      id: 'munition-trail',
      data: trails,
      getPath: (d) => d,
      // per point: transparent and thin at the tail, bright and wide at the munition
      getColor: (d) => d.map((_, k) => fade(k, d.length - 1)),
      getWidth: (d) => d.map((_, k) => 1.5 + (5.5 * k) / (d.length - 1)),
      widthUnits: 'pixels',
      capRounded: true,
      jointRounded: true,
      parameters: onTop,
    }),
    new ScatterplotLayer<Flash>({
      id: 'munition-flash',
      data: flashes,
      getPosition: (f) => f.at,
      getRadius: (f) => (f.ring ? 10 + 50 * f.p : 4 + 16 * (1 - f.p)),
      radiusUnits: 'pixels',
      stroked: true,
      filled: true,
      getFillColor: (f) => (f.ring ? [0, 0, 0, 0] : [255, 243, 193, Math.round(255 * (1 - f.p))]),
      getLineColor: (f) => [HOT[0], HOT[1], HOT[2], Math.round(240 * (1 - f.p) ** 1.5)],
      getLineWidth: (f) => (f.ring ? 3 : 2),
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new IconLayer<Flying>({
      id: 'munition',
      data: flying,
      getPosition: (f) => f.at,
      getIcon: () => ICON,
      getSize: ICON_PX,
      sizeUnits: 'pixels',
      getAngle: (f) => f.angle,
      parameters: onTop,
    }),
    new TextLayer<Flying>({
      id: 'munition-labels',
      data: flying.filter((f) => !f.aloft),
      getPosition: (f) => f.at,
      getText: (f) => f.label,
      getSize: 12,
      getColor: TEXT,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: [ICON_PX / 3, 0],
      fontFamily: FONT,
      fontWeight: 700,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
    new TextLayer<Flying>({
      id: 'munition-aloft',
      data: flying.filter((f) => f.aloft),
      getPosition: (f) => [f.at[0], f.at[1], 0],
      getText: (f) => `▲ ${f.label}`,
      getSize: 11,
      getColor: TEXT,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'bottom',
      getPixelOffset: [0, -8],
      fontFamily: FONT,
      fontWeight: 700,
      background: true,
      getBackgroundColor: [48, 26, 14, 225],
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}
