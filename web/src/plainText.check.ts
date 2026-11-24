// Checks for plainText.ts (no test runner in web/): node web/src/plainText.check.ts, as make check
// does. Throws on the first failure, prints "plainText: ok" otherwise. The engine's and the
// bridge's words are taken from a live run (hostile-act-engagement, autoplay, 2026-11-24). Not
// part of the bundle.
import assert from 'node:assert/strict'
import { againstPlan, agoWhen, clockText, epochMs, plainText, relWords, sensorList, situationWhen, waitingWords } from './plainText.ts'

const names: Record<string, string> = { '1': 'HUNTER 1', '2': 'REAPER 1' }
const nameOf = (uid: string) => names[uid] ?? `vehicle ${uid}`
const now = 1795533619054 + 121_000

// ---- sensors by vehicle ----------------------------------------------------------------------------
assert.equal(sensorList(['1:eo_ir'], nameOf), 'HUNTER 1 EO/IR')
assert.equal(sensorList(['1:radar', '1:eo_ir', '1:sonar', '2:radar', '2:eo_ir'], nameOf), 'HUNTER 1 (radar, EO/IR and sonar) and REAPER 1 (radar and EO/IR)')
assert.equal(sensorList(['1:radar', '1:radar'], nameOf), 'HUNTER 1 radar', 'a sensor named twice is said once')
assert.ok(!sensorList(['1:radar', '2:eo_ir', '2:sonar'], nameOf).includes(';'), 'no semicolon: bluf.ts would split the sentence there')

// ---- epoch stamps --------------------------------------------------------------------------------
assert.equal(epochMs(1795533627.2), 1795533627200, 'the bridge writes seconds')
assert.equal(epochMs(1795533619054), 1795533619054, 'the engine writes ms')
assert.equal(relWords(now - 121_000, now), '2 min 01 s ago')
assert.equal(relWords(now + 44_000, now), 'in 44 s')
assert.equal(relWords(now + 200, now), 'now')
assert.equal(clockText(225), '3:45')
assert.equal(clockText(3725), '1:02:05')

// ---- a flag's evidence, as the engine logs it ----------------------------------------------------
const flag = 'act A-1 against our vehicle 1, observed by 1:eo_ir at t=1795533619054: attributed to T-0008 at 0.88 (62 m from it, 23 m 1-sigma), below the threshold 0.90'
assert.equal(
  plainText(flag, { nameOf, when: agoWhen(now) }),
  'act A-1 against HUNTER 1, observed by HUNTER 1 EO/IR 2 min 01 s ago: attributed to T-0008 at 0.88 (62 m from it, 23 m 1-sigma), below the threshold 0.90',
)
// the same on a situation's clock (its start 3:30 before the act)
assert.equal(
  plainText('observed by 1:eo_ir at t=1795533619054', { nameOf, when: situationWhen(1795533619.054 - 210) }),
  'observed by HUNTER 1 EO/IR at T+3:30',
)
// an alert's detail, as the bridge words it (seconds)
assert.equal(
  plainText('reports propulsion lost since t=1795533627.2: it cannot move, so an order will not move it', { nameOf, when: agoWhen(1795533627200 + 588_000) }),
  'reports propulsion lost 9 min 48 s ago: it cannot move, so an order will not move it',
)
// an assessment's evidence
assert.equal(
  plainText('T-0008 detected +3.3 s after the effect by 1:radar, 1:eo_ir, 1:sonar, 2:radar, 2:eo_ir', { nameOf, when: agoWhen(now) }),
  'T-0008 detected +3.3 s after the effect by HUNTER 1 (radar, EO/IR and sonar) and REAPER 1 (radar and EO/IR)',
)
// what is not one of those tokens passes as it came
for (const s of ['attribution 1.00 >= 0.90', 'T-0008 at 0.88', 'the 1-sigma 23 m', 't=42 s', 'vehicle 1 of ours']) assert.equal(plainText(s, { nameOf, when: agoWhen(now) }), s)

// ---- the running card's own words ----------------------------------------------------------------
assert.equal(againstPlan(1000_000, 1013_000), '13 s after the time planned')
assert.equal(againstPlan(1013_000, 1000_000), '13 s before the time planned')
assert.equal(againstPlan(1000_000, 1000_400), 'on the time planned')
assert.equal(waitingWords(['arrival', 'effect_time'], 'REAPER 1'), 'waiting for REAPER 1 to arrive and for the effect time')
assert.equal(waitingWords(['keep_out_tracks'], 'REAPER 1'), 'waiting for keep-out of other tracks', 'any other check by its plain name')
assert.equal(waitingWords([], 'REAPER 1'), 'waiting for the next step')

console.log('plainText: ok')
