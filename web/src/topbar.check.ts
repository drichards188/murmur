// Checks for topbar.ts: node web/src/topbar.check.ts, as make check does. The claims (bd
// murmur-iobd, ledger 1 section 1): the top bar counts ours in trouble with the fleet rows' own
// slot, every kind of it, its tone the worst among them; a click walks through them in fleet order;
// the glance line is the Sitrep's bottom line (ours, contacts, hostile and suspect); the Sitrep's
// button waits for group scale. Prints "topbar: ok", throws on the first failure.
import { glanceOf, GROUP_SCALE, groupScale, mattersText, nextTrouble, troubleOf } from './topbar.ts'
import type { Vehicle } from './types'
import { NO_EXCEPTIONS, type ExceptionContext } from './vehicleException.ts'

function check(cond: unknown, what: string) {
  if (!cond) throw new Error(`topbar: ${what}`)
}

const v = (uid: number, o: { stale?: boolean; loss?: number } = {}): Vehicle =>
  ({
    uid, sysid: uid, kind: 'boat',
    link: { stale: o.stale ?? false, since_last_ms: o.stale ? 12_000 : 200, expected_interval_ms: 1000, loss_pct: o.loss ?? 0 },
    identity: { callsign: `HUNTER ${uid}`, platform: 'usv', short: 'USV', domain: 'surface' },
  }) as unknown as Vehicle

// nothing wrong: nothing counted
const calm = troubleOf([v(1), v(2)], NO_EXCEPTIONS)
check(calm.n === 0 && calm.tone === null && calm.uids.length === 0, 'none in trouble')

// every kind counts, the worst tone wins, fleet order kept
const ctx: ExceptionContext = { ...NO_EXCEPTIONS, damage: new Map([[3, { mobility: true, firepower: false }]]), noRounds: new Set([4]) }
const t = troubleOf([v(1), v(2, { stale: true }), v(3), v(4), v(5, { loss: 30 })], ctx)
check(t.n === 4 && t.uids.join() === '2,3,4,5', `silent, damaged, out of rounds and degraded all count: ${t.uids.join()}`)
check(t.tone === 'bad', 'a silent or damaged one makes it red')
check(t.says.includes('HUNTER 3: damaged: cannot move'), `the hover names each in its slot's words: ${t.says}`)
const amber = troubleOf([v(4)], ctx)
check(amber.n === 1 && amber.tone === 'warn', 'out of rounds alone is amber, as its slot')
const settled = troubleOf([v(6)], { ...NO_EXCEPTIONS, nodes: { on: true, review: new Map(), lost: new Map([[6, {} as never]]) } })
check(settled.n === 1 && settled.tone === 'settled', 'a loss an operator confirmed still counts, grey')

// a click walks through them
check(nextTrouble([2, 3, 5], null) === 2, 'the first')
check(nextTrouble([2, 3, 5], 2) === 3 && nextTrouble([2, 3, 5], 5) === 2, 'the next, and round again')
check(nextTrouble([2, 3, 5], 9) === 2, 'one no longer in trouble: from the first')
check(nextTrouble([], 2) === null, 'none')

// the glance line
const track = (affiliation: string, declared: string | null = null) => ({ affiliation, declared: declared ? { affiliation: declared } : null, state: 'active' }) as never
const g = glanceOf([v(1), v(2)], [track('neutral'), track('suspect'), track('unknown', 'hostile'), track('hostile')])
check(g.ours === 2 && g.contacts === 4 && g.hostile === 2 && g.suspect === 1, `ours, contacts, what matters: ${JSON.stringify(g)}`)
check(mattersText(g) === '2 hostile, 1 suspect' && mattersText({ hostile: 0, suspect: 0 }) === '', 'only what matters, in words')

// group scale
check(!groupScale(22) && !groupScale(GROUP_SCALE) && groupScale(GROUP_SCALE + 1), 'the Sitrep waits for more than GROUP_SCALE of ours')

console.log('topbar: ok')
