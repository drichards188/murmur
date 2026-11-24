// Opponent weapons on the map (weapon.ts, docs/OPPONENT-WEAPONS.md), in the hostile colours of
// weaponSymbols.ts, from the bridge's weapon picture only (never the simulator's truth), each drawn
// only as far as its identity has come (bd murmur-ju1h):
//
//   weapon-estimate   the one line a weapon has, from its launch: the estimated flight, dashed,
//                     only on a basis (a derived target and strike time), or a measured line's
//                     heading a few seconds ahead with no target
//   weapon-flash      the launch flash at the launch fix, and the strike's at the victim, once each
//   weapon-fix-line   the weapon's own fixes joined (from the launch fix), solid, and
//   weapon-fixes      each fix (and the launch fix) with its 1-sigma circle: the evidence behind
//                     the attribution, drawn only for the act the person viewed (its banner's View)
//   weapon            the hostile weapon symbol: kind unknown at the weapon stage, the class's at the
//                     class stage; dashed (predicted) where its place is an estimate, solid at a fix
//   weapon-labels     one short line a weapon, as far as its stage has come (weapon.ts weaponLine):
//                     "missile launch", "ATGM at HUNTER 1, 2 s", "weapon, 243 m/s"; a launch's
//                     placed by the one label budget (declutter.ts) against the other layers', a
//                     flying weapon's moving with it as drawn
//
// The strike on one of ours has no ring or label here: its exception slot and the banner say it
// (ledger 1, the map's weapons). MapView builds these on every animation frame while a weapon is in
// flight or a flash is on, and at the console's tick otherwise; under reduced motion nothing moves
// or flashes: the lines, the fixes and the labels, static.
import { IconLayer, PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { MapScreen } from './aloft'
import { type LabelCandidate, type LabelLevel, textBox } from './declutter'
import { z } from './map3d'
import { dashesAhead, inFlight, LAUNCH_FLASH_MS, munitionAt, screenAngle, type Munition, type Pt3 } from './munition'
import { estimate, FLY_ALT_M, heading, strikeFlash, weaponLine, type WeaponTrack } from './weapon'
import { weaponIcon, weaponSymbolKind, WEAPON_COLORS, type WeaponSymbolKind } from './weaponSymbols'

type RGBA = [number, number, number, number]
const HOSTILE = WEAPON_COLORS.hostile
const RED: [number, number, number] = [HOSTILE.rgba[0], HOSTILE.rgba[1], HOSTILE.rgba[2]]
const PALE: [number, number, number] = [HOSTILE.pale[0], HOSTILE.pale[1], HOSTILE.pale[2]]
const TEXT: RGBA = [255, 226, 226, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }
const rgba = (c: [number, number, number], a: number): RGBA => [c[0], c[1], c[2], Math.max(0, Math.min(255, Math.round(a)))]

const ICON_PX = 40
/** a fix this recent places the symbol at it (observed), ms */
const FIX_HOLD_MS = 2000

const drawn = ([lon, lat, alt]: Pt3): Pt3 => [lon, lat, z(alt)]
const low = (p: [number, number]): Pt3 => drawn([p[0], p[1], FLY_ALT_M])

interface Flying {
  at: Pt3
  angle: number
  label: string
  kind: WeaponSymbolKind
  predicted: boolean
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
interface Label {
  at: Pt3
  label: string
  a: number
  /** a launch's label: placed by the label budget (`w:launch:...`); a flying weapon's moves with
   *  it every frame and is drawn as it is */
  key?: string
  /** its short form, where the budget has room for no more */
  short?: string
}

/** where a weapon's label is drawn from its point, px (below and right of the symbol) */
const LABEL_AT: [number, number] = [ICON_PX / 3, ICON_PX / 2]
const LABEL_PAD: [number, number] = [5, 3]

const alphaAt = (w: WeaponTrack, nowMs: number) => (w.ended_t == null ? 1 : Math.max(0.15, 1 - (nowMs - w.ended_t) / 60_000))

/** The labels that stay where they are: a launch's, once a report ("missile launch"), while its
 *  weapon is at the launch stage and in flight. */
function keyedLabels(ws: WeaponTrack[], nowMs: number, nameOf: (uid: string) => string): Label[] {
  const launches = new Map<string, Label>()
  for (const w of ws) {
    const l = w.launch
    if (l && w.stage === 'launch' && w.status === 'in_flight' && !w.struck) {
      const report = w.report ?? w.id
      launches.set(report, { at: low(l.point), label: weaponLine(w, null, nowMs, nameOf), a: alphaAt(w, nowMs), key: `w:launch:${report}`, short: 'launch' })
    }
  }
  return [...launches.values()]
}

/** The launch labels for the one label budget (declutter.ts, bd murmur-iobd.3): placed or dropped
 *  against our vehicles' and the tracks' labels, never over them; with what waits on the person. */
export function weaponLabelCandidates(ws: WeaponTrack[], nowMs: number, screen: MapScreen, nameOf: (uid: string) => string): LabelCandidate[] {
  return keyedLabels(ws, nowMs, nameOf).map((l) => ({
    key: l.key!,
    at: screen.at(l.at[0], l.at[1], l.at[2]),
    full: textBox([l.label], 12, 'start', 'top', LABEL_AT, LABEL_PAD, 4),
    short: textBox([l.short ?? l.label], 12, 'start', 'top', LABEL_AT, LABEL_PAD, 4),
    priority: 1 as const,
  }))
}

/** The weapons `ws` (weapon.ts shownWeapons) at `nowMs`. `victimOf`: where the vehicle a weapon
 *  heads for was when this console first had its target (the estimate's end), and the struck
 *  vehicle's position; `nameOf`: a vehicle's callsign; `viewed`: the act the person viewed, whose
 *  weapon's fixes are drawn. */
export function buildWeaponLayers(
  ws: WeaponTrack[],
  nowMs: number,
  screen: MapScreen,
  reduced: boolean,
  victimOf: (w: WeaponTrack) => [number, number] | null,
  nameOf: (uid: string) => string,
  viewed: string | null,
  // the label budget's level for a launch's label; undefined for one the last plan did not weigh
  // (drawn in full until the next)
  level: (key: string) => LabelLevel | undefined = () => undefined,
) {
  if (!ws.length) return []
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const dashed: { path: Pt3[]; a: number }[] = []
  const flashes: Flash[] = []
  const flying: Flying[] = []
  const fixLines: { path: Pt3[]; a: number }[] = []
  const fixes: FixAt[] = []
  const labels: Label[] = []
  const alphaOf = (w: WeaponTrack) => (w.ended_t == null ? 1 : Math.max(0.15, 1 - (nowMs - w.ended_t) / 60_000))
  const angleTo = (from: Pt3, to: Pt3): number => {
    const sa = screen.at(from[0], from[1], from[2])
    const sb = screen.at(to[0], to[1], to[2])
    return sa && sb && Math.hypot(sb[0] - sa[0], sb[1] - sa[1]) > 2 ? screenAngle(sb[0] - sa[0], sb[1] - sa[1]) : 180
  }
  const dashLine = (from: Pt3, to: Pt3, a: number, n = 14, into = dashed) => {
    for (let k = 0; k < n; k++) {
      const p = (f: number): Pt3 => [from[0] + (to[0] - from[0]) * f, from[1] + (to[1] - from[1]) * f, from[2] + (to[2] - from[2]) * f]
      into.push({ path: [p(k / n), p((k + 0.5) / n)], a })
    }
  }
  for (const w of ws) {
    if (w.stage === 'plot' && !w.struck) continue // a lone fix is not called a weapon
    // ended ones fade over their last minute
    const a = alphaOf(w)
    const flyingNow = w.status === 'in_flight' && !w.struck
    const kind: WeaponSymbolKind = w.stage === 'class' ? weaponSymbolKind(w.class_symbol) : 'unknown'
    const l = w.launch
    // the launch's flash, once
    if (l && !reduced) {
      const p = (nowMs - l.flash_t) / LAUNCH_FLASH_MS
      if (p >= 0 && p < 1) flashes.push({ at: low(l.point), p, ring: true }, { at: low(l.point), p, ring: false })
    }
    // the estimate, only on a basis
    const victim = victimOf(w)
    const est: Munition | null = estimate(w, victim)
    let placed = false
    if (est) {
      const from = low(est.launch_point)
      const to = low(est.impact_point)
      dashLine(from, to, (inFlight(est, nowMs) ? 0.9 : 0.45) * a)
      if (inFlight(est, nowMs) && !w.struck) {
        const at = drawn(munitionAt(est, reduced ? est.launch_t : nowMs))
        flying.push({ at: reduced ? from : at, angle: angleTo(at, to), kind, predicted: true, label: weaponLine(w, est, nowMs, nameOf) })
        if (!reduced) for (const [d0, d1] of dashesAhead(est, nowMs)) dashed.push({ path: [drawn(d0), drawn(d1)], a })
        placed = true
      }
    }
    // a measured line with no target: its heading a few seconds ahead
    const h = heading(w, nowMs)
    if (h && !placed && w.stage !== 'launch') {
      const from = low(h.from)
      const at = low(h.at)
      dashLine(from, at, 0.8 * a, 4)
      const fresh = w.kin_t != null && nowMs - w.kin_t < 500
      flying.push({ at: reduced ? from : at, angle: angleTo(from, at), kind, predicted: !fresh, label: weaponLine(w, null, nowMs, nameOf) })
      placed = true
    }
    // otherwise at its last fix, while recent
    const last = w.fixes[w.fixes.length - 1]
    if (!placed && flyingNow && last && nowMs - last.t < FIX_HOLD_MS && w.stage !== 'launch') {
      const prev = w.fixes.length > 1 ? w.fixes[w.fixes.length - 2].point : l?.point
      const at = low(last.point)
      flying.push({ at, angle: prev ? angleTo(low(prev), at) : 180, kind, predicted: false, label: weaponLine(w, null, nowMs, nameOf) })
    }
    // the weapon's own fixes, solid: what our radars and cameras actually had, the evidence behind
    // the attribution, for the act the person viewed
    if (viewed != null && w.report === viewed) {
      const pts = w.fixes.map((f) => low(f.point))
      if (l && pts.length) pts.unshift(low(l.point))
      if (pts.length > 1) fixLines.push({ path: pts, a })
      for (const f of w.fixes) fixes.push({ at: low(f.point), sigma: f.sigma_m, a })
      if (l) fixes.push({ at: low(l.point), sigma: l.sigma_m, a })
    }
    if (w.struck && victim) {
      const at = low(victim)
      const p = strikeFlash(w, nowMs)
      if (p != null && !reduced) flashes.push({ at, p, ring: true }, { at, p, ring: false })
    }
  }
  // the launches' labels, each where the label budget placed it, in its short form or none
  for (const k of keyedLabels(ws, nowMs, nameOf)) {
    const lv = k.key ? level(k.key) : undefined
    if (lv === 'none') continue
    labels.push(lv === 'short' ? { ...k, label: k.short ?? k.label } : k)
  }
  return [
    new PathLayer<{ path: Pt3[]; a: number }>({
      id: 'weapon-estimate',
      data: dashed,
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
    new IconLayer<Flying>({
      id: 'weapon',
      data: flying,
      getPosition: (f) => f.at,
      getIcon: (f) => weaponIcon({ affiliation: 'hostile', kind: f.kind, predicted: f.predicted }),
      getSize: ICON_PX,
      sizeUnits: 'pixels',
      getAngle: (f) => f.angle,
      parameters: onTop,
    }),
    new TextLayer<Flying | Label>({
      id: 'weapon-labels',
      data: [...flying, ...labels],
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

/** Whether animation frames are wanted: a weapon flying on its estimate or its heading, or a flash on. */
export function weaponsAnimating(ws: WeaponTrack[], nowMs: number, victimOf: (w: WeaponTrack) => [number, number] | null): boolean {
  return ws.some((w) => {
    const est = estimate(w, victimOf(w))
    const flash = w.launch != null && nowMs - w.launch.flash_t >= 0 && nowMs - w.launch.flash_t < LAUNCH_FLASH_MS
    return (est != null && inFlight(est, nowMs) && !w.struck) || heading(w, nowMs) != null || flash || strikeFlash(w, nowMs) != null
  })
}
