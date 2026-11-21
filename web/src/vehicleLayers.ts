// Vehicles on the map as deck.gl layers (murmur-ica). Replaces one DOM marker per vehicle
// (about 7 elements each, re-laid out by MapLibre on every frame) with a few GPU layers whose
// attributes are rebuilt only when the vehicles, the selection or the declutter change (a
// vehicle's glide, drawn.ts, recomputes only positions, per animation frame, never the other
// attributes). Every vehicle with a position is drawn at its altitude or depth; one without an
// altitude now at the last one it reported, else at the surface, its label saying so
// (heightLabel): never left off the map.
//
//   vehicle-ring*     selection ring around selected vehicles
//   vehicle-group-rings  a flat ring in its group's colour under each member of a group (bd
//                     murmur-4nik), the frame standing on it as a unit on its selection circle;
//                     dashed and dim while its link is stale; never on the frame, whose colour is
//                     its affiliation's
//   vehicle-group-codes  its group's code under that, for each member heard that stands apart from
//                     the rest of its group on the screen (groupScreen.ts); in a pile, the chip
//   vehicles          the 2525 frame, one icon per symbol code and stale state (shared atlas)
//   vehicle-callsigns the callsign right of the frame, and with the full block its height or
//                     depth and (vehicle-types) the model label left of it: only for the vehicles
//                     the label budget placed (declutter.ts), which is bounded by the screen, not
//                     the fleet; the full block for the selected or while few labels are in view
//   vehicle-lost      "LOST 12s" under vehicles whose link is stale, where the budget placed it;
//                     in a run under rules 30 its standing instead (nodeLoss.ts): "SILENT 12s",
//                     "PRESUMED LOST 12s", or "LOST" once an operator confirmed it, grey
//   vehicle-oneway    "ONE-WAY > T-12" ("STRIKING > T-12" from its release) over a platform that is its
//                     own munition, on its way
//                     to its engagement's effect (bd murmur-64z)
//   vehicle-aloft     "▲ HAWK 1 · 18000 m" at the foot of the stem of one drawn above the view
//                     (aloft.ts), its selection ring there too
//
// Picking ('vehicles', 'vehicle-callsigns', 'vehicle-aloft') drives click to select and right
// click to deselect.
import { IconLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { besides, centred, type LabelCandidate, type LabelLevel, type Priority, rightOf } from './declutter'
import { drawnTick, drawnXYZ, lastKnownAlt, smoothing } from './drawn'
import { drawnHeight, HEIGHT_EXAGGERATION } from './map3d'
import { NO_NODE_LOSS, type NodeLossView, staleBadge, standingOf } from './nodeLoss'
import { frameIcon, typeLabel } from './symbols'
import { rgba, type GroupMark } from './swarmsModel'
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
/** rules 30: a loss an operator confirmed (settled: no longer an alarm) */
const SETTLED: RGBA = [92, 102, 114, 255]
/** a platform that is its own munition is ours: the friendly blue (weaponSymbols.ts), deep enough
 *  for white text */
const MUNITION: RGBA = [11, 100, 140, 240]
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

/** "LOST 12s": the badge under a vehicle whose link is stale; under rules 30 its standing
 *  ("SILENT 12s", "PRESUMED LOST 12s", "LOST" once confirmed) */
function lostText(v: Vehicle, nodes: NodeLossView = NO_NODE_LOSS) {
  return staleBadge(standingOf(nodes, v.uid, true), (v.link.since_last_ms ?? 0) / 1000)
}

/** A vehicle's labels as the budget weighs them (declutter.ts): its callsign (`v:<uid>`; the full
 *  block adds its height and model), and its LOST badge (`vl:<uid>`) while its link is stale, at
 *  `at` (where it is seen, CSS px), or at its foot when it is drawn above the view. */
export function vehicleLabelCandidates(v: Vehicle, at: [number, number] | null, aloft: boolean, priority: Priority, force: boolean, nodes?: NodeLossView): LabelCandidate[] {
  const stale = isStale(v)
  const icon = frameIcon(v, stale)
  const out: LabelCandidate[] = aloft
    ? [{ key: `v:${v.uid}`, at, short: centred(`▲ ${callsign(v)}`, 11, -8, 5), full: centred(`▲ ${vehicleLabel(v)}`, 11, -8, 5), priority, force }]
    : [{ key: `v:${v.uid}`, at, short: rightOf(icon.halfW, callsign(v), 12), full: besides(icon.halfW, typeLabel(v), 11, vehicleLabel(v), 12), priority, force }]
  if (stale && !aloft) {
    const badge = centred(lostText(v, nodes), 10, icon.halfH + 4, 5)
    out.push({ key: `vl:${v.uid}`, at, short: badge, full: badge, priority: priority === 4 ? 3 : priority, force })
  }
  return out
}

export interface VehicleLayerInput {
  vehicles: Vehicle[]
  selected: Set<number>
  /** each placed label's level (declutter.ts): absent, none */
  labels: ReadonlyMap<number, LabelLevel>
  /** the LOST badges placed */
  badges: ReadonlySet<number>
  /** drawn above the view (aloft.ts): labelled at the foot of their stem, here the surface's
   *  height under them (rendered metres: the water's 0, the ground's over land) */
  aloft: Map<number, number>
  /** platforms that are their own munition (catalog `expended_by_effect`) and whose engagement is
   *  dispatched: tagged, so the operator sees what is on the way */
  oneWay?: ReadonlyMap<number, string>
  /** rules 30: our vehicles' standing (the review's), for the stale badge */
  nodes?: NodeLossView
  /** the groups (swarms): each member's ring and code */
  groups?: GroupDraw | null
}

/** The groups as the vehicle layers draw them. */
export interface GroupDraw {
  ofUid: ReadonlyMap<number, GroupMark>
  /** the members crowding another of their group on the screen now (groupScreen.ts) */
  crowded: ReadonlySet<number>
  hovered: string | null
  selected: string | null
}

// a member's ring: a flat white ellipse (a light fill, a firm edge) the IconLayer tints with its
// group's colour (mask), under the frame's foot; dashed and unfilled while its link is stale (a
// place it may have left)
const RING_W = 88
const RING_H = 30
const ringSvg = (dashed: boolean) =>
  `<svg xmlns="http://www.w3.org/2000/svg" width="${RING_W}" height="${RING_H}"><ellipse cx="44" cy="15" rx="40" ry="11.5" fill="#fff" fill-opacity="${dashed ? 0 : 0.3}" stroke="#fff" stroke-width="5"${dashed ? ' stroke-dasharray="11 8"' : ''}/></svg>`
const ringIcon = (dashed: boolean) => ({ id: dashed ? 'group-ring-dashed' : 'group-ring', url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(ringSvg(dashed))}`, width: RING_W, height: RING_H, anchorX: RING_W / 2, anchorY: RING_H / 2, mask: true })
const RING_SOLID = ringIcon(false)
const RING_DASHED = ringIcon(true)

/** a member's ring, px high (IconLayer sizes by height): as wide as its frame and a little more */
export function groupRingSize(v: Vehicle): number {
  const icon = frameIcon(v, isStale(v))
  return Math.max(11, Math.min(16, ((2 * icon.halfW + 8) * RING_H) / RING_W))
}
/** where a member's ring sits: under its frame's foot, px below its anchor */
export function groupRingDrop(v: Vehicle): number {
  return frameIcon(v, isStale(v)).halfH - 2
}

interface Parts {
  placed: Vehicle[]
  labelled: Vehicle[]
  /** the full block: the model label left of the frame too */
  typed: Vehicle[]
  ringed: Vehicle[]
  lost: Vehicle[]
  aloft: Vehicle[]
  oneWay: Vehicle[]
  /** each one's tag, by uid */
  oneWayTags: ReadonlyMap<number, string>
  /** members of a group: ringed; and those with their group's code under them */
  members: Vehicle[]
  coded: Vehicle[]
}

// The partition is rebuilt only when its inputs change, so that the animation frames of a
// glide (same vehicles, new drawn positions) hand deck.gl the same data arrays and only
// getPosition is recomputed, not every attribute.
let memo: { vehicles: Vehicle[]; selected: Set<number>; labels: ReadonlyMap<number, LabelLevel>; badges: ReadonlySet<number>; aloft: Map<number, number>; oneWay?: ReadonlyMap<number, string>; groups?: GroupDraw | null; parts: Parts } | null = null

function partition({ vehicles, selected, labels, badges, aloft, oneWay, groups }: VehicleLayerInput): Parts {
  if (memo && memo.vehicles === vehicles && memo.selected === selected && memo.labels === labels && memo.badges === badges && memo.aloft === aloft && memo.oneWay === oneWay && memo.groups === groups) return memo.parts
  const parts: Parts = { placed: [], labelled: [], typed: [], ringed: [], lost: [], aloft: [], oneWay: [], oneWayTags: oneWay ?? new Map(), members: [], coded: [] }
  for (const v of vehicles) {
    // no position, nowhere to draw it (the fleet list still shows it); no altitude is no reason
    if (!v.position) continue
    parts.placed.push(v)
    if (isStale(v) && badges.has(v.uid)) parts.lost.push(v)
    if (selected.has(v.uid)) parts.ringed.push(v)
    const level = labels.get(v.uid) ?? 'none'
    if (aloft.has(v.uid)) {
      if (level !== 'none') parts.aloft.push(v)
    } else if (level !== 'none') {
      parts.labelled.push(v)
      if (level === 'full') parts.typed.push(v)
    }
    if (oneWay?.has(v.uid)) parts.oneWay.push(v)
    const g = groups?.ofUid.get(v.uid)
    if (g && !aloft.has(v.uid)) {
      parts.members.push(v)
      if (!isStale(v) && !groups!.crowded.has(v.uid)) parts.coded.push(v)
    }
  }
  memo = { vehicles, selected, labels, badges, aloft, oneWay, groups, parts }
  return parts
}

export function buildVehicleLayers(input: VehicleLayerInput) {
  const { placed, labelled, typed, ringed, lost, aloft, oneWay, oneWayTags, members, coded } = partition(input)
  const groups = input.groups
  const groupOf = (v: Vehicle) => groups?.ofUid.get(v.uid)
  // a member's ring: its group's colour, full on the hovered or selected group, dim on the others
  // while one is hovered, dim and dashed while its own link is stale
  const ringColor = (v: Vehicle): RGBA => {
    const g = groupOf(v)
    if (!g) return [0, 0, 0, 0]
    const strong = g.id === groups?.hovered || g.id === groups?.selected
    const a = isStale(v) ? (strong ? 200 : 120) : strong ? 255 : groups?.hovered ? 90 : 215
    return [g.rgb[0], g.rgb[1], g.rgb[2], a]
  }
  const groupTrigger = [groups?.hovered, groups?.selected, groups?.ofUid]
  const full = (v: Vehicle) => input.labels.get(v.uid) === 'full'
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
    new IconLayer<Vehicle>({
      id: 'vehicle-group-rings',
      data: members,
      getPosition: at,
      getIcon: (v) => (isStale(v) ? RING_DASHED : RING_SOLID),
      getSize: (v) => groupRingSize(v) + (groupOf(v)?.id === groups?.hovered ? 3 : 0),
      sizeUnits: 'pixels',
      getPixelOffset: (v) => [0, groupRingDrop(v)],
      getColor: ringColor,
      updateTriggers: { ...posTrigger, getColor: groupTrigger, getSize: groupTrigger, getPixelOffset: groupTrigger },
      parameters: onTop,
    }),
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
      id: 'vehicle-group-codes',
      data: coded,
      getPosition: at,
      getText: (v) => groupOf(v)?.code ?? '',
      getSize: 10,
      getColor: (v) => rgba(groupOf(v)?.ink ?? '#000000'),
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (v) => [0, groupRingDrop(v) + groupRingSize(v) / 2 + 1],
      fontFamily: FONT,
      fontWeight: 600,
      background: true,
      getBackgroundColor: (v) => groupOf(v)?.rgb ?? [0, 0, 0, 0],
      backgroundPadding: [3, 1, 3, 1],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: { ...posTrigger, getText: groupTrigger, getColor: groupTrigger, getBackgroundColor: groupTrigger },
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-callsigns',
      data: labelled,
      pickable: true,
      getPosition: at,
      getText: (v) => (full(v) ? vehicleLabel(v) : callsign(v)),
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
      data: typed,
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
      getText: (v) => lostText(v, input.nodes),
      getSize: 10,
      getColor: WHITE,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (v) => [0, frameIcon(v, true).halfH + 4],
      fontFamily: FONT,
      fontWeight: 600,
      background: true,
      getBackgroundColor: (v) => (input.nodes?.lost.has(v.uid) ? SETTLED : BAD),
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: { ...posTrigger, getText: input.nodes, getBackgroundColor: input.nodes },
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-oneway',
      data: oneWay,
      getPosition: at,
      getText: (v) => oneWayTags.get(v.uid) ?? 'ONE-WAY MUNITION',
      getSize: 10,
      getColor: WHITE,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'bottom',
      getPixelOffset: (v) => [0, -(frameIcon(v, isStale(v)).halfH + 4)],
      fontFamily: FONT,
      fontWeight: 700,
      background: true,
      getBackgroundColor: MUNITION,
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
      getText: (v) => `▲ ${full(v) ? vehicleLabel(v) : callsign(v)}`,
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
