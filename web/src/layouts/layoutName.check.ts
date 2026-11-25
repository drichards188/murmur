// Checks for layoutName.ts: node web/src/layouts/layoutName.check.ts (prints "layoutName: ok").
import assert from 'node:assert/strict'
import { LAYOUT_NAMES, layoutName } from './layoutName.ts'

assert.equal(LAYOUT_NAMES[0], 'decisions', 'the default is first')
assert.equal(layoutName(''), 'decisions', 'no query: the default')
assert.equal(layoutName('?perf'), 'decisions', 'other parameters only: the default')
assert.equal(layoutName('?layout='), 'decisions', 'empty: the default')
assert.equal(layoutName('?layout=radar'), 'decisions', 'unknown: the default')
assert.equal(layoutName('?layout=cockpit'), 'cockpit')
assert.equal(layoutName('?layout=rail'), 'rail')
assert.equal(layoutName('?layout=decisions'), 'decisions')
assert.equal(layoutName('?perf&layout=decisions'), 'decisions', 'beside other parameters')
assert.equal(layoutName('layout=rail'), 'rail', 'without the leading ?')
assert.equal(layoutName('?layout=Rail'), 'rail', 'in any case')
assert.equal(layoutName('?layout=%20decisions%20'), 'decisions', 'spaces around it')
assert.equal(layoutName('?layout=rail&layout=decisions'), 'rail', 'named twice: the first')
assert.equal(layoutName('?layout=toString'), 'decisions', 'never a property of an object')
for (const n of LAYOUT_NAMES) assert.equal(layoutName(`?layout=${n}`), n, `${n} is reachable`)

console.log('layoutName: ok')
