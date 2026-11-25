// Checks for topbar.ts: node web/src/topbar.check.ts, as make check does. The claims (bd
// murmur-iobd, ledger 1 section 1): the top bar counts ours in trouble with the fleet rows' own
// slot, every kind of it, its tone the worst among them; a click walks through them in fleet order;
// the glance line is the Sitrep's bottom line (ours, contacts, hostile and suspect); the Sitrep's
// button waits for group scale. Prints "topbar: ok", throws on the first failure.
import { glanceOf, glanceWords, GROUP_SCALE, groupScale, mattersOf, mattersText, nextOf, nextTrouble, runStanding, troubleOf } from './topbar.ts'
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
check(t.n === 3 && t.uids.join() === '2,3,4', `silent, damaged and out of rounds count, a degraded link does not: ${t.uids.join()}`)
check(troubleOf([v(5, { loss: 30 })], ctx).n === 0, 'a lossy link alone is the row warning, not the bar')
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

// the glance's problem counts link to the contacts they count, one id each, picture order
const tr = (id: string, affiliation: string, declared: string | null = null) => ({ id, affiliation, declared: declared ? { affiliation: declared } : null }) as never
const m = mattersOf([tr('T-1', 'neutral'), tr('T-2', 'suspect'), tr('T-3', 'unknown', 'hostile'), tr('T-4', 'hostile'), tr('T-5', 'hostile', 'neutral')])
check(m.hostile.join() === 'T-3,T-4' && m.suspect.join() === 'T-2', `hostile and suspect by the declaration first: ${JSON.stringify(m)}`)
check(nextOf(m.hostile, null) === 'T-3' && nextOf(m.hostile, 'T-3') === 'T-4' && nextOf(m.hostile, 'T-4') === 'T-3' && nextOf([], 'T-3') === null, 'a click walks through them')

// the glance in words: "N of ours in trouble", the contacts then what matters among them
const words = (p: ReturnType<typeof glanceWords>) => p.map((x) => (x.n != null ? `${x.n} ${x.text}` : x.text)).join('')
const big = { ours: 105, contacts: 2, hostile: 1, suspect: 0 }
check(words(glanceWords(big, 3, false)) === '105 vehicles · 3 of ours in trouble · 2 contacts, 1 hostile', `full: ${words(glanceWords(big, 3, false))}`)
check(words(glanceWords(big, 3, true)) === '3 of ours in trouble · 1 hostile', `folded into the problems: ${words(glanceWords(big, 3, true))}`)
check(words(glanceWords({ ours: 1, contacts: 1, hostile: 0, suspect: 2 }, 0, false)) === '1 vehicle · 1 contact, 2 suspect', 'singulars, and suspect without trouble')
check(words(glanceWords({ ...big, hostile: 0 }, 0, true)) === '' && glanceWords({ ...big, hostile: 0 }, 0, true).length === 0, 'folded with nothing wrong: nothing')
check(words(glanceWords(big, 0, true)) === '1 hostile' && words(glanceWords({ ...big, hostile: 0 }, 2, true)) === '2 of ours in trouble', 'folded, one side only: no stray separator')
check(glanceWords(big, 3, false).filter((x) => x.kind === 'trouble' || x.kind === 'hostile').every((x) => x.n! > 0), 'a problem shows only above 0')

// the run's standing beside its title
const run = { operator: 'autoplay' as const, tutorial: false, phase: 'running', stale: false }
check(runStanding(run).label === 'Autoplay' && runStanding({ ...run, operator: 'live' }).label === 'You decide', 'who decides while it runs')
check(runStanding({ ...run, operator: 'live', tutorial: true }).label === 'Tutorial', 'the tutorial says so')
check(runStanding({ ...run, phase: 'ended' }).label === 'Ended' && runStanding({ ...run, phase: 'ended' }).tone === 'quiet', 'ended, quietly')
check(runStanding({ ...run, stale: true }).tone === 'warn' && runStanding({ ...run, phase: 'failed' }).tone === 'warn', 'a silent or failed runner in amber')
check([runStanding(run), runStanding({ ...run, phase: 'ended' }), runStanding({ ...run, stale: true })].every((s) => !(s.title + s.label).includes(String.fromCharCode(0x2014))), 'no em dash in the words')

// group scale
check(!groupScale(22) && !groupScale(GROUP_SCALE) && groupScale(GROUP_SCALE + 1), 'the Sitrep waits for more than GROUP_SCALE of ours')

console.log('topbar: ok')
