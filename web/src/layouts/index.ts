// The layouts the console can show, one file each (parts.ts says how one is made), and the one this
// page shows: `?layout=` read once as the page loads (layoutName.ts; missing or unknown: decisions).
import { Cockpit } from './Cockpit'
import { Decisions } from './Decisions'
import { layoutName, type LayoutName } from './layoutName'
import type { LayoutComponent } from './parts'
import { Rail } from './Rail'

const LAYOUTS: Readonly<Record<LayoutName, LayoutComponent>> = { cockpit: Cockpit, rail: Rail, decisions: Decisions }

/** The layout this page shows (App.tsx puts it on the root, `data-layout`). */
export const LAYOUT: LayoutName = layoutName(typeof location === 'undefined' ? '' : location.search)

/** Its component. */
export const ShownLayout: LayoutComponent = LAYOUTS[LAYOUT]
