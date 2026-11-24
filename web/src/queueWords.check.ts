// Checks for queueWords.ts.
import assert from 'node:assert/strict'
import { leftWords, queueShown, whyLine } from './queueWords.ts'

assert.equal(leftWords(40), '40 s left')
assert.equal(leftWords(120), '2 min left')
assert.equal(leftWords(7200), '2 h left')
assert.equal(leftWords(0), 'out of time')

assert.equal(whyLine({}), null)
assert.match(whyLine({ resumed: {} })!, /restart/)
assert.match(whyLine({ dispatched_before: true })!, /holds first/)
assert.match(whyLine({ resumed: {}, dispatched_before: true })!, /restart.*; sent before/)

assert.equal(queueShown(0, 0, false), false)
assert.equal(queueShown(1, 0, false), true)
assert.equal(queueShown(0, 2, false), true)
assert.equal(queueShown(0, 0, true), true, 'an open drawer keeps its button to close it')
console.log('queueWords.check ok')
