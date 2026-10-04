export type LngLat = [number, number]

export type Kind = 'plane' | 'vtol' | 'copter' | 'blimp' | 'boat' | 'ugv' | 'sub' | 'tracker' | 'unknown'
export type Domain = 'air' | 'ground' | 'surface' | 'subsurface'

/** What a node is, from the launcher's fleet manifest (sim/platforms.toml). */
export interface Identity {
  callsign: string
  platform: string
  name: string // e.g. "MQ-9B SkyGuardian-class"
  short: string // compact model label for the map, e.g. "MQ-9B"
  role: string // e.g. "MALE ISR/strike UAS"
  domain: Domain
  link: string
  sidc: string
  sim: 'sitl' | 'standin'
  /** Carries an effect (`armed = true` in the catalog): any task needs a human. Not the MAVLink arm state. */
  armed?: boolean | null
}

export const AIR_KINDS: Kind[] = ['plane', 'vtol', 'copter', 'blimp']
export const SEA_KINDS: Kind[] = ['boat', 'sub']

export interface LinkHealth {
  /** bridge clock, seconds; a lite snapshot carries it only once the link is lost */
  last_rx_t?: number | null
  since_last_ms: number | null // computed in the browser from last_rx_t and the message time
  /** lite snapshots: the bridge's own verdict (a live link then costs nothing to keep reporting) */
  stale?: boolean
  loss_pct: number | null
  rtt_ms: number | null
  /** How often the link should be heard from: 1000 streaming, 20000 on HIGH_LATENCY2 (Rust bridge only). */
  expected_interval_ms?: number
  /** The link emulator's state for this link, when one runs: degraded or down and why
   *  (a linkemu drop cause, or `event`). Details from /api/links/{uid} (linkEmu.ts). */
  emu?: { state: 'ok' | 'degraded' | 'down'; cause: string | null }
  /** The telemetry plan the bridge chose for this link (Rust bridge only): the tier, the link
   *  profile it was chosen for and why (`link` table, stepped down for `bandwidth` or measured
   *  `queue` drops, or `uniform` with MURMUR_TELEMETRY=uniform). */
  telemetry?: TelemetryPlan | null
  /** The task layer's command ACK wait on this link now, ms (Rust bridge only). */
  ack_timeout_ms?: number
}

export interface TelemetryPlan {
  tier: 'full' | 'reduced' | 'minimal' | 'silent'
  profile: string | null
  label: string | null
  reason: 'link' | 'bandwidth' | 'queue' | 'uniform'
  msgs_per_s: number
  bytes_per_s: number
  /** Seconds between position/speed reports under this plan. */
  report_s: number
}

/** What a vehicle's position rests on (Rust bridge only). A submerged AUV dead-reckons: ready, but drifting. */
export type NavSource = 'gnss' | 'dead_reckoning' | 'none'

export interface Vehicle {
  uid: number // fleet-unique; what the console keys on
  sysid: number // MAVLink system ID: unique per link only, wraps in fleets over 250
  /** Level of detail (murmur-and): `lite` is what the bridge sends for a vehicle outside
   *  the view (identity, a coarse position, mode, link state, once a second, no trail);
   *  absent or `full` is the whole snapshot at 4 Hz. */
  lod?: 'full' | 'lite'
  identity: Identity | null
  kind: Kind | null
  mode: string | null
  armed: boolean
  position: LngLat | null
  alt_rel: number | null
  alt_msl: number | null
  roll: number | null
  pitch: number | null
  heading: number | null
  yaw: number | null
  tracking: number | null // uid
  nav_ready: boolean
  nav_source?: NavSource
  groundspeed: number | null
  airspeed: number | null
  battery_pct: number | null
  gps_fix: number
  satellites: number
  mission_seq: number | null
  home: LngLat | null
  link: LinkHealth
  trail: [number, number, number][] // lon, lat, metres above mean sea level
  statustext: { t: number; severity: number; text: string }[]
  /** Where the vehicle probably is while its link is silent (bridge `estimate` module, murmur-4y9):
   *  present only past the stale threshold. A motion model from `t0`, evaluated here by
   *  estimate.ts on every clock tick. Display only: nothing in the decision core reads it. */
  estimate?: Estimate | null
}

/** [lon, lat]; metres, seconds, degrees clockwise from north. The fields are the region's
 *  half-extents (about two sigma), see the bridge's `estimate.rs` for the growth law. */
export type Motion =
  | { kind: 'ring'; center: LngLat; radius_m: number; width_m: number; angle0_deg: number; rate_dps: number; spread0_deg: number; spread_dps: number }
  | { kind: 'route'; route: LngLat[]; lead?: LngLat; seg0: number; frac0: number; speed_mps: number; along0_m: number; along_mps: number; across0_m: number; across_mps: number }
  | { kind: 'line'; from: LngLat; bearing_deg: number; speed_mps: number; stop_m?: number; along0_m: number; along_mps: number; across0_m: number; across_mps: number; arrive?: Arrival }
  | { kind: 'point'; at: LngLat; radius0_m: number; rate_mps: number }

export interface Arrival {
  center: LngLat
  radius_m: number
  ring: boolean
  width_m: number
}

export interface FailsafeBranch {
  action: 'rtl' | 'surface' | 'land' | 'disarm'
  after_s: number
  fires_t: number
  profile: string
  detail: string
  motion: Motion
}

export interface Estimate {
  /** the last position report (bridge wall clock, s): the model's start */
  t0: number
  basis: 'orbit' | 'patrol' | 'hold' | 'rtl' | 'velocity' | 'stationary'
  detail: string
  motion: Motion
  reach_mps: number
  cap_m: number
  note?: string
  failsafe?: FailsafeBranch
  /** the bridge's own evaluation at a coarse step (API readers; the console evaluates itself) */
  now: { t: number; position: LngLat; radius_m: number }
}

export type OpState = 'queued' | 'sent' | 'acked' | 'active' | 'done' | 'failed' | 'superseded'

export interface Op {
  id: number
  tasking: number
  task: TaskKind
  uid: number
  sysid: number
  state: OpState
  detail: string
  progress: string | null
  history: { state: OpState; t: number }[]
}

export type TaskKind = 'orbit' | 'patrol' | 'hold' | 'rtl' | 'track'

export interface Tasking {
  id: number
  task: TaskKind
  params: { center?: LngLat; radius?: number; route?: LngLat[]; target?: number /* uid */ }
  created: number
  ops: Op[]
}

export interface BridgeState {
  type: 'state'
  t: number
  vehicles: Vehicle[]
  taskings: Tasking[]
  /** Counts per map tile while the view holds more vehicles than the bridge sends in full
   *  (zoomed out over a large fleet); null or absent otherwise. */
  clusters?: Clusters | null
}

/** One map tile with something in it (`push.rs` clusters). */
export interface Cluster {
  x: number
  y: number
  /** centroid of what is in the tile, lon lat: where the marker goes */
  c: LngLat
  /** vehicles in the tile */
  n: number
  /** of them: air, ground, surface, subsurface */
  d: [number, number, number, number]
  /** of them: links lost */
  lost: number
  /** tracks in the tile: hostile, suspect, unknown, neutral, assumed friend */
  tk: [number, number, number, number, number]
}

export interface Clusters {
  /** the tile zoom the cells are at (web mercator tiles) */
  z: number
  cells: Cluster[]
}

/** What the console looks at, sent to the bridge (`/ws?lod=1`): the map's bounds (w, s, e,
 *  n) and zoom, and the vehicles it wants at full detail whatever the view. */
export interface ViewSubscription {
  bounds: [number, number, number, number] | null
  zoom: number
  pin: number[]
}

export const LIFECYCLE: OpState[] = ['queued', 'sent', 'acked', 'active', 'done']

const PREFIX: Record<Kind, string> = {
  ugv: 'UGV',
  plane: 'AIR',
  vtol: 'VTL',
  copter: 'ROT',
  blimp: 'LTA',
  boat: 'SEA',
  sub: 'SUB',
  tracker: 'TRK',
  unknown: 'UNK',
}

export function callsign(v: Pick<Vehicle, 'uid' | 'kind'> & { sysid?: number; identity?: Identity | null }) {
  return v.identity?.callsign ?? `${PREFIX[v.kind ?? 'unknown']} ${String(v.sysid ?? v.uid).padStart(2, '0')}`
}

const DOMAIN_BY_KIND: Record<Kind, Domain> = {
  plane: 'air',
  vtol: 'air',
  copter: 'air',
  blimp: 'air',
  boat: 'surface',
  ugv: 'ground',
  sub: 'subsurface',
  tracker: 'ground',
  unknown: 'ground',
}

/** Display names: "surface" reads like ground, so the UI says water (the catalog key stays surface). */
export const DOMAIN_LABEL: Record<Domain, string> = { air: 'Air', ground: 'Ground', surface: 'Water', subsurface: 'Subsurface' }

export function domainOf(v: Pick<Vehicle, 'kind' | 'identity'>): Domain {
  return v.identity?.domain ?? DOMAIN_BY_KIND[v.kind ?? 'unknown']
}

export const STALE_MS = 3000

/** Silence after which a vehicle's link counts as lost: 3 of its expected intervals (the
 *  bridge's rule: 3 s when streaming, 60 s for an AUV reporting by HIGH_LATENCY2 over its
 *  acoustic link). STALE_MS when the bridge doesn't say. */
export function staleMs(v: Pick<Vehicle, 'link'>): number {
  const every = v.link.expected_interval_ms
  return every == null ? STALE_MS : 3 * every
}

export function isStale(v: Pick<Vehicle, 'link'>): boolean {
  return v.link.stale ?? (v.link.since_last_ms ?? 0) > staleMs(v)
}
