export type LngLat = [number, number]

export type Kind = 'plane' | 'vtol' | 'copter' | 'blimp' | 'boat' | 'sub' | 'tracker' | 'unknown'

export const AIR_KINDS: Kind[] = ['plane', 'vtol', 'copter', 'blimp']
export const SEA_KINDS: Kind[] = ['boat', 'sub']

export interface LinkHealth {
  since_last_ms: number | null
  loss_pct: number | null
  rtt_ms: number | null
  rx_total: number
  tx_total: number
}

export interface Vehicle {
  sysid: number
  kind: Kind | null
  mode: string | null
  armed: boolean
  position: LngLat | null
  alt_rel: number | null
  heading: number | null
  yaw: number | null
  tracking: number | null
  nav_ready: boolean
  groundspeed: number | null
  airspeed: number | null
  battery_pct: number | null
  gps_fix: number
  satellites: number
  mission_seq: number | null
  home: LngLat | null
  link: LinkHealth
  trail: LngLat[]
  statustext: { t: number; severity: number; text: string }[]
}

export type OpState = 'queued' | 'sent' | 'acked' | 'active' | 'done' | 'failed' | 'superseded'

export interface Op {
  id: number
  tasking: number
  task: TaskKind
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
  params: { center?: LngLat; radius?: number; route?: LngLat[]; target?: number }
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
  plane: 'AIR',
  vtol: 'VTL',
  copter: 'ROT',
  blimp: 'LTA',
  boat: 'SEA',
  sub: 'SUB',
  tracker: 'TRK',
  unknown: 'UNK',
}

export function callsign(v: Pick<Vehicle, 'sysid' | 'kind'>) {
  return `${PREFIX[v.kind ?? 'unknown']} ${String(v.sysid).padStart(2, '0')}`
}

export const STALE_MS = 3000
