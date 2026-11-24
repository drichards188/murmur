// The right dock (App.tsx, dock.css): the map's right edge holds at most one drawer at a time.
// Every drawer that opens over that edge (the Inbox, the watch officer, the Queue, the Sitrep,
// Swarms) is one of its tabs, and the top bar's buttons switch between them: two drawers never lie
// on top of each other, and the one shown is the one its button marks. What waits for a person
// stays in sight whichever is shown (Awaiting.tsx at the head of every drawer but the Inbox, the
// Inbox button's badges, the waiting strip across the top), and everything else anchored to the
// map's right edge (the hidden legend, the group strip, the weather details, the globe, the map's
// zoom buttons) sits beside the dock, `--dock-w` from that edge. The vehicle details, the
// Situation panel and the tutorial's card keep to the map's left and give way to the dock rather
// than lie under it. The Officer tab is there only where the run's AI takes requests (officer.ts
// `officerShown`): a situation's model officer, or in the open world one that has read the tasking.
// Pure: dock.check.ts.

export type DockId = 'inbox' | 'officer' | 'queue' | 'sitrep' | 'swarms'

/** A drawer's button: shows it in place of the one shown, or closes the dock when it is the one
 *  shown. */
export function toggleDock(shown: DockId | null, id: DockId): DockId | null {
  return shown === id ? null : id
}

/** A drawer's own close (its x, or a link that hands over to another view): closes the dock only
 *  when that drawer is the one shown, so a late close from a drawer already replaced never closes
 *  the one that replaced it. */
export function closeDock(shown: DockId | null, id: DockId): DockId | null {
  return shown === id ? null : shown
}

/** The dock's width in px for each drawer (the Sitrep is the wide one), at most the map's. */
export const DOCK_WIDTH: Readonly<Record<DockId, number>> = { inbox: 430, officer: 430, queue: 430, sitrep: 760, swarms: 430 }

/** The room a fit of the map leaves on its right (MapView's `fit.padRight`) with nothing docked. */
export const FIT_PAD = 70

/** The right padding a fit of the map leaves: clear of the drawer shown. */
export function fitPad(shown: DockId | null): number {
  return shown ? DOCK_WIDTH[shown] : FIT_PAD
}

/** The least of the map a fit keeps between its left padding and its right, in px. */
export const FIT_MIN = 120

/** The right padding a fit uses on a map `width` px wide with `left` px on its left: as asked,
 *  but never so much that less than FIT_MIN is left (a wide drawer on a narrow map: MapLibre
 *  refuses a fit whose padding leaves nothing). */
export function fitRight(asked: number, width: number, left: number): number {
  return Math.max(0, Math.min(asked, width - left - FIT_MIN))
}

/** Whether the waiting list (Awaiting.tsx) heads a drawer: every one but the Inbox, which is
 *  where those cards are. */
export function headsWithWaiting(id: DockId): boolean {
  return id !== 'inbox'
}
