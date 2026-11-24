// Checks for mapWords.ts (no test runner in web/): node web/src/mapWords.check.ts, as make check
// does. The map's labels say what a person reads, in words: no raw ids, no engine codes. Throws on
// the first failure, prints "mapWords: ok" otherwise.
import { collectWords, engageWho, engageWords, estimateWords, lostWords, mayBeRunningWords, queuedWords, resumedWords, spanWords, trackWords, type EngageWords } from './mapWords.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`mapWords: ${what}`)
}
const eq = (got: string, want: string) => check(got === want, `${JSON.stringify(got)} is not ${JSON.stringify(want)}`)

// spans: seconds under a minute and a half, then minutes, then hours
eq(spanWords(47), '47 s')
eq(spanWords(89.4), '89 s')
eq(spanWords(90), '1 min')
eq(spanWords(240), '4 min')
eq(spanWords(7500), '2 h 05 min')
eq(spanWords(-3), '0 s')

// a silent vehicle's estimate: what it is probably doing and how long it has been silent
eq(estimateWords({ failsafe: 'rtl' }, 47), 'probably returning (lost-link RTL); silent 47 s')
eq(estimateWords({ basis: 'orbit', second: false }, 47), 'probably on its orbit; silent 47 s')
eq(estimateWords({ basis: 'velocity', second: false }, 200), 'probably on its last course; silent 3 min')
// the task model once the failsafe leads: the other hypothesis, said as one
eq(estimateWords({ basis: 'patrol', second: true }, 47), 'if it kept its task: on its patrol route')
check(!/EST|RTL [A-Z]/.test(estimateWords({ basis: 'hold', second: false }, 5)), 'no engine codes in the task model')

// a contact's one label: its kind in words first, qualified below an identification, no id
eq(trackWords('small fast boat', 'identify'), 'small fast boat')
eq(trackWords('small air contact', 'detect'), 'small air contact (detected)')
eq(trackWords('vessel', 'recognise'), 'vessel (recognised)')
eq(trackWords(null, 'detect'), 'unclassified')
// its height only when the caller gives it (in focus)
eq(trackWords('light aircraft', 'identify', '450 m'), 'light aircraft · 450 m')
eq(trackWords('light aircraft', null, 'alt ?'), 'light aircraft · alt ?')
check(!/T-\d/.test(trackWords('fishing vessel', 'identify', '0 m')), 'no track id in the label')
// a lost contact: how long, the ring says the rest
eq(lostWords(240), 'lost 4 min')
eq(lostWords(56), 'lost 56 s')

// orders: what and why in words, never the raw intent id
eq(queuedWords('orbit', 'link down', 125), 'queued orbit: link down, 2 min left')
eq(queuedWords('patrol', 'silent for 12.3 s', null), 'queued patrol: silent for 12.3 s')
eq(resumedWords('patrol'), 'resumed patrol: waiting for the vehicle')
// a person decides: it keeps its capitals
eq(mayBeRunningWords('orbit', 'pending_approval'), 'MAY BE RUNNING orbit: its start went out unconfirmed (pending approval)')
eq(collectWords('SAT-2', '10:00Z', true), 'collect SAT-2 10:00Z')
eq(collectWords('SAT-2', '10:00Z', false), 'collect SAT-2 10:00Z (area)')
for (const t of [queuedWords('orbit', 'link down', 60), resumedWords('hold'), mayBeRunningWords('rtl', 'voided')]) check(!/\bi\d+\b/.test(t), `no raw id in ${t}`)

// engagements: who on what and its phase, then the outcome led by the target
const e: EngageWords = { platform: 'REAPER 1', track: 'T-0008', phase: 'running', effectInMs: 44_000, released: false, struck: false }
eq(engageWords(e), 'REAPER 1 on T-0008: effect in 44 s')
eq(engageWords({ ...e, phase: 'pending', effectInMs: 200_000 }), 'REAPER 1 on T-0008: effect in 3 min')
eq(engageWords({ ...e, effectInMs: -2_000 }), 'REAPER 1 on T-0008: effect due')
eq(engageWords({ ...e, released: true }), 'REAPER 1 on T-0008: released')
eq(engageWords({ ...e, phase: 'effected', released: true, struck: true }), 'T-0008 struck: assessing')
eq(engageWords({ ...e, phase: 'effected', released: true, struck: true, assessment: 'persists' }), 'T-0008 still detected')
eq(engageWords({ ...e, phase: 'effected', released: true, struck: true, assessment: 'ceased' }), 'T-0008 no longer detected')
eq(engageWords({ ...e, phase: 'effected', released: true, struck: true, assessment: 'unknown' }), 'T-0008: outcome unknown')
eq(engageWords({ ...e, phase: 'ended', state: 'aborted' }), 'REAPER 1 on T-0008: aborted')
// the nomination form's draft, with or without a target yet
eq(engageWords({ phase: 'draft', track: 'T-0008', effectInMs: 60_000, released: false, struck: false }), 'nomination on T-0008: effect in 60 s')
eq(engageWho({ phase: 'draft' }), 'the nomination')
eq(engageWho(e), 'REAPER 1 on T-0008')

console.log('mapWords: ok')
