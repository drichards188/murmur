// Checks for impact.ts: node web/src/impact.check.ts, as make check does. The claims (bd
// murmur-iobd, change 8, R14): the assessment's banner asks only when the target persists or the
// outcome is unknown, never for a ceased one; verdict first; once an engagement however many rounds;
// for IMPACT_STATUS_MS from the assessment, or until dismissed. Prints "impact: ok".
import { assessmentAsk, IMPACT_STATUS_MS, type ImpactEvent } from './impact.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`impact: ${what}`)
}

const T = 2_000_000
const strike = (intentId: number, result: 'persists' | 'ceased' | 'unknown' | null, t = T, key = `i${intentId}`): ImpactEvent => ({
  key, intentId, point: [-122.48, 37.82], t, radius_m: 75, platform: 'REAPER 1', track: 'T-0008', assessment: result ? { result, t: t + 15_000 } : undefined,
})
const none = new Set<number>()

check(assessmentAsk([strike(2, null)], T + 5000, none) === null, 'before the assessment: no banner (the strike itself asks nothing)')
check(assessmentAsk([strike(2, 'ceased')], T + 20_000, none) === null, 'ceased asks no decision')
const p = assessmentAsk([strike(2, 'persists')], T + 20_000, none)
check(p?.text === 'T-0008 still detected after the strike: decide whether to engage again.', `persists, verdict first (${p?.text})`)
const u = assessmentAsk([strike(3, 'unknown')], T + 20_000, none)
check(u?.text === 'Outcome unknown: none of ours covers the strike on T-0008. Decide whether to look again.', `unknown (${u?.text})`)
check(assessmentAsk([strike(2, 'persists')], T + 15_000 + IMPACT_STATUS_MS, none) === null, 'gone after IMPACT_STATUS_MS')
check(assessmentAsk([strike(2, 'persists')], T + 20_000, new Set([2])) === null, 'dismissed')
const salvo = [strike(4, 'persists', T, 'i4r1'), strike(4, 'persists', T + 5000, 'i4r2')]
check(assessmentAsk(salvo, T + 21_000, none)?.impact.key === 'i4r2' && assessmentAsk(salvo, T + 21_000, new Set([4])) === null, 'a salvo: one banner, one dismissal')

console.log('impact: ok')
