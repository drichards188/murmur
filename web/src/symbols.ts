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
  unknown: { code: 'SUPP-----------' },
}

export interface RenderedSymbol {
  svg: string
  anchor: { x: number; y: number }
}

const cache = new Map<string, RenderedSymbol>()

export function symbolSvg(v: Vehicle, opts: { stale: boolean; size?: number; labels?: boolean }): RenderedSymbol {
  const kind: Kind = v.kind ?? 'unknown'
  const heading = v.heading == null ? undefined : Math.round(v.heading / 5) * 5
  const labels = opts.labels ?? true
  const key = `${kind}|${heading}|${opts.stale}|${opts.size}|${labels}|${v.sysid}`
  const hit = cache.get(key)
  if (hit) return hit
  // stale tracks get status "A" (anticipated), which milsymbol draws with a dashed frame
  const base = SIDC[kind].code
  const sidc = opts.stale ? base.slice(0, 3) + 'A' + base.slice(4) : base
  const sym = new ms.Symbol(sidc, {
    size: opts.size ?? 28,
    uniqueDesignation: labels ? callsign(v) : undefined,
    type: labels ? SIDC[kind].type : undefined,
    direction: labels ? heading : undefined,
    infoColor: '#d7dde4',
    outlineColor: '#0b0f14',
    outlineWidth: 3,
  } as Record<string, unknown>)
  const out = { svg: sym.asSVG(), anchor: sym.getAnchor() }
  cache.set(key, out)
  return out
}
