// The vehicle details' words (VehicleDetails.tsx, ledger 1 section 14): its link in one line when
// normal ("SATCOM, good") and what failed when not (R5, change 34), and where a silent vehicle
// probably is in words rather than coordinates (change 33). Display only: nothing here is logged or
// decided. Pure (no browser): detailsText.check.ts.
import { LOSS_PCT } from './vehicleException.ts'

const POINTS = ['N', 'NE', 'E', 'SE', 'S', 'SW', 'W', 'NW']

/** A bearing as one of eight compass points. */
export function compass(deg: number): string {
  return POINTS[Math.round((((deg % 360) + 360) % 360) / 45) % 8]
}

function dist(m: number): string {
  return m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m)} m`
}

/** Where a silent vehicle probably is against its last report: "about 1.2 km NE of its last
 *  report", or "where it last reported" within a metre. */
export function awayText(m: number, bearingDeg: number): string {
  return m < 1 ? 'where it last reported' : `about ${dist(m)} ${compass(bearingDeg)} of its last report`
}

/** What a lost-link failsafe does, in words (the bridge's `estimate.rs` actions). */
export const FAILSAFE_WORDS: Record<string, string> = { rtl: 'returns home', surface: 'surfaces', disarm: 'stops where it is' }

/** A link's state as the details' Link section reads it. */
export interface LinkFacts {
  /** the link's kind in words ("SATCOM", "Mesh radio") */
  type: string
  stale: boolean
  /** ms since it was last heard, and how often it should be */
  sinceMs: number | null
  expectedMs: number | null
  lossPct: number | null
  /** the link emulator's state and its cause in words, when one runs */
  emu: { state: 'ok' | 'degraded' | 'down'; cause: string } | null
}

export interface LinkLine {
  text: string
  tone: '' | 'warn' | 'bad'
}

function secs(ms: number): string {
  const s = Math.floor(ms / 1000)
  return s < 90 ? `${s} s` : `${Math.round(s / 60)} min`
}

/** The link in one line: "SATCOM, good" when nothing is wrong; else what failed, the worst first
 *  (silent, down, degraded, measured loss past LOSS_PCT, heard late). */
export function linkLine(f: LinkFacts): LinkLine {
  const cause = (c: string) => (c ? `: ${c}` : '')
  if (f.stale) return { text: `${f.type}: silent ${secs(f.sinceMs ?? 0)}`, tone: 'bad' }
  if (f.emu?.state === 'down') return { text: `${f.type}: down${cause(f.emu.cause)}`, tone: 'bad' }
  if (f.emu?.state === 'degraded') return { text: `${f.type}: degraded${cause(f.emu.cause)}`, tone: 'warn' }
  if (f.lossPct != null && f.lossPct > LOSS_PCT) return { text: `${f.type}: ${Math.round(f.lossPct)}% loss`, tone: 'warn' }
  if (f.sinceMs != null && f.sinceMs > 2 * (f.expectedMs ?? 1000)) return { text: `${f.type}: last heard ${secs(f.sinceMs)} ago`, tone: 'warn' }
  return { text: `${f.type}, good`, tone: '' }
}
