// Checks for groups.ts: node web/src/groups.check.ts (prints "groups: ok").
import assert from 'node:assert/strict'
import { applyPatch, byOfficer, defaultName, groupOfSelection, groupsFor, groupsOf, lookOf, nameProblem, nextColor, officerNote, splitByMaker, strength, toAdd, toSeparate, type OperatorGroup } from './groups.ts'
import { GROUP_PALETTE } from './swarmsModel.ts'

const g = (id: string, members: number[], color: string | null = null, name = id): OperatorGroup => ({ id, name, color, members, created_t: 1, updated_t: 1 })
const north = g('og-1', [12, 44, 77], GROUP_PALETTE[0].color, 'North picket')

// names
assert.equal(nameProblem('  '), 'A group needs a name.')
assert.equal(nameProblem('x'.repeat(65)), 'A name is at most 64 characters.')
assert.equal(nameProblem('North picket'), null)
assert.equal(nameProblem('⚓'.repeat(64)), null, 'characters, not UTF-16 units, as the bridge counts')
assert.equal(nameProblem('🛥'.repeat(64)), null, 'one character each though two UTF-16 units')

// the optimistic PATCH is the bridge's (groups.rs): replace, then add, then remove; de-duplicated;
// order kept
assert.deepEqual(applyPatch(north, { add: [77, 5, 5] }).members, [12, 44, 77, 5], 'add: no duplicates, appended')
assert.deepEqual(applyPatch(north, { remove: [44, 99] }).members, [12, 77], 'remove: unknown ones ignored')
assert.deepEqual(applyPatch(north, { members: [3, 3, 4] }).members, [3, 4], 'members replaces wholesale')
assert.deepEqual(applyPatch(north, { members: [3, 4], remove: [3], add: [9] }).members, [4, 9], 'replace, add, remove')
assert.deepEqual(applyPatch(north, { add: [5], remove: [5] }).members, [12, 44, 77], 'a uid both added and removed ends out, as the bridge leaves it')
assert.equal(applyPatch(north, { name: '  Lime  ' }).name, 'Lime', 'a name is trimmed')
assert.equal(applyPatch(north, { color: null }).color, null, 'a colour can be cleared')
assert.equal(applyPatch(north, {}).color, north.color, 'and is kept when not named')
assert.deepEqual(north.members, [12, 44, 77], 'the original is not changed')

// colours: the first palette colour no group has, the palette again once all are used
assert.equal(nextColor([]), GROUP_PALETTE[0].color)
assert.equal(nextColor([north]), GROUP_PALETTE[1].color)
assert.equal(nextColor(GROUP_PALETTE.map((p) => ({ color: p.color }))), GROUP_PALETTE[0].color, 'all used: round again')
assert.deepEqual(lookOf(north, 0), GROUP_PALETTE[0], 'a palette colour keeps its ink')
assert.equal(lookOf(g('x', [], '#ffffff'), 0).ink, '#10141a', 'a light colour takes dark ink')
assert.equal(lookOf(g('x', [], '#000080'), 0).ink, '#ffffff', 'a dark one takes white')
assert.equal(lookOf(g('x', [], null), 1).color, GROUP_PALETTE[1].color, 'no colour: the palette by place')

// selection
assert.equal(groupOfSelection([north], new Set([77, 12, 44]))?.id, 'og-1', 'exactly its members, in any order')
assert.equal(groupOfSelection([north], new Set([12, 44])), null, 'a part of it is not the group')
assert.equal(groupOfSelection([north], new Set([12, 44, 77, 1])), null, 'nor more than it')
assert.equal(groupOfSelection([north], new Set()), null, 'nothing is no group')
assert.deepEqual(toAdd(north, new Set([12, 3, 1])), [1, 3], 'add: only those not in it, sorted')
assert.deepEqual(toSeparate(north, new Set([12, 3, 77])), [12, 77], 'separate: only its members that are selected')
assert.deepEqual(groupsOf([north, g('og-2', [12])], 12).map((x) => x.id), ['og-1', 'og-2'], 'a vehicle may be in several')
assert.deepEqual(strength(north, new Set([12, 44])), { here: 2, away: 1 })

// names for new groups
assert.equal(defaultName([]), 'Group 1')
assert.equal(defaultName([g('a', [], null, 'Group 1'), g('b', [], null, 'Group 3')]), 'Group 2', 'the lowest number free')

// who made a group: the operator's own, the officer's for a tasking item (an older bridge says
// nothing of either: the operator's)
const officers = { ...g('og-2', [1, 2], null, 'West picket'), by: 'agent-watch', agent: true, tasking: 'tk-3' }
const opsOwn = { ...g('og-3', [3]), by: 'op-1' }
const old = g('og-4', [4])
assert.ok(byOfficer(officers) && !byOfficer(opsOwn) && !byOfficer(old))
const split = splitByMaker([officers, opsOwn, old])
assert.deepEqual([split.yours.map((x) => x.id), split.officer.map((x) => x.id)], [['og-3', 'og-4'], ['og-2']])
assert.deepEqual(groupsFor([officers, opsOwn, { ...officers, id: 'og-5', tasking: 'tk-4' }], 'tk-3').map((x) => x.id), ['og-2'], "the officer's for that item only")
assert.match(officerNote(officers), /^Made by the AI watch officer \(agent-watch\) for tasking tk-3: organisation only, it orders nothing\. You may change or delete it/)
assert.match(officerNote({ ...officers, changed_by: 'agent-watch' }), /You may change or delete it/, 'its own change is still its own')
assert.match(officerNote({ ...officers, changed_by: 'op-1' }), /Changed by op-1 since, so the officer can no longer change it\.$/)
console.log('groups: ok')
