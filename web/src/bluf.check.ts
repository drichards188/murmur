// Checks for bluf.ts (no test runner in web/): node web/src/bluf.check.ts, as make check does.
// Throws on the first failure, prints "bluf: ok" otherwise. Not part of the bundle.
import { checksSummary, parseRationale, reasonBluf } from './bluf.ts'

function eq(a: unknown, b: unknown, what: string) {
  const x = JSON.stringify(a)
  const y = JSON.stringify(b)
  if (x !== y) throw new Error(`${what}: got ${x}, want ${y}`)
}

// the convention
eq(
  parseRationale('Orbit PUMA 1 on the unknown boat\n- 1.5 km west of the bridge, no AIS\n- orbit clears the lane\n\nThe boat has held position since 6:10.'),
  { headline: 'Orbit PUMA 1 on the unknown boat', bullets: ['1.5 km west of the bridge, no AIS', 'orbit clears the lane'], more: 'The boat has held position since 6:10.' },
  'convention',
)
eq(parseRationale('Hold PUMA 2'), { headline: 'Hold PUMA 2', bullets: [], more: '' }, 'one line')
eq(parseRationale('Headline\n\nDetail here.'), { headline: 'Headline', bullets: [], more: 'Detail here.' }, 'headline and detail')
eq(parseRationale('A\n- 1\n- 2\n- 3\n- 4\n- 5\n- 6').bullets.length, 4, 'bullets capped')
eq(parseRationale('A\n- 1\n- 2\n- 3\n- 4\n- 5\n- 6').more, '- 5\n- 6', 'extra bullets behind more')
// free text: first sentence, the rest behind More
const old = 'An unidentified fast boat is holding 1.5 km west of the bridge. It has no AIS and came in at 15 m/s. An orbit would let the camera identify it.'
eq(
  parseRationale(old),
  { headline: 'An unidentified fast boat is holding 1.5 km west of the bridge.', bullets: [], more: 'It has no AIS and came in at 15 m/s. An orbit would let the camera identify it.' },
  'free text',
)
const lp = parseRationale('x'.repeat(300))
if (lp.headline.length > 141 || !lp.more) throw new Error('a long headline is cut and kept behind more')
eq(parseRationale(''), { headline: '', bullets: [], more: '' }, 'empty')

// identify outcomes
const live =
  'track T-0007 identified as fishing_vessel by 1:sonar; it contradicts its identity before, fishing_vessel (identify): AIS says law_enforcement (CGC TERN) but sonar identifies it as fishing_vessel; holding: no order to go back to'
const r = reasonBluf(live, { who: 'PUMA 1' })
eq(r.headline, 'T-0007 identity conflict: sonar says fishing vessel, AIS says CGC TERN', 'identify headline')
eq(r.bullets, ['earlier identity: fishing vessel (identify)', 'PUMA 1 holding (no earlier order)'], 'identify bullets')
eq(r.more, live, 'identify keeps the original')
const old2 = reasonBluf(
  'track T-0001 identified as patrol_boat by puma-1:eo_ir; it contradicts its identity before, fishing_vessel (identify): AIS says fishing_vessel; puma-1:eo_ir identifies patrol_boat; holding: no order to go back to',
)
eq(old2.headline, 'T-0001 identity conflict: eo ir says patrol boat, AIS says fishing vessel', 'older conflict wording')
eq(reasonBluf('track T-0001 identified as small_fast_boat by puma-1:eo_ir; holding: no order to go back to').bullets, ['vehicle holding (no earlier order)'], 'plain identify')
eq(
  reasonBluf("track T-1 identified as ferry by 2:eo_ir; it agrees with its identity before, ferry (identify); back to i3's orbit").bullets,
  ['agrees with its earlier identity: ferry (identify)', "back to i3's orbit"],
  'agrees',
)
// other reasons split on "; "
eq(
  reasonBluf('no telemetry; link marginal; battery 31%'),
  { headline: 'no telemetry', bullets: ['link marginal', 'battery 31%'], more: 'no telemetry; link marginal; battery 31%' },
  'split',
)
eq(reasonBluf('approved by op-1'), { headline: 'approved by op-1', bullets: [], more: '' }, 'short reason')

// checks
eq(checksSummary(Array.from({ length: 7 }, () => ({ rule: 'x', outcome: 'pass' as const }))), { text: '7 checks passed', tone: 'ok' }, 'all pass')
eq(checksSummary([{ rule: 'domain', outcome: 'fail' }, { rule: 'a', outcome: 'pass' }]).text, '1 failed: domain', 'one failed')
eq(checksSummary([{ rule: 'a', outcome: 'pass' }, { rule: 'link', outcome: 'unverified' }]).tone, 'warn', 'unverified warns')
console.log('bluf: ok')
