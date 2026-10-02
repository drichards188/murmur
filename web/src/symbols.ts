import ms from 'milsymbol'
import type { Kind, Vehicle } from './types'
import { callsign } from './types'

// MIL-STD-2525C symbol IDs (friendly, present), plus a "type" label where the 2525C
// function code is generic. Air: fixed-wing drone, rotary-wing drone, lighter-than-air.
// Sea: generic surface and subsurface tracks labelled USV/UUV. Ground: equipment (antenna).
const SIDC: Record<Kind, { code: string; type?: string }> = {
  plane: { code: 'SFAPMFQ--------' },
  vtol: { code: 'SFAPMFQ--------', type: 'VTOL' },
  copter: { code: 'SFAPMHQ--------' },
  blimp: { code: 'SFAPML---------' },
  boat: { code: 'SFSP-----------', type: 'USV' },
  sub: { code: 'SFUP-----------', type: 'UUV' },
  tracker: { code: 'SFGPE----------', type: 'ANT TRK' },
  ugv: { code: 'SFGPEV---------', type: 'UGV' },
  unknown: { code: 'SUPP-----------' },
}

export interface RenderedSymbol {
  svg: string
  anchor: { x: number; y: number }
  size: { width: number; height: number }
}

const cache = new Map<string, RenderedSymbol>()

export function symbolSvg(v: Vehicle, opts: { stale: boolean; size?: number; labels?: boolean; heading?: boolean }): RenderedSymbol {
  const kind: Kind = v.kind ?? 'unknown'
  const heading = v.heading == null || opts.heading === false ? undefined : Math.round(v.heading / 5) * 5
  const labels = opts.labels ?? true
  // without labels the drawing depends only on the symbol code, so vehicles share entries
  const key = labels
    ? `${kind}|${heading}|${opts.stale}|${opts.size}|1|${v.uid}|${v.identity?.callsign}|${v.identity?.short}`
    : `${v.identity?.sidc ?? kind}|${opts.stale}|${opts.size}|0`
  const hit = cache.get(key)
  if (hit) return hit
  // stale tracks get status "A" (anticipated), which milsymbol draws with a dashed frame
  // the fleet manifest knows the platform; otherwise fall back to the autopilot type
  const sidc = sidcOf(v, opts.stale)
  const sym = new ms.Symbol(sidc, {
    size: opts.size ?? 28,
    uniqueDesignation: labels ? callsign(v) : undefined,
    type: labels ? (v.identity?.short ?? SIDC[kind].type) : undefined,
    direction: labels ? heading : undefined,
    infoColor: '#d7dde4',
    outlineColor: '#0b0f14',
    outlineWidth: 3,
  } as Record<string, unknown>)
  const out = { svg: sym.asSVG(), anchor: sym.getAnchor(), size: sym.getSize() }
  cache.set(key, out)
  return out
}

function sidcOf(v: Vehicle, stale: boolean) {
  const base = v.identity?.sidc ?? SIDC[v.kind ?? 'unknown'].code
  return stale ? base.slice(0, 3) + 'A' + base.slice(4) : base
}

/** The model label milsymbol draws left of the frame (map text in the GPU layers). */
export function typeLabel(v: Vehicle): string {
  return v.identity?.short ?? SIDC[v.kind ?? 'unknown'].type ?? ''
}

/** Icons are rasterised at this multiple of their on-screen size, so they stay sharp on HiDPI. */
const ICON_SCALE = 2
const ICON_PX = 28

/** A bare symbol frame (no labels, no heading) as a deck.gl IconLayer icon. One entry per
 *  symbol code and stale state, so the whole fleet shares a few dozen icons in one atlas. */
export interface FrameIcon {
  id: string
  url: string
  width: number
  height: number
  anchorX: number
  anchorY: number
  /** on-screen height in CSS pixels (getSize) */
  size: number
  /** half extents of the frame around its anchor, CSS pixels: where labels go */
  halfW: number
  halfH: number
}

const frames = new Map<string, FrameIcon>()

export function frameIcon(v: Vehicle, stale: boolean): FrameIcon {
  const sidc = sidcOf(v, stale)
  const hit = frames.get(sidc)
  if (hit) return hit
  const sym = new ms.Symbol(sidc, { size: ICON_PX * ICON_SCALE, outlineColor: '#0b0f14', outlineWidth: 3 * ICON_SCALE } as Record<string, unknown>)
  const size = sym.getSize()
  const anchor = sym.getAnchor()
  const width = Math.ceil(size.width)
  const height = Math.ceil(size.height)
  const icon: FrameIcon = {
    id: sidc,
    url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(sym.asSVG())}`,
    width,
    height,
    anchorX: anchor.x,
    anchorY: anchor.y,
    size: height / ICON_SCALE,
    halfW: Math.max(anchor.x, width - anchor.x) / ICON_SCALE,
    halfH: Math.max(anchor.y, height - anchor.y) / ICON_SCALE,
  }
  frames.set(sidc, icon)
  return icon
}

/** CSS px from the symbol centre to the arrow tip, about milsymbol's direction arrow at size 28 */
const ARROW_LEN = 34

function arrowSvg(w: number, h: number, s: number) {
  const d = `M${w / 2} ${h} V${3 * s} M${w / 2 - 4 * s} ${8 * s} L${w / 2} ${3 * s} L${w / 2 + 4 * s} ${8 * s}`
  return (
    `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">` +
    `<g stroke-linecap="round" stroke-linejoin="round" fill="none">` +
    `<path d="${d}" stroke="#0b0f14" stroke-width="${4.5 * s}"/><path d="${d}" stroke="#d7dde4" stroke-width="${1.6 * s}"/>` +
    `</g></svg>`
  )
}

/** Heading arrow: a shaft from the symbol centre (the icon's anchor, bottom middle) to an
 *  arrowhead. Rotated per vehicle (getAngle), so one icon serves every heading. */
export const ARROW_ICON = {
  id: 'heading-arrow',
  url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(arrowSvg(12 * ICON_SCALE, ARROW_LEN * ICON_SCALE, ICON_SCALE))}`,
  width: 12 * ICON_SCALE,
  height: ARROW_LEN * ICON_SCALE,
  anchorX: 6 * ICON_SCALE,
  anchorY: ARROW_LEN * ICON_SCALE,
  size: ARROW_LEN,
}
