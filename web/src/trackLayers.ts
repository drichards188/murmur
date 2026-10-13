import { IconLayer, LineLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { PickingInfo } from '@deck.gl/core'
import { NO_SCREEN, type MapScreen } from './aloft'
import { HEIGHT_EXAGGERATION } from './map3d'
import type { LostContact, Track, TrackDomain } from './tracks'
import { isTrackStale, kindShort, lostSidc, sidcFrameIcon, trackConfidence, trackFrameIcon, trackOpacity, trackPredicted, trackSigma } from './tracks'
import type { LngLat } from './types'

/** Tracks on the map (Phase C2), as deck.gl layers through MapView's `layers` prop, at their
 *  estimated height or depth. Like vehicleLayers.ts: one shared atlas icon per symbol code and
 *  stale state, labels as text layers, attributes rebuilt only when the data (or the clock
 *  tick that ages the tracks) changes. Per track:
 *
 *   tracks-uncertainty  the 1-sigma ring, in metres, growing with age
 *   tracks-stems        an air or subsurface track with a height or depth: sea level to it
 *   tracks-columns      one without (unranged): a dashed column through its plan position
 *   tracks-leaders      stale: a line from the last fix to the dead-reckoned position
 *   tracks-selection    ring around the selected track
 *   tracks              the 2525 frame with the estimated affiliation (pickable: onClick)
 *   tracks-ids          "T-0001 · 450 m" right of the frame (its height or depth, or "alt ?"),
 *                       tracks-kinds the estimated kind left of it
 *   tracks-aloft        "▲ T-0001 · 9500 m" at the foot of the stem of one drawn above the view
 *                       (aloft.ts)
 *
 *  Symbols fade with age and with falling existence confidence.
 *
 *  The tracker never invents an altitude (DECISIONS.md, 10-07: a 2D radar or a passive sensor
 *  gives none, and sonar no depth), so an aircraft we cannot range is never drawn as a point at
 *  sea level, where it would read as a boat: it is a dashed column from the surface up, fading
 *  as it rises (nothing bounds its height), its symbol on the column at a drawing height
 *  (UNKNOWN_DRAW_M) and its label "alt ?". A subsurface track with no depth the same, down to
 *  the seabed (the terrain hides the column below it), "depth ?". The drawing heights are not
 *  estimates: they keep the symbol off the water and on screen. Its uncertainty ring lies on
 *  the surface, the column's footprint. Surface tracks are at sea level; ground tracks too (no
 *  terrain-referenced altitude yet).
 *
 *  Rules 16 (murmur-533), the lost contacts (tracks the tracker dropped that every engagement
 *  keep-out still counts):
 *
 *   lost-regions        where the keep-out counts each now, growing from its last fix (faint)
 *   lost-columns        an air or subsurface one: its column (it carries no altitude)
 *   lost                its last-known symbol, anticipated and faded, at its last fix
 *   lost-labels         "T-0002 LOST 4 min": out of view since its last fix */

export interface TrackLayerInput {
  tracks: Track[]
  /** rules 16: the engine's lost contacts */
  lost?: LostContact[]
  /** bridge clock, ms (useBridgeClock) */
  nowMs: number
  selected: string | null
  staleAfterS: number
  onClick?: (id: string) => void
  /** what the camera cannot show at its height (aloft.ts) */
  screen?: MapScreen
}

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]

const RING: Record<Track['affiliation'], RGBA> = {
  hostile: [255, 93, 93, 255],
  suspect: [255, 93, 93, 255],
  unknown: [242, 193, 78, 255],
  neutral: [95, 211, 141, 255],
  assumed_friend: [127, 184, 255, 255],
}
const TEXT: RGBA = [215, 221, 228, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const ACCENT: RGBA = [255, 179, 71, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }

/** Where a track with no height or depth is drawn on its column, metres from sea level (a
 *  drawing height, not an estimate: see above). */
export const UNKNOWN_DRAW_M: Partial<Record<TrackDomain, number>> = { air: 300, subsurface: -20 }
/** How far its column is drawn from sea level, metres, in DASHES dashes fading away from the
 *  surface: not a ceiling (an unranged aircraft can be anywhere above), where the drawing stops. */
const COLUMN_END_M: Partial<Record<TrackDomain, number>> = { air: 3000, subsurface: -300 }
const DASHES = 15

function rgba(c: RGBA, alpha: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * alpha)]
}

/** "4 min", "56 s": how long out of view. */
function outOfView(s: number) {
  return s < 120 ? `${Math.round(s)} s` : `${Math.floor(s / 60)} min`
}

/** Whether a track is drawn on a column: an air or subsurface one without an altitude. */
function unranged(domain: TrackDomain, alt: number | null) {
  return alt == null && UNKNOWN_DRAW_M[domain] != null
}

/** A track's height or depth for its map label, as the track's details give it (TracksPanel):
 *  "450 m", "40 m depth"; "alt ?" or "depth ?" when it has none; nothing on the surface or the
 *  ground. */
export function trackHeightLabel(domain: TrackDomain, alt: number | null): string | null {
  if (domain === 'air') return alt == null ? 'alt ?' : `${alt.toFixed(0)} m`
  if (domain === 'subsurface') return alt == null ? 'depth ?' : `${Math.max(0, -alt).toFixed(0)} m depth`
  return null
}

interface Dash {
  from: XYZ
  to: XYZ
  color: RGBA
}

/** A column's dashes at `at`, from sea level to the domain's COLUMN_END_M, fading away from the
 *  surface. */
function columnDashes(at: LngLat, domain: TrackDomain, color: RGBA, alpha: number, out: Dash[]) {
  const end = COLUMN_END_M[domain]
  if (end == null) return
  const step = (end * HEIGHT_EXAGGERATION) / DASHES
  for (let k = 0; k < DASHES; k++) {
    const fade = 1 - k / DASHES
    out.push({ from: [at[0], at[1], k * step], to: [at[0], at[1], (k + 0.6) * step], color: rgba(color, alpha * (0.15 + 0.7 * fade)) })
  }
}

export function buildTrackLayers({ tracks, lost = [], nowMs, selected, staleAfterS, onClick, screen = NO_SCREEN }: TrackLayerInput) {
  /** the symbol's height, rendered metres: its altitude, else its column's drawing height */
  const symZ = (domain: TrackDomain, alt: number | null) => (alt ?? UNKNOWN_DRAW_M[domain] ?? 0) * HEIGHT_EXAGGERATION
  const z = (t: Track) => symZ(t.domain, t.alt_m)
  const at = (t: Track): XYZ => {
    const [lon, lat] = trackPredicted(t, nowMs)
    return [lon, lat, z(t)]
  }
  const stale = (t: Track) => isTrackStale(t, nowMs, staleAfterS)
  // the symbol fades with age and with falling existence confidence
  const alpha = (t: Track) => trackOpacity(t, nowMs) * (0.5 + 0.5 * Math.min(1, trackConfidence(t, nowMs) / 0.6))
  const tint = (t: Track): RGBA => rgba(TEXT, alpha(t))
  const icon = (t: Track) => trackFrameIcon(t, stale(t))
  // ages advance every clock tick: positions, rings, opacities and the stale state follow
  const tick = Math.floor(nowMs / 500)
  const triggers = { getPosition: tick, getRadius: tick, getFillColor: tick, getLineColor: tick, getColor: tick, getIcon: tick, getSize: tick, getText: tick, getPixelOffset: tick }
  // screen-space overlays: never cut by terrain
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const pick = (info: PickingInfo<Track>) => {
    if (info.object && onClick) onClick(info.object.id)
    return !!info.object
  }
  // rules 16: each lost contact's regions now, flattened (they come from the bridge each second)
  type LostRegion = { aff: Track['affiliation']; center: [number, number]; radius_m: number }
  const lostRegions: LostRegion[] = lost.flatMap((l) => l.regions.map((r) => ({ aff: l.affiliation, center: r.center, radius_m: r.radius_m })))
  const lostIcon = (l: LostContact) => sidcFrameIcon(lostSidc(l))
  const lostAt = (l: LostContact): XYZ => [l.position[0], l.position[1], symZ(l.domain, null)]
  // columns and stems, rebuilt each tick (the tracks move along their velocity)
  const columns: Dash[] = []
  const stems: Dash[] = []
  for (const t of tracks) {
    const p = trackPredicted(t, nowMs)
    if (unranged(t.domain, t.alt_m)) columnDashes(p, t.domain, RING[t.affiliation], trackOpacity(t, nowMs), columns)
    else if (t.alt_m != null && (t.domain === 'air' || t.domain === 'subsurface'))
      stems.push({ from: [p[0], p[1], 0], to: [p[0], p[1], z(t)], color: rgba(RING[t.affiliation], 0.6 * trackOpacity(t, nowMs)) })
  }
  // drawn above the view: labelled at the foot of their stem
  const aloft = tracks.filter((t) => {
    const [lon, lat] = trackPredicted(t, nowMs)
    return screen.aloft(lon, lat, z(t))
  })
  const lostColumns: Dash[] = []
  for (const l of lost) if (unranged(l.domain, null)) columnDashes(l.position, l.domain, RING[l.affiliation], 0.45, lostColumns)
  // a column, a stem: lines in the scene (the terrain and the seabed hide what is behind or below them)
  const lines = (id: string, data: Dash[], width: number) =>
    new LineLayer<Dash>({
      id,
      data,
      getSourcePosition: (d) => d.from,
      getTargetPosition: (d) => d.to,
      getColor: (d) => d.color,
      getWidth: width,
      widthUnits: 'pixels',
    })
  return [
    new ScatterplotLayer<LostRegion>({
      id: 'lost-regions',
      data: lostRegions,
      getPosition: (r) => [r.center[0], r.center[1], 0],
      getRadius: (r) => r.radius_m,
      radiusUnits: 'meters',
      stroked: true,
      filled: true,
      getFillColor: (r) => rgba(RING[r.aff], 0.035),
      getLineColor: (r) => rgba(RING[r.aff], 0.35),
      getLineWidth: 1,
      lineWidthUnits: 'pixels',
      parameters: onTop,
    }),
    lines('lost-columns', lostColumns, 1.5),
    new IconLayer<LostContact>({
      id: 'lost',
      data: lost,
      billboard: true,
      getPosition: lostAt,
      getIcon: lostIcon,
      getSize: (l) => lostIcon(l).size,
      sizeUnits: 'pixels',
      getColor: [255, 255, 255, 110],
      parameters: onTop,
    }),
    new TextLayer<LostContact>({
      id: 'lost-labels',
      data: lost,
      getPosition: lostAt,
      getText: (l) => {
        const h = trackHeightLabel(l.domain, null)
        return `${l.id} LOST ${outOfView(l.out_of_view_s)}${h ? ` · ${h}` : ''}`
      },
      getSize: 10,
      getColor: rgba(TEXT, 0.6),
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: (l) => [lostIcon(l).halfW + 4, 0],
      fontFamily: FONT,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      parameters: onTop,
    }),
    new ScatterplotLayer<Track>({
      id: 'tracks-uncertainty',
      data: tracks,
      // on the surface for a track on a column (its footprint), else around the symbol
      getPosition: (t) => {
        const [lon, lat] = trackPredicted(t, nowMs)
        return [lon, lat, unranged(t.domain, t.alt_m) ? 0 : z(t)]
      },
      getRadius: (t) => trackSigma(t, nowMs),
      radiusUnits: 'meters',
      radiusMinPixels: 6,
      stroked: true,
      filled: true,
      getFillColor: (t) => rgba(RING[t.affiliation], 0.08 * trackOpacity(t, nowMs)),
      getLineColor: (t) => rgba(RING[t.affiliation], 0.55 * trackOpacity(t, nowMs)),
      getLineWidth: (t) => (t.id === selected ? 2 : 1),
      lineWidthUnits: 'pixels',
      updateTriggers: { ...triggers, getLineWidth: selected },
      parameters: onTop,
    }),
    lines('tracks-stems', stems, 1.5),
    lines('tracks-columns', columns, 2),
    new LineLayer<Track>({
      id: 'tracks-leaders',
      data: tracks.filter((t) => stale(t) && t.velocity_mps),
      getSourcePosition: (t) => [t.position[0], t.position[1], z(t)],
      getTargetPosition: at,
      getColor: (t) => rgba(RING[t.affiliation], 0.5 * trackOpacity(t, nowMs)),
      getWidth: 1,
      widthUnits: 'pixels',
      updateTriggers: { getTargetPosition: tick, getColor: tick },
      parameters: onTop,
    }),
    new ScatterplotLayer<Track>({
      id: 'tracks-selection',
      data: tracks.filter((t) => t.id === selected),
      getPosition: at,
      getRadius: 22,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: false,
      getLineColor: ACCENT,
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      updateTriggers: { getPosition: tick },
      parameters: onTop,
    }),
    new IconLayer<Track>({
      id: 'tracks',
      data: tracks,
      pickable: true,
      billboard: true,
      getPosition: at,
      getIcon: icon,
      getSize: (t) => icon(t).size,
      sizeUnits: 'pixels',
      getColor: (t) => [255, 255, 255, Math.round(255 * alpha(t))],
      onClick: pick,
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new TextLayer<Track>({
      id: 'tracks-ids',
      data: tracks,
      pickable: true,
      getPosition: at,
      getText: (t) => {
        const h = trackHeightLabel(t.domain, t.alt_m)
        return h ? `${t.id} · ${h}` : t.id
      },
      getSize: 11,
      getColor: tint,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: (t) => [icon(t).halfW + 4, 0],
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      onClick: pick,
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new TextLayer<Track>({
      id: 'tracks-aloft',
      data: aloft,
      pickable: true,
      getPosition: (t) => {
        const [lon, lat] = trackPredicted(t, nowMs)
        return [lon, lat, 0]
      },
      getText: (t) => `▲ ${t.id} · ${trackHeightLabel(t.domain, t.alt_m)}`,
      getSize: 11,
      getColor: tint,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'bottom',
      getPixelOffset: [0, -8],
      fontFamily: FONT,
      fontWeight: 600,
      background: true,
      getBackgroundColor: [16, 22, 29, 210],
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      onClick: pick,
      updateTriggers: triggers,
      parameters: onTop,
    }),
    new TextLayer<Track>({
      id: 'tracks-kinds',
      data: tracks.filter((t) => t.identity.kind != null),
      getPosition: at,
      getText: (t) => kindShort(t.identity.kind) ?? '',
      getSize: 10,
      getColor: tint,
      getTextAnchor: 'end',
      getAlignmentBaseline: 'center',
      getPixelOffset: (t) => [-(icon(t).halfW + 4), 0],
      fontFamily: FONT,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: triggers,
      parameters: onTop,
    }),
  ]
}
