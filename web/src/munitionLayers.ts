// Munitions in flight on the map (bd murmur-64z): from its launch to its impact, each munition at
// its place on the flight (munition.ts) and its height (drawn twice over, as everything is:
// map3d.ts), as what it is (weaponSymbols.ts: a missile, bomb, torpedo or gun round by the
// engagement's weapon kind) in the friendly colours with the anticipated (dashed) look, since none
// of ours reports its position in flight, pointing along its flight on the screen, a thin dashed
// line on to the impact point, and "Hellfire to T-12, 9 s" beside it. No trail behind it and no
// flash at its launch or its strike: motion that asks no response (ledger 1, the map's
// munitions); the effect point's circle (engageLayers.ts) fills once it has struck. A "dive"
// missile (the drone-launcher UGV's rounds, bd murmur-4nik.5) is a quadcopter on its own path over
// the same ends and times (munition.ts divePath: up out of the tube, a cruise, a dive onto the
// point), and its heading, dashes and label follow that path. A gun's last reported burst is drawn
// as tracer lines from the platform to the target (munition-tracers). MapView builds these on every
// animation frame while one is in flight (and at the console's tick under reduced motion), so they
// move as smoothly as the vehicles' glides.
//
//   munition-ahead    the dashes from the munition on to the impact point
//   munition          the missile, turned along its flight as the camera sees it
//   munition-labels   name, target and seconds to impact, right of it; rounds of one weapon from
//                     one launcher at one target (a salvo's) share one label, at the round that
//                     strikes first: "HIV3 ×6 to T-0001, 20–39 s" (munition.ts flightGroupLabel);
//                     each round's own line is its hover tooltip (`full`, munition.ts flightTip)
//   munition-aloft    "▲ Hellfire to T-12, 9 s" at the surface under it while it is above the view
//                     (aloft.ts), as a vehicle there is labelled, grouped the same way
import { IconLayer, PathLayer, TextLayer } from '@deck.gl/layers'
import type { MapScreen } from './aloft'
import { z } from './map3d'
import { dashesAhead, flightFraction, flightGroupKey, flightGroupLabel, flightPath, flightTip, headingPoint, inFlight, munitionAt, screenAngle, tracerHeads, type BurstFire, type FlightPath, type GroundAt, type MunitionFlight, type Pt3 } from './munition'
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

export interface Flying {
  m: MunitionFlight['m']
  f: MunitionFlight
  kind: ReturnType<typeof weaponSymbolKind>
  /** where it is drawn, rendered metres */
  at: Pt3
  angle: number
  /** its group's label on the round that strikes first, else empty (drawn unlabelled) */
  label: string
  /** its own line ("HIV3 from HIVE 1 to T-0001, 23 s"): the tooltip when it is hovered */
  full: string
  /** above the view: labelled at the surface under it */
  aloft: boolean
}

/** Each flight's path (munition.ts flightPath), made once at its first frame from the terrain
 *  then (again once if there was none), so it does not shift while it flies; dropped once it is
 *  no longer drawn. */
const paths = new Map<string, { path: FlightPath; ground: boolean }>()
function pathOf(f: MunitionFlight, ground: GroundAt | undefined): FlightPath {
  const m = f.m
  const key = `${f.kind}|${f.profile}|${m.launch_t}|${m.impact_t}|${m.launch_point}|${m.impact_point}|${m.launch_alt_m}|${m.impact_alt_m}`
  let p = paths.get(key)
  if (!p || (!p.ground && ground)) paths.set(key, (p = { path: flightPath(f, ground), ground: !!ground }))
  return p.path
}

/** The munitions in flight at `nowMs` (engine ms, the bridge's clock) as deck.gl layers. `screen`
 *  says where the camera puts a point (for the missile's turn and what is above the view);
 *  `ground`: the terrain's height where the console has it, which a shaped path keeps above. */
export function buildMunitionLayers(munitions: MunitionFlight[], nowMs: number, screen: MapScreen, ground?: GroundAt) {
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const flying: Flying[] = []
  const dashes: Pt3[][] = []
  const used = new Set<FlightPath>()
  for (const f of munitions) {
    const m = f.m
    const path = pathOf(f, ground)
    used.add(path)
    if (!inFlight(m, nowMs)) continue
    const here = munitionAt(m, nowMs, path)
    const at = drawn(here)
    // its heading on the screen: along its path where it is (a straight one's towards the impact
    // point), or on from the launch point when it is nearly there; a drone popping up under a
    // camera straight above it faces along its track
    const a = screen.at(at[0], at[1], at[2])
    const toward = (q: Pt3) => {
      const b = screen.at(q[0], q[1], q[2])
      return a && b && Math.hypot(b[0] - a[0], b[1] - a[1]) > 2 ? screenAngle(b[0] - a[0], b[1] - a[1]) : null
    }
    const ahead: Pt3 = path.shaped ? headingPoint(path, path.flown(flightFraction(m, nowMs))) : [...m.impact_point, m.impact_alt_m]
    const track: Pt3 | null = path.shaped ? [...m.impact_point, here[2]] : null
    const from = drawn([...m.launch_point, m.launch_alt_m])
    const c = screen.at(from[0], from[1], from[2])
    const angle = toward(drawn(ahead)) ?? (track && toward(drawn(track))) ?? (a && c ? screenAngle(a[0] - c[0], a[1] - c[1]) : 180)
    flying.push({ m, f, kind: weaponSymbolKind(f.kind, f.profile), at, angle, label: '', full: flightTip(f, nowMs), aloft: screen.aloft(at[0], at[1], at[2]) })
    for (const d of dashesAhead(m, nowMs, path)) dashes.push(d.map(drawn))
  }
  for (const [k, p] of paths) if (!used.has(p.path)) paths.delete(k)
  if (!flying.length) return []
  // one label a group (a salvo's rounds: one weapon, launcher and target), at the round that
  // strikes first; those above the view are a group of their own, labelled at the surface
  const groups = new Map<string, Flying[]>()
  for (const x of flying) {
    const k = `${flightGroupKey(x.f)}|${x.aloft}`
    const g = groups.get(k)
    if (g) g.push(x)
    else groups.set(k, [x])
  }
  for (const g of groups.values()) {
    const lead = g.reduce((a, b) => (b.m.impact_t < a.m.impact_t ? b : a))
    lead.label = flightGroupLabel(g.map((x) => x.f), nowMs)
  }
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
    new IconLayer<Flying>({
      id: 'munition',
      data: flying,
      getPosition: (f) => f.at,
      getIcon: (f) => weaponIcon({ affiliation: 'friend', kind: f.kind, predicted: true }),
      getSize: ICON_PX,
      sizeUnits: 'pixels',
      getAngle: (f) => f.angle,
      // hovered, a round shows its own line (MapView's tooltip)
      pickable: true,
      parameters: onTop,
    }),
    new TextLayer<Flying>({
      id: 'munition-labels',
      data: flying.filter((f) => !f.aloft && f.label),
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
      data: flying.filter((f) => f.aloft && f.label),
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
