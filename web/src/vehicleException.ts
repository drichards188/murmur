// One exception slot per vehicle (bd murmur-iobd.1, logs/ui-sweep W1): the one thing wrong with one
// of ours, said in the same words on its fleet row and under its map symbol, and the only place on
// either that takes colour for it. HUNTER 1 lost its propulsion to a hostile act and its row kept
// reading "AUTO motors on" while the alert sat in the Inbox: the damage, a silent or dropped link
// and a spent loadout each had a mark of its own somewhere, or none.
//
// The most pressing one wins, in this order:
//
//   lost       an operator confirmed it lost (rules 30): settled, grey
//   presumed   the engine presumes it lost on evidence (rules 30)
//   damaged    its own damage report (an inbox `damaged` alert): what it can no longer do
//   silent     its link is stale: "SILENT 12s" (rules 30), "LOST 12s" before it (nodeLoss.ts)
//   link_down  the link emulator has its link down, before it goes silent
//   no_rounds  an armed platform with every round of its loadout spent
//   degraded   the link emulator degrades it, or the bridge measures more than LOSS_PCT loss
//
// Display only: nothing here is a decision or changes one. Pure (no browser):
// vehicleException.check.ts runs it under node.
import type { Priority } from './declutter.ts'
import { type NodeLossView, type NodeStanding, staleBadge, standingOf } from './nodeLoss.ts'
import type { Vehicle } from './types'
import { isStale } from './types.ts'

export type ExceptionKind = 'lost' | 'presumed' | 'damaged' | 'silent' | 'link_down' | 'no_rounds' | 'degraded'

/** `bad` red, `warn` amber, `settled` grey (a person decided it: no longer an alarm) */
export type ExceptionTone = 'bad' | 'warn' | 'settled'

export interface VehicleException {
  kind: ExceptionKind
  tone: ExceptionTone
  /** the slot's words, on the row and under the symbol alike: "DAMAGED: CANNOT MOVE" */
  text: string
  /** what it means and where to look, for the hover */
  title: string
}

/** What a damage report took away (the bridge's `damage` vehicle fact, as its alert says it). */
export interface Damage {
  mobility: boolean
  firepower: boolean
}

/** Measured packet loss above this, %, is a degraded link (as the fleet row's numbers turned amber). */
export const LOSS_PCT = 10

export interface ExceptionFacts {
  stale: boolean
  /** seconds since it was last heard */
  silentS: number
  /** rules 30's standing (null before rules 30, or reporting and not presumed) */
  standing: NodeStanding | null
  damage: Damage | null
  /** the link emulator's state for its link, the cause in words */
  emu: { state: 'ok' | 'degraded' | 'down'; cause: string } | null
  lossPct: number | null
  noRounds: boolean
  /** the map draws where it probably is (the bridge's estimate) */
  estimate: boolean
}

/** A damage report's words: what the vehicle can no longer do. */
export function damageText(d: Damage): string {
  if (d.mobility && d.firepower) return 'DAMAGED: CANNOT MOVE OR FIRE'
  if (d.mobility) return 'DAMAGED: CANNOT MOVE'
  if (d.firepower) return 'DAMAGED: CANNOT FIRE'
  return 'DAMAGED'
}

/** The one exception to show, or none. */
export function vehicleException(f: ExceptionFacts): VehicleException | null {
  const silence = f.stale ? staleBadge(f.standing, f.silentS) : ''
  const where = f.estimate ? ' The map shows where it probably is.' : ''
  if (f.standing === 'lost') {
    return { kind: 'lost', tone: 'settled', text: 'LOST', title: 'Confirmed lost by an operator: out of every engagement keep-out, it takes no order (Review).' }
  }
  if (f.standing === 'presumed_lost') {
    return { kind: 'presumed', tone: 'bad', text: silence || 'PRESUMED LOST', title: `Presumed lost by the engine on evidence: a flag that decides nothing until an operator with engagement authority confirms it (Review).${where}` }
  }
  if (f.damage) {
    const lost = [f.damage.mobility ? 'propulsion lost: an order will not move it' : '', f.damage.firepower ? 'weapons out of action' : ''].filter(Boolean).join('; ')
    return { kind: 'damaged', tone: 'bad', text: damageText(f.damage), title: `Its own damage report${lost ? `: ${lost}` : ''}${f.stale ? `. Not heard for ${Math.floor(f.silentS)} s.` : '.'} The alert is in the Inbox.` }
  }
  if (f.stale) {
    return { kind: 'silent', tone: 'bad', text: silence, title: `Not heard for ${Math.floor(f.silentS)} s: its link is lost.${where}` }
  }
  if (f.emu?.state === 'down') {
    return { kind: 'link_down', tone: 'bad', text: 'LINK DOWN', title: `The link emulator has its link down${f.emu.cause ? `: ${f.emu.cause}` : ''}. It goes silent once nothing more is heard.` }
  }
  if (f.noRounds) {
    return { kind: 'no_rounds', tone: 'warn', text: 'OUT OF ROUNDS', title: 'Every round of its loadout is spent: no engagement can name it (vehicle details, Loadout).' }
  }
  if (f.emu?.state === 'degraded') {
    return { kind: 'degraded', tone: 'warn', text: 'LINK DEGRADED', title: `The link emulator degrades its link${f.emu.cause ? `: ${f.emu.cause}` : ''}.` }
  }
  if (f.lossPct != null && f.lossPct > LOSS_PCT) {
    return { kind: 'degraded', tone: 'warn', text: `LINK ${Math.round(f.lossPct)}% LOSS`, title: `The bridge measures ${Math.round(f.lossPct)}% packet loss over the last 10 s.` }
  }
  return null
}

/** How the map's label budget weighs a slot under the symbol (declutter.ts), given its vehicle's
 *  own priority: damage and a presumed loss with what waits on the person (placed before an
 *  engagement's or an act's label, which give way to it), a silence or a dropped link
 *  with ours on alert (as the LOST badge always was), the rest as the vehicle. */
export function exceptionPriority(x: VehicleException, own: Priority): Priority {
  const most: Priority = x.kind === 'damaged' || x.kind === 'presumed' ? 1 : x.kind === 'silent' || x.kind === 'link_down' || x.kind === 'lost' ? 3 : 4
  return Math.min(own, most) as Priority
}

/** What the slots are made from, beside each vehicle's own report (App.tsx). */
export interface ExceptionContext {
  /** rules 30: our vehicles' standing (the review's) */
  nodes: NodeLossView
  /** vehicles with a `damaged` alert, and what it took */
  damage: ReadonlyMap<number, Damage>
  /** armed vehicles with no round left (GET /api/loadouts) */
  noRounds: ReadonlySet<number>
}

export const NO_EXCEPTIONS: ExceptionContext = {
  nodes: { on: false, review: new Map(), lost: new Map() },
  damage: new Map(),
  noRounds: new Set(),
}

/** A vehicle's slot. `cause` puts the link emulator's cause in words (linkEmu.ts emuCause). */
export function exceptionOf(v: Pick<Vehicle, 'uid' | 'link' | 'estimate'>, ctx: ExceptionContext, cause: (c: string | null) => string = (c) => c ?? ''): VehicleException | null {
  const stale = isStale(v)
  const emu = v.link.emu
  return vehicleException({
    stale,
    silentS: (v.link.since_last_ms ?? 0) / 1000,
    standing: standingOf(ctx.nodes, v.uid, stale),
    damage: ctx.damage.get(v.uid) ?? null,
    emu: emu ? { state: emu.state, cause: cause(emu.cause) } : null,
    lossPct: v.link.loss_pct,
    noRounds: ctx.noRounds.has(v.uid),
    estimate: stale && !!v.estimate,
  })
}

/** What a `damaged` alert's detail says was taken ("reports propulsion lost and weapons out of
 *  action since t=...", crates/murmur-bridge vehicle.rs `describe`). */
export function damageOf(detail: string): Damage {
  return { mobility: /propulsion lost/.test(detail), firepower: /weapons out of action/.test(detail) }
}

/** One weapon as GET /api/loadouts gives it. */
export interface LoadoutWeapon {
  kind: string
  left: number
  inert?: boolean
}

/** Every round spent: an armed platform whose usable weapons (not inert, not the platform itself,
 *  which goes with it) all have none left. A platform with none of those is never out. */
export function outOfRounds(weapons: readonly LoadoutWeapon[]): boolean {
  const usable = weapons.filter((w) => !w.inert && w.kind !== 'self')
  return usable.length > 0 && usable.every((w) => w.left <= 0)
}
