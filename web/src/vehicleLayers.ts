// Vehicles on the map as deck.gl layers, in 2D and 3D (murmur-ica). Replaces one DOM marker
// per vehicle (about 7 elements each, re-laid out by MapLibre on every frame) with a few GPU
// layers whose attributes are rebuilt only when the vehicles, the selection or the declutter
// change, never per frame:
//
//   vehicle-heading   heading arrow (2D, labelled vehicles), one rotated icon for every heading
//   vehicle-ring*     selection ring around selected vehicles
//   vehicles          the 2525 frame, one icon per symbol code and stale state (shared atlas)
//   vehicle-callsigns callsign right of the frame, vehicle-types the model label left of it:
//                     only for the decluttered labelled set (declutter.ts), which is bounded
//                     by the screen, not the fleet
//   vehicle-lost      "LOST 12s" under vehicles whose link is stale
//
// Picking ('vehicles', 'vehicle-callsigns') drives click to select and right click to deselect.
import { IconLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { drawnAlt, drawnHeading, drawnPosition, drawnTick, smoothing } from './drawn'
import { HEIGHT_EXAGGERATION } from './map3d'
import { ARROW_ICON, frameIcon, typeLabel } from './symbols'
import type { Vehicle } from './types'
import { callsign, isStale } from './types'

/** layer ids that stand for a vehicle under the pointer */
export const VEHICLE_PICK_LAYERS = ['vehicles', 'vehicle-callsigns']

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

export interface VehicleLayerInput {
  vehicles: Vehicle[]
  selected: Set<number>
  /** drawn without labels (declutter.ts) */
  bare: Set<number>
  is3d: boolean
}

interface Parts {
  placed: Vehicle[]
  labelled: Vehicle[]
  ringed: Vehicle[]
  lost: Vehicle[]
  headed: Vehicle[]
}

// The partition is rebuilt only when its inputs change, so that the animation frames of a
// glide (same vehicles, new drawn positions) hand deck.gl the same data arrays and only
// getPosition and getAngle are recomputed, not every attribute.
let memo: { vehicles: Vehicle[]; selected: Set<number>; bare: Set<number>; is3d: boolean; parts: Parts } | null = null

function partition({ vehicles, selected, bare, is3d }: VehicleLayerInput): Parts {
  if (memo && memo.vehicles === vehicles && memo.selected === selected && memo.bare === bare && memo.is3d === is3d) return memo.parts
  const parts: Parts = { placed: [], labelled: [], ringed: [], lost: [], headed: [] }
  for (const v of vehicles) {
    if (!v.position || (is3d && v.alt_msl == null)) continue
    parts.placed.push(v)
    const stale = isStale(v)
    if (stale) parts.lost.push(v)
    if (selected.has(v.uid)) parts.ringed.push(v)
    if (!bare.has(v.uid)) {
      parts.labelled.push(v)
      if (!is3d && v.heading != null) parts.headed.push(v)
    }
  }
  memo = { vehicles, selected, bare, is3d, parts }
  return parts
}

export function buildVehicleLayers(input: VehicleLayerInput) {
  const { is3d } = input
  const { placed, labelled, ringed, lost, headed } = partition(input)
  // where each vehicle is drawn: its glide (drawn.ts) or, with smoothing off, its report
  const smooth = smoothing()
  const at = smooth
    ? (v: Vehicle): XYZ => {
        const p = drawnPosition(v)
        return [p[0], p[1], is3d ? drawnAlt(v) * HEIGHT_EXAGGERATION : 0]
      }
    : (v: Vehicle): XYZ => [v.position![0], v.position![1], is3d ? (v.alt_msl ?? 0) * HEIGHT_EXAGGERATION : 0]
  const angle = (v: Vehicle) => -((smooth ? drawnHeading(v) : v.heading) ?? 0)
  const tint = (v: Vehicle) => (isStale(v) ? TEXT_STALE : TEXT)
  const posTrigger = { getPosition: smooth ? `${is3d}:${drawnTick()}` : is3d }
  // symbols and labels are screen-space overlays: never cut by terrain or the sea surface
  // (a buoy's symbol is centred at sea level, so half of it would be under the water)
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  return [
    new IconLayer<Vehicle>({
      id: 'vehicle-heading',
      visible: !is3d,
      data: headed,
      getPosition: at,
      getIcon: () => ARROW_ICON,
      getSize: ARROW_ICON.size,
      sizeUnits: 'pixels',
      // flat on the map, so the arrow turns with the map's bearing
      billboard: false,
      getAngle: angle,
      getColor: (v) => (isStale(v) ? FADED : OPAQUE),
      updateTriggers: smooth ? { ...posTrigger, getAngle: drawnTick() } : posTrigger,
      parameters: onTop,
    }),
    new ScatterplotLayer<Vehicle>({
      id: 'vehicle-ring-halo',
      data: ringed,
      getPosition: at,
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
      getPosition: at,
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
      getText: (v) => callsign(v),
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
  ]
}
