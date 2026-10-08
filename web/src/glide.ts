// Display smoothing for our own vehicles on the map. Pure functions and one small class, no DOM
// and no framework, so they can be checked on their own (glide.check.ts).
//
// A vehicle reports its position every 0.5 s to 20 s; the map used to draw it exactly where it
// was last heard, so movers hopped. Here the drawn position glides from where it is drawn now
// to each new report, over that vehicle's measured interval between reports (capped at
// MAX_GLIDE_MS), then rests. It is interpolation only: the drawn position is never past the
// last report, and a vehicle that goes silent stays at its last reported position. Numbers
// (lists, details, API, logs) are never touched; only drawing reads this.
//
// Snap (draw the report at once, no glide) on: a first fix, a jump implausible for the
// vehicle, a vehicle whose position is not a plain fresh fix (lost link, dead reckoning, a
// coarse out-of-view snapshot) and its first fix after that, and `snapAll` (tab was hidden,
// socket reconnected).

export const MAX_GLIDE_MS = 1000
export const MIN_GLIDE_MS = 150
/** glide used before an interval has been measured (a second report is the first measure) */
export const DEFAULT_GLIDE_MS = 250

export type Domain = 'air' | 'ground' | 'surface' | 'subsurface'

/** generous top speeds, m/s, for the plausibility check on a jump */
export const TOP_SPEED_MPS: Record<Domain, number> = { air: 350, ground: 40, surface: 45, subsurface: 12 }

/** What a vehicle reported: degrees, metres above mean sea level, degrees clockwise from north */
export interface Report {
  lon: number
  lat: number
  alt: number
  hdg: number | null
}

/** What is drawn now (same fields as a report) */
export type Pose = Report

export interface Context {
  /** the position is not a plain fresh fix (link lost, dead reckoning, coarse snapshot):
   *  draw it as reported, at once */
  hold: boolean
  domain: Domain
  /** reported ground or air speed, m/s, when known */
  speed?: number | null
}

/** shortest signed turn from a to b, degrees in (-180, 180] */
export function arcDelta(a: number, b: number): number {
  const d = (((b - a) % 360) + 540) % 360 - 180
  return d === -180 ? 180 : d
}

/** heading a + (b - a) * f along the shorter arc, in [0, 360) */
export function lerpHeading(a: number, b: number, f: number): number {
  return (((a + arcDelta(a, b) * f) % 360) + 360) % 360
}

/** flat-earth distance in metres (3D when altitudes differ), good for the short hops compared here */
export function hopM(a: Report, b: Report): number {
  const dLat = (b.lat - a.lat) * 111_320
  const dLon = (b.lon - a.lon) * 111_320 * Math.cos((((a.lat + b.lat) / 2) * Math.PI) / 180)
  const dAlt = b.alt - a.alt
  return Math.hypot(dLat, dLon, dAlt)
}

/** glide time for a measured interval between reports */
export function glideMs(intervalMs: number | null): number {
  if (intervalMs == null || !(intervalMs > 0)) return DEFAULT_GLIDE_MS
  return Math.min(MAX_GLIDE_MS, Math.max(MIN_GLIDE_MS, intervalMs))
}

/** Is a hop from the last report to this one beyond what the vehicle could have flown in the
 *  time between them (with a wide margin)? Then it is not motion, it is a different fix. */
export function implausibleHop(distM: number, elapsedMs: number, domain: Domain, speed?: number | null): boolean {
  const top = Math.max(TOP_SPEED_MPS[domain], 3 * (speed ?? 0))
  const s = Math.min(Math.max(elapsedMs, 500), 60_000) / 1000
  return distM > 1.5 * top * s + 30
}

interface Track {
  from: Report
  to: Report
  t0: number
  dur: number
  hFrom: number | null
  hTo: number | null
  hT0: number
  hDur: number
  /** the pose now: written by observe and advance, read by the drawing accessors */
  cur: Pose
  /** browser time of the last position change, and the smoothed gap between them (ms) */
  lastAt: number
  gap: number | null
  hLastAt: number
  hGap: number | null
  held: boolean
  gliding: boolean
}

function ema(prev: number | null, sample: number): number {
  const s = Math.min(sample, 5000)
  return prev == null ? s : prev * 0.7 + s * 0.3
}

export class Glider {
  private tracks = new Map<number, Track>()
  /** uids whose drawn pose differs from their report right now */
  private moving = new Set<number>()

  /** vehicles being glided: animation is needed while this is above zero */
  get activeCount(): number {
    return this.moving.size
  }

  /** The drawn pose, which is the last report once a glide has finished. Undefined before
   *  the first report. The object is reused: copy what you keep. */
  pose(uid: number): Pose | undefined {
    return this.tracks.get(uid)?.cur
  }

  has(uid: number): boolean {
    return this.tracks.has(uid)
  }

  isGliding(uid: number): boolean {
    return this.moving.has(uid)
  }

  /** Feed one vehicle's latest report (call for every vehicle on every state update: only a
   *  changed report starts a glide). `now` is browser time in ms. */
  observe(uid: number, rep: Report, ctx: Context, now: number): void {
    let t = this.tracks.get(uid)
    if (!t) {
      t = {
        from: rep,
        to: rep,
        t0: now,
        dur: 0,
        hFrom: rep.hdg,
        hTo: rep.hdg,
        hT0: now,
        hDur: 0,
        cur: { ...rep },
        lastAt: now,
        gap: null,
        hLastAt: now,
        hGap: null,
        held: ctx.hold,
        gliding: false,
      }
      this.tracks.set(uid, t)
      return
    }
    const moved = rep.lon !== t.to.lon || rep.lat !== t.to.lat || rep.alt !== t.to.alt
    const turned = rep.hdg !== t.hTo
    if (!moved && !turned) {
      if (ctx.hold && !t.held) this.snap(uid, t, now)
      t.held = ctx.hold
      return
    }
    // a vehicle whose position is not a plain fix is drawn as reported, and so is the first
    // plain fix after it (gliding there would hide that the state changed)
    if (ctx.hold || t.held) {
      t.held = ctx.hold
      t.lastAt = now
      t.gap = null
      this.jump(uid, t, rep)
      return
    }
    if (moved) {
      const elapsed = now - t.lastAt
      if (implausibleHop(hopM(t.to, rep), elapsed, ctx.domain, ctx.speed)) {
        t.lastAt = now
        t.gap = null
        this.jump(uid, t, rep)
        return
      }
      this.pose_(t, now)
      t.gap = ema(t.gap, elapsed)
      t.lastAt = now
      t.from = { ...t.cur }
      t.to = rep
      t.t0 = now
      t.dur = glideMs(t.gap)
      this.moving.add(uid)
      t.gliding = true
    }
    if (turned) {
      if (t.hTo == null || rep.hdg == null) {
        t.hFrom = t.hTo = rep.hdg
        t.hDur = 0
        t.cur.hdg = rep.hdg
      } else {
        const elapsed = now - t.hLastAt
        this.pose_(t, now)
        t.hGap = ema(t.hGap, elapsed)
        t.hFrom = t.cur.hdg ?? t.hTo
        t.hTo = rep.hdg
        t.hT0 = now
        t.hDur = glideMs(t.hGap)
        this.moving.add(uid)
        t.gliding = true
      }
      t.hLastAt = now
    }
    if (!moved) t.to = { ...t.to, hdg: rep.hdg }
    else t.to = rep
  }

  /** Step every glide to `now`; returns how many are still under way. */
  advance(now: number): number {
    for (const uid of this.moving) {
      const t = this.tracks.get(uid)
      if (!t) {
        this.moving.delete(uid)
        continue
      }
      this.pose_(t, now)
      if (now >= t.t0 + t.dur && now >= t.hT0 + t.hDur) {
        this.moving.delete(uid)
        t.gliding = false
      }
    }
    return this.moving.size
  }

  /** Forget everything: the next report of every vehicle is a first fix. */
  clear(): void {
    this.tracks.clear()
    this.moving.clear()
  }

  /** Draw every vehicle at its last report (tab was hidden, socket reconnected). */
  snapAll(now: number): void {
    for (const [uid, t] of this.tracks) this.snap(uid, t, now)
  }

  /** Forget vehicles that are gone. */
  prune(live: Set<number>): void {
    for (const uid of this.tracks.keys()) {
      if (!live.has(uid)) {
        this.tracks.delete(uid)
        this.moving.delete(uid)
      }
    }
  }

  private snap(uid: number, t: Track, now: number) {
    t.from = t.to
    t.t0 = now
    t.dur = 0
    t.hFrom = t.hTo
    t.hT0 = now
    t.hDur = 0
    t.cur = { ...t.to, hdg: t.hTo }
    t.gliding = false
    this.moving.delete(uid)
  }

  private jump(uid: number, t: Track, rep: Report) {
    t.from = t.to = rep
    t.dur = 0
    t.hFrom = t.hTo = rep.hdg
    t.hDur = 0
    t.cur = { ...rep }
    t.gliding = false
    this.moving.delete(uid)
  }

  /** write the pose at `now` into t.cur */
  private pose_(t: Track, now: number) {
    const f = t.dur > 0 ? Math.min(1, Math.max(0, (now - t.t0) / t.dur)) : 1
    const c = t.cur
    c.lon = t.from.lon + (t.to.lon - t.from.lon) * f
    c.lat = t.from.lat + (t.to.lat - t.from.lat) * f
    c.alt = t.from.alt + (t.to.alt - t.from.alt) * f
    if (t.hFrom != null && t.hTo != null) {
      const g = t.hDur > 0 ? Math.min(1, Math.max(0, (now - t.hT0) / t.hDur)) : 1
      c.hdg = g >= 1 ? t.hTo : lerpHeading(t.hFrom, t.hTo, g)
    } else c.hdg = t.hTo
  }
}

export type Pt = [number, number, number]

/** The newest end of a trail, ending at the drawn position. `tail` is the last few trail
 *  points, `report` the vehicle's last reported position and `drawn` where its symbol is. The
 *  trail may hold points the symbol has not reached yet (it is drawn up to a second behind the
 *  report): the path keeps the points up to the segment the symbol is on and ends at the
 *  symbol, so the trail meets the symbol and never runs past it. */
export function trailHeadPath(tail: Pt[], report: Pt, drawn: Pt): Pt[] {
  const pts = [...tail, report]
  const k = Math.cos((drawn[1] * Math.PI) / 180)
  const px = drawn[0] * k
  const py = drawn[1]
  let best = -1
  let bestD = Infinity
  for (let i = 0; i + 1 < pts.length; i++) {
    const ax = pts[i][0] * k
    const ay = pts[i][1]
    const dx = pts[i + 1][0] * k - ax
    const dy = pts[i + 1][1] - ay
    const len2 = dx * dx + dy * dy
    const f = len2 > 0 ? Math.min(1, Math.max(0, ((px - ax) * dx + (py - ay) * dy) / len2)) : 0
    const d = Math.hypot(px - (ax + dx * f), py - (ay + dy * f))
    // later segments win ties: a symbol at rest on the report is on the last one
    if (d <= bestD + 1e-12) {
      bestD = d
      best = i
    }
  }
  if (best < 0) return [drawn]
  return [...pts.slice(0, best + 1), drawn]
}
