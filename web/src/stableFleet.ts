// The order form's dry run, the refusal fold and the like read a vehicle only for its name and
// platform, which do not change while it is in the fleet, yet the vehicle list is a new array on
// every feed update. `sameFleet` says whether two lists are the same fleet for such a reader, so
// that it can be given the earlier list and not render again (bd murmur-yfp6).
import type { Vehicle } from './types'

/** The same vehicles, in the same order, with the same identity (kind, sysid, platform and callsign). */
export function sameFleet(a: readonly Vehicle[], b: readonly Vehicle[]): boolean {
  if (a === b) return true
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    const x = a[i]
    const y = b[i]
    if (x === y) continue
    if (x.uid !== y.uid || x.sysid !== y.sysid || x.kind !== y.kind || x.identity?.platform !== y.identity?.platform || x.identity?.callsign !== y.identity?.callsign) return false
  }
  return true
}
