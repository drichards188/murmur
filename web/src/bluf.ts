// BLUF (bottom line up front): how the console turns text into a headline, a few bullets and the
// rest behind "More" (Bluf.tsx draws it). Pure functions, checked by bluf.check.ts.
//
// Three sources:
//  - an agent's rationale, by a convention it is asked to follow (parseRationale): the first line
//    is the headline, then "- " bullets, then a blank line and any detail. The logged rationale
//    stays the plain string it always was; an older free-text one falls back to its first
//    sentence and the rest behind More.
//  - an engine or bridge reason (reasonBluf): an identify outcome the engine words from fixed
//    parts is taken apart into its fields; any other splits on "; ".
//  - a checks list (checksSummary).

export interface BlufParts {
  headline: string
  bullets: string[]
  /** the rest, shown behind "More"; empty when there is none */
  more: string
}

/** The longest headline shown whole (characters); longer ones are cut at a sentence or a word. */
export const HEADLINE_MAX = 140
/** The most bullets shown before More. */
export const BULLETS_MAX = 4

const clean = (s: string) => s.replace(/\s+/g, ' ').trim()

/** The first sentence of `s` (to a full stop, "?" or "!" followed by a space), or all of `s`. */
function firstSentence(s: string): { first: string; rest: string } {
  const m = /^(.+?[.!?])\s+(?=\S)/s.exec(s)
  if (m && m[1].length <= HEADLINE_MAX) return { first: m[1], rest: s.slice(m[0].length) }
  return { first: s, rest: '' }
}

/** `s` as a headline of at most HEADLINE_MAX: the whole, its first sentence, or a cut at a word
 *  (then `rest` is the whole, so nothing is lost). */
function cutHeadline(s: string): { first: string; rest: string } {
  if (s.length <= HEADLINE_MAX) return { first: s, rest: '' }
  const s1 = firstSentence(s)
  if (s1.rest) return s1
  const cut = s.lastIndexOf(' ', HEADLINE_MAX - 1)
  const at = cut > HEADLINE_MAX / 2 ? cut : HEADLINE_MAX - 1
  return { first: `${s.slice(0, at).trimEnd()}…`, rest: s }
}

function capBullets(bullets: string[], more: string): { bullets: string[]; more: string } {
  if (bullets.length <= BULLETS_MAX) return { bullets, more }
  const extra = bullets
    .slice(BULLETS_MAX)
    .map((b) => `- ${b}`)
    .join('\n')
  return { bullets: bullets.slice(0, BULLETS_MAX), more: more ? `${extra}\n\n${more}` : extra }
}

const BULLET = /^\s*[-*•]\s+\S/

/** A rationale in the convention (headline, "- " bullets, blank line, detail) or free text. */
export function parseRationale(text: string): BlufParts {
  const t = text.replace(/\r\n/g, '\n').trim()
  if (!t) return { headline: '', bullets: [], more: '' }
  const lines = t.split('\n')
  const head = lines[0].trim()
  let k = 1
  const bullets: string[] = []
  while (k < lines.length && BULLET.test(lines[k])) {
    bullets.push(lines[k].replace(/^\s*[-*•]\s+/, '').trim())
    k++
  }
  const rest = lines.slice(k).join('\n').trim()
  // the convention: a headline line alone, or followed by bullets or by a blank line
  const conventional = !BULLET.test(head) && (lines.length === 1 || bullets.length > 0 || (lines[1] ?? '').trim() === '')
  if (conventional) {
    const h = cutHeadline(clean(head))
    const more = [h.rest, rest].filter(Boolean).join('\n\n')
    return { headline: h.first, ...capBullets(bullets, more) }
  }
  // free text (an older rationale, or a paragraph wrapped over lines): its first sentence, the rest behind More
  const flat = clean(t)
  const s = firstSentence(flat)
  if (s.rest) return { headline: s.first, bullets: [], more: s.rest }
  const h = cutHeadline(flat)
  return { headline: h.first, bullets: [], more: h.rest ? t : '' }
}

// ---- engine and bridge reasons ----------------------------------------------------------------

const words = (s: string) => s.replace(/_/g, ' ')

/** a sensor as a person names it: an EO/IR turret is a camera */
const SENSOR_WORD: Record<string, string> = { eo_ir: 'camera', esm: 'ESM' }

/** "1:sonar" -> "sonar"; "puma-1:eo_ir" -> "camera"; "PUMA 1's eo_ir" -> "PUMA 1's camera": the
 *  sensor of a source id, in plain words. */
function sensorOf(by: string): string {
  const i = by.indexOf(':')
  return words((i >= 0 ? by.slice(i + 1) : by).replace(/\b(eo_ir|esm)\b/g, (k) => SENSOR_WORD[k]))
}

/** What the engine words an identify order's end from (crates/murmur-decide/src/identify.rs
 *  `outcome` and `then_phrase`):
 *  "track T-1 identified as K by S[, S]" (rules 23: "by PUMA 1's eo_ir") + "; it contradicts|agrees
 *  with|differs from its identity before, W[: conflict]" | "; it was W before" + rules 23 "; meanwhile
 *  HUNTER 1 identified it as K" + "; holding: ..." | "; back to ...". */
const IDENTIFIED =
  /^track (\S+) identified as (\S+?)(?: by (.+?))?(?:; it (contradicts|agrees with|differs from) its identity before, ([^:;]+?)(?:: (.+?))?|; it was (.+?) before)?(?:; meanwhile (.+?))?(?:; (holding: .*|back to .*))?$/s

/** A conflict as tracks word it: "AIS says X (NAME) but S identifies it as Y", or the older
 *  "AIS says X; S identifies Y". */
function conflictParts(c: string): { ais: string; sensor: string; kind: string } | null {
  const m = /^AIS says (.+?) but (.+?) identifies it as (.+)$/s.exec(c) ?? /^AIS says (.+?); (.+?) identifies (.+)$/s.exec(c)
  if (!m) return null
  return { ais: m[1], sensor: m[2], kind: m[3] }
}

/** "law_enforcement (CGC TERN)" -> "CGC TERN"; "fishing_vessel" -> "fishing vessel". */
function aisName(ais: string): string {
  const m = /\(([^)]+)\)\s*$/.exec(ais)
  return m ? m[1] : words(ais)
}

export interface ReasonContext {
  /** the vehicle the reason is about, by its callsign ("PUMA 1") */
  who?: string
}

function identifyBluf(text: string, ctx: ReasonContext): BlufParts | null {
  const m = IDENTIFIED.exec(text.trim())
  if (!m) return null
  const [, track, kind, by, rel, was, conflict, wasOnly, meanwhile, then] = m
  const sensors = by ? by.split(',').map((s) => s.trim()).filter(Boolean) : []
  const sensor = sensors.length ? sensors.map(sensorOf).join(' and ') : 'our sensors'
  const bullets: string[] = []
  const cp = conflict ? conflictParts(conflict) : null
  let headline: string
  if (rel === 'contradicts') {
    headline = cp
      ? `${track} identity conflict: ${sensorOf(cp.sensor)} says ${words(kind)}, AIS says ${aisName(cp.ais)}`
      : `${track} identity conflict: ${sensor} says ${words(kind)}, which contradicts what we had`
    bullets.push(`earlier identity: ${words(was)}`)
    if (!cp && conflict) bullets.push(conflict)
  } else {
    headline = `${track} identified: ${sensor} says ${words(kind)}`
    if (rel === 'agrees with') bullets.push(`agrees with its earlier identity: ${words(was)}`)
    else if (rel === 'differs from') bullets.push(`differs from its earlier identity: ${words(was)}`)
    else if (wasOnly) bullets.push(`was ${wasOnly} before`)
  }
  // rules 23: what other vehicles identified while it looked (it did not end the look)
  if (meanwhile) bullets.push(`meanwhile ${words(meanwhile)}`)
  if (then) {
    const who = ctx.who ?? 'vehicle'
    if (then.startsWith('holding: no order to go back to')) bullets.push(`${who} holding (no earlier order)`)
    else if (then.startsWith('holding: ')) bullets.push(`${who} holding: ${then.slice('holding: '.length)}`)
    else bullets.push(`${ctx.who ? `${ctx.who} ` : ''}${then}`)
  }
  return { headline, bullets, more: text.trim() }
}

/** An engine or bridge reason as a headline, bullets and the original behind More. Identify
 *  outcomes are taken apart by their fields; any other reason splits on "; " (the first part is
 *  the headline). A reason with nothing to split is its own headline, with no More. */
export function reasonBluf(text: string | null | undefined, ctx: ReasonContext = {}): BlufParts {
  const t = (text ?? '').trim()
  if (!t) return { headline: '', bullets: [], more: '' }
  const id = identifyBluf(t, ctx)
  if (id) return id
  const parts = t.split(/;\s+/).map((s) => s.trim()).filter(Boolean)
  const h = cutHeadline(parts[0] ?? t)
  const bullets = parts.slice(1)
  if (bullets.length === 0 && !h.rest) return { headline: h.first, bullets: [], more: '' }
  const c = capBullets(bullets, '')
  return { headline: h.first, bullets: c.bullets, more: t }
}

/** A situation's watch item: its written headline and bullets with the full text as More, or,
 *  without them, the text's first sentence and the rest. */
export function watchParts(w: { text: string; headline?: string; bullets?: string[] }): BlufParts {
  if (w.headline) return { headline: w.headline, bullets: w.bullets ?? [], more: w.text }
  return parseRationale(w.text)
}

// ---- alerts and review items ------------------------------------------------------------------

/** What an alert asks of the operator, by its kind, in plain words (the card's header already
 *  names the kind and the vehicle). The bridge's own detail follows as bullets, whole under More. */
const ALERT_HEADLINE: Record<string, string> = {
  lost_link: 'No telemetry: its link is down',
  not_moving: 'Not moving as ordered: look at it',
  left_task: 'Left its task: look at it, re-order or let it go',
  task_failed: 'Order failed: the vehicle is not carrying it out',
  ack_timeout: 'No acknowledgement: it may not have the order',
  may_be_running: 'The order may still be running: decide what to do',
  restore_pending: 'Its own failsafe values are not restored: write off or wait',
  space_missed: 'Satellite pass missed: pick another option',
  space_recheck_unknown: 'Satellite task could not be re-checked: decide',
  spent: 'Spent by its own effect but still heard: un-spend or leave',
  path_crosses_land: 'Its way home crosses land: watch it',
  identify_holding: 'Identified, and holding: it has no order to go back to',
  identify_lost: 'Identify order ended: its track was lost',
  identify_timed_out: 'Identify order ended: out of time, not identified',
  identify_held: "Its orbit can't follow the track: look at it",
  identify_meanwhile: 'For information: another vehicle identified its track, and its own look goes on',
}

/** An alert as headline (what it asks), the bridge's detail as up to three bullets, and the
 *  detail whole under More when it was cut. `lost_link` says how long, from the age. */
export function alertParts(a: { kind: string; detail: string }, ageText?: string): BlufParts {
  const headline = (a.kind === 'lost_link' && ageText ? `No telemetry for ${ageText}: its link is down` : ALERT_HEADLINE[a.kind]) ?? ''
  if (a.kind === 'lost_link') return { headline, bullets: [], more: '' }
  const d = reasonBluf(a.detail)
  if (!headline) return d
  const bullets = [d.headline, ...d.bullets].filter(Boolean)
  const c = capBullets(bullets.slice(0, 3), '')
  return { headline, bullets: c.bullets, more: bullets.length > 3 || d.more ? a.detail : '' }
}

/** What a review flag asks, by its kind; the engine's evidence follows as bullets. */
const FLAG_HEADLINE: Record<string, string> = {
  attribution: 'Not sure which track did it: confirm the concern or dismiss',
  conflict: 'The evidence conflicts: confirm the concern or dismiss',
  untracked: 'Act by something we hold no track of: confirm or dismiss',
  victim: 'Act against something that is not ours: confirm or dismiss',
  proximity: 'Keeps company with a declared hostile: confirm or dismiss',
  uncorroborated: 'One observation where policy wants two: confirm or dismiss',
}

export function flagParts(kind: string, evidence: string): BlufParts {
  const d = reasonBluf(evidence)
  const headline = FLAG_HEADLINE[kind]
  if (!headline) return d
  const c = capBullets([d.headline, ...d.bullets].filter(Boolean), '')
  return { headline, bullets: c.bullets, more: d.more || '' }
}

// ---- checks ---------------------------------------------------------------------------------------

export type Outcome = 'pass' | 'fail' | 'unverified' | 'not_evaluated'

/** The one line a checks list collapses to: "7 checks passed", or what did not pass first. */
export function checksSummary(checks: { rule: string; outcome: Outcome }[]): { text: string; tone: 'ok' | 'bad' | 'warn' } {
  const n = checks.length
  if (n === 0) return { text: 'no checks', tone: 'ok' }
  const by = (o: Outcome) => checks.filter((c) => c.outcome === o)
  const fail = by('fail')
  const unv = by('unverified')
  const ne = by('not_evaluated')
  const names = (cs: { rule: string }[]) => [...new Set(cs.map((c) => c.rule))].join(', ')
  if (fail.length) {
    const rest = [unv.length ? `${unv.length} unverified` : '', ne.length ? `${ne.length} not evaluated` : ''].filter(Boolean)
    return { text: `${fail.length} failed: ${names(fail)}${rest.length ? `; ${rest.join(', ')}` : ''}`, tone: 'bad' }
  }
  if (unv.length) return { text: `${unv.length} unverified: ${names(unv)}; judge ${unv.length === 1 ? 'it' : 'them'} yourself`, tone: 'warn' }
  if (ne.length) return { text: `${n - ne.length} of ${n} checks passed, ${ne.length} not evaluated: ${names(ne)}`, tone: 'warn' }
  return { text: `${n} check${n === 1 ? '' : 's'} passed`, tone: 'ok' }
}
