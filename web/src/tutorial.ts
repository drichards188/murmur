// The tutorial's walk-through (Tutorial.tsx, sim/situations/tutorial.toml): what the console reads
// of the bridge to know where the person is in the engagement chain, and when each step is done.
// Pure (no React, type-only imports), so `node src/tutorial.check.ts` runs it, as make check does.
// Every fact is one the console already shows: the tracks and their declarations, the review list,
// the observed acts, the intents with their signatures, release, munition and assessment. Display
// only: nothing here decides or sends anything.
import type { Check, Intent } from './decisions'
import type { ObservedAct, Review, ReviewFlag } from './ident'
import type { IdentityLevel, Track } from './tracks'

/** where the tutorial's boat holds, mid-strait 1.5 km west of the bridge ([lon, lat]) */
export const HOLD: [number, number] = [-122.495, 37.812]
/** the person at the console: the only proposer whose nomination the scripted pair signs */
export const CONSOLE_OPERATOR = 'op-1'
/** how near the holding point a surface contact without AIS counts as the boat, before any
 *  declaration names it */
const BOAT_RADIUS_M = 2500
const RUNNING = ['approved', 'dispatched', 'effected', 'done']
const ENDED = ['rejected', 'expired', 'voided', 'failed', 'aborted']

export type StepId = 'welcome' | 'map' | 'tracks' | 'act' | 'confirm' | 'nominate' | 'sign' | 'strike' | 'assess' | 'done'

/** Each step, in order: a `read` step moves on with Next; a `wait` step when the bridge shows it
 *  done (stepDone), but not before it has been shown `minShowS`, or with Next once it is done. */
export const STEPS: { id: StepId; kind: 'read' | 'wait'; minShowS?: number }[] = [
  { id: 'welcome', kind: 'read' },
  { id: 'map', kind: 'read' },
  { id: 'tracks', kind: 'read' },
  // shown a while even when it happened before the person got here: what it explains (the rule
  // declaration, the two-person rule) is the point of the step
  { id: 'act', kind: 'wait', minShowS: 12 },
  { id: 'confirm', kind: 'wait' },
  { id: 'nominate', kind: 'wait' },
  { id: 'sign', kind: 'wait', minShowS: 15 },
  { id: 'strike', kind: 'wait' },
  { id: 'assess', kind: 'wait' },
  { id: 'done', kind: 'read' },
]

export interface Facts {
  /** the boat: the track declared hostile, else the surface contact without AIS nearest the
   *  holding point (within BOAT_RADIUS_M); null until our sensors hold it */
  boat: Track | null
  /** the boat is declared hostile (by the engine's rule or an operator) */
  declared: boolean
  /** by the engine's hostile-act rule */
  byRule: boolean
  /** a declaration in force that needs no confirmation any more (an operator's, or a rule
   *  declaration an operator confirmed) */
  confirmed: boolean
  /** who confirmed it */
  confirmedBy: string | null
  /** the act the rule declaration rests on */
  declarationAct: string | null
  /** acts our sensors observed so far */
  acts: number
  /** the latest act's verdict when it was not a declaration: flagged (why) or only recorded */
  lastActNot: string | null
  /** an open review flag on the boat (an unsure attribution, never a declaration) */
  flag: ReviewFlag | null
  /** the person's latest engagement nomination */
  nomination: Intent | null
  /** its state when it ended without an effect (rejected, expired, voided, failed, aborted), and why */
  nominationEnded: { state: string; reason: string } | null
  signed: string[]
  needed: number
  /** whether the engine's live picture of the waiting nomination passes, and what fails */
  livePassed: boolean | null
  failing: string[]
  /** the nomination form's target-identity check on the boat, from its latest dry run (null with
   *  the form closed or on another track): how sure our sensors are of what the boat is against
   *  what the policy needs to nominate on it */
  identity: IdentityCheck | null
  /** both signatures given: approved and on its way */
  dispatched: boolean
  /** the engine released the effect (a munition's launch) */
  released: boolean
  /** when the effect lands (a munition's impact, else the release), bridge ms */
  impactT: number | null
  impacted: boolean
  assessment: { result: 'persists' | 'ceased' | 'unknown'; evidence: string } | null
}

export interface IdentityCheck {
  passed: boolean
  /** the label the check read (a kind, a family or a size class) */
  kind: string
  confidence: number
  /** the policy's threshold */
  needed: number
  /** how specific the label is, from the boat's track */
  level: IdentityLevel | null
}

/** The target-identity check's own words (crates/murmur-decide, engage.rs `identity_check`):
 *  "T-0006 identity small_craft at 0.58 is below 0.60", or on a pass "T-0006 identity
 *  small_craft at 0.66 (>= 0.60), existence ...". Null for anything else (an existence
 *  confidence below its threshold fails the same rule, but says nothing about the identity). */
export function identityCheck(checks: Check[], level: IdentityLevel | null = null): IdentityCheck | null {
  const c = checks.find((x) => x.rule === 'target_identity')
  if (!c) return null
  const fail = /identity (\S+) at ([\d.]+) is below ([\d.]+)/.exec(c.detail)
  const pass = /identity (\S+) at ([\d.]+) \(>= ([\d.]+)\)/.exec(c.detail)
  const m = fail ?? pass
  if (!m) return null
  return { passed: c.outcome === 'pass', kind: m[1], confidence: Number(m[2]), needed: Number(m[3]), level }
}

function distanceM(a: [number, number], b: [number, number]) {
  const toRad = Math.PI / 180
  const dLat = (b[1] - a[1]) * toRad
  const dLon = (b[0] - a[0]) * toRad
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a[1] * toRad) * Math.cos(b[1] * toRad) * Math.sin(dLon / 2) ** 2
  return 2 * 6_371_000 * Math.asin(Math.min(1, Math.sqrt(h)))
}

export function findBoat(tracks: Track[]): Track | null {
  const hostile = tracks.find((t) => t.declared?.affiliation === 'hostile')
  if (hostile) return hostile
  let best: Track | null = null
  let bestM = BOAT_RADIUS_M
  for (const t of tracks) {
    if (t.domain !== 'surface' || t.identity.ais) continue
    const m = distanceM(t.position, HOLD)
    if (m < bestM) {
      best = t
      bestM = m
    }
  }
  return best
}

const isEngage = (i: Intent) => i.kind === 'engage'

/** What the walk-through reads, from what the console holds now. `nowMs`: the bridge's clock.
 *  `pending`: the inbox's waiting items, whose engagement view (who signed, the live picture) is
 *  the one the Inbox's card shows; the intents list may carry an older one. */
export function tutorialFacts(
  tracks: Track[],
  review: Review | undefined,
  acts: ObservedAct[] | undefined,
  intents: Intent[],
  nowMs: number,
  pending: Intent[] = [],
  /** the nomination form's latest dry run on its target (null with the form closed) */
  form: { track: string; checks: Check[] } | null = null,
): Facts {
  const boat = findBoat(tracks)
  const d = boat?.declared ?? null
  const declared = d?.affiliation === 'hostile'
  const byRule = declared && !!d?.rule
  const confirmed = declared && (!d?.rule || !!d?.confirmed_by)
  const last = acts?.length ? acts[acts.length - 1] : null
  const lastActNot = last && last.verdict.verdict !== 'declare' ? (last.verdict.verdict === 'flag' ? `flagged for review: ${last.verdict.why}` : `recorded only: ${last.verdict.why}`) : null
  const flag = boat ? ((review?.flags ?? []).find((f) => f.tracks.includes(boat.id) && f.status.status === 'open') ?? null) : null
  const mine = intents.filter((i) => isEngage(i) && i.source.kind === 'human' && i.source.id === CONSOLE_OPERATOR).sort((a, b) => b.id - a.id)
  const nomination = mine[0] ? (pending.find((i) => i.id === mine[0].id && i.engage) ?? mine[0]) : null
  const ended = nomination && ENDED.includes(nomination.state)
  const lastStep = nomination?.history[nomination.history.length - 1]
  const view = nomination?.engage
  const signed = view?.signed ?? []
  const needed = view?.approvers_needed ?? 2
  const live = view?.live
  const e = nomination?.engagement
  const impactT = e?.munition ? e.munition.impact_t : (e?.effect?.t ?? null)
  return {
    boat,
    declared,
    byRule,
    confirmed,
    confirmedBy: d?.confirmed_by ?? (declared && !d?.rule ? (d?.by ?? null) : null),
    declarationAct: d?.evidence?.act ?? null,
    acts: acts?.length ?? 0,
    lastActNot,
    flag,
    nomination,
    nominationEnded: ended && nomination ? { state: nomination.state, reason: lastStep?.reason ?? '' } : null,
    signed,
    needed,
    livePassed: nomination?.state === 'pending_approval' && live ? live.passed : null,
    failing: (live?.checks ?? []).filter((c) => c.outcome !== 'pass').map((c) => c.rule),
    identity: boat && form?.track === boat.id ? identityCheck(form.checks, boat.identity.level ?? null) : null,
    dispatched: !!nomination && !ended && (RUNNING.includes(nomination.state) || signed.length >= needed),
    released: !!e?.effect,
    impactT,
    impacted: impactT != null && nowMs >= impactT,
    assessment: e?.assessment ? { result: e.assessment.result, evidence: e.assessment.evidence } : null,
  }
}

/** Whether the bridge shows a step done. A nomination standing means the declaration and its
 *  confirmation were done (the engine nominates on nothing less), even once the struck boat has
 *  left the picture and its declaration with it. */
export function stepDone(id: StepId, f: Facts): boolean {
  const nominated = f.nomination != null && f.nominationEnded == null
  switch (id) {
    case 'act':
      return f.declared || nominated
    case 'confirm':
      return f.confirmed || nominated
    case 'nominate':
      return nominated
    case 'sign':
      return f.dispatched
    case 'strike':
      return f.impacted || f.assessment != null
    case 'assess':
      return f.assessment != null
    default:
      return true
  }
}

/** Where a console opened mid-run (a reload with no stored step) picks the walk up: at the first
 *  step not done once the person has acted (a confirmation or a nomination), else at the start. */
export function resumeAt(f: Facts): number {
  if (!f.confirmed && !f.nomination) return 0
  const k = STEPS.findIndex((s) => s.kind === 'wait' && !stepDone(s.id, f))
  return k < 0 ? STEPS.length - 1 : k
}
