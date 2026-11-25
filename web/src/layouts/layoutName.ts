// Which layout the console shows (layouts/index.ts): `?layout=` in the address, read once as the
// page loads, so the layouts (bd epic murmur-iobd; decisions is the console, the others stay
// reachable to compare) can be shown side by side on the same live run. Display only. Pure: layoutName.check.ts.

/** The layouts, the default first. */
export const LAYOUT_NAMES = ['decisions', 'cockpit', 'rail'] as const

export type LayoutName = (typeof LAYOUT_NAMES)[number]

/** The layout for an address's query (`location.search`): the one `layout` names, in any case;
 *  missing, empty or unknown, the default (decisions). */
export function layoutName(search: string): LayoutName {
  const asked = new URLSearchParams(search).get('layout')?.trim().toLowerCase() ?? ''
  return LAYOUT_NAMES.find((n) => n === asked) ?? LAYOUT_NAMES[0]
}
