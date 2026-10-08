// Checks for glide.ts: there is no test runner in web/, so run them with Node 22.18 or later, which
// strips the types itself (make check does this):
//   node web/src/glide.check.ts
// Throws on the first failure, prints "glide: ok" otherwise. Not part of the bundle.
import { arcDelta, glideMs, Glider, implausibleHop, lerpHeading, MAX_GLIDE_MS, trailHeadPath, type Context, type Pt, type Report } from './glide.ts'

function check(cond: boolean, what: string) {
  if (!cond) throw new Error(`glide check failed: ${what}`)
}
const near = (a: number, b: number, eps = 1e-9) => Math.abs(a - b) <= eps
const rep = (lon: number, lat: number, alt = 0, hdg: number | null = 0): Report => ({ lon, lat, alt, hdg })
const air: Context = { hold: false, domain: 'air' }

// shorter arc
check(near(arcDelta(350, 10), 20), 'arc across north')
check(near(arcDelta(10, 350), -20), 'arc back across north')
check(near(lerpHeading(350, 10, 0.5), 0), 'midpoint across north')
check(near(lerpHeading(90, 90, 0.3), 90), 'no turn')

// duration: the measured interval, floored and capped
check(glideMs(null) === 250, 'default before a measure')
check(glideMs(5000) === MAX_GLIDE_MS, 'capped at a second')
check(glideMs(20) === 150, 'floored')
check(glideMs(1000) === 1000, 'a 1 Hz vehicle glides a second')
check(glideMs(500) === 600, 'a little longer than the interval, for jitter')
check(glideMs(900) === MAX_GLIDE_MS, 'but never past the cap')

// plausibility
check(!implausibleHop(115, 500, 'air', 230), 'a jet half a second of flight is plausible')
check(implausibleHop(20_000, 500, 'air'), '20 km in 0.5 s is not')
check(implausibleHop(2_000, 1000, 'ground'), 'a car 2 km in 1 s is not')

// first fix snaps; later reports glide from the drawn position, never past the report
const g = new Glider()
g.observe(1, rep(-122.0, 37.0), air, 0)
check(g.activeCount === 0 && g.pose(1)!.lon === -122.0, 'first fix is drawn at once')
g.observe(1, rep(-122.001, 37.0), air, 500) // about 89 m at 37 N
check(g.isGliding(1), 'a changed report starts a glide')
g.advance(500 + 100)
const mid = g.pose(1)!.lon
check(mid < -122.0 && mid > -122.001, 'part way during the glide')
g.advance(500 + 5000)
check(g.pose(1)!.lon === -122.001 && g.activeCount === 0, 'rests exactly at the report, and idle')
g.advance(500 + 60_000)
check(g.pose(1)!.lon === -122.001, 'a silent vehicle stays at its last report (no extrapolation)')

// a retarget mid-glide starts from where it is drawn, not from the old report
g.observe(1, rep(-122.002, 37.0), air, 10_000)
g.advance(10_100)
const before = g.pose(1)!.lon
g.observe(1, rep(-122.003, 37.0), air, 10_100)
check(near(g.pose(1)!.lon, before), 'no jump at the retarget')

// duration follows the interval (x 1.2): reports 400 ms apart glide in 480 ms
const h = new Glider()
h.observe(2, rep(0, 0), air, 0)
h.observe(2, rep(0.0001, 0), air, 400)
h.observe(2, rep(0.0002, 0), air, 800)
h.advance(800 + 399)
check(h.isGliding(2), 'still gliding just before the interval')
h.advance(800 + 481)
check(!h.isGliding(2), 'done after the measured interval')

// snaps: implausible jump, lost link, dead reckoning, and the first fix after it
const s = new Glider()
s.observe(3, rep(0, 0), air, 0)
s.observe(3, rep(1, 1), air, 500)
check(!s.isGliding(3) && s.pose(3)!.lon === 1, 'implausible jump snaps')
s.observe(3, rep(1.0001, 1), { ...air, hold: true }, 1000)
check(!s.isGliding(3) && s.pose(3)!.lon === 1.0001, 'a held vehicle is drawn as reported')
s.observe(3, rep(1.0002, 1), air, 1500)
check(!s.isGliding(3) && s.pose(3)!.lon === 1.0002, 'the first fix after a hold snaps')
s.observe(3, rep(1.0003, 1), air, 2000)
check(s.isGliding(3), 'then it glides again')
s.snapAll(2100)
check(!s.isGliding(3) && s.pose(3)!.lon === 1.0003, 'snapAll draws the reports')

// altitude and heading glide
const a = new Glider()
a.observe(4, rep(0, 0, 100, 350), air, 0)
a.observe(4, rep(0.0001, 0, 120, 10), air, 500)
a.advance(500 + 250)
const p = a.pose(4)!
check(p.alt > 100 && p.alt < 120, 'altitude glides')
check(p.hdg! > 350 || p.hdg! < 10, 'heading takes the short way round')
a.advance(500 + 2000)
check(a.pose(4)!.alt === 120 && a.pose(4)!.hdg === 10, 'ends on the report')

// trail head: ends at the symbol, drops trail points the symbol has not reached
const tail: Pt[] = [
  [0, 0, 0],
  [0.001, 0, 0],
  [0.002, 0, 0],
]
const reported: Pt = [0.003, 0, 0]
const mid1 = trailHeadPath(tail, reported, [0.0015, 0, 0])
check(mid1.length === 3 && mid1[mid1.length - 1][0] === 0.0015 && mid1[1][0] === 0.001, 'the head stops at the symbol, not at the report')
const rest = trailHeadPath(tail, reported, reported)
check(rest.length === 4 && rest[3][0] === 0.003, 'a symbol at rest on the report: the whole trail')
const dup = trailHeadPath([...tail, reported], reported, reported)
check(dup[dup.length - 1][0] === 0.003, 'a trail whose last point is the report')

// jitter: a 1 Hz vehicle reporting 250 ms late finds the symbol still moving
const j = new Glider()
j.observe(5, rep(0, 0), air, 0)
j.observe(5, rep(0.0001, 0), air, 1000)
j.observe(5, rep(0.0002, 0), air, 2000)
check(j.isGliding(5), 'glide starts')
j.advance(2000 + 1000)
check(!j.isGliding(5), 'ends within the one second cap')
const late = new Glider()
late.observe(6, rep(0, 0), air, 0)
late.observe(6, rep(0.0001, 0), air, 500)
late.observe(6, rep(0.0002, 0), air, 1000)
late.advance(1000 + 550)
check(late.isGliding(6), 'a report 550 ms after a 500 ms interval finds it still gliding (no stop and go)')

// a hold without a move snaps a glide in flight
const hd = new Glider()
hd.observe(7, rep(0, 0), air, 0)
hd.observe(7, rep(0.0001, 0), air, 500)
hd.observe(7, rep(0.0001, 0), { ...air, hold: true }, 600)
check(!hd.isGliding(7) && hd.pose(7)!.lon === 0.0001, 'going stale mid-glide draws the report at once')

// prune forgets vehicles that are gone
const pr = new Glider()
pr.observe(8, rep(0, 0), air, 0)
pr.observe(9, rep(0, 0), air, 0)
pr.observe(8, rep(0.0001, 0), air, 500)
pr.prune(new Set([9]))
check(!pr.has(8) && pr.has(9) && pr.activeCount === 0, 'prune drops the gone and their glides')

// heading with null at either end: no glide, no NaN
const hn = new Glider()
hn.observe(10, rep(0, 0, 0, null), air, 0)
hn.observe(10, rep(0, 0, 0, 90), air, 100)
check(hn.pose(10)!.hdg === 90 && !hn.isGliding(10), 'null to a heading: drawn at once')
hn.observe(10, rep(0, 0, 0, null), air, 200)
check(hn.pose(10)!.hdg === null, 'a heading to null: null')
hn.observe(10, rep(0, 0, 0, 10), air, 300)
hn.observe(10, rep(0, 0, 0, 20), air, 400)
hn.advance(400 + 50)
check(hn.pose(10)!.hdg! > 10 && hn.pose(10)!.hdg! < 20, 'then it turns normally')

// a tracker's heading is drawn as reported
const tr = new Glider()
tr.observe(11, rep(0, 0, 0, 0), { ...air, snapHeading: true }, 0)
tr.observe(11, rep(0, 0, 0, 40), { ...air, snapHeading: true }, 100)
check(tr.pose(11)!.hdg === 40 && !tr.isGliding(11), 'snapHeading')

// advance stamped before the observe time (rAF time is the frame start): the start of the glide
const early = new Glider()
early.observe(12, rep(0, 0), air, 1000)
early.observe(12, rep(0.0001, 0), air, 1500)
early.advance(1490)
check(early.pose(12)!.lon === 0, 'a frame stamped before the report leaves the symbol at the start')
check(early.isGliding(12), 'and the glide goes on')

// a parked vehicle that keeps reporting the same fix earns no long allowance
const ground: Context = { hold: false, domain: 'ground', maxGapMs: 3000 }
const park = new Glider()
park.observe(13, rep(-122, 37), ground, 0)
for (let t = 1000; t <= 600_000; t += 1000) park.observe(13, rep(-122, 37), ground, t)
park.observe(13, rep(-122, 37.027), ground, 601_000) // about 3 km
check(!park.isGliding(13) && park.pose(13)!.lat === 37.027, 'parked 10 min then 3 km: snaps')
const jet = new Glider()
const airMax: Context = { hold: false, domain: 'air', maxGapMs: 3000 }
jet.observe(14, rep(-122, 37), airMax, 0)
jet.observe(14, rep(-122, 37), airMax, 600_000)
jet.observe(14, rep(-122, 37.28), airMax, 601_000) // about 31 km
check(!jet.isGliding(14), 'a jet 31 km after sitting still: snaps')

// trail head across a U-turn: the return leg a metre from the outbound leg keeps the turn
const out: Pt[] = [
  [0, 0, 0],
  [0.001, 0, 0],
  [0.002, 0, 0],
  [0.002, 0.00001, 0], // turns back, about a metre north
]
const back: Pt = [0.001, 0.00001, 0]
const uturn = trailHeadPath(out, back, [0.0012, 0.00001, 0])
check(uturn.length === 5 && uturn[3][0] === 0.002, 'the head keeps the turn point (is on the return leg)')

// a slow boat: trail points 2 m apart, the symbol at 5 m: the head must not run on to 6 or 8 m
const m2 = 2 / 111_320
const boat: Pt[] = [
  [0, 0, 0],
  [m2, 0, 0],
  [2 * m2, 0, 0],
  [3 * m2, 0, 0],
]
const slow = trailHeadPath(boat, [4 * m2, 0, 0], [2.5 * m2, 0, 0])
check(slow.length === 4 && slow[slow.length - 1][0] === 2.5 * m2 && slow[slow.length - 2][0] === 2 * m2, 'the slow boat head ends at the symbol, 0 2 4 5')
// the U-turn keeps its turn at 0.5, 0.9 and 1.5 m between the legs
for (const gap of [0.5, 0.9, 1.5]) {
  const dy = gap / 111_320
  const turn = trailHeadPath(
    [
      [0, 0, 0],
      [0.001, 0, 0],
      [0.002, 0, 0],
      [0.002, dy, 0],
    ],
    [0.001, dy, 0],
    [0.0012, dy, 0],
  )
  check(turn.length === 5 && turn[3][0] === 0.002, `the U-turn keeps its turn at ${gap} m`)
}

console.log('glide: ok')
