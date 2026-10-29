// Checks for tutorial.ts (no test runner in web/): node web/src/tutorial.check.ts, as make check
// does. The claim: the walk-through advances on what the bridge shows, one step at a time through
// the engagement chain (a declaration, its confirmation, the person's nomination, both signatures,
// the impact, the assessment), and says so when the chain goes sideways (an act flagged instead of
// declared, the nomination form blocked on the target's identity, a nomination that ended without
// an effect) instead of moving on. Throws on the first failure, prints "tutorial: ok" otherwise.
import type { Intent } from './decisions'
import type { ObservedAct, Review } from './ident'
import type { Track } from './tracks'
import { CONSOLE_OPERATOR, findBoat, HOLD, identityCheck, resumeAt, STEPS, stepDone, type StepId, tutorialFacts } from './tutorial.ts'

function eq(a: unknown, b: unknown, what: string) {
  const x = JSON.stringify(a)
  const y = JSON.stringify(b)
  if (x !== y) throw new Error(`${what}: got ${x}, want ${y}`)
}

function track(id: string, position: [number, number], extra: Partial<Track> = {}): Track {
  return {
    id,
    domain: 'surface',
    position,
    identity: { kind: 'small_fast_boat', confidence: 0.8, ais: null, conflict: null, supported_by: ['eo_ir'] },
    declared: null,
    ...extra,
  } as Track
}

function engage(id: number, state: Intent['state'], extra: Partial<Intent> = {}, by = CONSOLE_OPERATOR): Intent {
  return {
    id,
    kind: 'engage',
    source: { kind: 'human', id: by },
    state,
    history: [{ state, t: 1000, reason: state === 'rejected' ? 'keep_out_tracks failed at the signature' : '' }],
    engage: { signed: [], approvers_needed: 2, live: { t: 0, passed: true, checks: [], preview: null, changed: [], track_drift_m: null } },
    ...extra,
  } as unknown as Intent
}

const at = (f: ReturnType<typeof tutorialFacts>) => STEPS.find((s) => s.kind === 'wait' && !stepDone(s.id, f))?.id ?? 'done'
const facts = (tracks: Track[], intents: Intent[] = [], opts: { review?: Review; acts?: ObservedAct[]; now?: number } = {}) =>
  tutorialFacts(tracks, opts.review, opts.acts, intents, opts.now ?? 0)

// the boat: the contact without AIS nearest the holding point, never one with AIS or far away
const ferry = track('T-0001', [-122.47, 37.84], { identity: { kind: 'ferry', confidence: 0.9, ais: { mmsi: '1', name: 'MV X', ship_type: 'passenger' }, conflict: null, supported_by: ['ais'] } })
const far = track('T-0002', [-122.53, 37.785])
const boat = track('T-0003', [HOLD[0] + 0.001, HOLD[1]])
eq(findBoat([ferry, far, boat])?.id, 'T-0003', 'the boat is the no-AIS contact at the holding point')
eq(findBoat([ferry, far]), null, 'nothing near the holding point: no boat yet')

// nothing has happened: the walk waits for the act
let f = facts([ferry, boat])
eq(at(f), 'act' satisfies StepId, 'before any act')
eq(resumeAt(f), 0, 'a fresh console starts at the welcome')

// an act flagged for review: still waiting for the act, saying why
const flagged: ObservedAct = {
  act: { act: 'act-1', against: { kind: 'vehicle', uid: 1 }, observed_t: 0, sensors: ['1:eo_ir'], location: HOLD, sigma_m: 400 },
  candidates: [],
  verdict: { verdict: 'flag', kind: 'low_attribution', tracks: ['T-0003'], why: 'attribution 0.62 below 0.90' },
  declaration: null,
  flag: 'F-1',
} as unknown as ObservedAct
f = facts([ferry, boat], [], { acts: [flagged] })
eq([at(f), f.acts, f.lastActNot], ['act', 1, 'flagged for review: attribution 0.62 below 0.90'], 'a flagged act declares nothing')

// declared by rule, not yet confirmed: the walk asks for the confirmation
const ruled = { ...boat, declared: { affiliation: 'hostile', by: 'rule:hostile_act', t: 1, note: null, rule: true, evidence: { act: 'act-2' } } } as Track
f = facts([ferry, ruled])
eq([at(f), f.byRule, f.confirmed, f.declarationAct], ['confirm', true, false, 'act-2'], 'a rule declaration waits for a confirmation')
const confirmed = { ...ruled, declared: { ...ruled.declared!, confirmed_by: 'op-1' } } as Track
f = facts([ferry, confirmed])
eq([at(f), f.confirmedBy], ['nominate', 'op-1'], 'confirmed: on to the nomination')
eq(resumeAt(f), STEPS.findIndex((s) => s.id === 'nominate'), 'a reload after the confirmation resumes at the nomination')

// the nomination form blocks on the target's identity: the card reads the form's own check, says
// how sure our sensors are against the policy's threshold, and the walk stays on the nomination
const idCheck = (outcome: 'pass' | 'fail', detail: string) => ({ rule: 'target_identity', uid: null, outcome, detail })
const recognised = { ...confirmed, identity: { ...confirmed.identity, kind: 'small_craft', level: 'recognise', confidence: 0.58 } } as Track
const blockedForm = { track: recognised.id, checks: [idCheck('fail', `${recognised.id} identity small_craft at 0.58 is below 0.60`)] }
f = tutorialFacts([recognised], undefined, undefined, [], 0, [], blockedForm)
eq([at(f), f.identity], ['nominate', { passed: false, kind: 'small_craft', confidence: 0.58, needed: 0.6, level: 'recognise' }], 'the form blocks on identity: still nominating, and why')
const clearedForm = { track: recognised.id, checks: [idCheck('pass', `${recognised.id} identity small_fast_boat at 0.93 (>= 0.60), existence 0.99 (>= 0.50), 3 source(s)`)] }
f = tutorialFacts([recognised], undefined, undefined, [], 0, [], clearedForm)
eq([at(f), f.identity?.passed, f.identity?.kind, f.identity?.confidence], ['nominate', true, 'small_fast_boat', 0.93], 'the identity clears: nominate')
f = tutorialFacts([recognised, ferry], undefined, undefined, [], 0, [], { track: ferry.id, checks: blockedForm.checks })
eq(f.identity, null, 'a form open on another track says nothing about the boat')
eq(identityCheck([idCheck('fail', 'T-0003 existence confidence 0.41 is below 0.50')]), null, 'an existence shortfall is not an identity block')
eq(tutorialFacts([recognised], undefined, undefined, [], 0).identity, null, 'no form open: nothing to say')

// only the person's own nomination counts; one that ended without an effect sends them back
f = facts([confirmed], [engage(7, 'pending_approval', {}, 'agent-scripted')])
eq(at(f), 'nominate', "a script's nomination is not the person's")
f = facts([confirmed], [engage(8, 'rejected')])
eq([at(f), f.nominationEnded?.state], ['nominate', 'rejected'], 'a rejected nomination: nominate again')
f = facts([confirmed], [engage(8, 'rejected'), engage(9, 'pending_approval')])
eq([at(f), f.nomination?.id, f.livePassed], ['sign', 9, true], 'the latest nomination waits for its signatures')

// the inbox's view of a waiting nomination is the one its card shows (the first signature)
const listed = engage(9, 'pending_approval')
const inInbox = engage(9, 'pending_approval', { engage: { signed: ['op-2'], approvers_needed: 2 } } as unknown as Partial<Intent>)
f = tutorialFacts([confirmed], undefined, undefined, [listed], 0, [inInbox])
eq([at(f), f.signed], ['sign', ['op-2']], "the inbox's signatures")

// the signatures, while the live picture fails: still waiting, and what fails
const blocked = engage(9, 'pending_approval', {
  engage: { signed: ['op-2'], approvers_needed: 2, live: { t: 0, passed: false, checks: [{ rule: 'keep_out_tracks', uid: null, outcome: 'fail', detail: '' }], preview: null, changed: [], track_drift_m: null } },
} as unknown as Partial<Intent>)
f = facts([confirmed], [blocked])
eq([at(f), f.signed, f.livePassed, f.failing], ['sign', ['op-2'], false, ['keep_out_tracks']], 'one signature, the picture failing')

// signed and dispatched, then released with a munition in flight, then the impact, then the assessment
const flying = engage(9, 'effected', {
  engage: { signed: ['op-2', 'op-3'], approvers_needed: 2 },
  engagement: { covered_s: 0, effect: { t: 5000, point: HOLD, radius_m: 75, planned_t: 6000 }, munition: { impact_t: 23_000 } },
} as unknown as Partial<Intent>)
f = facts([confirmed], [engage(9, 'dispatched', { engage: { signed: ['op-2', 'op-3'], approvers_needed: 2 } } as unknown as Partial<Intent>)])
eq([at(f), f.dispatched, f.released], ['strike', true, false], 'dispatched: waiting for the release')
f = facts([confirmed], [flying], { now: 10_000 })
eq([at(f), f.released, f.impacted, f.impactT], ['strike', true, false, 23_000], 'in flight: the impact is still to come')
f = facts([confirmed], [flying], { now: 23_500 })
eq(at(f), 'assess', 'impact: waiting for the assessment')
const assessed = { ...flying, engagement: { ...flying.engagement!, assessment: { t: 40_000, result: 'ceased', evidence: 'no detection where expected', covered_s: 15 } } } as Intent
f = facts([], [assessed], { now: 41_000 })
eq([at(f), f.assessment?.result], ['done', 'ceased'], 'assessed: the walk is done (the boat has left the picture)')

// an abort after dispatch ends it without an effect: back to nominating
f = facts([confirmed], [engage(9, 'aborted')])
eq([at(f), f.nominationEnded?.state], ['nominate', 'aborted'], 'aborted: nominate again')

console.log('tutorial: ok')
