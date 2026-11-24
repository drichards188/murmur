// One alarm per attacker's run (bd murmur-iobd, the cutdown's new piece 7, requirement R10): the
// act reports (rules 9: `inbox.acts`), the weapons our sensors tracked (the weapon picture,
// weapon.ts) and the victims' own damage reports (vehicleException.ts `Damage`) are three feeds,
// and each had a banner of its own: LAUNCH, INCOMING, HUNTER 1 STRUCK, HOSTILE ACT. Here they are
// joined by attacker (the track the engine attributed the act to, or the contact track the launch
// was placed on) into one event per run of shots, a run ending when the attacker has not fired
// for RUN_GAP_MS: who fired on whom, how often and how many hit, what it took from them, and the
// response it asks for. Only what the console already receives is read, never the simulator's
// truth. Display only. Pure (no browser): attackEvents.check.ts runs it under node.
import { span } from './engageSteps.ts'
import { attributionOf } from './hostileAct.ts'
import type { ObservedAct } from './ident'
import type { WeaponTrack } from './weapon'

/** Shots this far apart (or closer) are one run of an attacker's. */
export const RUN_GAP_MS = 120_000
/** The banner holds a run this long after its last shot or hit. */
export const EVENT_BANNER_MS = 45_000

/** What one of ours lost to it, as its own damage report says. */
export interface DamageLike {
  mobility: boolean
  firepower: boolean
}

/** A track's declaration as the tracks feed carries it. */
export interface DeclaredLike {
  affiliation: string
  /** declared by the engine's rule */
  rule?: boolean
  /** the operator who confirmed a rule declaration */
  confirmed_by?: string
  id?: number
}

export interface AttackEvent {
  /** the attacker's key and the run's first shot: stable while the run goes on */
  key: string
  /** the attacker's track, or null where no sensor placed the shooter */
  attacker: string | null
  /** our vehicles fired on (uids), in the order first fired on */
  victims: string[]
  /** engine ms of the run's first shot and of its last shot or hit */
  first: number
  last: number
  shots: number
  hits: number
  /** "T-0008 fired on HUNTER 1: hit, propulsion lost." */
  line: string
  /** the response it asks for: "Review T-0008: not declared." */
  response: string
  /** where the response is decided: a review item's card (D-n, F-n), else the attacker's track */
  review: string | null
  track: string | null
  /** the run's newest act report (its drawing frames the scene), or null */
  act: string | null
  /** a weapon of the run, for framing when no act was reported */
  weapon: string | null
}

interface Shot {
  attacker: string | null
  /** the attacker is the act's likeliest track only: attributed below the rule's threshold */
  unsure: boolean
  victim: string | null
  victimLabel: string
  t: number
  act: string | null
  weapon: string | null
  review: string | null
}

/** "propulsion lost", "weapons out of action", both, or null. */
export function damageWords(d: DamageLike | null | undefined): string | null {
  if (!d) return null
  if (d.mobility && d.firepower) return 'propulsion and weapons lost'
  if (d.mobility) return 'propulsion lost'
  if (d.firepower) return 'weapons out of action'
  return null
}

function and(xs: readonly string[]): string {
  return xs.length <= 1 ? (xs[0] ?? '') : `${xs.slice(0, -1).join(', ')} and ${xs[xs.length - 1]}`
}

/** The response a run asks for, by its attacker's standing now. */
function responseOf(attacker: string | null, declared: DeclaredLike | null, flag: string | null, declaration: string | null, engaged: string | null = null): { text: string; review: string | null; track: string | null } {
  if (!attacker) return { text: 'Review the act: no sensor saw who fired.', review: flag, track: null }
  if (declared?.affiliation === 'hostile') {
    if (declared.rule && !declared.confirmed_by) {
      const id = declared.id != null ? `D-${declared.id}` : declaration
      return { text: `Review ${attacker}: declared hostile by rule, not confirmed.`, review: id, track: attacker }
    }
    // an engagement on it already in play: the decision is made, say so rather than ask again
    if (engaged) return { text: `${attacker} is declared hostile; ${engaged}.`, review: null, track: attacker }
    return { text: `${attacker} is declared hostile: decide whether to engage.`, review: null, track: attacker }
  }
  return { text: `Review ${attacker}: not declared.`, review: flag, track: attacker }
}

/** The runs at `now` (engine ms), the newest first: those whose last shot or hit is within
 *  EVENT_BANNER_MS and after `dismissedAt` (a Dismiss acknowledges what came so far; a new shot
 *  raises the run again). `nameOf`: one of ours by uid; `declaredOf`: a track's declaration now;
 *  `engagedOn`: the engagement in play on a track, in words ("engagement i2 under way"), or null. */
export function attackEvents(
  acts: readonly ObservedAct[],
  weapons: readonly WeaponTrack[],
  damage: ReadonlyMap<number, DamageLike>,
  declaredOf: (track: string) => DeclaredLike | null,
  nameOf: (uid: string) => string,
  now: number,
  dismissedAt: number | null,
  engagedOn: (track: string) => string | null = () => null,
): AttackEvent[] {
  // the shots: each act report, and each weapon our sensors tracked whose launch no act reported
  const shots: Shot[] = []
  const byAct = new Map<string, string | null>()
  for (const a of acts) {
    const r = a.act
    // below the rule's threshold the act names no shooter, but its likeliest track is still the
    // one to look at: said as probable, never as the shooter
    const sure = attributionOf(a).track
    const who = sure ?? (a.verdict.verdict === 'flag' ? (a.candidates[0]?.track ?? null) : null)
    byAct.set(r.act, who)
    const victim = r.against.kind === 'vehicle' ? r.against.uid : null
    const victimLabel = r.against.kind === 'vehicle' ? nameOf(r.against.uid) : r.against.kind === 'track' ? `protected track ${r.against.track}` : 'something we hold no track of'
    shots.push({ attacker: who, unsure: !sure && who != null, victim, victimLabel, t: r.observed_t, act: r.act, weapon: null, review: a.flag ?? a.declaration })
  }
  for (const w of weapons) {
    if (w.report || w.stage === 'plot') continue
    const t = w.launch?.t ?? w.first_cue_t ?? w.struck?.t
    if (t == null) continue
    const victim = w.struck?.uid ?? w.aimed_at
    shots.push({ attacker: w.shooter, unsure: false, victim, victimLabel: victim ? nameOf(victim) : 'one of ours', t, act: null, weapon: w.id, review: null })
  }
  // the hits: each weapon that struck one of ours, its attacker by its act report, else its shooter
  const hits = weapons.flatMap((w) => (w.struck ? [{ attacker: w.report ? (byAct.get(w.report) ?? w.shooter) : w.shooter, uid: w.struck.uid, t: w.struck.t, weapon: w.id }] : []))
  // the runs, by attacker, split where it paused for more than RUN_GAP_MS
  const byAttacker = new Map<string, Shot[]>()
  for (const s of [...shots].sort((a, b) => a.t - b.t)) {
    const k = s.attacker ?? '?'
    byAttacker.set(k, [...(byAttacker.get(k) ?? []), s])
  }
  const out: AttackEvent[] = []
  for (const [k, list] of byAttacker) {
    let run: Shot[] = []
    const close = () => {
      if (!run.length) return
      const first = run[0].t
      const lastShot = run[run.length - 1].t
      const attacker = k === '?' ? null : k
      const victims = [...new Set(run.map((s) => s.victim).filter((v): v is string => v != null))]
      const labels = [...new Set(run.map((s) => s.victimLabel))]
      const struck = hits.filter((h) => h.attacker === attacker && h.t >= first - RUN_GAP_MS && h.t <= lastShot + RUN_GAP_MS)
      const last = Math.max(lastShot, ...struck.map((h) => h.t))
      // what it took: the victims' own damage reports
      const took = and(victims.flatMap((u) => {
        const words = damageWords(damage.get(Number(u)))
        return words ? [victims.length > 1 ? `${nameOf(u)} ${words}` : words] : []
      }))
      // hits are counted only as reported: a miss is never known (the victim cannot tell rounds went
      // past), and a hit our sensors did not tie to this attacker shows as its damage instead, so
      // "0 hits" is never said
      const hitWords = struck.length > 1 ? `${struck.length} hits` : struck.length ? '1 hit' : ''
      const counted = run.length > 1 ? [`${run.length} shots in ${span(Math.max(1000, last - first))}`, hitWords].filter(Boolean).join(', ') : struck.length ? (struck.length > 1 ? `${struck.length} hits` : 'hit') : ''
      const consequence = [counted, took].filter(Boolean).join(', ')
      const unsure = attacker != null && run.every((s) => s.unsure)
      const who = attacker ? (unsure ? `Probably ${attacker}` : attacker) : 'An unseen shooter'
      const line = `${who} fired on ${and(labels)}${consequence ? `: ${consequence}` : ''}.`
      const newest = [...run].reverse()
      const flag = newest.find((s) => s.review?.startsWith('F-'))?.review ?? null
      const declaration = newest.find((s) => s.review?.startsWith('D-'))?.review ?? null
      const resp = unsure
        ? { text: `Review the act: not sure enough who fired to declare ${attacker} hostile.`, review: flag, track: attacker }
        : responseOf(attacker, attacker ? declaredOf(attacker) : null, flag, declaration, attacker ? engagedOn(attacker) : null)
      out.push({
        key: `${k}@${first}`,
        attacker,
        victims,
        first,
        last,
        shots: run.length,
        hits: struck.length,
        line,
        response: resp.text,
        review: resp.review,
        track: resp.track,
        act: newest.find((s) => s.act)?.act ?? null,
        weapon: newest.find((s) => s.weapon)?.weapon ?? struck[struck.length - 1]?.weapon ?? null,
      })
      run = []
    }
    for (const s of list) {
      if (run.length && s.t - run[run.length - 1].t > RUN_GAP_MS) close()
      run.push(s)
    }
    close()
  }
  return out
    .filter((e) => now - e.last > -5000 && now - e.last < EVENT_BANNER_MS && !(dismissedAt != null && e.last <= dismissedAt))
    .sort((a, b) => b.last - a.last)
}
