// The console's parts: what App.tsx hands the layout it shows (bd epic murmur-iobd, the layout
// prototypes; `?layout=cockpit|rail|decisions`, layouts/index.ts). App keeps every piece of state,
// every data hook, effect and handler, and builds each part once a render; a layout only says where
// each part sits. A layout is built from this contract alone: it holds none of the console's state,
// asks the bridge nothing, and changes nothing a part shows or asks. Display only.
//
// How a layout is made (Cockpit.tsx is the model: the console as it always was):
// - one file, `layouts/<Name>.tsx`, exporting one component `(props: LayoutProps) => ReactNode`; it
//   returns the root's children (App renders the root, `div.app[data-layout=<name>]`, the
//   CSS grid of index.css, with `.waiting` while the waiting strip shows);
// - its own CSS, `layouts/<name>.css`, imported by that file. Every layout's CSS loads whatever
//   layout runs, so every rule is scoped: `.app[data-layout=rail] ...`;
// - small components of its own beside it (`layouts/<Name>*.tsx`), built from these parts too.
// A part is a rendered node: place it once, or not at all (a node placed twice is two of it). The
// map is the one call, `map(slot)`: its element is where the floating panels keep to, wherever the
// layout puts it.
import type { ReactNode } from 'react'
import type { Intent } from '../decisions'
import type { DockId } from '../dock'
import type { InboxSelection } from '../InboxPanel'
import type { NeedsYou } from '../needsYou'
import type { Reveal } from '../reveal'
import type { Glance, Trouble } from '../topbar'
import type { LngLat, Tasking } from '../types'

/** The top bar's pieces, each rendered, null while it is not shown. Cockpit sets them in
 *  `header.topbar` in this order (the drawer buttons in `nav.dock-tabs`). */
export interface BarParts {
  /** the main menu's button (R24); null without the launcher (`make up`, `make goldengate`) */
  menu: ReactNode
  /** "MURMUR", small */
  brand: ReactNode
  /** the Situation chip (R16: the run's title and the newest thing to watch): a click opens the
   *  Situation panel over the map, a second closes it. Null outside a situation. Show it only
   *  with `map({ runChip: false })`, or the chip shows twice (the map floats one by default) */
  run: ReactNode
  /** the glance (R1, R2, R6): ours, ours in trouble (a click shows the next in the Fleet list),
   *  the contacts and how many matter */
  glance: ReactNode
  /** the bridge's connection pill (R19), only while the console is not live */
  connection: ReactNode
  /** the Inbox button (R8): the needs-you count, red when urgent, the soonest time left; it
   *  shows the Inbox drawer, or closes it */
  inbox: ReactNode
  /** the watch officer's button (R22), only where the run's AI takes requests */
  officer: ReactNode
  /** the Queue's button (rules 5), only while an order is held for a link or its drawer is open */
  queue: ReactNode
  /** the Sitrep's button, only at group scale or while its drawer is open */
  sitrep: ReactNode
  /** the Groups button (R17), only when the fleet has swarms, the person made a group, or its
   *  drawer is open */
  groups: ReactNode
  /** the Views menu (R18): Weather, Space, Links, Platforms, each opening over the map */
  views: ReactNode
  /** who this console acts as, and the DEV mark in a dev run (R21) */
  operator: ReactNode
  /** the world's time, and in a situation its T+ and time left (R15) */
  clock: ReactNode
  /** the glance's and the run's figures, for a layout that words them itself in place of `glance`
   *  (Cockpit: topbar.ts `glanceWords`, `runStanding`). Optional: `glance` and `run` need none of it */
  figures?: BarFigures
}

/** The figures behind the glance and the run chip, each as the bar's own nodes count it. */
export interface BarFigures {
  /** ours, the contacts our sensors hold, and how many of them are hostile or suspect (topbar.ts
   *  `glanceOf`) */
  glance: Glance
  /** ours in trouble, by the fleet rows' own slot (topbar.ts `troubleOf`: how many, the worst tone,
   *  the hover's words) */
  trouble: Trouble
  /** the glance's own click on the trouble count: the next of ours in trouble shown in the Fleet list */
  showTrouble: () => void
  /** the hostile and the suspect contacts' ids, picture order (topbar.ts `mattersOf`): what their
   *  counts link to, with `ConsoleActs.showTrack` */
  hostile: string[]
  suspect: string[]
  /** the situation's standing, null outside one: who decides (its operator mode, the tutorial), its
   *  runner's phase, and whether the runner has gone quiet (topbar.ts `runStanding` words it) */
  run: { operator: 'live' | 'autoplay'; tutorial: boolean; phase: string; stale: boolean } | null
}

/** What a layout decides about the map, passed to `ConsoleParts.map`. Each left out is as Cockpit
 *  has it. */
export interface MapSlot {
  /** the drawer laid over the map's right edge (Cockpit: `dock.drawer`). The map makes room for it
   *  (its zoom buttons, the group strip and the floating panels keep clear) and a fit of the map
   *  pads for it. Leave it out when the layout puts the drawer beside the map: a fit then keeps
   *  the usual margin. */
  drawer?: ReactNode
  /** whether the alarm banners (R10) sit over the map's top. Default: while no drawer is shown
   *  anywhere (the drawer shown heads with them, `ConsoleParts.banners`) */
  banners?: boolean
  /** whether the Situation chip floats on the map (default true); false when the layout shows
   *  `bar.run`: the map then floats only the open Situation panel */
  runChip?: boolean
  /** whether what the layout shows at the right edge lies beside the map whatever is shown there
   *  (the Inbox kept beside it with no drawer open): a fit then keeps the usual margin, even one
   *  asked from an Inbox card. Default: beside only while a drawer is open and not in `drawer` */
  beside?: boolean
}

/** The right dock (dock.ts): one drawer at a time. */
export interface DockParts {
  /** which drawer is shown, or none */
  open: DockId | null
  /** the drawer shown (its panel, `aside.ix`, absolutely placed over the map's right edge by
   *  dock.css), null while none is; the banners and every order waiting for a person head it
   *  (the Inbox: the cards themselves) */
  drawer: ReactNode
  /** a drawer's button: shows it in place of the one shown, or closes the dock when it is that one */
  toggle: (id: DockId) => void
  /** shows this drawer in place of the one shown (null closes the dock) */
  show: (id: DockId | null) => void
}

/** The few facts a layout arranges by. */
export interface ConsoleFacts {
  /** ours in the fleet (the glance's count; platforms spent by their own effect left out) */
  vehicles: number
  /** of ours, how many are selected (the order form commands them) */
  selected: number
  /** a situation runs (its runner has posted its status; `bar.run` is not null) */
  situation: boolean
  /** what needs the person (needsYou.ts, the Inbox badge's own count): `total` (0: nothing),
   *  each kind, and `says`, the sentence the badge's title reads */
  needsYou: NeedsYou
  /** something urgent is among what needs the person (the Inbox badge is red) */
  urgent: boolean
  /** something a person alone must do holds up a step (`waiting` shows its strip; the root has
   *  `.waiting`) */
  waiting: boolean
  /** a drawer is shown (`dock.open` says which) */
  drawerOpen: boolean
  /** the bridge answers (false: connecting, or down: `bar.connection` shows) */
  live: boolean
  // For a layout that shows one list at a time (Rail.tsx): what each list holds, so its tab can say
  // so while the list is out of sight. Each optional: a layout that shows every list needs none.
  /** the glance's figures (topbar.ts `glanceOf`): ours, the contacts, how many hostile, suspect */
  glance?: Glance
  /** ours in trouble (topbar.ts `troubleOf`): the glance's count, by the rows' own exception slot */
  trouble?: Trouble
  /** the newest request to show rows in the Fleet list and in the Tracks list (a vehicle clicked on
   *  the map, the glance's trouble count, a card's track; reveal.ts: a new object each request),
   *  null before the first */
  reveal?: { fleet: Reveal | null; tracks: Reveal | null }
  /** the command log's orders (the bridge's taskings) and the intents they carry out */
  orders?: { taskings: readonly Tasking[]; intents: readonly Intent[] | null }
  /** in a situation, its newest thing to watch for (the Situation chip's line); null when none */
  watch?: string | null
  /** the console's last ask to show a track in the Tracks list (a contact clicked on the map, a
   *  card's or a banner's track link, `act.showTrack`): a new object at each ask, null before the
   *  first. A layout that keeps the Tracks list closed opens it on a new one. */
  trackShown?: { ids: string[] } | null
}

/** What a layout may do for the person, each as the console's own controls already do it. None
 *  decides anything: an approval, a signature or a review is made on its card. */
export interface ConsoleActs {
  /** one of ours' details open over the map, the map flying there and following it */
  showVehicle: (uid: number) => void
  /** these vehicles selected (the order form then commands them), the first shown in the Fleet list */
  selectVehicles: (uids: number[]) => void
  /** a track selected, its row shown in the Tracks list, the map centred on it */
  showTrack: (id: string) => void
  /** an Inbox item selected, as a click on its card: its vehicles selected and framed on the map
   *  with the order's preview */
  focusInbox: (sel: InboxSelection, uids: number[], intent: Intent | null) => void
  /** an order's card: in the Inbox while it waits for a person, else in Decisions (the Inbox drawer
   *  is shown) */
  openIntent: (id: number) => void
  /** a review or waiting item's card in the Inbox, brought into view (the person decides there) */
  openReview: (id: string) => void
  /** an engagement's card in the Inbox (waiting, under way or lately ended), or in Decisions when
   *  it never went */
  openEngagement: (id: number) => void
  /** frame these points on the map */
  fit: (points: LngLat[]) => void
}

/** Everything a layout receives. */
export interface ConsoleParts {
  /** the top bar's pieces */
  bar: BarParts
  /** the waiting strip (R8): one line while something a person alone must do holds up a step,
   *  nothing otherwise (`section.wait-strip`, grid area `wait`; Cockpit: between the bar and the
   *  columns) */
  waiting: ReactNode
  /** the Fleet list (R1 to R3): `aside.panel.fleet`, grid area `fleet` */
  fleet: ReactNode
  /** the Tracks list (R6, R7): `aside.tk`, folding to its header; Cockpit puts it in `.rail > .tk-slot` */
  tracks: ReactNode
  /** the command log with the order form (R11, R12): `aside.panel.tasking`; Cockpit puts it in
   *  `.rail` under the tracks */
  command: ReactNode
  /** the nomination form's layer (R11, Phase E): empty until a nomination is opened. Put it as the
   *  last child of a positioned element (Cockpit: `.rail`): the form docks over that element and
   *  drags out over the map to its left. Place it after `map()` in the tree: it reads the map's
   *  element as it mounts */
  nominate: ReactNode
  /** the map, `main.mapwrap` (grid area `map`), with everything drawn on it, the floating panels
   *  (Weather, Links, Space's globe, Platforms, the vehicle details, the Situation panel), the
   *  weather bar, the tutorial's card, the group strip, the Space timeline and the map's own
   *  banners. The floating panels keep to this element, wherever it sits. Call it once a render. */
  map: (slot?: MapSlot) => ReactNode
  /** the right dock: the drawer shown, and the way to show or close one */
  dock: DockParts
  /** the Inbox drawer's content (approvals, reviews, alerts, engagements; R8, R9), for a layout that
   *  keeps it outside the dock: the same node `dock.drawer` is while the Inbox is the drawer shown,
   *  so place one or the other, never both. The banners head it, as they head the drawer shown */
  inbox: ReactNode
  /** the alarm banners (R10): an attack on ours, a strike that asks a decision, a group that waits;
   *  null when none. The map shows them unless `MapSlot.banners` says otherwise */
  banners: ReactNode
  /** the facts a layout arranges by */
  facts: ConsoleFacts
  /** what a layout may do for the person */
  act: ConsoleActs
}

/** A layout's props. */
export interface LayoutProps {
  parts: ConsoleParts
}

/** A layout: the root's children, arranged from the parts. */
export type LayoutComponent = (props: LayoutProps) => ReactNode
