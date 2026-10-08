// Where our own vehicles are drawn on the map (display smoothing, glide.ts). One shared
// glider, because everything drawn attached to a vehicle must read the same position: the
// symbol, rings, labels, drop line, trail end, order badges, the follow camera. Layers call
// drawnPosition / drawnAlt / drawnHeading instead of reading `v.position` and friends; with
// smoothing off these return the reported values, exactly as before.
//
// Only map drawing reads this. Lists, the details card, the API and the logs show the
// reported numbers.
import { type Context, Glider, type Pose } from './glide'
import type { LngLat, Vehicle } from './types'
import { domainOf, isStale, staleMs } from './types'

const glider = new Glider()
let enabled = true
let tick = 0

/** Smoothing on or off (MapView's toggle). Either way every glide is forgotten: the next
 *  report of each vehicle is drawn as it is. */
export function setSmoothing(on: boolean) {
  if (on === enabled) return
  enabled = on
  glider.clear()
  tick++
}

export function smoothing() {
  return enabled
}

/** Changes whenever drawn positions may have changed: use in layer updateTriggers. 0 while
 *  smoothing is off, so those layers behave exactly as before. */
export function drawnTick(): number {
  return enabled ? tick : 0
}

function contextOf(v: Vehicle): Context {
  const speed = Math.max(v.groundspeed ?? 0, v.airspeed ?? 0)
  return {
    // lost link, dead reckoning, or a coarse out-of-view snapshot: not a plain fix, so drawn as reported
    hold: isStale(v) || v.nav_source === 'dead_reckoning' || v.lod === 'lite',
    domain: domainOf(v),
    speed,
    maxGapMs: staleMs(v),
    // the antenna tracker's beam is drawn from its raw yaw: its arrow must not lag behind it
    snapHeading: v.kind === 'tracker',
  }
}

/** Feed the latest state (every vehicle, on every state update). `now` is performance.now(). */
export function observeVehicles(vehicles: Vehicle[], now: number) {
  if (!enabled) return
  const live = new Set<number>()
  for (const v of vehicles) {
    live.add(v.uid)
    if (!v.position) continue
    glider.observe(v.uid, { lon: v.position[0], lat: v.position[1], alt: v.alt_msl ?? 0, hdg: v.heading }, contextOf(v), now)
  }
  glider.prune(live)
}

/** Draw every vehicle at its last report now (tab hidden, socket reconnected). */
export function snapAllDrawn(now: number) {
  glider.snapAll(now)
  tick++
}

/** Step the glides to `now`; returns how many vehicles are still gliding. */
export function advanceDrawn(now: number): number {
  if (!enabled) return 0
  tick++
  return glider.advance(now)
}

/** how many vehicles are gliding now (the animation runs only while this is above zero) */
export function glidingCount(): number {
  return enabled ? glider.activeCount : 0
}

export function isGliding(uid: number): boolean {
  return enabled && glider.isGliding(uid)
}

/** the glide's current pose for a vehicle, or undefined when smoothing is off or it has none */
function poseOf(uid: number): Pose | undefined {
  return enabled ? glider.pose(uid) : undefined
}

/** Forget every glide (a MapView mounted: its predecessor's poses are of no use). */
export function resetDrawn() {
  glider.clear()
  tick++
}

/** The glide's pose for a vehicle (reused, not copied: do not keep it), undefined when smoothing
 *  is off or the vehicle has none. Allocation-free, for per-frame accessors. */
export function drawnPose(v: Vehicle): Pose | undefined {
  return poseOf(v.uid)
}

/** [lon, lat, alt x altScale] to draw, written into deck.gl's `target` array when it gives one
 *  (no allocation per vehicle per frame); altScale 0 for ground level. */
export function drawnXYZ(v: Vehicle, target: number[] | undefined, altScale: number): number[] {
  const out = target ?? [0, 0, 0]
  const p = poseOf(v.uid)
  out[0] = p ? p.lon : v.position![0]
  out[1] = p ? p.lat : v.position![1]
  out[2] = altScale === 0 ? 0 : (p ? p.alt : (v.alt_msl ?? 0)) * altScale
  return out
}

/** [lon, lat] to draw: the glide's position, else the report */
export function drawnPosition(v: Vehicle): LngLat {
  const p = poseOf(v.uid)
  return p ? [p.lon, p.lat] : v.position!
}

/** same by uid, for layers that keep only the id; null when there is no position */
export function drawnPositionOf(uid: number, fallback: LngLat | null): LngLat | null {
  const p = poseOf(uid)
  return p ? [p.lon, p.lat] : fallback
}

/** metres above mean sea level to draw */
export function drawnAlt(v: Vehicle): number {
  return poseOf(v.uid)?.alt ?? v.alt_msl ?? 0
}

/** degrees clockwise from north to draw (the shorter arc between reports) */
export function drawnHeading(v: Vehicle): number | null {
  const p = poseOf(v.uid)
  return p ? p.hdg : v.heading
}
