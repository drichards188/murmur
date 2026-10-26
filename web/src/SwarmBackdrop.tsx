import { useEffect, useRef } from 'react'

/** The main menu's backdrop: a strait in cross-section (the sky, a headland each side, the water
 *  and the seabed) with about a hundred nodes in it, each moving as its domain does: aircraft
 *  flocking, ground vehicles along the ridges, boats on the waterline, submarines in the water
 *  column. Neighbours link as radio and sound would carry (radio above the water, sound under it,
 *  nothing between the air and the deep), and now and then a radar sweeps the sky or a sonar pings.
 *
 *  Decoration only: none of it is a vehicle, a link or a figure of any run, and it says no number.
 *  One canvas, at most 30 frames a second, nothing while the tab is hidden, one still frame under
 *  prefers-reduced-motion. Its colours are the menu's domain colours (menu.css). */

type Dom = 'air' | 'ground' | 'surface' | 'subsurface'
interface Node {
  d: Dom
  x: number
  y: number
  vx: number
  vy: number
  /** a phase for its own slow oscillation */
  ph: number
  /** lit by a sweep: 1, fading to 0 */
  lit: number
}
interface Geo {
  w: number
  h: number
  /** the waterline (y, CSS px) */
  sea: number
  /** the solid surface (ridge or seabed) every STEP px across */
  ground: Float32Array
  water: Path2D
  sky: Path2D
  /** the two headlands' summits (radar sites) */
  summits: [number, number][]
}
interface Sweep {
  kind: 'radar' | 'sonar'
  t0: number
  x: number
  y: number
}

const COUNTS: Record<Dom, number> = { air: 46, ground: 12, surface: 24, subsurface: 16 }
const STEP = 4
const FRAME_MS = 1000 / 30
const SWEEP_EVERY_S = 6.5
const RADAR_S = 2.6
const SONAR_S = 3.2

/** a small seeded generator, so the backdrop opens the same each time */
function rng(seed: number) {
  let a = seed >>> 0
  return () => {
    a = (a + 0x6d2b79f5) >>> 0
    let t = a
    t = Math.imul(t ^ (t >>> 15), t | 1)
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const smooth = (a: number, b: number, u: number) => {
  const t = Math.min(1, Math.max(0, (u - a) / (b - a)))
  return t * t * (3 - 2 * t)
}

function geometry(w: number, h: number): Geo {
  const sea = Math.round(h * 0.68)
  const n = Math.ceil(w / STEP) + 2
  const ground = new Float32Array(n)
  for (let i = 0; i < n; i++) {
    const u = (i * STEP) / w
    // a headland at each edge, the channel between them
    const land = Math.max(smooth(0.17, 0.06, u), smooth(0.83, 0.95, u))
    const hills = h * (0.075 + 0.03 * Math.sin(u * 23 + 1.3) + 0.016 * Math.sin(u * 71))
    const depth = h * (0.17 + 0.03 * Math.sin(u * 13 + 0.4) + 0.012 * Math.sin(u * 53 + 2))
    ground[i] = sea + depth * (1 - land) - hills * land
  }
  const water = new Path2D()
  water.moveTo(0, sea)
  water.lineTo(w, sea)
  for (let i = n - 1; i >= 0; i--) water.lineTo(i * STEP, Math.max(sea, ground[i]))
  water.closePath()
  const sky = new Path2D()
  sky.moveTo(0, 0)
  sky.lineTo(w, 0)
  for (let i = n - 1; i >= 0; i--) sky.lineTo(i * STEP, Math.min(sea, ground[i]))
  sky.closePath()
  const summit = (from: number, to: number): [number, number] => {
    let best = Math.max(0, Math.floor(from / STEP))
    for (let i = best; i < Math.min(n, to / STEP); i++) if (ground[i] < ground[best]) best = i
    return [best * STEP, ground[best]]
  }
  return { w, h, sea, ground, water, sky, summits: [summit(0, w * 0.16), summit(w * 0.84, w)] }
}

const groundAt = (g: Geo, x: number) => g.ground[Math.max(0, Math.min(g.ground.length - 1, Math.round(x / STEP)))]
const onLand = (g: Geo, x: number) => x > 2 && x < g.w - 2 && groundAt(g, x) < g.sea - 6
const inWater = (g: Geo, x: number, deep = 0) => groundAt(g, x) > g.sea + 10 + deep
const airTop = (g: Geo) => g.h * 0.07
const airBottom = (g: Geo, x: number) => Math.min(g.sea, groundAt(g, x)) - g.h * 0.08

/** a place for a node of its domain, or false if it has none (a narrow window) */
function place(n: Node, g: Geo, r: () => number): boolean {
  for (let k = 0; k < 60; k++) {
    const x = r() * g.w
    if (n.d === 'air') {
      n.x = x
      n.y = airTop(g) + r() * Math.max(10, airBottom(g, x) - airTop(g))
      return true
    }
    if (n.d === 'ground' && onLand(g, x)) {
      n.x = x
      n.y = groundAt(g, x) - 2
      return true
    }
    if (n.d === 'surface' && inWater(g, x)) {
      n.x = x
      n.y = g.sea
      return true
    }
    if (n.d === 'subsurface' && inWater(g, x, 50)) {
      n.x = x
      n.y = g.sea + 20 + r() * (groundAt(g, x) - g.sea - 40)
      return true
    }
  }
  return false
}

function seed(g: Geo): Node[] {
  const r = rng(7)
  const nodes: Node[] = []
  // all of them from a laptop's screen up; fewer on a small one, where they would crowd the words
  const share = Math.min(1, Math.max(0.35, (g.w * g.h) / (1280 * 720)))
  for (const d of Object.keys(COUNTS) as Dom[]) {
    for (let i = 0; i < Math.round(COUNTS[d] * share); i++) {
      const sp = d === 'air' ? 26 : d === 'ground' ? 9 : d === 'surface' ? 10 : 6
      const dir = r() < 0.5 ? -1 : 1
      const n: Node = { d, x: 0, y: 0, vx: dir * sp * (0.6 + r() * 0.6), vy: d === 'air' ? (r() - 0.5) * 10 : 0, ph: r() * Math.PI * 2, lit: 0 }
      if (place(n, g, r)) nodes.push(n)
    }
  }
  return nodes
}

/** one step of dt seconds */
function step(nodes: Node[], g: Geo, dt: number) {
  const air = nodes.filter((n) => n.d === 'air')
  for (const a of air) {
    // flocking: keep apart, fly with the neighbours, close on them; drift east; stay in the sky
    let sx = 0, sy = 0, ax = 0, ay = 0, cx = 0, cy = 0, k = 0
    for (const b of air) {
      if (a === b) continue
      const dx = b.x - a.x, dy = b.y - a.y
      const d2 = dx * dx + dy * dy
      if (d2 > 80 * 80) continue
      k++
      ax += b.vx
      ay += b.vy
      cx += dx
      cy += dy
      if (d2 < 30 * 30) {
        const f = 1 / Math.max(16, d2)
        sx -= dx * f
        sy -= dy * f
      }
    }
    let fx = 3, fy = Math.sin(a.ph) * 2
    if (k) {
      fx += (ax / k - a.vx) * 0.9 + (cx / k) * 0.16
      fy += (ay / k - a.vy) * 0.9 + (cy / k) * 0.16
    }
    fx += sx * 900
    fy += sy * 900
    const top = airTop(g), bottom = airBottom(g, a.x)
    if (a.y < top + 24) fy += (top + 24 - a.y) * 2
    if (a.y > bottom - 24) fy -= (a.y - bottom + 24) * 2
    a.vx += fx * dt
    a.vy += fy * dt
    const sp = Math.hypot(a.vx, a.vy)
    const lim = Math.min(44, Math.max(18, sp))
    a.vx *= lim / sp
    a.vy *= lim / sp
    a.x += a.vx * dt
    a.y += a.vy * dt
    a.ph += dt * 0.4
    if (a.x > g.w + 20) a.x = -20
    if (a.x < -20) a.x = g.w + 20
  }
  for (const n of nodes) {
    n.lit = Math.max(0, n.lit - dt * 0.9)
    if (n.d === 'air') continue
    n.ph += dt * 0.5
    const nx = n.x + n.vx * dt
    if (n.d === 'ground') {
      if (!onLand(g, nx)) n.vx = -n.vx
      else n.x = nx
      n.y = groundAt(g, n.x) - 2
    } else if (n.d === 'surface') {
      if (!inWater(g, nx)) n.vx = -n.vx
      else n.x = nx
      n.y = g.sea + Math.sin(n.ph * 2) * 0.8
    } else {
      if (!inWater(g, nx, 50)) n.vx = -n.vx
      else n.x = nx
      const floor = groundAt(g, n.x)
      const target = g.sea + 20 + (floor - g.sea - 40) * (0.5 + 0.42 * Math.sin(n.ph * 0.35))
      n.y += (target - n.y) * Math.min(1, dt * 0.5)
    }
  }
}

interface Palette {
  air: string
  ground: string
  surface: string
  subsurface: string
  line: string
  terrain: string
  link: string
}

function palette(el: Element): Palette {
  const css = getComputedStyle(el)
  const v = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback
  return {
    air: v('--dom-air', '#7fb8ff'),
    ground: v('--dom-ground', '#c4b483'),
    surface: v('--dom-surface', '#6fe0c4'),
    subsurface: v('--dom-subsurface', '#9b8cff'),
    line: v('--line', '#233040'),
    terrain: v('--mm-terrain', '#0e151c'),
    link: v('--mm-link', '#8aa2bd'),
  }
}

/** what does not move, drawn once a size */
function drawStatic(ctx: CanvasRenderingContext2D, g: Geo, p: Palette) {
  ctx.clearRect(0, 0, g.w, g.h)
  // the sky's height bands and the water's depth lines, faint
  ctx.save()
  ctx.clip(g.sky)
  ctx.strokeStyle = p.line
  ctx.globalAlpha = 0.35
  ctx.setLineDash([1, 7])
  ctx.beginPath()
  for (let y = g.sea - 64; y > 0; y -= 64) {
    ctx.moveTo(0, y + 0.5)
    ctx.lineTo(g.w, y + 0.5)
  }
  ctx.stroke()
  ctx.restore()
  ctx.save()
  ctx.clip(g.water)
  const deep = ctx.createLinearGradient(0, g.sea, 0, g.h)
  deep.addColorStop(0, 'rgba(111, 224, 196, 0.05)')
  deep.addColorStop(1, 'rgba(155, 140, 255, 0.04)')
  ctx.fillStyle = deep
  ctx.fillRect(0, g.sea, g.w, g.h - g.sea)
  ctx.strokeStyle = p.surface
  ctx.globalAlpha = 0.09
  ctx.setLineDash([6, 10])
  ctx.beginPath()
  for (let y = g.sea + 28; y < g.h; y += 28) {
    ctx.moveTo(0, y + 0.5)
    ctx.lineTo(g.w, y + 0.5)
  }
  ctx.stroke()
  ctx.restore()
  // the land and the seabed, engraved with a few contours under the surface
  const n = g.ground.length
  ctx.beginPath()
  ctx.moveTo(0, g.h)
  for (let i = 0; i < n; i++) ctx.lineTo(i * STEP, g.ground[i])
  ctx.lineTo(g.w, g.h)
  ctx.closePath()
  ctx.fillStyle = p.terrain
  ctx.fill()
  ctx.setLineDash([])
  for (let c = 0; c < 4; c++) {
    ctx.beginPath()
    for (let i = 0; i < n; i++) {
      const y = g.ground[i] + c * 7
      if (i) ctx.lineTo(i * STEP, y)
      else ctx.moveTo(0, y)
    }
    ctx.strokeStyle = c ? p.line : '#3a4f66'
    ctx.globalAlpha = c ? 0.7 / c : 0.9
    ctx.lineWidth = 1
    ctx.stroke()
  }
  // the waterline
  ctx.save()
  ctx.clip(g.water)
  ctx.globalAlpha = 0.45
  ctx.strokeStyle = p.surface
  ctx.beginPath()
  ctx.moveTo(0, g.sea + 0.5)
  ctx.lineTo(g.w, g.sea + 0.5)
  ctx.stroke()
  ctx.restore()
  ctx.globalAlpha = 1
}

function drawLinks(ctx: CanvasRenderingContext2D, nodes: Node[], p: Palette) {
  // radio: in five strengths, one path each; sound (under the water): dashed
  const radio: Path2D[] = [new Path2D(), new Path2D(), new Path2D(), new Path2D(), new Path2D()]
  const sound = new Path2D()
  for (let i = 0; i < nodes.length; i++) {
    const a = nodes[i]
    for (let j = i + 1; j < nodes.length; j++) {
      const b = nodes[j]
      const subA = a.d === 'subsurface', subB = b.d === 'subsurface'
      let range: number
      if (subA || subB) {
        // sound carries between the deep and the deep or the surface; never to the air or ashore
        const other = subA ? b : a
        if (other.d !== 'subsurface' && other.d !== 'surface') continue
        range = other.d === 'surface' && !(subA && subB) ? 150 : 120
      } else range = a.d === 'air' && b.d === 'air' ? 84 : 130
      const dx = a.x - b.x, dy = a.y - b.y
      const d2 = dx * dx + dy * dy
      if (d2 > range * range) continue
      if (subA || subB) {
        sound.moveTo(a.x, a.y)
        sound.lineTo(b.x, b.y)
      } else {
        const s = 1 - Math.sqrt(d2) / range
        const path = radio[Math.min(4, Math.floor(s * 5))]
        path.moveTo(a.x, a.y)
        path.lineTo(b.x, b.y)
      }
    }
  }
  ctx.lineWidth = 1
  ctx.strokeStyle = p.link
  radio.forEach((path, k) => {
    ctx.globalAlpha = 0.05 + k * 0.07
    ctx.stroke(path)
  })
  ctx.setLineDash([2, 4])
  ctx.strokeStyle = p.subsurface
  ctx.globalAlpha = 0.32
  ctx.stroke(sound)
  ctx.setLineDash([])
}

function drawNodes(ctx: CanvasRenderingContext2D, nodes: Node[], p: Palette) {
  for (const n of nodes) {
    const c = p[n.d]
    ctx.fillStyle = c
    ctx.strokeStyle = c
    ctx.globalAlpha = 0.72 + 0.28 * n.lit
    if (n.d === 'air') {
      // a chevron along its heading
      const h = Math.atan2(n.vy, n.vx)
      const cos = Math.cos(h), sin = Math.sin(h)
      ctx.beginPath()
      ctx.moveTo(n.x + cos * 5, n.y + sin * 5)
      ctx.lineTo(n.x - cos * 3 - sin * 2.8, n.y - sin * 3 + cos * 2.8)
      ctx.lineTo(n.x - cos * 1.4, n.y - sin * 1.4)
      ctx.lineTo(n.x - cos * 3 + sin * 2.8, n.y - sin * 3 - cos * 2.8)
      ctx.closePath()
      ctx.fill()
    } else if (n.d === 'ground') {
      ctx.fillRect(n.x - 2.5, n.y - 4.5, 5, 4)
    } else if (n.d === 'surface') {
      const dir = Math.sign(n.vx) || 1
      ctx.globalAlpha = 0.25
      ctx.beginPath()
      ctx.moveTo(n.x - dir * 4, n.y + 0.5)
      ctx.lineTo(n.x - dir * 16, n.y + 0.5)
      ctx.stroke()
      ctx.globalAlpha = 0.72 + 0.28 * n.lit
      ctx.fillRect(n.x - 4, n.y - 2.5, 8, 2.5)
    } else {
      ctx.beginPath()
      ctx.ellipse(n.x, n.y, 4, 1.6, 0, 0, Math.PI * 2)
      ctx.fill()
    }
    if (n.lit > 0.02) {
      ctx.globalAlpha = n.lit * 0.8
      ctx.lineWidth = 1
      ctx.beginPath()
      ctx.arc(n.x, n.y, 6 + (1 - n.lit) * 5, 0, Math.PI * 2)
      ctx.stroke()
    }
  }
  ctx.globalAlpha = 1
}

const easeOut = (t: number) => 1 - Math.pow(1 - t, 3)

/** a radar's beam over the sky, or a sonar's ring under the water; lights what it passes */
function drawSweep(ctx: CanvasRenderingContext2D, s: Sweep, t: number, nodes: Node[], g: Geo, p: Palette) {
  const e = t - s.t0
  if (s.kind === 'radar') {
    if (e > RADAR_S) return
    const a0 = Math.PI * 1.04, a1 = Math.PI * 1.96
    const ang = a0 + (a1 - a0) * (e / RADAR_S)
    const len = g.w * 0.95
    ctx.save()
    ctx.clip(g.sky)
    ctx.fillStyle = p.air
    for (let k = 0; k < 8; k++) {
      const b = ang - k * 0.035
      ctx.globalAlpha = 0.06 * (1 - k / 8) * Math.min(1, (RADAR_S - e) * 2)
      ctx.beginPath()
      ctx.moveTo(s.x, s.y)
      ctx.arc(s.x, s.y, len, b - 0.035, b)
      ctx.closePath()
      ctx.fill()
    }
    ctx.globalAlpha = 0.35 * Math.min(1, (RADAR_S - e) * 2)
    ctx.strokeStyle = p.air
    ctx.beginPath()
    ctx.moveTo(s.x, s.y)
    ctx.lineTo(s.x + Math.cos(ang) * len, s.y + Math.sin(ang) * len)
    ctx.stroke()
    ctx.restore()
    for (const n of nodes) {
      if (n.d !== 'air') continue
      const a = Math.atan2(n.y - s.y, n.x - s.x) + Math.PI * 2
      if (Math.abs(a - ang) < 0.03) n.lit = 1
    }
  } else {
    if (e > SONAR_S) return
    const r = 340 * easeOut(e / SONAR_S)
    const fade = 1 - e / SONAR_S
    ctx.save()
    ctx.clip(g.water)
    ctx.strokeStyle = p.surface
    for (const [k, f] of [[1, 0.55], [0.86, 0.22], [0.72, 0.1]] as const) {
      ctx.globalAlpha = f * fade
      ctx.lineWidth = k === 1 ? 1.5 : 1
      ctx.beginPath()
      ctx.arc(s.x, s.y, r * k, 0, Math.PI * 2)
      ctx.stroke()
    }
    ctx.restore()
    for (const n of nodes) {
      if (n.d !== 'subsurface' && n.d !== 'surface') continue
      if (Math.abs(Math.hypot(n.x - s.x, n.y - s.y) - r) < 5) n.lit = 1
    }
  }
}

export function SwarmBackdrop() {
  const ref = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const canvas = ref.current
    const ctx = canvas?.getContext('2d')
    if (!canvas || !ctx) return
    const still = window.matchMedia('(prefers-reduced-motion: reduce)')
    const layer = document.createElement('canvas')
    const lctx = layer.getContext('2d')
    if (!lctx) return
    const p = palette(canvas)
    let g: Geo | null = null
    let nodes: Node[] = []
    let sweeps: Sweep[] = []
    let sweepN = 0
    let raf = 0
    let last = 0
    let clock = 0
    let nextSweep = 1.5
    let dpr = 1

    const frame = () => {
      if (!g) return
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      ctx.clearRect(0, 0, g.w, g.h)
      ctx.drawImage(layer, 0, 0, g.w, g.h)
      for (const s of sweeps) drawSweep(ctx, s, clock, nodes, g, p)
      drawLinks(ctx, nodes, p)
      drawNodes(ctx, nodes, p)
    }
    const startSweep = () => {
      if (!g) return
      const kind = sweepN++ % 2 ? 'sonar' : 'radar'
      if (kind === 'radar') {
        const [x, y] = g.summits[sweepN % 4 < 2 ? 0 : 1]
        sweeps = [{ kind, t0: clock, x, y: y - 6 }]
      } else {
        const subs = nodes.filter((n) => n.d === 'subsurface')
        const from = subs[Math.floor((clock * 7.3) % Math.max(1, subs.length))]
        if (from) sweeps = [{ kind, t0: clock, x: from.x, y: from.y }]
      }
    }
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick)
      if (!g || now - last < FRAME_MS) return
      const dt = Math.min(0.1, (now - last) / 1000)
      last = now
      clock += dt
      step(nodes, g, dt)
      if (clock > nextSweep) {
        startSweep()
        nextSweep = clock + SWEEP_EVERY_S
      }
      frame()
    }
    const play = () => {
      cancelAnimationFrame(raf)
      raf = 0
      if (still.matches || document.hidden) {
        frame()
        return
      }
      last = performance.now()
      raf = requestAnimationFrame(tick)
    }
    const resize = () => {
      const w = canvas.clientWidth, h = canvas.clientHeight
      if (!w || !h) return
      dpr = Math.min(1.5, window.devicePixelRatio || 1)
      canvas.width = layer.width = Math.round(w * dpr)
      canvas.height = layer.height = Math.round(h * dpr)
      const old = g
      g = geometry(w, h)
      if (!old || !nodes.length) {
        nodes = seed(g)
        // a still frame shows a flock already formed
        for (let k = 0; k < 150; k++) step(nodes, g, 1 / 30)
      } else {
        const r = rng(11)
        for (const n of nodes) {
          n.x *= w / old.w
          n.y *= h / old.h
          const ok = n.d === 'air' || (n.d === 'ground' ? onLand(g, n.x) : inWater(g, n.x, n.d === 'subsurface' ? 50 : 0))
          if (!ok) place(n, g, r)
        }
      }
      lctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      drawStatic(lctx, g, p)
      frame()
    }
    const ro = new ResizeObserver(resize)
    ro.observe(canvas)
    resize()
    play()
    document.addEventListener('visibilitychange', play)
    still.addEventListener('change', play)
    return () => {
      cancelAnimationFrame(raf)
      ro.disconnect()
      document.removeEventListener('visibilitychange', play)
      still.removeEventListener('change', play)
    }
  }, [])
  return <canvas ref={ref} className="mm-swarm" aria-hidden="true" />
}
