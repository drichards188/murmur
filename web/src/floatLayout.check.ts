// Checks for floatLayout.ts: node web/src/floatLayout.check.ts (prints "floatLayout: ok").
import assert from 'node:assert/strict'
import { LAYOUT_MAX, LAYOUT_VERSION, layoutText, OFFSET_MAX, parseLayout, spotFrom, withSpot, type Layout } from './floatLayout.ts'

const weather = { ax: 'r', x: 12, ay: 't', y: 12 } as const
const links = { ax: 'l', x: 12, ay: 'b', y: 88 } as const
const text = (spots: unknown) => JSON.stringify({ v: LAYOUT_VERSION, spots })

// ---- read back what was written --------------------------------------------------------------------
const one = withSpot({}, 'weather', weather)
assert.deepEqual(parseLayout(layoutText(one)), { weather }, 'a dragged panel comes back where it was')
const two = withSpot(one, 'links', links)
assert.deepEqual(parseLayout(layoutText(two)), { weather, links })
assert.deepEqual(withSpot(two, 'weather', null), { links }, 'a double-click on the bar forgets the spot')
assert.deepEqual(withSpot(two, 'weather', { ...weather, x: 40 }).weather, { ...weather, x: 40 }, 'dragged again: the new spot')
assert.deepEqual(withSpot({}, 'globe', null), {}, 'forgetting one never dragged changes nothing')

// ---- nothing usable stored: the empty layout (every panel auto-placed) -------------------------------
for (const raw of [null, undefined, '', 'not json', '42', 'null', '[]', '{"spots":{}}', JSON.stringify({ v: 2, spots: { weather } }), text([]), text('x'), text(null)]) {
  assert.deepEqual(parseLayout(raw), {}, `stored ${String(raw)}`)
}

// ---- a bad entry is dropped on its own, the rest kept -------------------------------------------------
const mixed = parseLayout(
  text({
    weather,
    links,
    globe: { ax: 'up', x: 1, ay: 't', y: 1 },
    situation: { ax: 'l', x: '12', ay: 't', y: 1 },
    platforms: { ax: 'l', x: 1, ay: 'middle', y: 1 },
    nominate: { ax: 'l', x: Infinity, ay: 't', y: 1 },
    'vehicle-details': { ax: 'l', x: OFFSET_MAX + 1, ay: 't', y: 1 },
    Weather: weather,
    'a b': weather,
    longer: [1, 2],
    spare: null,
  }),
)
assert.deepEqual(mixed, { weather, links }, 'only well-formed spots under well-formed ids')
// a stored "__proto__" key (JSON.parse makes it an own key) is not an id, and never a prototype
const proto = parseLayout(`{"v":${LAYOUT_VERSION},"spots":{"__proto__":${JSON.stringify(weather)},"links":${JSON.stringify(links)}}}`)
assert.deepEqual(proto, { links })
assert.equal(Object.getPrototypeOf(proto), Object.prototype)
assert.equal((proto as Record<string, unknown>).ax, undefined)
// extra fields in a spot are not carried
assert.deepEqual(parseLayout(text({ weather: { ...weather, z: 9, note: 'x' } })), { weather })
assert.deepEqual(spotFrom({ ax: 'l', x: -OFFSET_MAX, ay: 'b', y: 0 }), { ax: 'l', x: -OFFSET_MAX, ay: 'b', y: 0 }, 'the bound itself is a spot')
assert.equal(spotFrom([1, 2]), null)
assert.equal(spotFrom(null), null)

// ---- bounded: never more than LAYOUT_MAX panels -------------------------------------------------------
const lots: Record<string, unknown> = {}
for (let k = 0; k < LAYOUT_MAX + 10; k++) lots[`p${k}`] = weather
assert.equal(Object.keys(parseLayout(text(lots))).length, LAYOUT_MAX)
let full: Layout = {}
for (let k = 0; k < LAYOUT_MAX; k++) full = withSpot(full, `p${k}`, weather)
assert.equal(withSpot(full, 'extra', weather), full, 'a full layout takes no new panel')
assert.deepEqual(withSpot(full, 'p3', links).p3, links, 'but one already in it moves')

// ---- a bad id or spot changes nothing -------------------------------------------------------------------
assert.equal(withSpot(one, 'Bad Id', weather), one)
assert.equal(withSpot(one, 'links', { ax: 'l', x: NaN, ay: 't', y: 0 }), one)

console.log('floatLayout: ok')
