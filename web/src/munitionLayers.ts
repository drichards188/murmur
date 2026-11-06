// Munitions in flight on the map (bd murmur-64z): from its launch to its impact, each munition at
// its place on the flight (munition.ts) and its height (drawn twice over, as everything is:
// map3d.ts), as what it is (weaponSymbols.ts: a missile, bomb, torpedo or gun round by the
// engagement's weapon kind) in the friendly colours with the anticipated (dashed) look, since none
// of ours reports its position in flight, pointing along its flight on the screen, a short fading
// trail behind it, a thin dashed line on to the impact point, and "HELLFIRE · REAPER 1 > T-12 ·
// 9 s PRED" beside it; a flash at the launch point as it leaves. A gun's last reported burst is
// drawn as tracer lines from the platform to the target (munition-tracers). MapView builds these on every animation frame while one is
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
import { dashesAhead, flightLabel, inFlight, launchFlash, munitionAt, screenAngle, trailPoints, tracerHeads, type BurstFire, type MunitionFlight, type Pt3 } from './munition'
import { weaponIcon, weaponSymbolKind, WEAPON_COLORS } from './weaponSymbols'

type RGBA = [number, number, number, number]
/** the flight is ours, in the friendly blue (weaponSymbols.ts); the strike itself stays the
 *  impact marker's orange (impact.css) */
const FRIEND = WEAPON_COLORS.friend
const PALE = FRIEND.pale
const TEXT: RGBA = [226, 247, 255, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }

/** on-screen height of the weapon's icon (its glow included), CSS pixels */
const ICON_PX = 44

/** metres above mean sea level to rendered metres (map3d.ts) */
const drawn = ([lon, lat, alt]: Pt3): Pt3 => [lon, lat, z(alt)]

interface Flying {
  m: MunitionFlight['m']
  kind: ReturnType<typeof weaponSymbolKind>
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
export function buildMunitionLayers(munitions: MunitionFlight[], nowMs: number, screen: MapScreen, reduced: boolean) {
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const flying: Flying[] = []
  const flashes: Flash[] = []
  const trails: Pt3[][] = []
  const dashes: Pt3[][] = []
  for (const f of munitions) {
    const m = f.m
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
    flying.push({ m, kind: weaponSymbolKind(f.kind), at, angle, label: flightLabel(f, nowMs), aloft: screen.aloft(at[0], at[1], at[2]) })
    const trail = trailPoints(m, nowMs).map(drawn)
    if (trail.length > 1) trails.push(trail)
    for (const [d0, d1] of dashesAhead(m, nowMs)) dashes.push([drawn(d0), drawn(d1)])
  }
  if (!flying.length && !flashes.length) return []
  // smoke at the tail, hot at the munition
  const fade = (k: number, n: number): RGBA => [60 + Math.round(136 * (k / n)), 170 + Math.round(72 * (k / n)), 255, Math.round(235 * (k / n))]
  return [
    new PathLayer<Pt3[]>({
      id: 'munition-ahead',
      data: dashes,
      getPath: (d) => d,
      getColor: [PALE[0], PALE[1], PALE[2], 200],
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
      getFillColor: (f) => (f.ring ? [0, 0, 0, 0] : [230, 250, 255, Math.round(255 * (1 - f.p))]),
      getLineColor: (f) => [FRIEND.rgba[0], FRIEND.rgba[1], FRIEND.rgba[2], Math.round(240 * (1 - f.p) ** 1.5)],
      getLineWidth: (f) => (f.ring ? 3 : 2),
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    new IconLayer<Flying>({
      id: 'munition',
      data: flying,
      getPosition: (f) => f.at,
      getIcon: (f) => weaponIcon({ affiliation: 'friend', kind: f.kind, predicted: true }),
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
      getBackgroundColor: [10, 38, 52, 225],
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}

/** a gun's last burst on its way: the firing platform (where it is drawn, rendered metres) and
 *  its target track's fix */
export interface Tracer {
  fire: BurstFire
  from: Pt3
  to: Pt3
}

/** Tracer lines for each gun burst the bridge reported (`engagement.fire`): short bright dashes
 *  that cross from the platform to the target track over the burst, in the friendly blue.
 *  `reduced`: one still line for the burst's duration. Nothing for a burst not reported. */
export function buildTracerLayers(tracers: Tracer[], nowMs: number, reduced: boolean) {
  const lines: Pt3[][] = []
  for (const t of tracers) {
    for (const p of tracerHeads(t.fire, nowMs, reduced)) {
      const at = (f: number): Pt3 => [t.from[0] + (t.to[0] - t.from[0]) * f, t.from[1] + (t.to[1] - t.from[1]) * f, t.from[2] + (t.to[2] - t.from[2]) * f]
      lines.push(reduced ? [at(0), at(1)] : [at(Math.max(0, p - 0.14)), at(p)])
    }
  }
  if (!lines.length) return []
  return [
    new PathLayer<Pt3[]>({
      id: 'munition-tracers',
      data: lines,
      getPath: (d) => d,
      getColor: reduced ? [FRIEND.rgba[0], FRIEND.rgba[1], FRIEND.rgba[2], 170] : [230, 250, 255, 255],
      getWidth: reduced ? 1.5 : 3,
      widthUnits: 'pixels',
      capRounded: true,
      parameters: { depthCompare: 'always' as const, depthWriteEnabled: false },
    }),
  ]
}
