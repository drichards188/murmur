// Checks for engageSteps.ts (no test runner in web/): node web/src/engageSteps.check.ts, as make
// check does. The claim: in the nomination form nothing fails at the end that a step could have
// shown, so every check a dry run fails is owned by a step, said there with what would clear it,
// and keeps Nominate disabled until cleared. Throws on the first failure, prints
// "engageSteps: ok" otherwise. The checks' words are the engine's, from a live dry run
// (situation hostile-act-engagement, 2026-10-27).
import type { Check } from './decisions'
import type { EngageOption, KeepOutExplanation } from './engage'
import {
  checksLine,
  declarationLine,
  evidenceBullets,
  fallbackNote,
  fallbacksFor,
  nominateReady,
  platformLine,
  radiusFor,
  sensorsText,
  stepOf,
  targetState,
  type DeclFacts,
} from './engageSteps.ts'

function eq(a: unknown, b: unknown, what: string) {
  const x = JSON.stringify(a)
  const y = JSON.stringify(b)
  if (x !== y) throw new Error(`${what}: got ${x}, want ${y}`)
}

const pass = (rule: string, detail = 'ok'): Check => ({ rule, uid: null, outcome: 'pass', detail })
const fail = (rule: string, detail: string): Check => ({ rule, uid: null, outcome: 'fail', detail })

const UNCONFIRMED =
  "T-0001 declared hostile by rule hostile_act (D-1) on act A-1 against our vehicle 1, observed by 1:sonar, 1:eo_ir at t=1793123518710, attribution 0.98 >= 0.90; not confirmed by an operator: the policy needs an operator's confirmation before a nomination"

/** The live dry run's checks, every one passing but those given. */
function checks(over: Record<string, Check> = {}): Check[] {
  const rules = [
    'capability', 'readiness', 'platform_limits', 'domain', 'geofence', 'reserves',
    'target_declared', 'target_identity', 'target_recency', 'target_uncertainty',
    'engagement_zone', 'no_strike', 'keep_out_friendly', 'keep_out_tracks',
    'platform_armed', 'platform_free', 'fallback_preload', 'reach',
  ]
  return rules.map((r) => over[r] ?? pass(r))
}

const NOW = 1_000_000
const preview = { effect_t: NOW + 64_400, distance_m: 3992, cruise_mps: 90 } as EngageOption['preview']
const option = (cs: Check[], keep_outs: KeepOutExplanation | null = null): EngageOption => ({
  uid: 2,
  callsign: 'REAPER 1',
  passed: cs.every((c) => c.outcome === 'pass'),
  checks: cs,
  preview,
  keep_outs,
})
const ruleDecl: DeclFacts = { affiliation: 'hostile', by: 'hostile_act', rule: true, act: 'A-1', confirmedBy: null }
const noAcc = { lost: {}, behaving: {} }

// every check has a step; the target's own are step 1
eq(stepOf('target_declared'), 'target', 'declared is the target step')
eq(stepOf('target_uncertainty'), 'effect', 'accuracy is cleared by the radius')
eq(stepOf('keep_out_tracks'), 'checks', 'keep-outs are the checks step')
eq(stepOf('reach'), 'platform', 'reach is the platform')
eq(stepOf('some_new_rule'), 'platform', 'an unknown rule falls to the platform step, never nowhere')

// what the owner hit: a rule declaration nobody confirmed. Step 1 asks for the confirmation, the
// platform's line does not repeat it, and Nominate says why it is disabled
{
  const opts = [option(checks({ target_declared: fail('target_declared', UNCONFIRMED) }))]
  const t = targetState(ruleDecl, opts, true)
  eq(t.kind, 'confirm', 'unconfirmed rule declaration: confirm at step 1')
  eq(platformLine(opts[0], NOW), { ok: true, text: 'Can do it: effect in 1 min 04 s, 3.99 km at 90 m/s' }, 'the platform line leaves the target out')
  const r = nominateReady({ target: t, option: opts[0], acc: noAcc, stepUpOk: true })
  eq(r, { ok: false, missing: ['1 Target: confirm the declaration'], onAcceptances: false }, 'nominate waits for the confirmation')
  const l = checksLine(opts[0].checks, opts[0], { now: NOW, platform: 'REAPER 1' }, true)
  eq(l.text, '1 blocks: hostile declaration (declared by rule, not confirmed by an operator)', 'the checks line')
  eq(l.clear, 'confirm the declaration (step 1)', 'and what clears it')
}

// confirmed: everything passes, Nominate needs only a platform
{
  const opts = [option(checks())]
  const t = targetState({ ...ruleDecl, confirmedBy: 'op-1' }, opts, true)
  eq(t, { kind: 'ready', line: "Declared hostile by the engine's rule (act A-1), confirmed by op-1" }, 'ready')
  eq(nominateReady({ target: t, option: null, acc: noAcc, stepUpOk: true }).missing, ['2 Platform: choose one'], 'no platform chosen')
  eq(nominateReady({ target: t, option: opts[0], acc: noAcc, stepUpOk: true }).ok, true, 'all clear')
  eq(checksLine(opts[0].checks, opts[0], { now: NOW }).text, 'All 18 pass on the picture now', 'all pass')
}

// nothing chosen, nothing checked
eq(targetState(null, null, false).kind, 'none', 'no target')
eq(nominateReady({ target: { kind: 'none' }, option: null, acc: noAcc, stepUpOk: true }).missing, ['1 Target: choose one', '2 Platform: choose one'], 'both missing')

// a platform blocked by its own check, said on its line and named by its step
{
  const o = option(checks({ platform_free: fail('platform_free', 'platform has engagement i2 in play: one engagement per platform') }))
  eq(platformLine(o, NOW), { ok: false, text: 'Blocked: platform free of other engagements, platform has engagement i2 in play: one engagement per platform' }, 'platform_free line')
  eq(nominateReady({ target: { kind: 'ready', line: '' }, option: o, acc: noAcc, stepUpOk: true }).missing, ['2 Platform: platform free of other engagements'], 'named under step 2')
}

// the radius the accuracy check asks for, offered at step 3
eq(radiusFor({ rule: 'target_uncertainty', detail: 'T-0001 1-sigma 41.8 m (limit 37.5 m = 0.50 x effect radius 75 m)' }), 85, 'radius fix')
eq(radiusFor({ rule: 'target_uncertainty', detail: 'something else' }), null, 'no fix from other words')
{
  const o = option(checks({ target_uncertainty: fail('target_uncertainty', 'T-0001 1-sigma 41.8 m (limit 37.5 m = 0.50 x effect radius 75 m)') }))
  eq(nominateReady({ target: { kind: 'ready', line: '' }, option: o, acc: noAcc, stepUpOk: true }).missing, ['3 Effect: target position accuracy'], 'accuracy is step 3')
}

// a keep-out: what would clear it, and an acceptance that covers it enables Nominate
{
  const ex = {
    blocked: true,
    blocking: 1,
    omitted: 0,
    items: [
      {
        rule: 'keep_out_tracks', subject: 'track', id: 'T-0003', label: 'T-0003 sailboat', blocks: true, short_m: 120,
        behaving: { track: 'T-0003', acceptable: true, clears: true },
      },
    ],
    move_within: { within_m: 693, holding: true },
    wait: { nominate_t: NOW + 40_000 },
  } as unknown as KeepOutExplanation
  const o = option(checks({ keep_out_tracks: fail('keep_out_tracks', 'T-0003 inside the keep-out at the effect time') }), ex)
  const l = checksLine(o.checks, o, { now: NOW, platform: 'REAPER 1' })
  eq(l.text, '1 blocks: keep-out of other tracks (T-0003 sailboat, short 120 m)', 'keep-out line')
  eq(l.clear, 'REAPER 1 within 693 m of the target; or wait about 40 s (if everything keeps behaving as now); or accept T-0003 as behaving now (below)', 'keep-out clear')
  const t = { kind: 'ready', line: '' } as const
  eq(nominateReady({ target: t, option: o, acc: noAcc, stepUpOk: true }).missing, ['4 Checks: keep-out of other tracks'], 'blocked without the acceptance')
  const acc = { lost: {}, behaving: { 'T-0003': {} as never } }
  eq(nominateReady({ target: t, option: o, acc, stepUpOk: true }), { ok: true, missing: [], onAcceptances: true }, 'the acceptance covers it')
  eq(nominateReady({ target: t, option: o, acc, stepUpOk: false }).missing, ['Your password or passkey, for the acceptance'], 'the step-up still asked')
}

// the evidence and the declaration in plain words (vehicle names, not uids; no epoch times)
const nameOf = (uid: string) => ({ '1': 'HUNTER 1', '2': 'REAPER 1' })[uid] ?? `vehicle ${uid}`
eq(sensorsText(['1:sonar', '1:eo_ir', '2:radar'], nameOf), 'HUNTER 1 sonar and EO/IR; REAPER 1 radar', 'sensors by vehicle')
eq(
  evidenceBullets({ act: 'A-1', against: { kind: 'vehicle', uid: '1' }, observed_t: NOW - 130_000, sensors: ['1:sonar', '1:eo_ir'], attribution: 0.9833, threshold: 0.9 }, nameOf, NOW),
  ['Act A-1 against HUNTER 1, 2 min 10 s ago', 'Seen by HUNTER 1 sonar and EO/IR', 'Attributed to this track at 0.98 (the rule needs 0.90)'],
  'evidence',
)
eq(declarationLine({ affiliation: 'hostile', by: 'op-1', rule: false }), 'Declared hostile by op-1', 'operator declaration')
eq(declarationLine(ruleDecl), "Declared hostile by the engine's rule (act A-1), not confirmed by an operator", 'rule declaration')

console.log('engageSteps: ok')

// fallbacks: a plane can only return (ArduPlane has no lost-link hold), a sub only hold, a boat all three
eq(fallbacksFor('plane'), ['return'], 'a plane can only return')
eq(fallbacksFor('sub'), ['abort', 'loiter'], 'a sub cannot return')
eq(fallbacksFor('unknown'), null, 'an unknown kind is left to the engine')
eq(fallbackNote('REAPER 1', 'plane'), 'REAPER 1 can only return home if its link is lost: its autopilot has no other lost-link action', 'the plane note')
eq(fallbackNote('HUNTER 1', 'boat'), null, 'no note when every fallback is offered')
