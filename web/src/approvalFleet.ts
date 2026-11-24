// The fleet as the approval cards read it: a vehicle by uid, its callsign, and the ask sentence's
// view of it (approvalText.ts). Display only.
import type { AskFleet } from './approvalText'
import type { Vehicle } from './types'
import { callsign } from './types'

export function vehicleOf(vehicles: Vehicle[], uid: string) {
  return vehicles.find((v) => String(v.uid) === uid)
}

export function nameOf(vehicles: Vehicle[], uid: string) {
  const v = vehicleOf(vehicles, uid)
  return v ? callsign(v) : `vehicle ${uid}`
}

/** The fleet as the ask sentence reads it (approvalText.ts). */
export function askFleet(vehicles: Vehicle[]): AskFleet {
  const by = new Map(vehicles.map((v) => [String(v.uid), v]))
  return {
    callsign: (u) => (by.get(u) ? callsign(by.get(u) as Vehicle) : `vehicle ${u}`),
    model: (u) => by.get(u)?.identity?.short ?? null,
    swarm: (u) => by.get(u)?.swarm ?? by.get(u)?.identity?.swarm ?? null,
  }
}
