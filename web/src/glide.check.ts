// Checks for glide.ts: there is no test runner in web/, so run them with Node (type stripping):
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
check(glideMs(500) === 500, 'the interval itself')

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

// duration follows the interval: reports 400 ms apart glide in 400 ms
const h = new Glider()
h.observe(2, rep(0, 0), air, 0)
h.observe(2, rep(0.0001, 0), air, 400)
h.observe(2, rep(0.0002, 0), air, 800)
h.advance(800 + 399)
check(h.isGliding(2), 'still gliding just before the interval')
h.advance(800 + 401)
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

console.log('glide: ok')
