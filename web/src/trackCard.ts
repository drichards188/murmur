// The track card's words (TracksPanel.tsx, ledger 1 section 5): the one line it leads with (R7,
// change 15: "Suspect: small fast boat, no AIS, identified by HUNTER 1's camera; seen 2 s ago; 82°
// at 15 m/s"), the estimate's reason in words, its position's uncertainty in words, and the one
// line saying what deciding a held-up item lets go on (the strip holds the alarm and its timer; the
// card the consequence, change 16). Display only. Pure (no browser): trackCard.check.ts. The kind
// names (tracks.ts `kindLabel`) and our vehicles' names come in as functions.

type Level = 'detect' | 'recognise' | 'identify'

/** One sensor's look at the track, as the bridge's picture gives it (`identity.looks`). */
export interface Look {
  uid: number
  sensor: string
  level: Level | null
  confidence: number
}

export interface CardTrack {
  affiliation: string
  domain: string
  identity: {
    kind: string | null
    level?: Level | null
    ais: { name: string } | null
    supported_by: readonly string[]
    looks?: readonly Look[]
  }
  speed_mps: number
  course_deg: number | null
  alt_m: number | null
  last_seen_t: number
}

const AFF_WORD: Record<string, string> = { hostile: 'Hostile', suspect: 'Suspect', unknown: 'Unknown', neutral: 'Neutral', assumed_friend: 'Assumed friend' }
export const LEVEL_VERB: Record<Level, string> = { identify: 'identified', recognise: 'recognised', detect: 'detected' }
const SENSOR_WORD: Record<string, string> = { eo_ir: 'camera', radar: 'radar', sonar: 'sonar', esm: 'ESM', ais: 'AIS' }
const RANK: Record<Level, number> = { detect: 0, recognise: 1, identify: 2 }

/** The look that got furthest (then the surest): whose sensor says what the track is. */
export function bestLook(looks: readonly Look[] | undefined): Look | null {
  let best: Look | null = null
  for (const l of looks ?? []) {
    const r = l.level ? RANK[l.level] : -1
    const b = best?.level ? RANK[best.level] : -1
    if (!best || r > b || (r === b && l.confidence > best.confidence)) best = l
  }
  return best
}

/** "2 s", "4 min", "1.2 h" */
function ago(s: number): string {
  const n = Math.max(0, s)
  return n < 90 ? `${Math.round(n)} s` : n < 5400 ? `${Math.round(n / 60)} min` : `${(n / 3600).toFixed(1)} h`
}

/** Who says what it is: "identified by HUNTER 1's camera", else "by radar, EO/IR" from the sensors
 *  behind its identity, else nothing. */
function bySensor(t: CardTrack, name: (uid: number) => string): string {
  const l = bestLook(t.identity.looks)
  if (l && l.level) return `${LEVEL_VERB[l.level]} by ${name(l.uid)}'s ${SENSOR_WORD[l.sensor] ?? l.sensor}`
  const level = t.identity.kind != null && t.identity.level ? LEVEL_VERB[t.identity.level] : null
  const by = t.identity.supported_by.map((s) => SENSOR_WORD[s] ?? s)
  if (!by.length) return level ?? ''
  return `${level ?? 'seen'} by ${by.join(', ')}`
}

/** Where it heads: "82° at 15 m/s", with its height or depth off the surface. */
export function motionWords(t: Pick<CardTrack, 'speed_mps' | 'course_deg' | 'alt_m' | 'domain'>): string {
  const head = t.course_deg != null && t.speed_mps >= 0.5 ? `${Math.round(t.course_deg)}° at ${t.speed_mps.toFixed(0)} m/s` : t.speed_mps >= 0.5 ? `${t.speed_mps.toFixed(0)} m/s` : 'still'
  const up = t.alt_m != null && t.domain === 'air' ? `, ${Math.round(t.alt_m)} m up` : t.alt_m != null && t.domain === 'subsurface' ? `, ${Math.round(Math.max(0, -t.alt_m))} m deep` : ''
  return `${head}${up}`
}

/** The card's first line: its affiliation and what it is, its AIS, who says so; how fresh; where
 *  it heads. `nowS`: the bridge clock, s. */
export function leadLine(t: CardTrack, nowS: number, kindLabel: (k: string | null) => string, name: (uid: number) => string): string {
  const what = [kindLabel(t.identity.kind), t.identity.ais ? `AIS ${t.identity.ais.name}` : 'no AIS', bySensor(t, name)].filter(Boolean).join(', ')
  return `${AFF_WORD[t.affiliation] ?? t.affiliation}: ${what}; seen ${ago(nowS - t.last_seen_t)} ago; ${motionWords(t)}`
}

/** The engine's reason for its estimate in words: its kind names as the console says them
 *  ("fishing_vessel, small craft..." becomes "fishing vessel, small craft..."). */
export function reasonWords(reason: string, kindLabel: (k: string | null) => string): string {
  return reason.replace(/\b[a-z]+(?:_[a-z]+)+\b/g, (k) => kindLabel(k))
}

/** Its position's uncertainty in words: "good to about 10 m". */
export function positionWords(sigmaM: number): string {
  const m = Math.max(1, sigmaM)
  const nice = m < 20 ? Math.ceil(m / 5) * 5 : m < 200 ? Math.round(m / 10) * 10 : m < 1000 ? Math.round(m / 50) * 50 : null
  return nice != null ? `good to about ${nice} m` : `good to about ${(m / 1000).toFixed(1)} km`
}

/** An item that alone holds up a nomination (blocking.ts), as its card says what deciding it lets
 *  go on, in one line: no item number, no timer (the strip has them). */
export function holdingLine(b: { kind: 'declaration' | 'flag'; track: string; platforms: readonly { uid: number; callsign: string | null }[] }): string {
  const names = b.platforms.map((p) => p.callsign ?? `vehicle ${p.uid}`)
  const what =
    names.length === 1
      ? `${names[0]}'s nomination on ${b.track}`
      : names.length > 1
        ? `a nomination on ${b.track} with ${names.slice(0, -1).join(', ')} or ${names[names.length - 1]}`
        : `a nomination on ${b.track}`
  const ask = b.kind === 'declaration' ? 'Confirming it lets' : 'Only a hostile declaration lets'
  return `${ask} ${what} go ahead: every other check passes.`
}
