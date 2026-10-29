// Checks for hostileAct.ts (no test runner in web/): node web/src/hostileAct.check.ts, as make
// check does. The claims: an observed act's burst plays once, from when the console first saw it
// (never replayed for an act seen late); an abstract act is drawn as a report (a dashed arrow, no
// streak), a weapon act without an arrow (its weapon is drawn); its label fades after some seconds
// and its line and marker after a minute or a dismissal; at most a few are drawn and labelled at
// once; the author is drawn where our sensors placed it and named only when the engine attributed
// the act; the victim stays where it was when the act came in. Throws on the first failure,
// prints "hostileAct: ok" otherwise.
import type { ObservedAct } from './ident'
import {
  actDrawings,
  animating,
  animStart,
  arrowHead,
  attributionOf,
  bannerAct,
  bannerLine,
  BANNER_MS,
  dashes,
  FRESH_MS,
  headline,
  LABEL_FADE_MS,
  LABEL_HOLD_MS,
  labelAlpha,
  labelledKeys,
  LINGER_FADE_MS,
  LINGER_MS,
  lingerAlpha,
  MAX_DRAWN,
  MAX_LABELS,
  muzzleFlash,
  subline,
  visibleActs,
  type ActDrawing,
  type Pt3,
  type Seen,
  type Victim,
} from './hostileAct.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`hostileAct check failed: ${what}`)
}
const near = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps

// ---- when it starts --------------------------------------------------------------------------
check(animStart(10_000, 11_200) === 11_200, 'an act seen a moment after it was observed plays from when it is seen')
check(animStart(10_000, 9_900) === 10_000, 'never before its observation')
check(animStart(10_000, 10_000 + FRESH_MS + 1) === 10_000, 'an act seen late is placed at its observation, not replayed')

// ---- a burst where it fired; the arrow's head at the victim ------------------------------------
const a: Pt3 = [-122.495, 37.812, 0]
const b: Pt3 = [-122.47, 37.81, 30]
check(muzzleFlash(0) === 0 && muzzleFlash(-1) === null && muzzleFlash(10_000) === null, 'the burst starts with the act')
const head = arrowHead(a, b)
check(head.length === 2 && head.every(([tip, end]) => tip === b && end[2] === b[2]), 'two strokes back from the tip, at its height')
const back = head.map(([, e]) => Math.hypot((e[0] - b[0]) * 88_000, (e[1] - b[1]) * 111_320))
check(back.every((m) => m > 100 && m <= 181), `each stroke at most 180 m: ${back}`)
check(head.every(([, e]) => e[0] < b[0]), 'pointing back toward the author')
check(arrowHead(a, a).length === 0, 'no head for a point')

// ---- the label fades, the line and marker linger a minute --------------------------------------
check(labelAlpha(0) === 1 && labelAlpha(LABEL_HOLD_MS - 1) === 1, 'the label is held')
check(near(labelAlpha(LABEL_HOLD_MS + LABEL_FADE_MS / 2), 0.5), 'then fades')
check(labelAlpha(LABEL_HOLD_MS + LABEL_FADE_MS) === 0 && labelAlpha(-5) === 0, 'and is gone')
check(lingerAlpha(0) === 1 && lingerAlpha(LINGER_MS - LINGER_FADE_MS) === 1, 'the line and marker are kept')
check(near(lingerAlpha(LINGER_MS - LINGER_FADE_MS / 2), 0.5), 'then fade')
check(lingerAlpha(LINGER_MS) === 0, 'gone after a minute')
check(animating(0) && animating(LABEL_HOLD_MS + LABEL_FADE_MS - 1) && !animating(LABEL_HOLD_MS + LABEL_FADE_MS) && !animating(-1), 'frames only while something moves or fades fast')
const d = dashes(a, b, 4)
check(d.length === 4 && near(d[0][0][0], a[0]) && near(d[0][1][0], a[0] + (b[0] - a[0]) * 0.125) && near(d[3][1][0], a[0] + (b[0] - a[0]) * 0.875), 'dashes from the author, half of each period drawn')

// ---- from what the bridge sends ----------------------------------------------------------------
const act = (n: number, observed_t: number, verdict: ObservedAct['verdict'], over: Partial<ObservedAct> = {}): ObservedAct => ({
  act: { act: `A-${n}`, against: { kind: 'vehicle', uid: '1' }, observed_t, sensors: ['1:eo_ir', '1:sonar', '3:eo_ir'], location: [-122.495, 37.812], sigma_m: 42 },
  candidates: [{ track: 'T-0006', distance_m: 30, sigma_m: 40, confidence: 0.97 }],
  verdict,
  declaration: null,
  flag: null,
  ...over,
})
const declared = act(1, 100_000, { verdict: 'declare', track: 'T-0006', attribution: 0.97 }, { declaration: 'D-1' })
const flagged = act(2, 160_000, { verdict: 'flag', kind: 'conflict', tracks: ['T-0006', 'T-0002'], why: 'two tracks' }, { flag: 'F-2' })
const recorded = act(3, 220_000, { verdict: 'recorded', why: 'T-0006 is already declared hostile' })
const nobody = act(4, 280_000, { verdict: 'flag', kind: 'untracked', tracks: [], why: 'no track near' }, { candidates: [], flag: 'F-3' })
check(attributionOf(declared).track === 'T-0006' && attributionOf(declared).score === 0.97, 'declared: the track it was declared on')
check(attributionOf(flagged).track === null && attributionOf(flagged).note === 'flag F-2', 'flagged: unattributed, saying which flag')
check(attributionOf(recorded).track === 'T-0006', 'recorded against a track already declared: that track')
check(attributionOf(act(5, 0, { verdict: 'recorded', why: 'rule off' }, { candidates: [] })).track === null, 'recorded with no candidate: unattributed')

const names: Record<string, string> = { '1': 'HUNTER 1', '3': 'REAPER 1' }
const nameOf = (uid: string) => names[uid] ?? `vehicle ${uid}`
let hunter: Victim | null = { at: [-122.47, 37.81], alt: 0 }
const victimAt = () => hunter
const seen = new Map<string, Seen>()
// the console first sees act A-1 a second after it was observed
let ds = actDrawings([declared], seen, 101_000, victimAt, nameOf)
check(ds.length === 1 && ds[0].start === 101_000, 'it plays from when it is first seen')
check(ds[0].author[0] === -122.495 && ds[0].sigma_m === 42, 'the author where our sensors placed it, with its error')
check(ds[0].victim?.at[0] === -122.47, 'the victim where it was')
check(headline(ds[0]) === 'Act A-1: reported fire on HUNTER 1' && !ds[0].weapon, headline(ds[0]))
const shot = actDrawings([declared], new Map(), 101_000, victimAt, nameOf, new Set(['A-1']))[0]
check(shot.weapon && headline(shot) === 'Act A-1: launch at HUNTER 1', 'a weapon act: its launch, drawn as its weapon')
check(subline(ds[0]) === 'from T-0006 (0.97) · seen by HUNTER 1 EO/IR and sonar; REAPER 1 EO/IR', subline(ds[0]))
check(bannerLine(ds[0]) === 'T-0006 fired on HUNTER 1', bannerLine(ds[0]))
// the victim moves on: the act still ends where it was hit, and does not start again
hunter = { at: [-122.46, 37.80], alt: 0 }
ds = actDrawings([declared], seen, 105_000, victimAt, nameOf)
check(ds[0].victim?.at[0] === -122.47 && ds[0].start === 101_000, 'the victim and the start are kept from the first sight')
// a victim not found at first is looked for again while the act is fresh
hunter = null
const seen2 = new Map<string, Seen>()
check(actDrawings([declared], seen2, 101_000, victimAt, nameOf)[0].victim === null, 'no victim yet')
hunter = { at: [-122.45, 37.8], alt: 7600 }
check(actDrawings([declared], seen2, 102_000, victimAt, nameOf)[0].victim?.alt === 7600, 'found a moment later')
// a console opened two minutes after an act does not replay it
const late = actDrawings([declared], new Map(), 220_000, victimAt, nameOf)[0]
check(late.start === 100_000 && visibleActs([late], 220_000, null).length === 0, 'an old act is not drawn')

const fl = actDrawings([flagged], new Map(), 160_500, victimAt, nameOf)[0]
check(fl.authorTrack === null && subline(fl).startsWith('author unattributed (flag F-2) · seen by'), subline(fl))
check(bannerLine(fl) === 'Unattributed fire on HUNTER 1', bannerLine(fl))
const no = actDrawings([nobody], new Map(), 280_500, () => null, nameOf)[0]
check(no.victim === null && no.authorTrack === null, 'no victim position, unattributed: still drawn at the author')
const onTrack = actDrawings([act(6, 0, { verdict: 'recorded', why: '' }, { act: { ...declared.act, act: 'A-6', against: { kind: 'track', track: 'T-0003' } } })], new Map(), 0, victimAt, nameOf)[0]
check(headline(onTrack) === 'Act A-6: reported fire on protected track T-0003', headline(onTrack))

// ---- many acts: legible ----------------------------------------------------------------------
const many: ActDrawing[] = Array.from({ length: 12 }, (_, k) => ({ ...ds[0], key: `A-${k}`, act: `A-${k}`, start: 1_000_000 + k * 5000 }))
const now = 1_000_000 + 11 * 5000 + 500
const vis = visibleActs(many, now, null)
check(vis.length === MAX_DRAWN && vis[0].act === 'A-11', `the newest ${MAX_DRAWN}, newest first`)
const lab = labelledKeys(vis, now)
check(lab.size === MAX_LABELS && lab.has('A-11') && lab.has('A-10'), `full labels on the newest only: ${[...lab]}`)
check(!labelledKeys(vis, now + 20_000).size, 'none once every label has faded')
check(visibleActs(many, now, 1_000_000 + 10 * 5000).map((x) => x.act).join() === 'A-11', 'a dismissal takes off what started by then')
check(visibleActs(many, 1_000_000 - 1, null).length === 0, 'nothing before it starts')
const ban = bannerAct(many, now, null)!
check(ban.d.act === 'A-11' && ban.others === MAX_DRAWN - 1, 'the banner: the newest, and how many more are drawn')
check(bannerAct(many, now + BANNER_MS, null) === null && bannerAct(many, now + BANNER_MS - 1000, null) != null, 'the banner goes after BANNER_MS')
check(bannerAct(many, now, now) === null, 'and on a dismissal')

console.log('hostileAct: ok')
