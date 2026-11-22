// Checks for dock.ts and officer.ts: node web/src/dock.check.ts (prints "dock: ok").
import assert from 'node:assert/strict'
import { closeDock, DOCK_WIDTH, fitPad, fitRight, FIT_MIN, FIT_PAD, headsWithWaiting, toggleDock, type DockId } from './dock.ts'
import { awaitingRows, composeKey, countTone, excerpt, insertAt, INSERT_MAX, proposedIn, selectionInsert, sendState, turnItems, turnOf } from './officer.ts'
import type { Intent } from './decisions'
import type { TaskingItem } from './tasking.ts'

// ---- the dock: one drawer at a time ------------------------------------------------------------
assert.equal(toggleDock(null, 'inbox'), 'inbox', 'a button opens its drawer')
assert.equal(toggleDock('inbox', 'swarms'), 'swarms', 'another button shows its drawer in place of the one shown: never on top of it')
assert.equal(toggleDock('swarms', 'swarms'), null, 'the button of the drawer shown closes the dock')
assert.equal(closeDock('officer', 'officer'), null, "a drawer's own close closes it")
assert.equal(closeDock('inbox', 'sitrep'), 'inbox', 'a late close from a drawer already replaced leaves the one shown')
assert.equal(closeDock(null, 'queue'), null)
// opening every drawer one after another leaves exactly the last one shown (the overlap this fixes)
const ids: DockId[] = ['inbox', 'officer', 'swarms', 'sitrep']
assert.equal(ids.reduce<DockId | null>((d, id) => toggleDock(d, id), null), 'sitrep')

assert.equal(fitPad(null), FIT_PAD, 'nothing docked: the usual margin')
assert.equal(fitPad('inbox'), 430)
assert.equal(fitPad('sitrep'), DOCK_WIDTH.sitrep, 'a fit clears the wide drawer too (it used 70 under the Sitrep)')
assert.equal(fitRight(450, 1200, 70), 450, 'room enough: as asked')
assert.equal(fitRight(780, 685, 70), 685 - 70 - FIT_MIN, 'a wide drawer on a narrow map: FIT_MIN of the map is kept')
assert.equal(fitRight(780, 100, 70), 0, 'never negative')
assert.equal(headsWithWaiting('inbox'), false, 'the Inbox shows the cards themselves')
for (const id of ['officer', 'queue', 'sitrep', 'swarms'] as const) assert.equal(headsWithWaiting(id), true, `${id}: what waits for a person heads it`)

// ---- the turn link: guidance and the orders proposed in the turn that read it -------------------
const item = (id: string, at_t: number, extra: Partial<TaskingItem> = {}): TaskingItem => ({ id, text: `guidance ${id}`, by: 'op-1', at_t, active: true, ...extra })
const intent = (id: number, created_t: number, source: Intent['source'], extra: Partial<Intent> = {}): Intent =>
  ({
    id, created_t, source, kind: 'orbit', params: {}, uids: ['1', '2'], constraints: {}, rationale: null, state: 'pending_approval', checks: [],
    policy: null, approval: null, expires_t: null, history: [], ...extra,
  }) as Intent
const watch = { kind: 'agent' as const, id: 'agent-watch' }
const done = item('tk-1', 1000, { delivery: 'processed', officer: 'agent-watch', seen_t: 2000, seen_wake: 4, processed_t: 9000 })
const going = item('tk-2', 1500, { delivery: 'seen', officer: 'agent-watch', seen_t: 2000, seen_wake: 4 })
const sent = item('tk-3', 1800, { delivery: 'posted' })
assert.equal(turnOf(intent(1, 5000, watch), [sent]), null, 'guidance the officer has not been given: no turn')
assert.deepEqual(turnOf(intent(1, 5000, watch), [done]), { item: done, also: 0, wake: 4 }, 'proposed during the turn that read it')
assert.equal(turnOf(intent(1, 1900, watch), [done]), null, 'proposed before the officer was given it')
assert.equal(turnOf(intent(1, 9500, watch), [done]), null, 'proposed after that turn finished: a later turn')
assert.equal(turnOf(intent(1, 99_000, watch), [going])?.item.id, 'tk-2', 'a turn still going: open-ended')
const failed = item('tk-4', 1000, { delivery: 'seen', officer: 'agent-watch', seen_t: 2000, failed: { t: 3000, why: 'the turn ended in an error' } })
assert.equal(turnOf(intent(1, 4000, watch), [failed]), null, 'a failed turn ended at its failure')
assert.equal(turnOf(intent(1, 5000, { kind: 'agent', id: 'agent-scripted' }), [done]), null, 'another agent (a situation script) did not read it')
assert.equal(turnOf(intent(1, 5000, { kind: 'human', id: 'op-1' }), [done]), null, "a person's order: no turn")
const both = turnOf(intent(1, 5000, watch), [done, going])!
assert.deepEqual([both.item.id, both.also], ['tk-2', 1], 'one turn read two: the newest named, the other counted')
assert.deepEqual(turnItems(intent(1, 5000, watch), [done, going]).map((i) => i.id), ['tk-2', 'tk-1'])
const ins = [intent(3, 6000, watch), intent(2, 4000, watch), intent(4, 9500, watch), intent(5, 5000, { kind: 'human', id: 'op-1' })]
assert.deepEqual(proposedIn(done, ins).map((i) => i.id), [2, 3], "the other way round: that turn's orders, oldest first")

assert.equal(excerpt('  Hold   the\nferry lane  '), 'Hold the ferry lane')
assert.equal(excerpt('x'.repeat(100), 10), `${'x'.repeat(9)}…`)
assert.equal([...excerpt('🛥'.repeat(20), 5)].length, 5, 'cut by characters, not UTF-16 units')

// ---- what waits for a person, at the head of any drawer --------------------------------------------
const pend = [
  intent(7, 5000, watch, { expires_t: 100_000, group: { swarm: 'CRISSY' }, uids: ['1', '2', '3', '4', '5', '6', '7', '8'] }),
  intent(6, 5000, { kind: 'human', id: 'op-2' }, { expires_t: 40_000, uids: ['9'] }),
  intent(5, 5000, { kind: 'agent', id: 'agent-scripted' }, { expires_t: null, kind: 'engage', uids: ['3'] }),
  intent(8, 5000, { kind: 'agent', id: 'agent-scripted' }, { expires_t: 100_000 }),
]
const rows = awaitingRows(pend, [done], 10_000, (u) => `UNIT ${u}`)
assert.deepEqual(rows.map((r) => r.id), [6, 7, 8, 5], 'soonest expiry first, as the Inbox; one that never expires last; ties by number')
assert.deepEqual(rows.map((r) => r.what), ['UNIT 9', 'CRISSY (8)', '2 vehicles', 'UNIT 3'])
assert.equal(rows[0].left_s, 30)
assert.equal(rows[1].turn?.item.id, 'tk-1', "the officer's order names the guidance its turn read")
assert.equal(rows[2].turn, null)
assert.equal(rows[3].engage, true)

// ---- the message box ------------------------------------------------------------------------------
const k = (key: string, mods: Partial<{ shiftKey: boolean; altKey: boolean; isComposing: boolean }> = {}) => composeKey({ key, shiftKey: false, altKey: false, ...mods })
assert.equal(k('Enter'), 'send', 'Enter sends')
assert.equal(k('Enter', { shiftKey: true }), null, 'Shift+Enter: a new line')
assert.equal(k('Enter', { isComposing: true }), null, 'an input method choosing a word keeps its Enter')
assert.equal(k('a'), null)

const live = { t: 50_000, officer: { agent: 'agent-watch', read_t: 49_000 } }
assert.deepEqual(sendState('Watch the ferry lane', 'ok', live, false), { ok: true, why: null })
assert.deepEqual(sendState('   ', 'ok', live, false), { ok: false, why: null }, 'an empty box says nothing (its placeholder does)')
assert.deepEqual(sendState('x'.repeat(503), 'ok', live, false), { ok: false, why: 'At most 500 characters: 3 too many.' })
const gone = sendState('Watch the ferry lane', 'ok', { t: 50_000, officer: { agent: 'agent-watch', read_t: 20_000 } }, false)
assert.equal(gone.ok, false, 'no officer reads it now: Send waits')
assert.match(gone.why!, /^No watch officer is connected \(agent-watch last read 30 s ago\): nobody would read this now\. The draft stays here/)
assert.match(sendState('x', 'ok', { t: 50_000, officer: null }, false).why!, /^No watch officer is connected: /, 'none has ever read it')
assert.deepEqual(sendState('x', 'ok', { t: 50_000, officer: undefined }, false), { ok: true, why: null }, 'a bridge that does not say lets it go')
assert.equal(sendState('x', 'loading', live, false).ok, false)
assert.equal(sendState('x', 'missing', live, false).ok, false)
assert.deepEqual(sendState('x', 'ok', live, true), { ok: false, why: null }, 'sending: the button says so')
assert.equal(countTone('x'.repeat(449)), 'muted')
assert.equal(countTone('x'.repeat(450)), 'warn')
assert.equal(countTone('x'.repeat(501)), 'bad')

const name = (u: number) => `SKY ${u}`
assert.equal(selectionInsert(null, [], name), null, 'nothing selected: nothing to insert')
assert.deepEqual(selectionInsert('PICKET', Array.from({ length: 60 }, (_, i) => i + 1), name), { text: 'PICKET (60)' }, 'a group by its name and size')
assert.deepEqual(selectionInsert(null, [27, 8], name), { text: 'SKY 8, SKY 27' }, 'a few by callsign, in fleet order')
const many = selectionInsert(null, Array.from({ length: INSERT_MAX + 1 }, (_, i) => i), name)!
assert.equal(many.text, null)
assert.match('why' in many ? many.why : '', /^9 vehicles are selected and they are not one group/)

assert.deepEqual(insertAt('', 0, 0, 'PICKET (60)'), { text: 'PICKET (60)', caret: 11 })
assert.deepEqual(insertAt('Hold', 4, 4, 'PICKET (60)'), { text: 'Hold PICKET (60)', caret: 16 }, 'a space before, none at the end')
assert.deepEqual(insertAt('Hold  here', 5, 5, 'T-0001'), { text: 'Hold T-0001 here', caret: 11 }, 'between two spaces: none added')
assert.deepEqual(insertAt('Move x.', 5, 6, 'SKY 8'), { text: 'Move SKY 8.', caret: 10 }, 'over the selected part; no space before a full stop')

console.log('dock: ok')
