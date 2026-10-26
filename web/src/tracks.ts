import ms from 'milsymbol'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, StepUpNeeded } from './decisions'
import type { FlagKind, RuleEvidence } from './ident'
import type { LngLat } from './types'

/** Tracks (Phase C2): contacts that are not ours, as the bridge's sensors see them. The
 *  WebSocket `{"type":"tracks"}` message and GET /api/tracks carry the estimate as of
 *  `last_seen_t`; the console carries it forward (position along the velocity, the
 *  uncertainty growing at `sigma_growth_mps`, confidence decaying) so the message only
 *  changes when a track does. Everything here is perception: there is no truth in it. */

export type Affiliation = 'unknown' | 'suspect' | 'hostile' | 'neutral' | 'assumed_friend'
export type SensorType = 'radar' | 'eo_ir' | 'ais' | 'sonar' | 'esm'
export type TrackDomain = 'air' | 'surface' | 'subsurface' | 'ground'

export interface TrackSource {
  uid: number
  sensor: SensorType
  hits: number
  last_t: number
}

export interface Track {
  id: string
  domain: TrackDomain
  position: LngLat
  alt_m: number | null
  velocity_mps: [number, number] | null // east, north
  speed_mps: number
  course_deg: number | null
  pos_sigma_m: number
  sigma_growth_mps: number
  confidence: number
  confidence_decay_s: number
  affiliation: Affiliation
  affiliation_confidence: number
  affiliation_reason: string
  identity: {
    kind: string | null
    /** how specific `kind` is: a size class (or from ESM alone the domain), a family, or a kind */
    level?: IdentityLevel | null
    confidence: number
    ais: { mmsi: string; name: string; ship_type: string } | null
    conflict: string | null
    supported_by: SensorType[]
  }
  sources: TrackSource[]
  first_seen_t: number
  last_seen_t: number
  hits: number
  state: 'active' | 'stale'
  /** the declaration in force (rules 9: the engine's ledger, an operator's or the
   *  hostile-act rule's, with its basis), mirrored for display */
  declared: TrackDeclared | null
  /** rules 9: the review flags in force on the track (never blocking) */
  review?: { flag: number; kind: FlagKind; status: string; t: number; evidence: string }[]
  sidc: string
}

export interface TrackDeclared {
  affiliation: Affiliation
  /** an operator, or `rule:hostile_act` */
  by: string
  t: number
  note: string | null
  id?: number
  /** declared by the engine's hostile-act rule */
  rule?: boolean
  evidence?: RuleEvidence
  /** the operator who confirmed a rule declaration (absent: not confirmed) */
  confirmed_by?: string
  /** the basis in a sentence */
  basis?: string
}

/** Rules 16 (murmur-533): a contact the tracker dropped (no detection for 180 s) that every
 *  engagement's keep-out still counts, from its last fix (`position` at `since_t`), until an
 *  operator confirms a track carrying its AIS as it (a claim in the review list) or an
 *  operator with engagement authority accepts it for one engagement. `regions`: where the keep-out counts it now (refreshed every
 *  second; they only grow). Times are engine ms. */
export interface LostContact {
  id: string
  position: LngLat
  since_t: number
  dropped_t: number
  out_of_view_s: number
  domain: TrackDomain
  identity: string | null
  affiliation: Affiliation
  declared_by: string | null
  velocity_mps: [number, number] | null
  ais: { mmsi: string; name: string; ship_type?: string } | null
  regions: { kind: string; center: LngLat; radius_m: number }[]
}

export interface TracksMessage {
  type: 'tracks'
  t: number
  stale_after_s: number
  tracks: Track[]
  /** rules 16: the engine's lost contacts */
  lost?: LostContact[]
}

/** Rules 16: a lost contact's map symbol: its affiliation and domain, anticipated (dashed),
 *  no function (what it is, it was). */
export function lostSidc(l: LostContact): string {
  const aff = { unknown: 'U', suspect: 'S', hostile: 'H', neutral: 'N', assumed_friend: 'A' }[l.affiliation] ?? 'U'
  const dim = { air: 'A', surface: 'S', subsurface: 'U', ground: 'G' }[l.domain] ?? 'S'
  return `S${aff}${dim}A-----------`
}

export const AFFILIATION_LABEL: Record<Affiliation, string> = {
  hostile: 'Hostile',
  suspect: 'Suspect',
  unknown: 'Unknown',
  neutral: 'Neutral',
  assumed_friend: 'Assumed friend',
}

/** Sort order for the list and the badge: what needs a look first. */
export const AFFILIATION_RANK: Record<Affiliation, number> = { hostile: 0, suspect: 1, unknown: 2, neutral: 3, assumed_friend: 4 }

export const SENSOR_LABEL: Record<SensorType, string> = { radar: 'radar', eo_ir: 'EO/IR', ais: 'AIS', sonar: 'sonar', esm: 'ESM' }

export const KIND_LABEL: Record<string, string> = {
  cargo_ship: 'cargo ship',
  tanker: 'tanker',
  ferry: 'ferry',
  patrol_boat: 'patrol boat',
  fishing_vessel: 'fishing vessel',
  sailboat: 'sailboat',
  small_fast_boat: 'small fast boat',
  light_aircraft: 'light aircraft',
  helicopter: 'helicopter',
  unknown_uas: 'UAS, no transponder',
  submarine: 'submarine',
  uuv: 'UUV',
  vehicle: 'vehicle',
  truck: 'truck',
  large_vessel: 'large vessel',
  small_craft: 'small craft',
  aircraft: 'aircraft',
  small_uas: 'small UAS',
  ground_vehicle: 'ground vehicle',
  submerged: 'submerged contact',
  vessel: 'vessel',
  small_surface: 'small surface contact',
  large_surface: 'large surface contact',
  small_air: 'small air contact',
  large_air: 'large air contact',
  small_ground: 'small ground contact',
  large_ground: 'large ground contact',
  subsurface: 'subsurface contact',
}

export function kindLabel(kind: string | null) {
  return kind == null ? 'unclassified' : (KIND_LABEL[kind] ?? kind.replace(/_/g, ' '))
}

/** How specific a track's kind is (SCORING-V3.md 15 (a)): a detection gives a size class (or, from
 *  ESM alone, the domain), a recognition a family, an identification a kind. */
export type IdentityLevel = 'detect' | 'recognise' | 'identify'

export const LEVEL_LABEL: Record<IdentityLevel, string> = {
  detect: 'detected: size class only',
  recognise: 'recognised: family only',
  identify: 'identified',
}

const LEVEL_SHORT: Record<IdentityLevel, string> = { detect: 'DET', recognise: 'REC', identify: 'ID' }

/** The kind with its level, wherever the kind is shown: "small air contact (detected: size
 *  class only)". A bridge before the level gives the kind alone. */
export function kindWithLevel(identity: { kind: string | null; level?: IdentityLevel | null }) {
  const k = kindLabel(identity.kind)
  return identity.kind != null && identity.level ? `${k} (${LEVEL_LABEL[identity.level]})` : k
}

/** The map symbol's short kind with its level: "SM AIR (DET)". */
export function kindShortWithLevel(identity: { kind: string | null; level?: IdentityLevel | null }) {
  const k = kindShort(identity.kind)
  return k != null && identity.level ? `${k} (${LEVEL_SHORT[identity.level]})` : k
}

/** Short form for the map symbol's type field. */
const KIND_SHORT: Record<string, string> = {
  cargo_ship: 'CARGO',
  tanker: 'TANKER',
  ferry: 'FERRY',
  patrol_boat: 'PATROL',
  fishing_vessel: 'FISHING',
  sailboat: 'SAIL',
  small_fast_boat: 'FAST BOAT',
  light_aircraft: 'LT ACFT',
  helicopter: 'HELO',
  unknown_uas: 'UAS',
  submarine: 'SUB',
  uuv: 'UUV',
  vehicle: 'VEH',
  truck: 'TRUCK',
  large_vessel: 'LARGE',
  small_craft: 'SMALL',
  aircraft: 'ACFT',
  small_uas: 'SM UAS',
  ground_vehicle: 'GND VEH',
  submerged: 'SUBM',
  vessel: 'VESSEL',
  small_surface: 'SM SURF',
  large_surface: 'LG SURF',
  small_air: 'SM AIR',
  large_air: 'LG AIR',
  small_ground: 'SM GND',
  large_ground: 'LG GND',
  subsurface: 'SUBSURF',
}

export function kindShort(kind: string | null) {
  return kind == null ? undefined : (KIND_SHORT[kind] ?? kind.replace(/_/g, ' ').toUpperCase().slice(0, 9))
}

/** Seconds since the track was last updated, on the bridge's clock (`nowMs` from useBridgeClock). */
export function trackAge(t: Track, nowMs: number) {
  return Math.max(0, nowMs / 1000 - t.last_seen_t)
}

/** 1-sigma position uncertainty now, metres: grows with age. */
export function trackSigma(t: Track, nowMs: number) {
  return t.pos_sigma_m + t.sigma_growth_mps * trackAge(t, nowMs)
}

/** Existence confidence now: decays while unobserved. */
export function trackConfidence(t: Track, nowMs: number) {
  return t.confidence * Math.exp(-trackAge(t, nowMs) / (t.confidence_decay_s || 60))
}

/** Where the track is believed to be now: the last fix carried along its velocity. */
export function trackPredicted(t: Track, nowMs: number): LngLat {
  if (!t.velocity_mps) return t.position
  const dt = trackAge(t, nowMs)
  const [lon, lat] = t.position
  const [e, n] = t.velocity_mps
  return [lon + (e * dt) / (111_320 * Math.cos((lat * Math.PI) / 180)), lat + (n * dt) / 111_320]
}

export function isTrackStale(t: Track, nowMs: number, staleAfterS = 10) {
  return trackAge(t, nowMs) > staleAfterS
}

/** Fades from 1 (fresh) to 0.3 over two minutes of silence. */
export function trackOpacity(t: Track, nowMs: number) {
  return 1 - 0.7 * Math.min(1, trackAge(t, nowMs) / 120)
}

export function fmtAge(s: number) {
  const n = Math.round(s)
  if (n < 60) return `${n}s`
  if (n < 3600) return `${Math.floor(n / 60)}m${String(n % 60).padStart(2, '0')}s`
  return `${Math.floor(n / 3600)}h${String(Math.floor((n % 3600) / 60)).padStart(2, '0')}m`
}

export interface RenderedTrackSymbol {
  svg: string
  anchor: { x: number; y: number }
  size: { width: number; height: number }
}

const cache = new Map<string, RenderedTrackSymbol>()

/** The 2525 symbol for a track: the bridge gives the SIDC (affiliation frame, dimension,
 *  function by the estimated kind, status A dashed when stale). Labels: id, and kind with its level. */
export function trackSymbolSvg(t: Track, opts: { stale: boolean; size?: number; labels?: boolean }): RenderedTrackSymbol {
  const labels = opts.labels ?? true
  const sidc = opts.stale ? t.sidc.slice(0, 3) + 'A' + t.sidc.slice(4) : t.sidc.slice(0, 3) + 'P' + t.sidc.slice(4)
  const kind = t.identity.kind
  const key = `${sidc}|${opts.size}|${labels}|${t.id}|${kind}|${t.identity.level ?? ''}`
  const hit = cache.get(key)
  if (hit) return hit
  const sym = new ms.Symbol(sidc, {
    size: opts.size ?? 24,
    uniqueDesignation: labels ? t.id : undefined,
    type: labels ? kindShortWithLevel(t.identity) : undefined,
    infoColor: '#d7dde4',
    outlineColor: '#0b0f14',
    outlineWidth: 3,
  } as Record<string, unknown>)
  const out = { svg: sym.asSVG(), anchor: sym.getAnchor(), size: sym.getSize() }
  cache.set(key, out)
  return out
}

/** Icons are rasterised at this multiple of their on-screen size, so they stay sharp on HiDPI
 *  (as symbols.ts does for vehicles). */
const ICON_SCALE = 2
const ICON_PX = 24

/** A bare track frame (no labels) as a deck.gl IconLayer icon, one atlas entry per symbol
 *  code and stale state, shared by every track with that estimate. */
export interface TrackFrameIcon {
  id: string
  url: string
  width: number
  height: number
  anchorX: number
  anchorY: number
  /** on-screen height in CSS pixels (getSize) */
  size: number
  /** half extents of the frame around its anchor, CSS pixels: where labels go */
  halfW: number
  halfH: number
}

const frames = new Map<string, TrackFrameIcon>()

export function trackFrameIcon(t: Track, stale: boolean): TrackFrameIcon {
  const sidc = stale ? t.sidc.slice(0, 3) + 'A' + t.sidc.slice(4) : t.sidc.slice(0, 3) + 'P' + t.sidc.slice(4)
  return sidcFrameIcon(sidc)
}

/** The atlas icon for a 15-character SIDC (shared, built once per code). */
export function sidcFrameIcon(sidc: string): TrackFrameIcon {
  const hit = frames.get(sidc)
  if (hit) return hit
  const sym = new ms.Symbol(sidc, { size: ICON_PX * ICON_SCALE, outlineColor: '#0b0f14', outlineWidth: 3 * ICON_SCALE } as Record<string, unknown>)
  const size = sym.getSize()
  const anchor = sym.getAnchor()
  const width = Math.ceil(size.width)
  const height = Math.ceil(size.height)
  const icon: TrackFrameIcon = {
    id: `track-${sidc}`,
    url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sym.asSVG())}`,
    width,
    height,
    anchorX: anchor.x,
    anchorY: anchor.y,
    size: height / ICON_SCALE,
    halfW: Math.max(anchor.x, width - anchor.x) / ICON_SCALE,
    halfH: Math.max(anchor.y, height - anchor.y) / ICON_SCALE,
  }
  frames.set(sidc, icon)
  return icon
}

/** Sorted for the operator: hostile, suspect, unknown first, then the freshest. */
export function sortTracks(tracks: Track[]) {
  return [...tracks].sort((a, b) => AFFILIATION_RANK[a.affiliation] - AFFILIATION_RANK[b.affiliation] || b.last_seen_t - a.last_seen_t || a.id.localeCompare(b.id))
}

/** Declare a track's affiliation (an operator action, logged by the bridge's engine under
 *  the operator's session). `password`: the operator's again, when the policy marks a hostile
 *  declaration consequential (a step-up); sent in this one request only. `flag`: the review
 *  flag this declaration answers. Throws [`StepUpNeeded`] when the password is missing or
 *  wrong. A declaration in force with another affiliation is revoked first (the engagements
 *  in play on the track stop). `passkey` (rules 10): instead of the password, a passkey's
 *  assertion made for declaring this track hostile. */
export async function declareTrack(id: string, affiliation: Affiliation, note?: string, password?: string, flag?: number, passkey?: Record<string, unknown>): Promise<Track> {
  const res = await operatorFetch(`/api/tracks/${encodeURIComponent(id)}/declare`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
    body: JSON.stringify({
      affiliation,
      note: note || null,
      ...(password ? { password } : {}),
      ...(passkey ? { passkey } : {}),
      ...(flag != null ? { flag } : {}),
    }),
  })
  const body = await res.json().catch(() => ({}))
  if (!res.ok) {
    const msg = body.detail ?? `bridge returned ${res.status}`
    throw body.step_up ? new StepUpNeeded(msg) : new Error(msg)
  }
  return body as Track
}
