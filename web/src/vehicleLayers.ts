// Vehicles on the map as deck.gl layers (murmur-ica). Replaces one DOM marker per vehicle
// (about 7 elements each, re-laid out by MapLibre on every frame) with a few GPU layers whose
// attributes are rebuilt only when the vehicles, the selection or the declutter change (a
// vehicle's glide, drawn.ts, recomputes only positions, per animation frame, never the other
// attributes). Every vehicle with a position is drawn at its altitude or depth; one without an
// altitude now at the last one it reported, else at the surface, its label saying so
// (heightLabel): never left off the map.
//
//   vehicle-ring*     selection ring around selected vehicles
//   vehicles          the 2525 frame, one icon per symbol code and stale state (shared atlas)
//   vehicle-callsigns callsign and height or depth right of the frame, vehicle-types the model
//                     label left of it: only for the decluttered labelled set (declutter.ts),
//                     which is bounded by the screen, not the fleet
//   vehicle-lost      "LOST 12s" under vehicles whose link is stale
//   vehicle-aloft     "▲ HAWK 1 · 18000 m" at the foot of the stem of one drawn above the view
//                     (aloft.ts), its selection ring there too
//
// Picking ('vehicles', 'vehicle-callsigns', 'vehicle-aloft') drives click to select and right
// click to deselect.
import { IconLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { drawnTick, drawnXYZ, lastKnownAlt, smoothing } from './drawn'
import { drawnHeight, HEIGHT_EXAGGERATION } from './map3d'
import { frameIcon, typeLabel } from './symbols'
import type { Vehicle } from './types'
import { callsign, domainOf, isStale } from './types'

/** layer ids that stand for a vehicle under the pointer */
export const VEHICLE_PICK_LAYERS = ['vehicles', 'vehicle-callsigns', 'vehicle-aloft']

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]
const TEXT: RGBA = [215, 221, 228, 255]
const TEXT_STALE: RGBA = [215, 221, 228, 140]
const OUTLINE: RGBA = [11, 15, 20, 255]
const ACCENT: RGBA = [255, 179, 71, 255]
const ACCENT_HALO: RGBA = [255, 179, 71, 46]
const BAD: RGBA = [255, 93, 93, 255]
const WHITE: RGBA = [255, 255, 255, 255]
const OPAQUE: RGBA = [255, 255, 255, 255]
const FADED: RGBA = [255, 255, 255, 140]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }

/** A vehicle's height or depth as its map label gives it, in the fleet list's format
 *  (FleetPanel): an aircraft's altitude above mean sea level ("120 m"), a submarine's depth
 *  ("35.0 m dp"); nothing for a surface vessel or a ground vehicle, whose height is the water's
 *  or the ground's. With no altitude reported now it says so ("alt ?", "depth ?"), with the last
 *  one it reported, which it is drawn at (drawn.ts shownAlt); a ground vehicle too, since it is
 *  then drawn at a guess. */
export function heightLabel(v: Vehicle): string | null {
  const d = domainOf(v)
  const fmt = (alt: number) => (d === 'subsurface' ? `${Math.max(0, -alt).toFixed(1)} m dp` : `${alt.toFixed(0)} m`)
  if (v.alt_msl != null) return d === 'air' || d === 'subsurface' ? fmt(v.alt_msl) : null
  if (d === 'surface') return null
  const unknown = d === 'subsurface' ? 'depth ?' : 'alt ?'
  const last = lastKnownAlt(v)
  return last == null ? unknown : `${unknown} (last ${fmt(last)})`
}

/** Callsign and height or depth: "PUMA 2 · 120 m", "AUV 1 · 35.0 m dp", "MAGURA 1". */
export function vehicleLabel(v: Vehicle): string {
  const h = heightLabel(v)
  return h ? `${callsign(v)} · ${h}` : callsign(v)
}

export interface VehicleLayerInput {
  vehicles: Vehicle[]
  selected: Set<number>
  /** drawn without labels (declutter.ts) */
  bare: Set<number>
  /** drawn above the view (aloft.ts): labelled at the foot of their stem, here the surface's
   *  height under them (rendered metres: the water's 0, the ground's over land) */
  aloft: Map<number, number>
}

interface Parts {
  placed: Vehicle[]
  labelled: Vehicle[]
  ringed: Vehicle[]
  lost: Vehicle[]
  aloft: Vehicle[]
}

// The partition is rebuilt only when its inputs change, so that the animation frames of a
// glide (same vehicles, new drawn positions) hand deck.gl the same data arrays and only
// getPosition is recomputed, not every attribute.
let memo: { vehicles: Vehicle[]; selected: Set<number>; bare: Set<number>; aloft: Map<number, number>; parts: Parts } | null = null

function partition({ vehicles, selected, bare, aloft }: VehicleLayerInput): Parts {
  if (memo && memo.vehicles === vehicles && memo.selected === selected && memo.bare === bare && memo.aloft === aloft) return memo.parts
  const parts: Parts = { placed: [], labelled: [], ringed: [], lost: [], aloft: [] }
  for (const v of vehicles) {
    // no position, nowhere to draw it (the fleet list still shows it); no altitude is no reason
    if (!v.position) continue
    parts.placed.push(v)
    if (isStale(v)) parts.lost.push(v)
    if (selected.has(v.uid)) parts.ringed.push(v)
    if (!bare.has(v.uid)) parts.labelled.push(v)
    if (aloft.has(v.uid)) parts.aloft.push(v)
  }
  memo = { vehicles, selected, bare, aloft, parts }
  return parts
}

export function buildVehicleLayers(input: VehicleLayerInput) {
  const { placed, labelled, ringed, lost, aloft } = partition(input)
  // where each vehicle is drawn: its glide (drawn.ts) or, with smoothing off, its report
  const smooth = smoothing()
  const at = smooth
    ? (v: Vehicle, info?: { target?: number[] }): XYZ => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], drawnHeight(v)]
  // the foot of its stem, on the surface under it
  const foot = (v: Vehicle, info?: { target?: number[] }): XYZ => {
    const p = (smooth ? drawnXYZ(v, info?.target, 0) : [v.position![0], v.position![1], 0]) as XYZ
    p[2] = input.aloft.get(v.uid) ?? 0
    return p
  }
  // a selection ring goes where the vehicle is seen: at its foot while it is above the view
  const ringAt = (v: Vehicle, info?: { target?: number[] }) => (input.aloft.has(v.uid) ? foot(v, info) : at(v, info))
  const tint = (v: Vehicle) => (isStale(v) ? TEXT_STALE : TEXT)
  const posTrigger = { getPosition: drawnTick() }
  // symbols and labels are screen-space overlays: never cut by terrain (a buoy's symbol is
  // centred at sea level, so over a shore higher than 0 m half of it would be under the ground)
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  return [
    new ScatterplotLayer<Vehicle>({
      id: 'vehicle-ring-halo',
      data: ringed,
      getPosition: ringAt,
      getRadius: 24.5,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: false,
      getLineColor: ACCENT_HALO,
      getLineWidth: 5,
      lineWidthUnits: 'pixels',
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
    new ScatterplotLayer<Vehicle>({
      id: 'vehicle-ring',
      data: ringed,
      getPosition: ringAt,
      getRadius: 23,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: false,
      getLineColor: ACCENT,
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
    new IconLayer<Vehicle>({
      id: 'vehicles',
      data: placed,
      pickable: true,
      getPosition: at,
      getIcon: (v) => frameIcon(v, isStale(v)),
      getSize: (v) => frameIcon(v, isStale(v)).size,
      sizeUnits: 'pixels',
      getColor: (v) => (isStale(v) ? FADED : OPAQUE),
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-callsigns',
      data: labelled,
      pickable: true,
      getPosition: at,
      getText: vehicleLabel,
      getSize: 12,
      getColor: tint,
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: (v) => [frameIcon(v, isStale(v)).halfW + 4, 0],
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-types',
      data: labelled,
      getPosition: at,
      getText: typeLabel,
      getSize: 11,
      getColor: tint,
      getTextAnchor: 'end',
      getAlignmentBaseline: 'center',
      getPixelOffset: (v) => [-(frameIcon(v, isStale(v)).halfW + 4), 0],
      fontFamily: FONT,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-lost',
      data: lost,
      getPosition: at,
      getText: (v) => `LOST ${Math.floor((v.link.since_last_ms ?? 0) / 1000)}s`,
      getSize: 10,
      getColor: WHITE,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (v) => [0, frameIcon(v, true).halfH + 4],
      fontFamily: FONT,
      fontWeight: 600,
      background: true,
      getBackgroundColor: BAD,
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-aloft',
      data: aloft,
      pickable: true,
      getPosition: foot,
      getText: (v) => `▲ ${vehicleLabel(v)}`,
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
      updateTriggers: posTrigger,
      parameters: onTop,
    }),
  ]
}
