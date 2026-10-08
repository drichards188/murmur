import { IconLayer, LineLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { PickingInfo } from '@deck.gl/core'
import { HEIGHT_EXAGGERATION } from './map3d'
import type { ViewMode } from './MapView'
import type { LostContact, Track } from './tracks'
import { isTrackStale, kindShort, lostSidc, sidcFrameIcon, trackConfidence, trackFrameIcon, trackOpacity, trackPredicted, trackSigma } from './tracks'

/** Tracks on the map (Phase C2), as deck.gl layers through MapView's `layers` prop, in 2D at
 *  ground level and in 3D at the estimated height. Like vehicleLayers.ts: one shared atlas
 *  icon per symbol code and stale state, labels as text layers, attributes rebuilt only
 *  when the data (or the clock tick that ages the tracks) changes. Per track:
 *
 *   tracks-uncertainty  the 1-sigma ring, in metres, growing with age
 *   tracks-leaders      stale: a line from the last fix to the dead-reckoned position
 *   tracks-selection    ring around the selected track
 *   tracks              the 2525 frame with the estimated affiliation (pickable: onClick)
 *   tracks-ids          T-0001 right of the frame, tracks-kinds the estimated kind left of it
 *
 *  Symbols fade with age and with falling existence confidence.
 *
 *  Rules 16 (murmur-533), the lost contacts (tracks the tracker dropped that every engagement
 *  keep-out still counts):
 *
 *   lost-regions        where the keep-out counts each now, growing from its last fix (faint)
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

function rgba(c: RGBA, alpha: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * alpha)]
}

/** "4 min", "56 s": how long out of view. */
function outOfView(s: number) {
  return s < 120 ? `${Math.round(s)} s` : `${Math.floor(s / 60)} min`
}

export function buildTrackLayers({ tracks, lost = [], nowMs, selected, staleAfterS, onClick }: TrackLayerInput, view: ViewMode) {
  const is3d = view === '3d'
  const z = (t: Track) => (is3d && t.alt_m != null ? t.alt_m * HEIGHT_EXAGGERATION : 0)
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
  // screen-space overlays: never cut by terrain or the sea surface
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  const pick = (info: PickingInfo<Track>) => {
    if (info.object && onClick) onClick(info.object.id)
    return !!info.object
  }
  // rules 16: each lost contact's regions now, flattened (they come from the bridge each second)
  type LostRegion = { aff: Track['affiliation']; center: [number, number]; radius_m: number }
  const lostRegions: LostRegion[] = lost.flatMap((l) => l.regions.map((r) => ({ aff: l.affiliation, center: r.center, radius_m: r.radius_m })))
  const lostIcon = (l: LostContact) => sidcFrameIcon(lostSidc(l))
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
    new IconLayer<LostContact>({
      id: 'lost',
      data: lost,
      billboard: true,
      getPosition: (l) => [l.position[0], l.position[1], 0],
      getIcon: lostIcon,
      getSize: (l) => lostIcon(l).size,
      sizeUnits: 'pixels',
      getColor: [255, 255, 255, 110],
      parameters: onTop,
    }),
    new TextLayer<LostContact>({
      id: 'lost-labels',
      data: lost,
      getPosition: (l) => [l.position[0], l.position[1], 0],
      getText: (l) => `${l.id} LOST ${outOfView(l.out_of_view_s)}`,
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
      getPosition: at,
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
      getText: (t) => t.id,
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
