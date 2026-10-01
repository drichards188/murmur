export type LngLat = [number, number]

export interface LinkHealth {
  since_last_ms: number | null
  loss_pct: number | null
  rtt_ms: number | null
  rx_total: number
  tx_total: number
}

export interface Vehicle {
  sysid: number
  kind: 'plane' | 'boat' | 'unknown' | null
  mode: string | null
  armed: boolean
  position: LngLat | null
  alt_rel: number | null
  heading: number | null
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

export type TaskKind = 'orbit' | 'patrol' | 'hold' | 'rtl'

export interface Tasking {
  id: number
  task: TaskKind
  params: { center?: LngLat; radius?: number; route?: LngLat[] }
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

export function callsign(v: Pick<Vehicle, 'sysid' | 'kind'>) {
  const n = String(v.sysid).padStart(2, '0')
  return v.kind === 'plane' ? `AIR ${n}` : v.kind === 'boat' ? `SEA ${n}` : `UNK ${n}`
}

export const STALE_MS = 3000
