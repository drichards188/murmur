// Opponent weapons on the map (weapon.ts, docs/OPPONENT-WEAPONS.md), in the hostile colours of
// weaponSymbols.ts, from the bridge's weapon picture only (never the simulator's truth), each drawn
// only as far as its identity has come (bd murmur-ju1h):
//
//   weapon-bearing    a seen launch's bearing line, from the observer that saw it to the launch
//   weapon-estimate   the estimated flight, dashed, only on a basis (a derived target and strike
//                     time), or a measured line's heading a few seconds ahead with no target
//   weapon-flash      the launch flash at the launch fix, and the strike's at the victim
//   weapon-fix-line   the weapon's own fixes joined (from the launch fix), solid
//   weapon-fixes      each fix (and the launch fix) with its 1-sigma circle, solid
//   weapon            the hostile weapon symbol: kind unknown at the weapon stage, the class's at the
//                     class stage; dashed (predicted) where its place is an estimate, solid at a fix
//   weapon-struck     a ring on the victim once it reports the strike (one a vehicle, at the newest)
//   weapon-labels     what each stage says: "LAUNCH · 270° from HUNTER 1", "WEAPON? · 243 m/s",
//                     "ATGM-class: 250 m/s, from T-0004 · 2.1 s", the strikes on each vehicle once
//                     ("GUN STRUCK HUNTER 1 ×3": a gun's every burst is a weapon), a launch once
//
// MapView builds these on every animation frame while a weapon is in flight or a flash is on, and
// at the console's tick otherwise; under reduced motion nothing moves or flashes: the lines, the
// fixes, the rings and the labels, static.
import { IconLayer, PathLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { MapScreen } from './aloft'
import { z } from './map3d'
import { dashesAhead, inFlight, LAUNCH_FLASH_MS, munitionAt, screenAngle, type Munition, type Pt3 } from './munition'
import { classText, countdownS, estimate, FLY_ALT_M, heading, stageText, strikeFlash, struckGroups, weaponShort, type WeaponTrack } from './weapon'
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
}

/** The weapons `ws` (weapon.ts shownWeapons) at `nowMs`. `victimOf`: where the vehicle a weapon
 *  heads for was when this console first had its target (the estimate's end), and the struck
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
  const dashed: { path: Pt3[]; a: number }[] = []
  const bearings: { path: Pt3[]; a: number }[] = []
  const flashes: Flash[] = []
  const flying: Flying[] = []
  const fixLines: { path: Pt3[]; a: number }[] = []
  const fixes: FixAt[] = []
  const labels: Label[] = []
  // a launch labelled once a report (a gun's bursts in flight together share their act's)
  const launchLabels = new Map<string, Label>()
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
    // the launch: its fix, the bearing from the observer, its flash
    if (l) {
      if (l.observer) dashLine(low(l.observer.point), low(l.point), a, 20, bearings)
      if (!reduced) {
        const p = (nowMs - l.flash_t) / LAUNCH_FLASH_MS
        if (p >= 0 && p < 1) flashes.push({ at: low(l.point), p, ring: true }, { at: low(l.point), p, ring: false })
      }
      if (w.stage === 'launch' && flyingNow) launchLabels.set(w.report ?? w.id, { at: low(l.point), label: `LAUNCH · ${stageText(w, nameOf).replace(/^launch, /, '')}`, a })
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
        const c = countdownS(est, w, nowMs)
        const what = w.stage === 'class' ? (classText(w) ?? weaponShort(w.class)) : 'WEAPON?'
        flying.push({ at: reduced ? from : at, angle: angleTo(at, to), kind, predicted: true, label: `EST. ${what} · ${c == null ? '' : c > 0 ? `${c.toFixed(1)} s` : 'due'}` })
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
      flying.push({ at: reduced ? from : at, angle: angleTo(from, at), kind, predicted: !fresh, label: labelOf(w, nameOf) })
      placed = true
    }
    // otherwise at its last fix, while recent
    const last = w.fixes[w.fixes.length - 1]
    if (!placed && flyingNow && last && nowMs - last.t < FIX_HOLD_MS && w.stage !== 'launch') {
      const prev = w.fixes.length > 1 ? w.fixes[w.fixes.length - 2].point : l?.point
      const at = low(last.point)
      flying.push({ at, angle: prev ? angleTo(low(prev), at) : 180, kind, predicted: false, label: labelOf(w, nameOf) })
    }
    // the weapon's own fixes, solid: what our radars and cameras actually had
    const pts = w.fixes.map((f) => low(f.point))
    if (l && pts.length) pts.unshift(low(l.point))
    if (pts.length > 1) fixLines.push({ path: pts, a })
    for (const f of w.fixes) fixes.push({ at: low(f.point), sigma: f.sigma_m, a })
    if (l) fixes.push({ at: low(l.point), sigma: l.sigma_m, a })
    if (w.struck && victim) {
      const at = low(victim)
      const p = strikeFlash(w, nowMs)
      if (p != null && !reduced) flashes.push({ at, p, ring: true }, { at, p, ring: false })
    }
  }
  labels.push(...launchLabels.values())
  // the strikes on each vehicle, once: its ring and label where the newest struck
  const struckRings: Label[] = []
  for (const g of struckGroups(ws, nameOf)) {
    const victim = victimOf(g.newest)
    if (!victim) continue
    const s = { at: low(victim), label: g.label, a: alphaOf(g.newest) }
    struckRings.push(s)
    labels.push(s)
  }
  return [
    new PathLayer<{ path: Pt3[]; a: number }>({
      id: 'weapon-bearing',
      data: bearings,
      getPath: (d) => d.path,
      getColor: (d) => rgba(PALE, 150 * d.a),
      getWidth: 1.5,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
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
    new ScatterplotLayer<Label>({
      id: 'weapon-struck',
      data: struckRings,
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

/** the symbol's label without an estimate: "WEAPON? · 243 m/s", "ATGM-class: 250 m/s, from T-0004" */
function labelOf(w: WeaponTrack, nameOf: (uid: string) => string): string {
  if (w.stage === 'class') return classText(w) ?? stageText(w, nameOf)
  return `WEAPON?${w.speed_mps != null ? ` · ${Math.round(w.speed_mps)} m/s` : ''}${w.track ? ` · ${w.track}` : ''}`
}

/** Whether animation frames are wanted: a weapon flying on its estimate or its heading, or a flash on. */
export function weaponsAnimating(ws: WeaponTrack[], nowMs: number, victimOf: (w: WeaponTrack) => [number, number] | null): boolean {
  return ws.some((w) => {
    const est = estimate(w, victimOf(w))
    const flash = w.launch != null && nowMs - w.launch.flash_t >= 0 && nowMs - w.launch.flash_t < LAUNCH_FLASH_MS
    return (est != null && inFlight(est, nowMs) && !w.struck) || heading(w, nowMs) != null || flash || strikeFlash(w, nowMs) != null
  })
}
