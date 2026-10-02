import { IconLayer, LineLayer, ScatterplotLayer } from '@deck.gl/layers'
import { HEIGHT_EXAGGERATION } from './map3d'
import type { Track } from './tracks'
import { isTrackStale, trackConfidence, trackOpacity, trackPredicted, trackSigma, trackSymbolSvg } from './tracks'

/** Tracks on the map, as deck.gl layers for both views (2D at ground level, 3D at the
 *  estimated height). Each track: an uncertainty ring (1 sigma now, growing with age), a
 *  leader from the last fix to the dead-reckoned position once stale, and the 2525 symbol,
 *  all fading with age. Kept out of MapView so the vehicle marker work can move
 *  independently; MapView only spreads these into its deck layer list and picks
 *  `tracks-icons` on click. */

export interface TrackView {
  tracks: Track[]
  /** bridge clock, ms (useBridgeClock) */
  nowMs: number
  selected: string | null
  staleAfterS: number
}

export const EMPTY_TRACK_VIEW: TrackView = { tracks: [], nowMs: 0, selected: null, staleAfterS: 10 }

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]

const RING: Record<Track['affiliation'], RGBA> = {
  hostile: [255, 93, 93, 255],
  suspect: [255, 93, 93, 255],
  unknown: [242, 193, 78, 255],
  neutral: [95, 211, 141, 255],
  assumed_friend: [127, 184, 255, 255],
}

function rgba(c: RGBA, alpha: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * alpha)]
}

function iconFor(t: Track, stale: boolean, labels: boolean) {
  const sym = trackSymbolSvg(t, { stale, labels })
  return {
    url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sym.svg)}`,
    // deck.gl caches the atlas entry (image, size, anchor) by id: the rendered size is part
    // of it so a symbol whose labels changed gets a fresh entry, not the old geometry
    id: `track|${t.sidc}|${stale}|${labels}|${t.id}|${t.identity.kind}|${Math.ceil(sym.size.width)}x${Math.ceil(sym.size.height)}`,
    width: Math.ceil(sym.size.width),
    height: Math.ceil(sym.size.height),
    anchorX: sym.anchor.x,
    anchorY: sym.anchor.y,
  }
}

export function trackLayers(view: TrackView, is3d: boolean, visible = true) {
  const { tracks, nowMs, selected, staleAfterS } = view
  const z = (t: Track) => (is3d && t.alt_m != null ? t.alt_m * HEIGHT_EXAGGERATION : 0)
  const at = (t: Track): XYZ => {
    const [lon, lat] = trackPredicted(t, nowMs)
    return [lon, lat, z(t)]
  }
  const stale = (t: Track) => isTrackStale(t, nowMs, staleAfterS)
  const trigger = tracks.map((t) => `${t.id}|${t.last_seen_t}|${stale(t)}|${t.sidc}|${t.identity.kind}`).join()
  // ages advance every clock tick: the ring radius, opacity and prediction follow
  const tick = Math.floor(nowMs / 500)
  return [
    new ScatterplotLayer<Track>({
      id: 'tracks-uncertainty',
      visible,
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
      updateTriggers: { getPosition: tick, getRadius: tick, getFillColor: tick, getLineColor: tick, getLineWidth: selected },
    }),
    // stale: a leader from the last fix to where the track is now dead-reckoned to be
    new LineLayer<Track>({
      id: 'tracks-leaders',
      visible,
      data: tracks.filter((t) => stale(t) && t.velocity_mps),
      getSourcePosition: (t) => [t.position[0], t.position[1], z(t)],
      getTargetPosition: at,
      getColor: (t) => rgba(RING[t.affiliation], 0.5 * trackOpacity(t, nowMs)),
      getWidth: 1,
      widthUnits: 'pixels',
      updateTriggers: { getTargetPosition: tick, getColor: tick },
    }),
    new ScatterplotLayer<Track>({
      id: 'tracks-selection',
      visible,
      data: tracks.filter((t) => t.id === selected),
      getPosition: at,
      getRadius: 22,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: false,
      getLineColor: [255, 179, 71, 255],
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      updateTriggers: { getPosition: tick },
    }),
    new IconLayer<Track>({
      id: 'tracks-icons',
      visible,
      data: tracks,
      pickable: true,
      billboard: true,
      getPosition: at,
      getIcon: (t) => iconFor(t, stale(t), true),
      getSize: (t) => iconFor(t, stale(t), true).height,
      sizeUnits: 'pixels',
      // the symbol fades with age and with falling existence confidence
      getColor: (t) => [255, 255, 255, Math.round(255 * trackOpacity(t, nowMs) * (0.5 + 0.5 * Math.min(1, trackConfidence(t, nowMs) / 0.6)))],
      updateTriggers: { getIcon: trigger, getSize: trigger, getPosition: tick, getColor: tick },
    }),
  ]
}
