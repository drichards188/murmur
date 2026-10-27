// The engagement screens as steps (declare on a track card, nominate, sign in the inbox): which
// check belongs to which step of the nomination form, the one line a step, a platform or a checks
// list shows, what would clear a block, a rule declaration's evidence in plain words, and whether
// Nominate may be pressed. Pure functions over what the bridge already sends (the form's dry run,
// `POST /api/engage/options`, and the track's declaration); EngagePanel, EngageCard and
// TracksPanel draw them, engageSteps.check.ts checks them. Nothing here decides: the engine
// checks every nomination and every signature again, and these lines only say ahead of time what
// it will say.
//
// The form's rule: nothing may fail at the end that a step could have shown. Every check the dry
// run fails is owned by one step (`stepOf`), shown there with what would clear it (`clearText`),
// and named in what keeps Nominate disabled (`nominateReady`).
import { reasonBluf } from './bluf.ts'
import type { Check } from './decisions'
import type { BehavingInfo, EngageOption, KeepOutExplanation, KeepOutItem, KeepOutLost } from './engage'
import type { Against, Declaration, RuleEvidence } from './ident'
import type { Track } from './tracks'

export type Step = 'target' | 'platform' | 'effect' | 'checks'

export const STEP_NUMBER: Record<Step, number> = { target: 1, platform: 2, effect: 3, checks: 4 }
export const STEP_NAME: Record<Step, string> = { target: 'Target', platform: 'Platform', effect: 'Effect', checks: 'Checks' }

/** Each engagement check in plain words, as a noun ("1 blocks: keep-out of other tracks"); the
 *  rule's code stays beside it, small, for whoever knows it. */
export const CHECK_LABEL: Record<string, string> = {
  target_declared: 'hostile declaration',
  target_identity: 'target identity',
  target_recency: 'target seen recently',
  target_uncertainty: 'target position accuracy',
  engagement_zone: 'engagement zone',
  no_strike: 'no-strike areas',
  keep_out_friendly: 'keep-out of our vehicles',
  keep_out_tracks: 'keep-out of other tracks',
  platform_armed: 'platform armed',
  platform_free: 'platform free of other engagements',
  fallback_preload: 'fallback set on the platform',
  reach: 'reach within the window',
  capability: 'platform can engage',
  readiness: 'platform ready',
  platform_limits: 'platform limits',
  domain: 'land or water on the way',
  geofence: 'no-go areas on the way',
  reserves: 'reserves',
  plan: 'plan holds',
  signing_time: 'signing time',
  track_deviation: 'target where the plan expects it',
  platform_nav: 'platform navigation',
  platform_link: 'platform link',
  engage_window: 'window open',
  arrival: 'arrival at the point',
  effect_time: 'effect time',
  alignment: 'alignment',
}

export function checkLabel(rule: string): string {
  return CHECK_LABEL[rule] ?? rule.replace(/_/g, ' ')
}

/** The same, capitalised, for a list's row. */
export function checkTitle(rule: string): string {
  const l = checkLabel(rule)
  return l.charAt(0).toUpperCase() + l.slice(1)
}

/** The step whose choice can change a check: the target's own (the same for every platform), the
 *  effect's (the radius against the target's accuracy, the fallback the platform must hold), where
 *  and when the effect falls (zone, no-strike, keep-outs: the checks step, with the keep-out
 *  explanation), and everything else the platform's. */
const STEP_OF: Record<string, Step> = {
  target_declared: 'target',
  target_identity: 'target',
  target_recency: 'target',
  target_uncertainty: 'effect',
  fallback_preload: 'effect',
  engagement_zone: 'checks',
  no_strike: 'checks',
  keep_out_friendly: 'checks',
  keep_out_tracks: 'checks',
}

export function stepOf(rule: string): Step {
  return STEP_OF[rule] ?? 'platform'
}

/** An engagement is consequential: anything but a pass blocks (unverified and not evaluated too). */
export const blocks = (c: Pick<Check, 'outcome'>) => c.outcome !== 'pass'

const KEEP_OUT_RULES = ['keep_out_tracks', 'keep_out_friendly']

/** The checks on the target alone: the same on every platform's option, so a platform's line
 *  leaves them to steps 1 and 3. */
const TARGET_LEVEL = ['target_declared', 'target_identity', 'target_recency', 'target_uncertainty']

// ---- small formats (engage.ts has the same, but imports the bridge client) --------------------

/** "450 m", "2.30 km". */
export function dist(m: number): string {
  return Math.abs(m) >= 1000 ? `${(m / 1000).toFixed(2)} km` : `${Math.round(m)} m`
}

/** "40 s", "1 min 04 s", "12 min". */
export function span(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  if (s < 60) return `${s} s`
  if (s < 600) return `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, '0')} s`
  return `${Math.round(s / 60)} min`
}

// ---- a rule declaration's evidence -------------------------------------------------------------

const SENSOR: Record<string, string> = { radar: 'radar', eo_ir: 'EO/IR', ais: 'AIS', sonar: 'sonar', esm: 'ESM' }

/** "1:sonar", "1:eo_ir" -> "HUNTER 1 sonar and EO/IR": the observing sensors by vehicle. */
export function sensorsText(sensors: string[], nameOf: (uid: string) => string): string {
  const by = new Map<string, string[]>()
  for (const s of sensors) {
    const i = s.indexOf(':')
    const uid = i >= 0 ? s.slice(0, i) : ''
    const kind = i >= 0 ? s.slice(i + 1) : s
    const label = SENSOR[kind] ?? kind.replace(/_/g, ' ')
    by.set(uid, [...(by.get(uid) ?? []), label])
  }
  const and = (xs: string[]) => (xs.length <= 1 ? (xs[0] ?? '') : `${xs.slice(0, -1).join(', ')} and ${xs[xs.length - 1]}`)
  return [...by.entries()].map(([uid, kinds]) => (uid ? `${nameOf(uid)} ${and(kinds)}` : and(kinds))).join('; ')
}

export function againstText(a: Against, nameOf: (uid: string) => string): string {
  return a.kind === 'vehicle' ? nameOf(a.uid) : a.kind === 'track' ? `protected track ${a.track}` : 'something we hold no track of'
}

/** A rule declaration's evidence as the few facts a person checks before confirming it: the act and
 *  whom it was against, when, who saw it, the attribution against the rule's threshold, and a
 *  corroborating observation. `now` and `observed_t` are engine ms. */
export function evidenceBullets(e: RuleEvidence, nameOf: (uid: string) => string, now: number): string[] {
  const out = [
    `Act ${e.act} against ${againstText(e.against, nameOf)}, ${span(now - e.observed_t)} ago`,
    e.sensors.length ? `Seen by ${sensorsText(e.sensors, nameOf)}` : 'No sensor named',
    `Attributed to this track at ${e.attribution.toFixed(2)} (the rule needs ${e.threshold.toFixed(2)})`,
  ]
  const c = e.corroborated_by
  if (c) out.push(c.observer ? `Corroborated by ${nameOf(c.observer)}'s fix at ${c.attribution.toFixed(2)}` : `Corroborated by act ${c.act} at ${c.attribution.toFixed(2)}`)
  return out
}

/** A declaration as tracks and plans carry it, reduced to what the one line needs. */
export interface DeclFacts {
  affiliation: string
  /** an operator, or (rule) the rule's name */
  by: string
  rule: boolean
  act?: string
  confirmedBy?: string | null
}

/** A declaration as the engine's ledger holds it (and plans, pictures and the card's track carry
 *  it), reduced to the same few facts. */
export function declFactsOf(d: Declaration): DeclFacts {
  return d.by.kind === 'operator'
    ? { affiliation: d.affiliation, by: d.by.operator, rule: false }
    : { affiliation: d.affiliation, by: d.by.rule, rule: true, act: d.by.evidence.act, confirmedBy: d.confirmed?.by ?? null }
}

/** "Declared hostile by op-1"; "Declared hostile by the engine's rule (act A-1), confirmed by
 *  op-1"; "... not confirmed by an operator". */
export function declarationLine(d: DeclFacts): string {
  if (!d.rule) return `Declared ${d.affiliation} by ${d.by}`
  const on = d.act ? ` (act ${d.act})` : ''
  return d.confirmedBy ? `Declared ${d.affiliation} by the engine's rule${on}, confirmed by ${d.confirmedBy}` : `Declared ${d.affiliation} by the engine's rule${on}, not confirmed by an operator`
}

/** The declaration on a track (the tracks message's) as the steps read it. */
export function declFacts(t: Track): DeclFacts | null {
  const d = t.declared
  if (!d) return null
  return { affiliation: d.affiliation, by: d.rule ? d.by.replace(/^rule:/, '') : d.by, rule: !!d.rule, act: d.evidence?.act, confirmedBy: d.confirmed_by ?? null }
}

/** The track's AIS in a bullet: who it says it is, or that it sends none. */
export function aisText(t: Track): string {
  const a = t.identity.ais
  return a ? `AIS: ${a.name}, MMSI ${a.mmsi}, ${a.ship_type}` : 'No AIS'
}

// ---- step 1: the target --------------------------------------------------------------------------

export type TargetState =
  /** nothing chosen yet */
  | { kind: 'none' }
  /** the dry run has not answered yet */
  | { kind: 'checking' }
  | { kind: 'ready'; line: string }
  /** a rule declaration nobody confirmed, and the policy wants one before a nomination: the form
   *  offers the confirmation where it stands */
  | { kind: 'confirm'; line: string }
  | { kind: 'blocked'; line: string; checks: Check[] }

/** The target checks of a dry run: every option carries the same ones (the target's own). */
export function targetChecks(options: EngageOption[] | null): Check[] | null {
  if (!options || !options.length) return null
  return options[0].checks.filter((c) => stepOf(c.rule) === 'target')
}

export function targetState(decl: DeclFacts | null, options: EngageOption[] | null, chosen: boolean): TargetState {
  if (!chosen) return { kind: 'none' }
  const checks = targetChecks(options)
  const line = decl ? declarationLine(decl) : 'Not declared hostile'
  if (checks == null) {
    // no armed platform answered (none in the fleet), or not yet: judge by the declaration alone
    if (options == null) return { kind: 'checking' }
    return decl && decl.affiliation === 'hostile' ? { kind: 'ready', line } : { kind: 'blocked', line, checks: [] }
  }
  const failing = checks.filter(blocks)
  if (!failing.length) return { kind: 'ready', line }
  const declared = failing.find((c) => c.rule === 'target_declared')
  if (declared && failing.length === 1 && decl?.rule && !decl.confirmedBy && decl.affiliation === 'hostile') {
    return { kind: 'confirm', line }
  }
  return { kind: 'blocked', line, checks: failing }
}

// ---- what would clear a block ------------------------------------------------------------------

export interface ClearContext {
  /** the platform as the form names it ("REAPER 1") */
  platform?: string
  /** engine ms, for the keep-out's wait */
  now: number
}

/** The radius whose limit covers the target's 1-sigma now (rounded up to 5 m), read from the
 *  uncertainty check's own words ("T-0001 1-sigma 41.8 m (limit 37.5 m = 0.50 x effect radius 75
 *  m)"); null when the words are not those. */
export function radiusFor(c: Pick<Check, 'rule' | 'detail'>): number | null {
  if (c.rule !== 'target_uncertainty') return null
  const m = /1-sigma ([\d.]+) m \(limit [\d.]+ m = ([\d.]+) x effect radius/.exec(c.detail)
  if (!m) return null
  const sigma = Number(m[1])
  const frac = Number(m[2])
  if (!(sigma > 0) || !(frac > 0)) return null
  return Math.ceil(sigma / frac / 5) * 5
}

/** A window long enough for the reach check's predicted effect, with a minute to spare (rounded up
 *  to half a minute), read from its words ("the effect is predicted +71.0 s, after the window
 *  closes (+30.0 s)"); null for other words. In seconds. */
export function windowFor(c: Pick<Check, 'rule' | 'detail'>): number | null {
  if (c.rule !== 'reach') return null
  const m = /the effect is predicted \+([\d.]+) s, after the window closes/.exec(c.detail)
  if (!m) return null
  const s = Number(m[1])
  if (!(s > 0)) return null
  return Math.ceil((s + 60) / 30) * 30
}

/** The worst blocking item of a keep-out, in a few words: "T-0003 sailboat, short 120 m". */
export function keepOutWorst(ex: KeepOutExplanation | null | undefined): KeepOutItem | null {
  return ex?.items.find((i) => i.blocks) ?? null
}

/** What would clear a keep-out, on the picture now: the platform closer, a wait, or an acceptance
 *  (each the explanation's own: nothing here relaxes a keep-out). */
export function keepOutClear(ex: KeepOutExplanation | null | undefined, ctx: ClearContext): string {
  if (!ex?.blocked) return ''
  const ways: string[] = []
  const mw = ex.move_within
  if (mw?.within_m != null) ways.push(`${ctx.platform ?? 'the platform'} within ${dist(mw.within_m)} of ${mw.holding ? 'the target' : 'the marked point'}`)
  if (ex.wait) ways.push(`wait about ${span(ex.wait.nominate_t - ctx.now)} (if everything keeps behaving as now)`)
  const worst = keepOutWorst(ex)
  if (worst?.behaving?.acceptable) ways.push(`accept ${worst.behaving.track} as behaving now (below)`)
  if (worst?.lost) ways.push(`accept lost contact ${worst.lost.track} (below)`)
  if (!ways.length) return 'nothing on the picture now; see why below'
  return ways.join('; or ')
}

/** What would clear one failing check, in a few words, naming the step that offers it. */
export function clearText(c: Check, o: EngageOption | null, ctx: ClearContext, needsConfirm = false): string {
  switch (c.rule) {
    case 'target_declared':
      return needsConfirm ? 'confirm the declaration (step 1)' : 'an operator declares it hostile, from its track card'
    case 'target_identity':
      return 'a closer look by our sensors (identify, on its track card)'
    case 'target_recency':
      return 'a fresh detection of it'
    case 'target_uncertainty': {
      const r = radiusFor(c)
      return r != null ? `a radius of ${r} m or more (step 3), or a better fix` : 'a wider radius (step 3), or a better fix'
    }
    case 'keep_out_tracks':
    case 'keep_out_friendly':
      return keepOutClear(o?.keep_outs, ctx) || 'nothing on the picture now'
    case 'reach': {
      const w = windowFor(c)
      return w != null ? `a window of ${span(w * 1000)} or more (step 3), or a closer platform` : 'a closer or faster platform, or a longer window (step 3)'
    }
    case 'platform_free':
      return 'another platform: this one has an engagement in play'
    case 'platform_armed':
      return 'an armed platform'
    case 'readiness':
      return 'the platform navigation-ready'
    case 'engagement_zone':
      return 'the target inside an engagement zone'
    case 'no_strike':
      return 'a smaller radius (step 3), or the point clear of the no-strike area'
    case 'fallback_preload':
      return 'another fallback (step 3)'
    default:
      return ''
  }
}

/** A failing check's reason in one short line: the engine's first clause (its whole words stay
 *  behind More); for a keep-out, the worst blocker. */
export function whyText(c: Check, o: EngageOption | null): string {
  if (KEEP_OUT_RULES.includes(c.rule)) {
    const w = keepOutWorst(o?.keep_outs)
    if (w) return `${w.label}${w.short_m != null && w.short_m > 0 ? `, short ${dist(w.short_m)}` : ''}`
  }
  if (c.rule === 'target_declared' && /not confirmed by an operator/.test(c.detail)) return 'declared by rule, not confirmed by an operator'
  return reasonBluf(c.detail).headline
}

// ---- step 2: a platform's one line -----------------------------------------------------------------

export interface PlatformLine {
  ok: boolean
  text: string
}

/** A platform's line in the (unranked) list: whether it could take this engagement now and, if not,
 *  why, leaving out the target's own checks (step 1 shows those, the same for every platform). */
export function platformLine(o: EngageOption, now: number): PlatformLine {
  const order: Step[] = ['platform', 'effect', 'checks']
  const failing = o.checks.filter((c) => blocks(c) && !TARGET_LEVEL.includes(c.rule))
  failing.sort((a, b) => order.indexOf(stepOf(a.rule)) - order.indexOf(stepOf(b.rule)))
  const f = failing[0]
  if (!f) {
    const p = o.preview
    return { ok: true, text: p ? `Can do it: effect in ${span(p.effect_t - now)}, ${dist(p.distance_m)} at ${Math.round(p.cruise_mps)} m/s` : 'Passes its checks' }
  }
  const more = failing.length > 1 ? ` (+${failing.length - 1} more)` : ''
  return { ok: false, text: `Blocked: ${checkLabel(f.rule)}, ${whyText(f, o)}${more}` }
}

// ---- step 4: the checks in one line -------------------------------------------------------------

export interface ChecksLine {
  tone: 'ok' | 'bad' | 'none'
  /** "All 18 pass on the picture now", "1 blocks: keep-out of other tracks (T-0003, short 120 m)" */
  text: string
  /** what would clear the first blocker, when anything would */
  clear: string
  failing: Check[]
}

export function checksLine(checks: Check[] | null, o: EngageOption | null, ctx: ClearContext, needsConfirm = false, where = 'on the picture now'): ChecksLine {
  if (!checks) return { tone: 'none', text: 'They run once a platform is chosen (step 2)', clear: '', failing: [] }
  const failing = checks.filter(blocks)
  if (!failing.length) return { tone: 'ok', text: `All ${checks.length} pass ${where}`, clear: '', failing }
  const f = failing[0]
  const head = failing.length === 1 ? `1 blocks: ${checkLabel(f.rule)} (${whyText(f, o)})` : `${failing.length} block: ${[...new Set(failing.map((c) => checkLabel(c.rule)))].join(', ')}`
  return { tone: 'bad', text: head, clear: clearText(f, o, ctx, needsConfirm), failing }
}

// ---- Nominate -----------------------------------------------------------------------------------

export interface Acceptances {
  lost: Record<string, KeepOutLost>
  behaving: Record<string, BehavingInfo>
}

/** Whether the operator's acceptances cover every blocker of a keep-out: each blocking item a lost
 *  contact they accept, or a live track they accept as behaving now whose acceptance would clear it
 *  (the explanation's own `clears`). The dry run itself does not count acceptances; the engine
 *  checks with them at the nomination. */
export function acceptancesCover(ex: KeepOutExplanation | null | undefined, acc: Acceptances): boolean {
  if (!ex?.blocked || ex.omitted > 0) return false
  const blocking = ex.items.filter((i) => i.blocks)
  return (
    blocking.length > 0 &&
    blocking.every((i) => (i.lost && acc.lost[i.lost.track] != null) || (i.behaving != null && acc.behaving[i.behaving.track] != null && i.behaving.clears))
  )
}

export interface Ready {
  ok: boolean
  /** what keeps Nominate disabled, by step: "1 Target: confirm the declaration" */
  missing: string[]
  /** every blocker is a keep-out the operator's acceptances cover */
  onAcceptances: boolean
}

export function nominateReady(a: { target: TargetState; option: EngageOption | null; acc: Acceptances; stepUpOk: boolean }): Ready {
  const missing: string[] = []
  const t = a.target
  if (t.kind === 'none') missing.push('1 Target: choose one')
  else if (t.kind === 'checking') missing.push('1 Target: checking')
  else if (t.kind === 'confirm') missing.push('1 Target: confirm the declaration')
  else if (t.kind === 'blocked') missing.push(`1 Target: ${t.checks.length ? t.checks.map((c) => checkLabel(c.rule)).join(', ') : 'not declared hostile'}`)
  let onAcceptances = false
  if (!a.option) missing.push('2 Platform: choose one')
  else {
    const failing = a.option.checks.filter((c) => blocks(c) && stepOf(c.rule) !== 'target')
    const keepOutsOnly = failing.length > 0 && failing.every((c) => KEEP_OUT_RULES.includes(c.rule))
    if (keepOutsOnly && acceptancesCover(a.option.keep_outs, a.acc)) onAcceptances = true
    else {
      const bySteps = new Map<Step, string[]>()
      for (const c of failing) bySteps.set(stepOf(c.rule), [...(bySteps.get(stepOf(c.rule)) ?? []), checkLabel(c.rule)])
      for (const s of ['platform', 'effect', 'checks'] as Step[]) {
        const ls = bySteps.get(s)
        if (ls) missing.push(`${STEP_NUMBER[s]} ${STEP_NAME[s]}: ${[...new Set(ls)].join(', ')}`)
      }
    }
  }
  if (!a.stepUpOk) missing.push('Your password or passkey, for the acceptance')
  return { ok: missing.length === 0, missing, onAcceptances }
}
