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
  last_rx_t: number | null // bridge clock, seconds
  since_last_ms: number | null // computed in the browser from last_rx_t and the message time
  loss_pct: number | null
  rtt_ms: number | null
  /** How often the link should be heard from: 1000 streaming, 20000 on HIGH_LATENCY2 (Rust bridge only). */
  expected_interval_ms?: number
}

/** What a vehicle's position rests on (Rust bridge only). A submerged AUV dead-reckons: ready, but drifting. */
export type NavSource = 'gnss' | 'dead_reckoning' | 'none'

export interface Vehicle {
  uid: number // fleet-unique; what the console keys on
  sysid: number // MAVLink system ID: unique per link only, wraps in fleets over 250
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
  return (v.link.since_last_ms ?? 0) > staleMs(v)
}
