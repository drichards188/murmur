// Engine and bridge free text in a person's words, formatted around it in the console. The engine's
// own strings stay exactly as logged (replay, fixtures and digests read them); the console shows
// them with callsigns for our vehicles' uids, sensor names for sensor ids and times in words for
// epoch stamps. Only well-formed tokens are rewritten ("1:eo_ir", "our vehicle 1", "t=1795533619054");
// anything else passes through as it came. Also the few words the engagement cards build themselves
// (a time before or after now, a release against its plan, what a running engagement waits for).
// Pure: plainText.check.ts.
import { checkLabel, SENSOR, span } from './engageSteps.ts'

/** "a and b", "a, b and c". */
function and(xs: readonly string[]): string {
  return xs.length <= 1 ? (xs[0] ?? '') : `${xs.slice(0, -1).join(', ')} and ${xs[xs.length - 1]}`
}

/** Sensor ids as the engine names them ("1:radar", "1:eo_ir", "2:radar") by vehicle, in words:
 *  "HUNTER 1 (radar and EO/IR) and REAPER 1 radar". No semicolon, so a sentence it sits in is not
 *  split into bullets (bluf.ts splits on "; "). */
export function sensorList(ids: readonly string[], nameOf: (uid: string) => string): string {
  const by = new Map<string, string[]>()
  for (const s of ids) {
    const i = s.indexOf(':')
    const uid = i >= 0 ? s.slice(0, i).trim() : ''
    const kind = (i >= 0 ? s.slice(i + 1) : s).trim()
    const label = SENSOR[kind] ?? kind.replace(/_/g, ' ')
    const list = by.get(uid) ?? []
    if (!list.includes(label)) list.push(label)
    by.set(uid, list)
  }
  return and(
    [...by.entries()].map(([uid, kinds]) => {
      const who = uid ? nameOf(uid) : ''
      if (kinds.length === 1) return who ? `${who} ${kinds[0]}` : kinds[0]
      return who ? `${who} (${and(kinds)})` : and(kinds)
    }),
  )
}

/** An epoch stamp as the engine (ms) or the bridge (s) writes it, in ms. */
export function epochMs(n: number): number {
  return n >= 1e11 ? n : n * 1000
}

export interface PlainContext {
  /** one of ours by its uid as the engine names it ("1" -> "HUNTER 1") */
  nameOf: (uid: string) => string
  /** an epoch time (ms) in words, with the preposition the text had before it ("at", "since"),
   *  or null: "2 min 01 s ago", "at T+3:45" */
  when: (ms: number, prep: 'at' | 'since' | null) => string
}

const SENSOR_KINDS = 'eo_ir|radar|sonar|esm|ais'
/** a run of sensor ids: "1:radar, 1:eo_ir, 2:radar" */
const SENSOR_RUN = new RegExp(`\\b\\d+:(?:${SENSOR_KINDS})\\b(?:,\\s*\\d+:(?:${SENSOR_KINDS})\\b)*`, 'g')
/** an epoch stamp: "at t=1795533619054", "since t=1795533627.2" (nine digits or more: never a
 *  small number of seconds) */
const EPOCH = /\b(?:(at|since) )?t=(\d{9,}(?:\.\d+)?)/g
const OUR_VEHICLE = /\bour vehicle (\d+)\b/g

/** `text` with its sensor ids, epoch stamps and "our vehicle <uid>" in words. */
export function plainText(text: string, ctx: PlainContext): string {
  return text
    .replace(SENSOR_RUN, (run) => sensorList(run.split(/,\s*/), ctx.nameOf))
    .replace(EPOCH, (_m, prep: string | undefined, n: string) => ctx.when(epochMs(Number(n)), (prep as 'at' | 'since' | undefined) ?? null))
    .replace(OUR_VEHICLE, (_m, uid: string) => ctx.nameOf(uid))
}

/** How long before or after `nowMs` (both ms) a time is, in words: "6 min 02 s ago", "in 44 s",
 *  "now". */
export function relWords(atMs: number, nowMs: number): string {
  const d = atMs - nowMs
  if (Math.abs(d) < 500) return 'now'
  return d < 0 ? `${span(-d)} ago` : `in ${span(d)}`
}

/** A `when` for PlainContext: times in words from now (the cards in the Inbox). */
export function agoWhen(nowMs: number): PlainContext['when'] {
  return (ms) => relWords(ms, nowMs)
}

/** m:ss (h:mm:ss past an hour) of `s` seconds, never negative. */
export function clockText(s: number): string {
  const n = Math.max(0, Math.round(s))
  const h = Math.floor(n / 3600)
  const m = Math.floor((n % 3600) / 60)
  const ss = String(n % 60).padStart(2, '0')
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${ss}` : `${m}:${ss}`
}

/** A `when` for PlainContext on a situation's clock: "at T+3:45" from its start (`zeroS`, epoch s). */
export function situationWhen(zeroS: number): PlainContext['when'] {
  return (ms, prep) => `${prep ?? 'at'} T+${clockText(ms / 1000 - zeroS)}`
}

/** A release against the time its plan predicted (`plannedMs - releasedMs`): "13 s after the time
 *  planned", "on the time planned". */
export function againstPlan(plannedMs: number, releasedMs: number): string {
  const d = releasedMs - plannedMs
  if (Math.abs(d) < 1000) return 'on the time planned'
  return d > 0 ? `${span(d)} after the time planned` : `${span(-d)} before the time planned`
}

/** What a running engagement's picture still waits for, from the checks not met yet (`rules`, the
 *  watch verdict's), with its platform's name: "waiting for REAPER 1 to arrive and for the effect
 *  time". */
export function waitingWords(rules: readonly string[], platform: string): string {
  const what = (rule: string) => {
    switch (rule) {
      case 'arrival':
        return `${platform} to arrive`
      case 'effect_time':
        return 'the effect time'
      case 'alignment':
        return `${platform} to line up on the target`
      case 'engage_window':
        return 'the window to open'
      default:
        return checkLabel(rule)
    }
  }
  const parts = [...new Set(rules)].map(what)
  if (!parts.length) return 'waiting for the next step'
  return `waiting for ${and(parts.map((p, k) => (k ? `for ${p}` : p)))}`
}
