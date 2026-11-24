// Checks for attackEvents.ts: node web/src/attackEvents.check.ts, as make check does. The claims (bd
// murmur-iobd, new piece 7, R10): an attacker's shots, its weapons' strikes on ours and the
// victims' damage reports are one event per run; a pause of more than RUN_GAP_MS starts another;
// the line says who fired on whom, how often, how many hit and what it took; the response follows
// the attacker's standing (not declared, declared by rule and not confirmed, declared hostile, or
// no shooter placed); a Dismiss holds until a new shot. Prints "attackEvents: ok".
import { attackEvents, damageWords, EVENT_BANNER_MS, RUN_GAP_MS, type DeclaredLike } from './attackEvents.ts'
import type { ObservedAct } from './ident'
import type { WeaponTrack } from './weapon'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`attackEvents: ${what}`)
}

const names: Record<string, string> = { '1': 'HUNTER 1', '2': 'HUNTER 2' }
const nameOf = (uid: string) => names[uid] ?? `vehicle ${uid}`
const act = (n: number, t: number, over: Partial<ObservedAct> = {}, uid = '1'): ObservedAct => ({
  act: { act: `A-${n}`, against: { kind: 'vehicle', uid }, observed_t: t, sensors: ['1:eo_ir'], location: [-122.495, 37.812], sigma_m: 40 },
  candidates: [{ track: 'T-0008', distance_m: 30, sigma_m: 40, confidence: 0.95 }],
  verdict: { verdict: 'recorded', why: 'rule off' },
  declaration: null,
  flag: null,
  ...over,
})
const weapon = (id: string, report: string | null, struck: { uid: string; t: number } | null, over: Partial<WeaponTrack> = {}): WeaponTrack =>
  ({ id, report, stage: 'class', shooter: 'T-0008', aimed_at: '1', struck, launch: null, first_cue_t: struck ? struck.t - 3000 : null, fixes: [], status: struck ? 'expended' : 'in_flight', ...over }) as unknown as WeaponTrack
const none = () => null
const T = 1_000_000

// one shot, one hit, the damage it took: one event
{
  const e = attackEvents([act(1, T, { flag: 'F-1' })], [weapon('W-1', 'A-1', { uid: '1', t: T + 3000 })], new Map([[1, { mobility: true, firepower: false }]]), none, nameOf, T + 5000, null)
  check(e.length === 1, `one event (${e.length})`)
  check(e[0].line === 'T-0008 fired on HUNTER 1: hit, propulsion lost.', e[0].line)
  check(e[0].response === 'Review T-0008: not declared.' && e[0].review === 'F-1' && e[0].track === 'T-0008', `the response: review its flag (${e[0].response})`)
  check(e[0].act === 'A-1' && e[0].hits === 1 && e[0].shots === 1, 'the act it came from')
}

// a run of shots: counted with its hits, not one banner each
{
  const acts = [act(1, T), act(2, T + 30_000), act(3, T + 60_000), act(4, T + 100_000)]
  const ws = [weapon('W-1', 'A-1', { uid: '1', t: T + 3000 }), weapon('W-2', 'A-2', null, { status: 'expended' }), weapon('W-3', 'A-3', { uid: '1', t: T + 63_000 })]
  const e = attackEvents(acts, ws, new Map(), none, nameOf, T + 110_000, null)
  check(e.length === 1 && e[0].shots === 4 && e[0].hits === 2, `four shots, two hits, one event (${JSON.stringify(e.map((x) => [x.shots, x.hits]))})`)
  check(e[0].line === 'T-0008 fired on HUNTER 1: 4 shots in 1 min 40 s, 2 hits.', e[0].line)
  // no hit tied to it: its shots counted, never "0 hits" (a miss is not known), the damage said
  const unhit = attackEvents([act(1, T), act(2, T + 60_000)], [], new Map([[1, { mobility: true, firepower: false }]]), none, nameOf, T + 61_000, null)
  check(unhit[0].line === 'T-0008 fired on HUNTER 1: 2 shots in 1 min 00 s, propulsion lost.', unhit[0].line)
}

// a pause past RUN_GAP_MS: a new run (the old one has gone from the banner by then)
{
  const acts = [act(1, T), act(2, T + RUN_GAP_MS + 1000)]
  const e = attackEvents(acts, [], new Map(), none, nameOf, T + RUN_GAP_MS + 2000, null)
  check(e.length === 1 && e[0].first === T + RUN_GAP_MS + 1000 && e[0].shots === 1, 'the second run alone is in the banner')
}

// the response by the attacker's standing
{
  const rule: DeclaredLike = { affiliation: 'hostile', rule: true, id: 3 }
  const e = attackEvents([act(1, T, { declaration: 'D-3', verdict: { verdict: 'declare', track: 'T-0008', attribution: 0.97 } })], [], new Map(), () => rule, nameOf, T + 1000, null)
  check(e[0].response === 'Review T-0008: declared hostile by rule, not confirmed.' && e[0].review === 'D-3', `a rule declaration to confirm (${e[0].response})`)
  const confirmed = attackEvents([act(1, T)], [], new Map(), () => ({ ...rule, confirmed_by: 'op-1' }), nameOf, T + 1000, null)
  check(confirmed[0].response === 'T-0008 is declared hostile: decide whether to engage.' && confirmed[0].review === null && confirmed[0].track === 'T-0008', confirmed[0].response)
  // an engagement on it already in play: said, not asked again
  const engaged = attackEvents([act(1, T)], [], new Map(), () => ({ ...rule, confirmed_by: 'op-1' }), nameOf, T + 1000, null, (t) => (t === 'T-0008' ? 'engagement i2 dispatched to REAPER 1' : null))
  check(engaged[0].response === 'T-0008 is declared hostile; engagement i2 dispatched to REAPER 1.', engaged[0].response)
  const unplaced = attackEvents([act(1, T, { candidates: [], flag: 'F-4', verdict: { verdict: 'flag', kind: 'untracked', tracks: [], why: 'no track near' } })], [], new Map(), none, nameOf, T + 1000, null)
  check(unplaced[0].line === 'An unseen shooter fired on HUNTER 1.' && unplaced[0].review === 'F-4' && unplaced[0].track === null, `no shooter placed (${unplaced[0].line})`)
  // attributed below the threshold: its likeliest track named as probable, the flag to review
  // (hostile-act-engagement's A-1, T-0008 at 0.88 against 0.90)
  const unsure = attackEvents([act(1, T, { flag: 'F-1', verdict: { verdict: 'flag', kind: 'attribution', tracks: ['T-0008'], why: 'below the threshold' } })], [], new Map(), none, nameOf, T + 1000, null)
  check(unsure[0].line === 'Probably T-0008 fired on HUNTER 1.' && unsure[0].review === 'F-1' && unsure[0].track === 'T-0008' && unsure[0].response === 'Review the act: not sure enough who fired to declare T-0008 hostile.', `unsure (${unsure[0].line} / ${unsure[0].response})`)
}

// a launch no sensor saw: its weapon's strike is still the attacker's event
{
  const blind = weapon('W-9', null, { uid: '2', t: T + 2000 }, { shooter: null, aimed_at: null })
  const e = attackEvents([], [blind], new Map([[2, { mobility: false, firepower: true }]]), none, nameOf, T + 4000, null)
  check(e.length === 1 && e[0].line === 'An unseen shooter fired on HUNTER 2: hit, weapons out of action.' && e[0].weapon === 'W-9', e[0]?.line)
}

// a lone fix is not a weapon, so not a shot
{
  const plot = weapon('W-3', null, null, { stage: 'plot', shooter: null, first_cue_t: T })
  check(attackEvents([], [plot], new Map(), none, nameOf, T + 1000, null).length === 0, 'a lone fix raises nothing')
}

// timing: the banner holds EVENT_BANNER_MS from the last shot or hit; a Dismiss until a new shot
{
  const acts = [act(1, T)]
  check(attackEvents(acts, [], new Map(), none, nameOf, T + EVENT_BANNER_MS - 1, null).length === 1, 'still up')
  check(attackEvents(acts, [], new Map(), none, nameOf, T + EVENT_BANNER_MS, null).length === 0, 'gone after EVENT_BANNER_MS')
  check(attackEvents(acts, [], new Map(), none, nameOf, T + 1000, T + 500).length === 0, 'dismissed')
  check(attackEvents([...acts, act(2, T + 2000)], [], new Map(), none, nameOf, T + 3000, T + 500).length === 1, 'a new shot raises it again')
}

// what it took, in words
check(damageWords({ mobility: true, firepower: true }) === 'propulsion and weapons lost' && damageWords(null) === null && damageWords({ mobility: false, firepower: false }) === null, 'damage words')

console.log('attackEvents: ok')
