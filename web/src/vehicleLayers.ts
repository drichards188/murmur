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
//                     depth: only for the vehicles the label budget placed (declutter.ts), which
//                     is bounded by the screen, not the fleet; the full block only for the
//                     selected and the one whose details are open (hover shows it for the rest)
//   vehicle-exception its one exception slot under the symbol (vehicleException.ts), in the same
//                     words as its fleet row, where the budget placed it: "LOST 12s" while its
//                     link is stale (under rules 30 its standing: "SILENT 12s", "PRESUMED LOST
//                     12s", or "LOST" once an operator confirmed it, grey), "DAMAGED: CANNOT
//                     MOVE", "LINK DOWN", "OUT OF ROUNDS", "LINK DEGRADED"
//   vehicle-oneway    "striking T-0012" over a platform that is its own munition, on its way to
//                     its engagement's effect (bd murmur-64z)
//   vehicle-aloft     "▲ HAWK 1 · 18000 m" at the foot of the stem of one drawn above the view
//                     (aloft.ts), its selection ring there too
//
// Picking ('vehicles', 'vehicle-callsigns', 'vehicle-aloft') drives click to select and right
// click to deselect.
import { IconLayer, ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import { type Box, centred, type LabelCandidate, type LabelLevel, type Priority, rightOf } from './declutter'
import { drawnTick, drawnXYZ, lastKnownAlt } from './drawn'
import { HEIGHT_EXAGGERATION, NO_NEAR_GROWTH } from './map3d'
import { emuCause } from './linkEmu'
import { exceptionOf, exceptionPriority, type ExceptionContext, type ExceptionTone, NO_EXCEPTIONS, type VehicleException } from './vehicleException'
import { frameIcon } from './symbols'
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

/** a node's id as the map shows it under its callsign: its fleet uid, the id the Fleet list and the
 *  orders name it by */
export function idLine(v: Pick<Vehicle, 'uid'>): string {
  return `id ${v.uid}`
}
/** px one id line adds to a vehicle label's box */
const ID_LINE_PX = 11

/** Callsign and height or depth: "PUMA 2 · 120 m", "AUV 1 · 35.0 m dp", "MAGURA 1". */
export function vehicleLabel(v: Vehicle): string {
  const h = heightLabel(v)
  return h ? `${callsign(v)} · ${h}` : callsign(v)
}

/** A vehicle's labels as the budget weighs them (declutter.ts): its callsign (`v:<uid>`; the full
 *  block, its height or depth too, only for one `focused`: selected or its details open), and its
 *  exception slot (`vl:<uid>`) when something is wrong with it (vehicleException.ts), at `at`
 *  (where it is seen, CSS px), or at its foot when it is drawn above the view. */
export function vehicleLabelCandidates(v: Vehicle, at: [number, number] | null, aloft: boolean, priority: Priority, focused: boolean, exceptions?: ExceptionContext): LabelCandidate[] {
  const stale = isStale(v)
  const icon = frameIcon(v, stale)
  const text = focused ? vehicleLabel(v) : callsign(v)
  // the node's id sits on a line under its callsign (idLine), so each box is a line taller
  const aloftBox = (t: string): Box => { const b = centred(t, 11, -8, 5); return [b[0], b[1] - ID_LINE_PX, b[2], b[3]] }
  const rightBox = (t: string): Box => { const b = rightOf(icon.halfW, t, 12); return [b[0], b[1], b[2], b[3] + ID_LINE_PX] }
  const out: LabelCandidate[] = aloft
    ? [{ key: `v:${v.uid}`, at, short: aloftBox(`▲ ${callsign(v)}`), full: aloftBox(`▲ ${text}`), priority, force: focused }]
    : [{ key: `v:${v.uid}`, at, short: rightBox(callsign(v)), full: rightBox(text), priority, force: focused }]
  const x = aloft ? null : exceptionOf(v, exceptions ?? NO_EXCEPTIONS, emuCause)
  if (x) {
    const badge = centred(x.text, 10, icon.halfH + 4, 5)
    out.push({ key: `vl:${v.uid}`, at, short: badge, full: badge, priority: exceptionPriority(x, priority), force: focused })
  }
  return out
}

/** the slot's background by its tone: the alarm red, amber, a settled grey */
const TONE_BG: Record<ExceptionTone, RGBA> = { bad: BAD, warn: [242, 193, 78, 255], settled: SETTLED }
const INK_DARK: RGBA = [26, 18, 6, 255]

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
  /** what each one's exception slot is made from (vehicleException.ts) */
  exceptions?: ExceptionContext
  /** the vehicle whose details are open: its label is the full block, as a selected one's */
  follow?: number | null
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
  const parts: Parts = { placed: [], labelled: [], ringed: [], lost: [], aloft: [], oneWay: [], oneWayTags: oneWay ?? new Map(), members: [], coded: [] }
  for (const v of vehicles) {
    // no position, nowhere to draw it (the fleet list still shows it); no altitude is no reason
    if (!v.position) continue
    parts.placed.push(v)
    if (badges.has(v.uid)) parts.lost.push(v)
    if (selected.has(v.uid)) parts.ringed.push(v)
    const level = labels.get(v.uid) ?? 'none'
    if (aloft.has(v.uid)) {
      if (level !== 'none') parts.aloft.push(v)
    } else if (level !== 'none') parts.labelled.push(v)
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
  const { placed, labelled, ringed, lost, aloft, oneWay, oneWayTags, members, coded } = partition(input)
  // the exception slots the budget placed, as they stand now (a few: those with something wrong)
  const slots = new Map<number, VehicleException>()
  for (const v of lost) {
    const x = exceptionOf(v, input.exceptions ?? NO_EXCEPTIONS, emuCause)
    if (x) slots.set(v.uid, x)
  }
  const slotted = slots.size === lost.length ? lost : lost.filter((v) => slots.has(v.uid))
  const slotKey = [...slots].map(([u, x]) => `${u}:${x.tone}:${x.text}`).join('|')
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
  // the full block (height or depth) only for the selected and the one whose details are open
  const full = (v: Vehicle) => input.labels.get(v.uid) === 'full' && (input.selected.has(v.uid) || input.follow === v.uid)
  // where each vehicle is drawn: its glide (drawn.ts), else its report
  const at = (v: Vehicle, info?: { target?: number[] }): XYZ => drawnXYZ(v, info?.target, HEIGHT_EXAGGERATION) as XYZ
  // the foot of its stem, on the surface under it
  const foot = (v: Vehicle, info?: { target?: number[] }): XYZ => {
    const p = drawnXYZ(v, info?.target, 0) as XYZ
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
      extensions: NO_NEAR_GROWTH,
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
      extensions: NO_NEAR_GROWTH,
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
      extensions: NO_NEAR_GROWTH,
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
      extensions: NO_NEAR_GROWTH,
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
      extensions: NO_NEAR_GROWTH,
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
      extensions: NO_NEAR_GROWTH,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-ids',
      data: labelled,
      getPosition: at,
      getText: idLine,
      getSize: 10,
      getColor: (v) => (isStale(v) ? TEXT_STALE : [...TEXT.slice(0, 3), 190] as RGBA),
      getTextAnchor: 'start',
      getAlignmentBaseline: 'center',
      getPixelOffset: (v) => [frameIcon(v, isStale(v)).halfW + 4, 12],
      fontFamily: FONT,
      fontWeight: 500,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: posTrigger,
      extensions: NO_NEAR_GROWTH,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-exception',
      data: slotted,
      getPosition: at,
      getText: (v) => slots.get(v.uid)?.text ?? '',
      getSize: 10,
      getColor: (v) => (slots.get(v.uid)?.tone === 'warn' ? INK_DARK : WHITE),
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (v) => [0, frameIcon(v, isStale(v)).halfH + 4],
      fontFamily: FONT,
      fontWeight: 600,
      background: true,
      getBackgroundColor: (v) => TONE_BG[slots.get(v.uid)?.tone ?? 'bad'],
      backgroundPadding: [5, 3, 5, 3],
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      // the words tick with a silence and change with a report: rebuilt with each new slot list
      updateTriggers: { ...posTrigger, getText: slotKey, getColor: slotKey, getBackgroundColor: slotKey, getPixelOffset: slotKey },
      extensions: NO_NEAR_GROWTH,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-oneway',
      data: oneWay,
      getPosition: at,
      getText: (v) => oneWayTags.get(v.uid) ?? 'striking',
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
      extensions: NO_NEAR_GROWTH,
      parameters: onTop,
    }),
    new TextLayer<Vehicle>({
      id: 'vehicle-aloft',
      data: aloft,
      pickable: true,
      getPosition: foot,
      getText: (v) => `▲ ${full(v) ? vehicleLabel(v) : callsign(v)}\n${idLine(v)}`,
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
      extensions: NO_NEAR_GROWTH,
      parameters: onTop,
    }),
  ]
}
