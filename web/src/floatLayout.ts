// The person's own layout of the floating panels (FloatingPanel.tsx): the spot of each panel they
// dragged, remembered in this browser only (localStorage), a convenience: a panel nobody dragged
// opens on its own free spot, and the console works the same when storage is unavailable or holds
// something else. Double-clicking a panel's bar forgets its spot. Pure but for the last two
// functions (the storage): floatLayout.check.ts.
import type { Spot } from './floatPlace'

export const LAYOUT_KEY = 'murmur.panels'
export const LAYOUT_VERSION = 1
/** At most this many panels remembered (the console has fewer than ten). */
export const LAYOUT_MAX = 24
/** An offset further than this from an edge is not a spot this console made, px. */
export const OFFSET_MAX = 20000

/** Each remembered panel's spot, by the panel's id. */
export type Layout = Readonly<Record<string, Spot>>

/** A panel's id: lower case words joined by hyphens ("weather", "vehicle-details"). */
const PANEL_ID = /^[a-z][a-z0-9-]{0,31}$/

const offset = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v) && Math.abs(v) <= OFFSET_MAX

/** One stored spot, or null when it is not one. */
export function spotFrom(v: unknown): Spot | null {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return null
  const s = v as Record<string, unknown>
  if ((s.ax !== 'l' && s.ax !== 'r') || (s.ay !== 't' && s.ay !== 'b') || !offset(s.x) || !offset(s.y)) return null
  return { ax: s.ax, x: s.x, ay: s.ay, y: s.y }
}

/** The stored layout's text read back: every well-formed spot under a well-formed id, the rest
 *  dropped one by one; anything else (nothing, not JSON, another version) the empty layout. */
export function parseLayout(raw: string | null | undefined): Layout {
  if (!raw) return {}
  let v: unknown
  try {
    v = JSON.parse(raw)
  } catch {
    return {}
  }
  if (!v || typeof v !== 'object' || (v as { v?: unknown }).v !== LAYOUT_VERSION) return {}
  const spots = (v as { spots?: unknown }).spots
  if (!spots || typeof spots !== 'object' || Array.isArray(spots)) return {}
  const out: Record<string, Spot> = {}
  let n = 0
  for (const [id, entry] of Object.entries(spots)) {
    if (n >= LAYOUT_MAX) break
    const s = spotFrom(entry)
    if (!PANEL_ID.test(id) || !s) continue
    out[id] = s
    n++
  }
  return out
}

/** The layout with a panel's spot set, or forgotten (null). A bad id or spot changes nothing. */
export function withSpot(layout: Layout, id: string, spot: Spot | null): Layout {
  if (!PANEL_ID.test(id)) return layout
  const rest = Object.fromEntries(Object.entries(layout).filter(([k]) => k !== id))
  if (spot == null) return rest
  const s = spotFrom(spot)
  if (!s || (!(id in layout) && Object.keys(rest).length >= LAYOUT_MAX)) return layout
  return { ...rest, [id]: s }
}

/** The text stored for a layout. */
export function layoutText(layout: Layout): string {
  return JSON.stringify({ v: LAYOUT_VERSION, spots: layout })
}

// ---- this browser's storage: every read and write may fail (a private window, blocked storage) ----

let held: Layout | null = null

/** The layout remembered in this browser (read once, then kept in memory). */
export function savedLayout(): Layout {
  if (held) return held
  try {
    held = parseLayout(localStorage.getItem(LAYOUT_KEY))
  } catch {
    held = {}
  }
  return held
}

/** Remember a panel's spot, or forget it (null). Without storage it holds for this page only. */
export function saveSpot(id: string, spot: Spot | null) {
  held = withSpot(savedLayout(), id, spot)
  try {
    localStorage.setItem(LAYOUT_KEY, layoutText(held))
  } catch {
    // storage unavailable: the spot holds until the page reloads
  }
}
