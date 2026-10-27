import { useEffect, useRef } from 'react'
import { drawShape, fit, type Ink, LINKS, type LinkKind, type Shape, SHAPES } from './swarmShapes'

/** The main menu's backdrop: a coast in cross-section (space and its satellites, the sky, the sea
 *  surface, the water column and the seabed, a ridge ashore), each band named at the screen's
 *  edge, with fifteen nodes in it, one of each kind of thing. Each node is a ring in its domain's
 *  colour round its vehicle's silhouette (swarmShapes.ts), moving as its kind does: aircraft at
 *  their heights, a ground vehicle along the ridge, ships and submarines coming in from the
 *  open sea and going back out, the XLUUV rising to raise its mast, the ROV on its tether.
 *
 *  The links between them are the networks a force runs, each in its own style (LINKS) with its
 *  traffic moving along it: a line-of-sight mesh radio among the boats, the ground vehicles and the
 *  airship, which the terrain cuts; a data link among the larger aircraft, passed round in turn; a
 *  ground control station on the ridge the small drones hang off; SATCOM from the platforms that
 *  carry it to a GEO satellite and to passing LEO ones, handed over as they pass; slow, short
 *  acoustic links from the submarines to the buoy and the Saildrone at the surface, so an AUV deep
 *  is out of contact (its ring broken) until it comes up; the ROV's tether. A link the terrain cuts
 *  or a burst of loss drops shows red, and recovers. Now and then a radar on the ridge sweeps the
 *  whole sky behind the terrain, or a submarine's sonar pings, lighting what it passes.
 *
 *  Decoration only: none of it is a vehicle, a link or a figure of any run, and it says no number.
 *  It reads the menu's layout (the gutters and the gap between the two columns, which the panels
 *  leave open) and keeps the nodes that hold station there. One canvas, at most 30 frames a
 *  second, DPR capped at 1.5, nothing while the tab is hidden, one still frame under
 *  prefers-reduced-motion. Its colours are the menu's (menu.css). */

type Dom = 'space' | 'air' | 'ground' | 'surface' | 'subsurface'

const DOMAIN: Record<Shape, Dom> = {
  rq4: 'air', mq9b: 'air', mq9b_armed: 'air', cca: 'air', fighter: 'air', fvr90: 'air', puma: 'air', switchblade: 'air', x10: 'air', fpv: 'air', lta: 'air',
  ugv_logistics: 'ground', ugv_armed: 'ground', ugv_mines: 'ground', ugv_recon: 'ground', tracker: 'ground', mine: 'ground',
  magura: 'surface', corsair: 'surface', seahunter: 'surface', saildrone: 'surface', buoy: 'surface',
  bluerov: 'subsurface', remus: 'subsurface', orca: 'subsurface',
  sat_optical: 'space', sat_sar: 'space', sat_satcom: 'space', sat_leo: 'space', sat_gnss: 'space', iss: 'space',
}
const BIG = new Set<Shape>(['rq4', 'mq9b', 'mq9b_armed', 'seahunter', 'orca', 'sat_satcom', 'iss'])
/** who is on which network */
const MESH = new Set<Shape>(['ugv_logistics', 'ugv_armed', 'ugv_mines', 'ugv_recon', 'tracker', 'magura', 'corsair', 'seahunter', 'saildrone', 'buoy', 'lta'])
const DATALINK = new Set<Shape>(['rq4', 'mq9b', 'mq9b_armed', 'cca', 'fighter'])
const CONTROLLED = new Set<Shape>(['fvr90', 'puma', 'switchblade', 'x10', 'fpv'])
const SATCOM_GEO = new Set<Shape>(['rq4', 'mq9b', 'mq9b_armed'])
const SATCOM_LEO = new Set<Shape>(['seahunter', 'saildrone', 'orca'])
const GATEWAY = new Set<Shape>(['buoy', 'saildrone'])
const QUADS = new Set<Shape>(['x10', 'fpv'])

interface Swarm {
  x: number
  y: number
  vy: number
  dir: number
  sp: number
  alt: number
  ph: number
  /** how far its members wander from their places (px at scale 1) */
  jit: number
  /** a station it hovers about (x) instead of flying on */
  hold?: number
}
interface Node {
  id: number
  s: Shape
  d: Dom
  /** its silhouette's origin: its centre, or on the ground or the waterline */
  x: number
  y: number
  /** facing: 1 east, -1 west */
  dir: number
  /** speed along x (px/s at scale 1) */
  sp: number
  /** attitude (rad, nose down positive) */
  rot: number
  ph: number
  /** lit by a sweep or a reconnection: 1, fading to 0 */
  lit: number
  /** air: its height in the sky (0 the top, 1 the bottom); the deep: in the water column (0 the
   *  surface, 1 the seabed); space: its orbit's height at the screen's centre (px) */
  alt: number
  /** the ring's radius, the silhouette's scale in it and its offset from the origin; the ring's
   *  centre, each frame */
  r: number
  k: number
  fx: number
  fy: number
  cx: number
  cy: number
  /** it carries its type's tag */
  tag: boolean
  /** a formation it flies in, and its place in it (px at scale 1: + ahead, + below) */
  sw?: Swarm
  ox: number
  oy: number
  /** a station it holds (x) */
  hold?: number
  /** a ground vehicle's patrol */
  lo: number
  hi: number
  /** seconds left off-screen before it turns back in */
  wait: number
  /** the ROV's ship */
  host?: Node
  /** a ground station's dish (rad) */
  aim: number
  /** under water and in touch by sound; the XLUUV with its mast up */
  heard: boolean
  up: boolean
  /** ground covered since its last mine */
  run: number
  /** how far it has faded in (1) or out (0), and which way it is going: a scene's change */
  fade: number
  goal: 0 | 1
}
interface Geo {
  w: number
  h: number
  /** the nodes' scale */
  S: number
  sea: number
  /** the solid surface (ridge or seabed) every STEP px across */
  ground: Float32Array
  /** where the coast meets the sea */
  shore: number
  /** the ridge's summit: the radar and the ground station */
  summit: [number, number]
  /** the edge of space, the Earth's limb: a circle */
  lx: number
  ly: number
  lr: number
  water: Path2D
  /** above the terrain and below the limb */
  sky: Path2D
  stars: [number, number, number, number][]
}
/** what the menu's panels leave open */
interface Open {
  /** the left gutter's right edge and the right gutter's left edge */
  left: number
  right: number
  /** the gap between the two columns */
  gap: [number, number] | null
  /** the top of the panels */
  top: number
  /** the title and its pitch (x0, y0, x1, y1), where no tag goes */
  title: [number, number, number, number] | null
}
interface Link {
  kind: LinkKind
  a: Node
  b: Node
  /** its quality: 1, falling to 0 at the edge of its range */
  q: number
  key: string
}
interface Ghost {
  a: Node
  b: Node
  /** where the terrain cut it, along a to b (or -1) */
  u: number
  t0: number
}
interface Sweep {
  kind: 'radar' | 'sonar'
  t0: number
  x: number
  y: number
}

const STEP = 4
const FRAME_MS = 1000 / 30
const SWEEP_EVERY_S = 6.5
const RADAR_S = 2.6
const SONAR_S = 3.2
const GHOST_S = 1.8
const FLASH_S = 0.9
const BURST_S = 2.6
const CUT_EVERY_S = 5
/** a scene is held this long, and fades into the next over FADE_S */
const SCENE_S = 15
const FADE_S = 2.5
/** the scene the opening one first changes to (SCENES) */
const FIRST_CHANGE = 1

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
const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v))
/** a number in [0, 1) from any number, steady for the same input */
const hash = (v: number) => {
  const s = Math.sin(v * 12.9898) * 43758.5453
  return s - Math.floor(s)
}

// ---- the scene's shape ---------------------------------------------------------------------------

function geometry(w: number, h: number): Geo {
  const S = clamp(Math.min(w / 1250, h / 780), 0.8, 1.45)
  const sea = Math.round(h * 0.66)
  const n = Math.ceil(w / STEP) + 2
  const ground = new Float32Array(n)
  const bump = (u: number, c: number, s: number) => Math.exp(-(((u - c) / s) ** 2))
  for (let i = 0; i < n; i++) {
    const u = (i * STEP) / w
    // the open sea to the west, the coast rising to a ridge in the east, its summit near the edge
    const land = smooth(0.71, 0.78, u)
    const depth = h * (0.2 + 0.014 * Math.sin(u * 11 + 0.4) + 0.008 * Math.sin(u * 37 + 2))
    const rise = h * (0.022 + 0.07 * bump(u, 0.85, 0.05) + 0.105 * bump(u, 0.972, 0.06) + 0.005 * Math.sin(u * 90))
    ground[i] = sea + depth * (1 - land) - rise * land
  }
  let shore = w
  for (let i = 0; i < n; i++) {
    if (ground[i] < sea) {
      shore = i * STEP
      break
    }
  }
  let top = Math.floor((w * 0.9) / STEP)
  for (let i = top; i < n && i * STEP <= w; i++) if (ground[i] < ground[top]) top = i
  const lr = w * 9
  const ly = clamp(h * 0.115, 96, 140) + lr
  const limbY = (x: number) => ly - Math.sqrt(lr * lr - (x - w / 2) ** 2)
  const water = new Path2D()
  water.moveTo(0, sea)
  water.lineTo(w, sea)
  for (let i = n - 1; i >= 0; i--) water.lineTo(i * STEP, Math.max(sea, ground[i]))
  water.closePath()
  const sky = new Path2D()
  for (let i = 0; i < n; i++) sky.lineTo(i * STEP, limbY(i * STEP))
  for (let i = n - 1; i >= 0; i--) sky.lineTo(i * STEP, Math.min(sea, ground[i]))
  sky.closePath()
  const r = rng(3)
  const stars: [number, number, number, number][] = []
  for (let k = 0; k < Math.round(w / 13); k++) {
    const x = r() * w
    const y = r() * (limbY(x) - 8)
    stars.push([x, y, 0.45 + r() * 0.7, 0.18 + r() * 0.5])
  }
  return { w, h, S, sea, ground, shore, summit: [top * STEP, ground[top]], lx: w / 2, ly, lr, water, sky, stars }
}

const groundAt = (g: Geo, x: number) => g.ground[Math.max(0, Math.min(g.ground.length - 1, Math.round(x / STEP)))]
const limbAt = (g: Geo, x: number) => g.ly - Math.sqrt(g.lr * g.lr - (x - g.lx) ** 2)
const inWater = (g: Geo, x: number, deep = 0) => groundAt(g, x) > g.sea + 10 + deep
/** a height `a` in the sky (0 its top, 1 its bottom) over x, for a ring of radius r */
function skyY(g: Geo, x: number, a: number, r: number) {
  const top = limbAt(g, x) + r + 8
  const bottom = Math.min(g.sea, groundAt(g, x)) - r * 1.5
  return top + clamp(a, 0, 1) * Math.max(0, bottom - top)
}
/** an orbit whose height at the centre is y0, over x: the limb's curve */
const orbitY = (g: Geo, x: number, y0: number) => y0 + (g.lr - Math.sqrt(g.lr * g.lr - (x - g.lx) ** 2))

function measure(canvas: HTMLCanvasElement, w: number): Open {
  const c = canvas.getBoundingClientRect()
  const box = (sel: string) => {
    const r = canvas.parentElement?.querySelector(sel)?.getBoundingClientRect()
    return r && r.width > 0 ? r : null
  }
  const l = box('.mm-left'), r = box('.mm-right'), id = box('.mm-ident')
  const title: Open['title'] = id ? [id.left - c.left - 10, id.top - c.top - 10, id.right - c.left + 10, id.bottom - c.top + 10] : null
  // one column (a small screen): nothing between, a narrow gutter each side
  if (!l || !r) return { left: 16, right: w - 16, gap: null, top: 82, title }
  return {
    left: l.left - c.left,
    right: r.right - c.left,
    gap: r.left - l.right > 24 ? [l.right - c.left, r.left - c.left] : null,
    top: Math.max(56, r.top - c.top),
    title,
  }
}

// ---- the cast ------------------------------------------------------------------------------------

/** what a scene places its nodes with */
interface Cast {
  g: Geo
  S: number
  /** a laptop's screen or larger: room for the extras */
  full: boolean
  add: (s: Shape, o?: Partial<Node>) => Node
  formation: (s: Shape, places: [number, number][], o: { alt: number; sp: number; jit: number; x?: number; dir?: number; hold?: number }) => Swarm
  /** the windows the panels leave open, where a scene puts what it is about: the gutters each side
   *  of the menu (the west over the open sea, the east over the ridge), stations in them for a ship
   *  and a buoy (at sea, west) and for something in the air (west and east), the strip under the
   *  top rail */
  west: [number, number]
  east: [number, number]
  shipX: number
  buoyX: number
  westX: number
  eastX: number
  strip: number
  /** the GEO satellite's station in the top rail, between the legend and the scene's name */
  geoX: number
}

/** One mix of the force with its own focus, held SCENE_S before the backdrop moves to another.
 *  Each has a node in every domain where the panels leave it seen (on a station, or patrolling
 *  near one, so it is never off-screen waiting to come back in), and each network it uses has
 *  someone at both ends. */
interface Scene {
  name: string
  cast: (c: Cast) => void
}

/** a patrol of its own about x, where it stays seen */
const near = (x: number, S: number) => ({ lo: Math.max(1, x - 70 * S), hi: x + 70 * S })

/** n stretches of the ridge in the east gutter, short of the summit's ground station: a ground
 *  vehicle each, so they never bunch; the whole ridge if the gutter is too narrow for them */
function ridge(g: Geo, S: number, east: [number, number], n: number): { lo: number; hi: number }[] {
  const hi = g.summit[0] - 34 * S
  let lo = Math.max(g.shore + 30 * S, east[0] + 16 * S)
  if (hi - lo < n * 70 * S) lo = g.shore + 30 * S
  const w = (hi - lo) / n
  return Array.from({ length: n }, (_, i) => ({ lo: lo + i * w, hi: lo + (i + 1) * w - 8 * S }))
}

/** a patrol offshore in the west gutter, short of the buoy; a free run in from the sea if there
 *  is no room */
const offshore = (west: [number, number], buoyX: number, S: number) => {
  const lo = west[0] + 24 * S, hi = Math.min(west[1], buoyX) - 54 * S
  return hi - lo > 40 * S ? { lo, hi } : {}
}

const SCENES: Scene[] = [
  {
    // one of each kind of thing: the opening scene
    name: 'combined force',
    cast: ({ g, S, full, add, east, shipX, buoyX, eastX, strip, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('sat_leo', { alt: strip, sp: 30, dir: 1 })
      add('mq9b', { alt: 0.12, sp: 19 })
      if (full) add('cca', { alt: 0.3, sp: 44 })
      add('lta', { hold: eastX, alt: 0.38 })
      if (full) add('puma', { alt: 0.6, sp: 21 })
      add('x10', { alt: 0.72, sp: 12 })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      add('ugv_armed', { ...ridge(g, S, east, 1)[0], sp: 9 })
      const ship = add('corsair', { hold: shipX, dir: 1 })
      add('bluerov', { host: ship, x: shipX, alt: 0.45 })
      add('buoy', { hold: buoyX })
      add('seahunter', { sp: 13 })
      add('remus', { sp: 12, ...near(buoyX, S) })
      add('orca', { sp: 8 })
    },
  },
  {
    // loyal wingmen: a crewed fighter leading three CCAs in echelon on the data link, in from the
    // west through the west gutter, low enough to pass under the title; an RQ-4 high above on
    // SATCOM. The first scene changed to.
    name: 'loyal wingman',
    cast: ({ g, S, add, formation, buoyX, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('rq4', { alt: 0.03, sp: 15 })
      const sw = formation('cca', [[-50, 16], [-100, 32], [-50, -18]], { alt: 0.62, sp: 22, jit: 0, x: 0, dir: 1 })
      add('fighter', { sw, ox: 0, oy: 0, sp: sw.sp, alt: sw.alt })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      add('buoy', { hold: buoyX })
      add('remus', { sp: 10, ...near(buoyX, S) })
    },
  },
  {
    // watch over the approaches: the sensor ship patrolling offshore, fast boats in from the sea,
    // a drone overhead on SATCOM and a small one off the ground station, an AUV by the buoy
    name: 'maritime patrol',
    cast: ({ g, S, full, add, west, buoyX, westX, strip, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('sat_leo', { alt: strip, sp: 30, dir: 1 })
      add('mq9b', { alt: 0.14, sp: 19 })
      add('puma', { hold: westX, alt: 0.42 })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      add('seahunter', { sp: 9, ...offshore(west, buoyX, S) })
      add('magura', { sp: 34 })
      if (full) add('magura', { sp: 30 })
      add('buoy', { hold: buoyX })
      add('remus', { sp: 12, ...near(buoyX, S) })
    },
  },
  {
    // under the water: the XLUUV by the ship's station and two AUVs, heard through the buoy and
    // the Saildrone, the ROV on its ship's tether; a MALE drone overhead, the ground station ashore
    name: 'undersea',
    cast: ({ g, S, full, add, shipX, buoyX, strip, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('sat_leo', { alt: strip, sp: 30, dir: 1 })
      add('mq9b', { alt: 0.1, sp: 19 })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      const ship = add('corsair', { hold: shipX, dir: 1 })
      add('bluerov', { host: ship, x: shipX, alt: 0.45 })
      add('buoy', { hold: buoyX })
      add('saildrone', { sp: 6, lo: shipX + 150 * S, hi: g.shore - 50 * S })
      add('orca', { sp: 7, ...near(shipX, S) })
      add('remus', { sp: 12, ...near(buoyX, S) })
      if (full) add('remus', { sp: 10 })
    },
  },
  {
    // a drone swarm off the ground station: quadcopters hovering as a group over the sea, FPVs over
    // the ridge under the airship relaying, a Puma, a VTOL and a loitering munition passing, a recon
    // UGV on the ridge, a MALE drone high above; a ship and its ROV offshore
    name: 'drone swarm',
    cast: ({ g, S, full, add, formation, east, shipX, westX, eastX, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('mq9b', { alt: 0.08, sp: 19 })
      add('lta', { hold: eastX, alt: 0.34 })
      add('fvr90', { alt: 0.48, sp: 24 })
      add('puma', { alt: 0.56, sp: 21 })
      if (full) add('switchblade', { alt: 0.64, sp: 28 })
      formation('x10', [[0, 0], [-34, -12], [-22, 16]], { alt: 0.6, sp: 12, jit: 5, hold: westX })
      if (full) formation('fpv', [[0, 0], [-32, 10]], { alt: 0.72, sp: 26, jit: 4, hold: eastX })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      add('ugv_recon', { ...ridge(g, S, east, 1)[0], sp: 11 })
      const ship = add('corsair', { hold: shipX, dir: 1 })
      add('bluerov', { host: ship, x: shipX, alt: 0.45 })
    },
  },
  {
    // reconnaissance from on high: the imaging, radar and navigation satellites and the station
    // overhead, the RQ-4 holding high over the ridge, a MALE drone on SATCOM and two CCAs on the
    // data link passing; the XLUUV coming up to its mast by the ship's station, a buoy
    name: 'high-altitude ISR',
    cast: ({ g, S, full, add, formation, shipX, buoyX, eastX, strip, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('sat_leo', { alt: strip, sp: 30, dir: 1 })
      add('sat_optical', { alt: strip + 1, sp: 24, dir: -1 })
      if (full) add('sat_sar', { alt: strip + 2, sp: 18, dir: 1 })
      add('sat_gnss', { alt: strip - 2, sp: 6, dir: 1 })
      add('iss', { alt: strip - 1, sp: 34, dir: -1 })
      add('rq4', { hold: eastX, alt: 0.04 })
      add('mq9b', { alt: 0.18, sp: 19 })
      formation('cca', [[0, 0], [-42, 14]], { alt: 0.34, sp: 44, jit: 0 })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      add('seahunter', { sp: 13 })
      add('buoy', { hold: buoyX })
      add('orca', { sp: 7, ...near(shipX, S) })
    },
  },
  {
    // ashore: logistics, armed, recon and (with room) mine-laying UGVs along the ridge, each on its
    // own stretch, the airship relaying over them, quadcopters scouting low, an armed MALE drone
    // overhead; a ship and its ROV offshore
    name: 'ground convoy',
    cast: ({ g, S, add, formation, east, shipX, eastX, geoX }) => {
      add('sat_satcom', { hold: geoX, alt: 24 })
      add('mq9b_armed', { alt: 0.16, sp: 18 })
      add('lta', { hold: eastX, alt: 0.3 })
      formation('x10', [[0, 0], [-34, -12]], { alt: 0.62, sp: 12, jit: 5, hold: eastX })
      add('tracker', { x: g.summit[0], hold: g.summit[0], dir: 1 })
      const room = g.summit[0] - 34 * S - Math.max(g.shore + 30 * S, east[0] + 16 * S) >= 4 * 90 * S
      const seg = ridge(g, S, east, room ? 4 : 3)
      add('ugv_logistics', { ...seg[0], sp: 7 })
      add('ugv_armed', { ...seg[1], sp: 8 })
      add('ugv_recon', { ...seg[2], sp: 9 })
      if (room) add('ugv_mines', { ...seg[3], sp: 6 })
      const ship = add('corsair', { hold: shipX, dir: 1 })
      add('bluerov', { host: ship, x: shipX, alt: 0.45 })
    },
  },
]

/** a scene's nodes (ids from `ids`, so they stay unique across scenes) */
function seed(g: Geo, open: Open, k: number, ids: { n: number }): Node[] {
  const r = rng(7 + k * 101)
  const S = g.S
  // all of them from a laptop's screen up; fewer on a small one, where they would crowd the words
  const full = (g.w * g.h) / (1280 * 720) >= 0.6
  const nodes: Node[] = []
  const seen = new Set<Shape>()
  const add = (s: Shape, o: Partial<Node> = {}): Node => {
    // the loyal wingmen's lead a size up on its CCAs
    const rad = (s === 'fighter' ? 25 : BIG.has(s) ? 22 : DOMAIN[s] === 'space' ? 15 : 19) * S
    const f = fit(s, rad)
    const n: Node = {
      id: ids.n++,
      s,
      d: DOMAIN[s],
      x: r() * g.w,
      y: 0,
      dir: r() < 0.5 ? -1 : 1,
      sp: 0,
      rot: 0,
      ph: r() * Math.PI * 2,
      lit: 0,
      alt: 0.5,
      r: rad,
      k: f.k,
      fx: f.k * f.cx,
      fy: f.k * f.cy,
      cx: 0,
      cy: 0,
      tag: !seen.has(s),
      ox: r() * 10,
      oy: 0,
      lo: 0,
      hi: g.w,
      wait: 0,
      aim: -Math.PI * 0.75,
      heard: false,
      up: false,
      run: 0,
      fade: 1,
      goal: 1,
      ...o,
    }
    seen.add(s)
    nodes.push(n)
    return n
  }
  const formation = (s: Shape, places: [number, number][], o: { alt: number; sp: number; jit: number; x?: number; dir?: number; hold?: number }) => {
    const sw: Swarm = { x: o.hold ?? o.x ?? r() * g.w, y: 0, vy: 0, dir: o.dir ?? (r() < 0.5 ? -1 : 1), sp: o.sp, alt: o.alt, ph: r() * 6, jit: o.jit, hold: o.hold }
    sw.y = skyY(g, sw.x, sw.alt, 19 * S)
    for (const [ox, oy] of places) add(s, { sw, ox, oy, sp: o.sp, alt: o.alt })
    return sw
  }
  // clear of the band names up the left edge
  const west: [number, number] = [44, Math.max(44, open.left - 10)]
  const east: [number, number] = [Math.min(g.w - 12, open.right + 10), g.w - 12]
  const ww = west[1] - west[0], ew = east[1] - east[0]
  // with too narrow a gutter, the column gap, else a place the panels may cover
  const gapX = open.gap ? (open.gap[0] + open.gap[1]) / 2 : g.w * 0.36
  const shipX = ww >= 60 * S ? west[0] + ww * (ww >= 96 * S ? 0.28 : 0.5) : gapX
  const buoyX = ww >= 96 * S ? west[0] + ww * 0.8 : open.gap ? gapX : g.w * 0.15
  const westX = ww >= 60 * S ? (west[0] + west[1]) / 2 : gapX
  const eastX = ew >= 60 * S ? (east[0] + east[1]) / 2 : gapX
  const strip = (48 + open.top) / 2
  const geoX = Math.min(g.w * 0.72, g.w - 470)
  SCENES[k].cast({ g, S, full, add, formation, west, east, shipX, buoyX, westX, eastX, strip, geoX })

  for (const n of nodes) {
    if (n.d === 'ground' || n.d === 'surface' || n.d === 'subsurface') {
      if (n.hold == null && !n.host) n.x = n.d === 'ground' || n.lo > 0 ? n.lo + r() * (n.hi - n.lo) : r() * Math.max(40, g.shore - 80 * S)
    }
    if (n.d === 'air' && !n.sw) n.y = skyY(g, n.x, n.alt, n.r)
    if (n.d === 'subsurface') n.y = g.sea + 40 * S
    if (n.d === 'surface') n.y = g.sea
    if (n.d === 'ground') n.y = groundAt(g, n.x)
  }
  return nodes
}

// ---- motion --------------------------------------------------------------------------------------

function step(nodes: Node[], g: Geo, dt: number, t: number, mines: [number, number, number][]) {
  const S = g.S
  const swarms = new Set<Swarm>()
  for (const n of nodes) if (n.sw) swarms.add(n.sw)
  for (const sw of swarms) {
    if (sw.hold != null) sw.x = sw.hold + Math.sin(t * 0.12 + sw.ph) * 30 * S
    else {
      sw.x += sw.dir * sw.sp * S * dt
      const m = 150 * S
      if (sw.x > g.w + m) sw.x = -m
      else if (sw.x < -m) sw.x = g.w + m
    }
    sw.ph += dt * 0.3
    const py = sw.y
    sw.y += (skyY(g, sw.x, sw.alt + 0.03 * Math.sin(sw.ph), 19 * S) - sw.y) * Math.min(1, dt * 1.2)
    sw.vy = (sw.y - py) / dt
  }
  for (const n of nodes) {
    n.lit = Math.max(0, n.lit - dt * 0.9)
    n.ph += dt * 0.5
    if (n.d === 'space') {
      if (n.hold != null) n.x = n.hold + Math.sin(t * 0.01) * 4 * S
      else {
        n.x += n.dir * n.sp * S * dt
        const m = n.r * 3
        if (n.x > g.w + m) n.x = -m
        else if (n.x < -m) n.x = g.w + m
      }
      n.y = orbitY(g, n.x, n.alt)
      n.rot = Math.atan((n.x - g.lx) / Math.sqrt(g.lr * g.lr - (n.x - g.lx) ** 2))
      n.dir = 1
    } else if (n.d === 'air') {
      if (n.sw) {
        const sw = n.sw
        n.dir = sw.dir
        n.x = sw.x + sw.dir * n.ox * S + Math.sin(t * 0.9 + n.ox) * sw.jit * S
        n.y = Math.min(sw.y + n.oy * S + Math.cos(t * 0.7 + n.ox * 1.3) * sw.jit * 0.7 * S, groundAt(g, n.x) - n.r * 1.4)
        n.rot = QUADS.has(n.s) ? 0.2 + 0.05 * Math.sin(t * 1.3 + n.ox) : clamp(sw.vy / (sw.sp * S), -0.25, 0.25)
      } else if (n.hold != null) {
        const u = t * 0.07 + n.ox
        n.x = n.hold + Math.sin(u) * 5 * S
        n.dir = Math.cos(u) >= 0 ? 1 : -1
        n.y = skyY(g, n.x, n.alt, n.r) + Math.sin(t * 0.25) * 2 * S
        n.rot = 0
      } else {
        const py = n.y
        n.x += n.dir * n.sp * S * dt
        const m = n.r * 3
        if (n.x > g.w + m) n.x = -m
        else if (n.x < -m) n.x = g.w + m
        n.y += (skyY(g, n.x, n.alt + 0.025 * Math.sin(n.ph * 0.5), n.r) - n.y) * Math.min(1, dt * 1.2)
        n.rot = clamp((n.y - py) / dt / (n.sp * S), -0.25, 0.25)
      }
    } else if (n.d === 'ground') {
      if (n.hold == null) {
        const px = n.x
        n.x += n.dir * n.sp * S * dt
        if (n.x > n.hi) n.dir = -1
        else if (n.x < n.lo) n.dir = 1
        n.run += Math.abs(n.x - px)
        if (n.s === 'ugv_mines' && n.run > 46 * S) {
          n.run = 0
          mines.push([n.x - n.dir * 12 * n.k, groundAt(g, n.x - n.dir * 12 * n.k), t])
          if (mines.length > 6) mines.shift()
        }
      }
      n.y = groundAt(g, n.x)
      n.rot = n.dir * Math.atan2(groundAt(g, n.x + 6) - groundAt(g, n.x - 6), 12)
    } else if (n.host) {
      // the ROV: under its ship, trailing it, up and down the water column
      const host = n.host
      n.x += (host.x - host.dir * 4 * S - n.x) * Math.min(1, dt * 0.4)
      n.dir = host.dir
      const floor = groundAt(g, n.x)
      const ty = g.sea + 24 * S + (0.42 + 0.18 * Math.sin(t * 0.05 + 1)) * (floor - g.sea - 48 * S)
      n.y += (ty - n.y) * Math.min(1, dt * 0.5)
      n.rot = 0
    } else if (n.hold != null) {
      n.x = n.hold + Math.sin(t * 0.05 + n.ox) * (n.s === 'buoy' ? 4 : 6) * S
      n.y = g.sea + Math.sin(n.ph * 2.4) * (n.s === 'buoy' ? 1.2 : 0.8)
      n.rot = n.s === 'buoy' ? 0.07 * Math.sin(n.ph * 1.7) : 0.025 * Math.sin(n.ph * 1.3)
    } else {
      // boats and submarines: in from the open sea to the west, back out when they reach the coast
      if (n.wait > 0) {
        n.wait -= dt
        if (n.wait <= 0) n.dir = 1
        continue
      }
      n.x += n.dir * n.sp * S * dt
      const deep = n.d === 'subsurface' ? 2 * n.r : 0
      if (n.lo > 0) {
        // a patrol of its own
        if (n.x > n.hi) n.dir = -1
        else if (n.x < n.lo) n.dir = 1
      } else {
        if (n.dir > 0 && !inWater(g, n.x + n.r * 1.3, deep)) n.dir = -1
        if (n.dir < 0 && n.x < -n.r * 2.5) n.wait = 2 + 7 * hash(t + n.id)
      }
      if (n.d === 'surface') {
        n.y = g.sea + Math.sin(n.ph * 2) * 0.8
        n.rot = 0.03 * Math.sin(n.ph * 1.3) - (n.s === 'magura' ? 0.08 : 0)
      } else {
        const floor = groundAt(g, n.x)
        const column = floor - g.sea - 48 * S
        let ty: number
        if (n.s === 'orca') {
          // long dives, and now and then up to raise its mast
          const up = (t * 0.021 + n.ox) % 1 < 0.3
          ty = up ? g.sea + 9.6 * n.k : g.sea + 24 * S + 0.62 * column
        } else {
          // an AUV: deep for long spells (out of contact), now and then up the column
          const a = 0.5 + 0.5 * Math.sin(t * 0.045 + n.ox)
          ty = g.sea + 24 * S + (0.08 + 0.84 * a) * column
        }
        const py = n.y
        n.y += (ty - n.y) * Math.min(1, dt * 0.35)
        n.rot = clamp((n.y - py) / dt / (n.sp * S), -0.3, 0.3)
        n.up = n.s === 'orca' && n.y - 12 * n.k < g.sea
      }
    }
  }
  for (const n of nodes) {
    n.cx = n.x + n.dir * n.fx
    n.cy = n.y + n.fy
  }
}

/** where the terrain cuts the line from a to b (a fraction along it), or null if it is clear */
function cut(g: Geo, ax: number, ay: number, bx: number, by: number): number | null {
  const n = Math.ceil(Math.hypot(bx - ax, by - ay) / 10)
  for (let i = 1; i < n; i++) {
    const u = i / n
    const x = ax + (bx - ax) * u
    const y = ay + (by - ay) * u
    if (x >= 0 && x <= g.w && y > Math.min(groundAt(g, x), g.sea + 2) - 1) return u
  }
  return null
}

/** a ground station's dish, where its links start */
const dishAt = (n: Node): [number, number] => {
  const d = SHAPES[n.s].dish
  return d ? [n.x + n.dir * d[0] * n.k, n.y + d[1] * n.k] : [n.cx, n.cy]
}
const end = (n: Node): [number, number] => (n.s === 'tracker' ? dishAt(n) : [n.cx, n.cy])
const offscreen = (n: Node) => n.wait > 0

/** this frame's links, and the pairs in range the terrain cuts (with where) */
function network(nodes: Node[], g: Geo): { links: Link[]; blocked: Map<string, number> } {
  const S = g.S
  const links: Link[] = []
  const blocked = new Map<string, number>()
  const key = (k: LinkKind, a: Node, b: Node) => `${k}:${Math.min(a.id, b.id)}:${Math.max(a.id, b.id)}`
  const link = (kind: LinkKind, a: Node, b: Node, range: number, los: boolean) => {
    if (offscreen(a) || offscreen(b)) return
    const [ax, ay] = end(a), [bx, by] = end(b)
    const d = Math.hypot(bx - ax, by - ay)
    if (d > range) return
    const k = key(kind, a, b)
    const u = los ? cut(g, ax, ay, bx, by) : null
    if (u != null) blocked.set(k, u)
    else links.push({ kind, a, b, q: 1 - smooth(0.72, 1, d / range), key: k })
  }
  const mesh = nodes.filter((n) => MESH.has(n.s))
  for (let i = 0; i < mesh.length; i++)
    for (let j = i + 1; j < mesh.length; j++) {
      const a = mesh[i], b = mesh[j]
      link('mesh', a, b, (a.s === 'lta' || b.s === 'lta' ? 330 : 200) * S, true)
    }
  const dl = nodes.filter((n) => DATALINK.has(n.s))
  for (let i = 0; i < dl.length; i++) for (let j = i + 1; j < dl.length; j++) link('datalink', dl[i], dl[j], g.w * 0.6, true)
  const gcs = nodes.find((n) => n.s === 'tracker')
  if (gcs) for (const n of nodes) if (CONTROLLED.has(n.s)) link('control', gcs, n, g.w * 0.95, true)
  const geo = nodes.find((n) => n.s === 'sat_satcom')
  const leo = nodes.filter((n) => n.s === 'sat_leo')
  for (const n of nodes) {
    if (offscreen(n)) continue
    if (SATCOM_GEO.has(n.s) && geo) link('satcom', n, geo, Infinity, false)
    if (SATCOM_LEO.has(n.s) && (n.s !== 'orca' || n.up)) {
      let best: Node | undefined
      for (const s of leo) if (Math.abs(s.cx - n.cx) < g.w * 0.3 && (!best || Math.abs(s.cx - n.cx) < Math.abs(best.cx - n.cx))) best = s
      const to = best ?? geo
      if (to) link('satcom', n, to, Infinity, false)
    }
  }
  const gates = nodes.filter((n) => GATEWAY.has(n.s))
  for (const n of nodes) {
    n.heard = false
    if (n.d !== 'subsurface' || n.host || n.up) continue
    for (const gw of gates) link('acoustic', n, gw, 150 * S, false)
  }
  for (const l of links) if (l.kind === 'acoustic') l.a.heard = true
  for (const n of nodes) if (n.host) links.push({ kind: 'tether', a: n.host, b: n, q: 1, key: key('tether', n.host, n) })
  return { links, blocked }
}

// ---- drawing -------------------------------------------------------------------------------------

interface Palette {
  dom: Record<Dom, string>
  link: Record<LinkKind, string>
  bg: string
  disc: string
  lamp: string
  space: string
  skyTop: string
  skyLow: string
  water: string
  deep: string
  land: string
  seabed: string
  line: string
  font: string
}

function palette(el: Element): Palette {
  const css = getComputedStyle(el)
  const v = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback
  const fallback: Record<string, string> = { '--mm-link': '#8aa2bd', '--dom-air': '#7fb8ff', '--dom-ground': '#c9b98a', '--dom-space': '#d6deef', '--dom-subsurface': '#a397ff', '--mm-tether': '#b7c3cf', '--bad': '#ff5d5d' }
  const link = Object.fromEntries((Object.keys(LINKS) as LinkKind[]).map((k) => [k, v(LINKS[k].color, fallback[LINKS[k].color] ?? '#8aa2bd')])) as Record<LinkKind, string>
  return {
    dom: {
      space: v('--dom-space', '#d6deef'),
      air: v('--dom-air', '#7fb8ff'),
      ground: v('--dom-ground', '#c9b98a'),
      surface: v('--dom-surface', '#6fe0c4'),
      subsurface: v('--dom-subsurface', '#a397ff'),
    },
    link,
    bg: v('--bg', '#0b0f14'),
    disc: v('--mm-disc', 'rgba(8, 11, 15, 0.8)'),
    lamp: v('--mm-lamp', '#fff3c4'),
    space: v('--mm-space', '#05070b'),
    skyTop: v('--mm-sky', '#0a131d'),
    skyLow: v('--mm-sky-low', '#11202e'),
    water: v('--mm-water', '#0b2227'),
    deep: v('--mm-deep', '#0d1325'),
    land: v('--mm-land', '#201d16'),
    seabed: v('--mm-seabed', '#15131f'),
    line: v('--line', '#233040'),
    font: v('--mono', 'monospace'),
  }
}

function spaced(ctx: CanvasRenderingContext2D, em: string) {
  const c = ctx as CanvasRenderingContext2D & { letterSpacing?: string }
  if ('letterSpacing' in c) c.letterSpacing = em
}

/** a band's name, written up the screen's edge beside a bracket from y0 to y1 */
function caption(ctx: CanvasRenderingContext2D, text: string, x: number, y0: number, y1: number, color: string, font: string, side: 1 | -1) {
  if (y1 - y0 < 30) return
  ctx.save()
  ctx.font = `600 9px ${font}`
  spaced(ctx, '0.2em')
  const len = ctx.measureText(text).width
  const bx = x - side * 9
  ctx.strokeStyle = color
  ctx.lineWidth = 1
  ctx.globalAlpha = 0.4
  ctx.beginPath()
  ctx.moveTo(bx + side * 4, y0 + 0.5)
  ctx.lineTo(bx, y0 + 0.5)
  ctx.lineTo(bx, y1 - 0.5)
  ctx.lineTo(bx + side * 4, y1 - 0.5)
  ctx.stroke()
  ctx.globalAlpha = 0.85
  ctx.fillStyle = color
  ctx.translate(x, (y0 + y1) / 2)
  ctx.rotate(-Math.PI / 2)
  ctx.textAlign = 'center'
  ctx.textBaseline = 'middle'
  if (len < y1 - y0 - 8) ctx.fillText(text, 0, 0)
  ctx.restore()
}

/** what does not move, drawn once a size */
function drawStatic(ctx: CanvasRenderingContext2D, g: Geo, p: Palette, open: Open) {
  const { w, h, sea } = g
  ctx.clearRect(0, 0, w, h)
  // space and its stars
  ctx.fillStyle = p.space
  ctx.fillRect(0, 0, w, h)
  ctx.fillStyle = '#dfe6f2'
  for (const [x, y, r, a] of g.stars) {
    ctx.globalAlpha = a
    ctx.beginPath()
    ctx.arc(x, y, r, 0, Math.PI * 2)
    ctx.fill()
  }
  ctx.globalAlpha = 1
  // the sky, lighter towards the horizon, glowing under the limb
  const sky = ctx.createLinearGradient(0, g.ly - g.lr, 0, sea)
  sky.addColorStop(0, p.skyTop)
  sky.addColorStop(1, p.skyLow)
  ctx.fillStyle = sky
  ctx.fill(g.sky)
  ctx.save()
  ctx.clip(g.sky)
  const glow = ctx.createRadialGradient(g.lx, g.ly, g.lr - 70 * g.S, g.lx, g.ly, g.lr)
  glow.addColorStop(0, 'rgba(127, 184, 255, 0)')
  glow.addColorStop(1, 'rgba(127, 184, 255, 0.16)')
  ctx.fillStyle = glow
  ctx.fillRect(0, 0, w, sea)
  ctx.restore()
  const half = Math.asin(Math.min(1, (w / 2 + 20) / g.lr))
  ctx.strokeStyle = p.dom.air
  ctx.globalAlpha = 0.45
  ctx.lineWidth = 1
  ctx.beginPath()
  ctx.arc(g.lx, g.ly, g.lr, -Math.PI / 2 - half, -Math.PI / 2 + half)
  ctx.stroke()
  ctx.globalAlpha = 1
  // the water, darker with depth
  const water = ctx.createLinearGradient(0, sea, 0, sea + h * 0.22)
  water.addColorStop(0, p.water)
  water.addColorStop(1, p.deep)
  ctx.fillStyle = water
  ctx.fill(g.water)
  // the solid: the seabed under the sea, the land ashore
  const n = g.ground.length
  const solid = new Path2D()
  solid.moveTo(0, h)
  for (let i = 0; i < n; i++) solid.lineTo(i * STEP, g.ground[i])
  solid.lineTo(w, h)
  solid.closePath()
  const earth = ctx.createLinearGradient(g.shore - 90 * g.S, 0, g.shore + 10 * g.S, 0)
  earth.addColorStop(0, p.seabed)
  earth.addColorStop(1, p.land)
  ctx.fillStyle = earth
  ctx.fill(solid)
  for (let c = 3; c >= 0; c--) {
    for (const [from, to, color] of [
      [0, g.shore, p.dom.subsurface],
      [g.shore, w + STEP, p.dom.ground],
    ] as const) {
      ctx.beginPath()
      for (let i = Math.floor(from / STEP); i < Math.min(n, Math.ceil(to / STEP) + 1); i++) ctx.lineTo(i * STEP, g.ground[i] + c * 7)
      ctx.strokeStyle = c ? p.line : color
      ctx.globalAlpha = c ? 0.6 / c : color === p.dom.ground ? 0.6 : 0.4
      ctx.lineWidth = c ? 1 : 1.2
      ctx.stroke()
    }
  }
  // the waterline
  ctx.globalAlpha = 0.7
  ctx.strokeStyle = p.dom.surface
  ctx.lineWidth = 1.2
  ctx.beginPath()
  ctx.moveTo(0, sea + 0.5)
  ctx.lineTo(g.shore, sea + 0.5)
  ctx.stroke()
  ctx.globalAlpha = 1
  // the bands' names: up the left edge (the open sea), the ground's up the right (the ridge)
  const lx = Math.min(31, Math.max(14, open.left - 14))
  const l0 = limbAt(g, 0)
  caption(ctx, 'SPACE', lx, 54, l0 - 8, p.dom.space, p.font, 1)
  caption(ctx, 'AIR', lx, l0 + 10, sea - 42, p.dom.air, p.font, 1)
  caption(ctx, 'SURFACE', lx, sea - 36, sea + 36, p.dom.surface, p.font, 1)
  caption(ctx, 'SUBSURFACE', lx, sea + 42, groundAt(g, 0) - 10, p.dom.subsurface, p.font, 1)
  const rx = Math.max(w - 31, Math.min(w - 14, open.right + 14))
  const gr = groundAt(g, rx)
  caption(ctx, 'GROUND', rx, gr + 16, Math.min(h - 16, gr + 96), p.dom.ground, p.font, -1)
}

const easeOut = (t: number) => 1 - Math.pow(1 - t, 3)

/** a radar's beam over the whole sky from the ridge (behind the terrain), or a sonar's ring
 *  under the water; lights what it passes */
function drawSweep(ctx: CanvasRenderingContext2D, s: Sweep, t: number, nodes: Node[], g: Geo, p: Palette) {
  const e = t - s.t0
  if (s.kind === 'radar') {
    if (e > RADAR_S) return
    const a0 = Math.PI * 1.04, a1 = Math.PI * 1.96
    const ang = a0 + (a1 - a0) * (e / RADAR_S)
    const len = g.w * 1.1
    ctx.save()
    ctx.clip(g.sky)
    ctx.fillStyle = p.dom.air
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
    ctx.strokeStyle = p.dom.air
    ctx.lineWidth = 1
    ctx.beginPath()
    ctx.moveTo(s.x, s.y)
    ctx.lineTo(s.x + Math.cos(ang) * len, s.y + Math.sin(ang) * len)
    ctx.stroke()
    ctx.restore()
    for (const n of nodes) {
      if (n.d !== 'air') continue
      const a = Math.atan2(n.cy - s.y, n.cx - s.x) + Math.PI * 2
      if (Math.abs(a - ang) < 0.03) n.lit = 1
    }
  } else {
    if (e > SONAR_S) return
    const r = 340 * g.S * easeOut(e / SONAR_S)
    const fade = 1 - e / SONAR_S
    ctx.save()
    ctx.clip(g.water)
    ctx.strokeStyle = p.dom.surface
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
      if (Math.abs(Math.hypot(n.cx - s.x, n.cy - s.y) - r) < 6) n.lit = 1
    }
  }
}

interface Net {
  links: Link[]
  /** when each link came up */
  since: Map<string, number>
  ghosts: Ghost[]
  /** when the terrain last cut one */
  cutT: number
  burst: { key: string; t0: number } | null
}

/** the links' lines, a SATCOM link's beam, a dropped link's red ghost */
function drawLinks(ctx: CanvasRenderingContext2D, net: Net, g: Geo, p: Palette, t: number) {
  ctx.lineCap = 'butt'
  for (const l of net.links) {
    if (l.kind === 'tether') continue
    const st = LINKS[l.kind]
    const f = Math.min(l.a.fade, l.b.fade)
    const [ax, ay] = end(l.a), [bx, by] = end(l.b)
    const fresh = t - (net.since.get(l.key) ?? -9)
    const burst = net.burst?.key === l.key && t - net.burst.t0 < BURST_S
    let a = st.alpha * (0.3 + 0.7 * l.q) + (fresh < FLASH_S ? 0.45 * (1 - fresh / FLASH_S) : 0)
    if (l.kind === 'satcom') {
      // the satellite's beam, narrowing up to it
      const dx = ax - bx, dy = ay - by, d = Math.hypot(dx, dy) || 1
      const nx = -dy / d, ny = dx / d, half = l.a.r * 1.2
      ctx.globalAlpha = 0.06 * f
      ctx.fillStyle = p.link.satcom
      ctx.beginPath()
      ctx.moveTo(bx, by)
      ctx.lineTo(ax + nx * half, ay + ny * half)
      ctx.lineTo(ax - nx * half, ay - ny * half)
      ctx.closePath()
      ctx.fill()
    }
    const s = burst ? LINKS.lost : st
    if (burst) a = s.alpha * (0.45 + 0.55 * Math.abs(Math.sin(t * 9)))
    ctx.globalAlpha = Math.min(1, a) * f
    ctx.strokeStyle = burst ? p.link.lost : p.link[l.kind]
    ctx.lineWidth = s.width
    ctx.setLineDash(s.dash)
    ctx.beginPath()
    ctx.moveTo(ax, ay)
    ctx.lineTo(bx, by)
    ctx.stroke()
  }
  ctx.setLineDash(LINKS.lost.dash)
  ctx.strokeStyle = p.link.lost
  ctx.lineWidth = LINKS.lost.width
  for (const gh of net.ghosts) {
    const f = 1 - (t - gh.t0) / GHOST_S
    if (f <= 0) continue
    const [ax, ay] = end(gh.a), [bx, by] = end(gh.b)
    ctx.globalAlpha = LINKS.lost.alpha * f * Math.min(gh.a.fade, gh.b.fade)
    ctx.beginPath()
    ctx.moveTo(ax, ay)
    ctx.lineTo(bx, by)
    ctx.stroke()
    if (gh.u >= 0) {
      // where the terrain cut it
      const x = ax + (bx - ax) * gh.u, y = ay + (by - ay) * gh.u, k = 3.5 * g.S
      ctx.setLineDash([])
      ctx.lineWidth = 1.4
      ctx.beginPath()
      ctx.moveTo(x - k, y - k)
      ctx.lineTo(x + k, y + k)
      ctx.moveTo(x + k, y - k)
      ctx.lineTo(x - k, y + k)
      ctx.stroke()
      ctx.setLineDash(LINKS.lost.dash)
      ctx.lineWidth = LINKS.lost.width
    }
  }
  ctx.setLineDash([])
  // the ROV's tether, hanging from the ship's A-frame
  for (const l of net.links) {
    if (l.kind !== 'tether') continue
    const ship = l.a, rov = l.b
    const sx = ship.x - ship.dir * 11 * ship.k, sy = ship.y - 8 * ship.k
    const ex = rov.cx, ey = rov.cy - 4.6 * rov.k
    ctx.globalAlpha = LINKS.tether.alpha * Math.min(ship.fade, rov.fade)
    ctx.strokeStyle = p.link.tether
    ctx.lineWidth = LINKS.tether.width
    ctx.beginPath()
    ctx.moveTo(sx, sy)
    ctx.quadraticCurveTo((sx + ex) / 2 - ship.dir * 14 * g.S, Math.max(sy, ey) + 10 * g.S, ex, ey)
    ctx.stroke()
  }
  ctx.globalAlpha = 1
}

/** the traffic on each link: dots for radio and SATCOM, passed round in turn on the data link,
 *  slow arcs for sound */
function drawTraffic(ctx: CanvasRenderingContext2D, net: Net, g: Geo, p: Palette, t: number) {
  const S = g.S
  const dl = [...new Set(net.links.filter((l) => l.kind === 'datalink').flatMap((l) => [l.a.id, l.b.id]))].sort((x, y) => x - y)
  const slot = 0.5
  const talker = dl.length ? dl[Math.floor(t / slot) % dl.length] : -1
  for (const l of net.links) {
    if (l.kind === 'tether') continue
    const [ax, ay] = end(l.a), [bx, by] = end(l.b)
    const len = Math.hypot(bx - ax, by - ay)
    if (len < 1) continue
    const burst = net.burst?.key === l.key && t - net.burst.t0 < BURST_S
    const col = burst ? p.link.lost : p.link[l.kind]
    const f = Math.min(l.a.fade, l.b.fade)
    const seed = hash(l.a.id * 31 + l.b.id * 7)
    /** packets at fractions `us` along it, from a (or from b) */
    const dots = (us: number[], fromB: boolean) => {
      for (const u0 of us) {
        if (u0 < 0 || u0 > 1) continue
        // a burst of loss: packets die halfway
        if (burst && u0 > 0.5) continue
        const u = fromB ? 1 - u0 : u0
        ctx.globalAlpha = (burst ? 0.9 * (1 - u0 * 1.6) : 0.9) * (0.4 + 0.6 * l.q) * f
        ctx.fillStyle = col
        ctx.beginPath()
        ctx.arc(ax + (bx - ax) * u, ay + (by - ay) * u, 1.5 * S, 0, Math.PI * 2)
        ctx.fill()
      }
    }
    /** a packet each period, travelling at `speed` px/s (at scale 1) */
    const run = (speed: number, rest: number, phase: number) => {
      const travel = len / (speed * S)
      const tau = (t + phase * (travel + rest)) % (travel + rest)
      return tau < travel ? tau / travel : -1
    }
    if (l.kind === 'mesh') {
      dots([run(170, 1.4, seed)], false)
      dots([run(170, 1.4, seed + 0.5)], true)
    } else if (l.kind === 'datalink') {
      // its time slots: the one whose turn it is sends to all the others
      if (l.a.id === talker || l.b.id === talker) {
        const u = (t % slot) / slot
        dots([u, u - 0.04], l.b.id === talker)
      }
    } else if (l.kind === 'control') {
      // commands up to the drone, its video down
      dots([run(240, 1.2, seed)], false)
      const v = run(240, 0.5, seed + 0.3)
      dots([v, v - 0.06, v - 0.12], true)
    } else if (l.kind === 'satcom') {
      dots([run(110, 0.9, seed)], false)
      dots([run(110, 0.9, seed + 0.5)], true)
    } else if (l.kind === 'acoustic') {
      // sound is slow: a pulse at a time, each way in turn
      const travel = len / (24 * S)
      const tau = (t + seed * 2 * (travel + 2)) % (2 * (travel + 2))
      const back = tau >= travel + 2
      const u0 = (back ? tau - travel - 2 : tau) / travel
      if (u0 <= 1 && !(burst && u0 > 0.5)) {
        const u = back ? 1 - u0 : u0
        const x = ax + (bx - ax) * u, y = ay + (by - ay) * u
        const ang = Math.atan2(by - ay, bx - ax) + (back ? Math.PI : 0)
        ctx.globalAlpha = 0.8 * (0.4 + 0.6 * l.q) * f
        ctx.strokeStyle = col
        ctx.lineWidth = 1.2
        for (const rr of [3.5, 6]) {
          ctx.beginPath()
          ctx.arc(x, y, rr * S, ang - 0.8, ang + 0.8)
          ctx.stroke()
        }
      }
    }
  }
  ctx.globalAlpha = 1
}

/** wakes and spray behind the boats, the XLUUV's mast breaking the surface, the mines laid */
function drawWakes(ctx: CanvasRenderingContext2D, nodes: Node[], mines: [number, number, number][], g: Geo, p: Palette, t: number, inks: Record<Dom, Ink>) {
  const S = g.S
  ctx.strokeStyle = p.dom.surface
  ctx.fillStyle = p.dom.surface
  ctx.lineWidth = 1
  for (const n of nodes) {
    if (n.d !== 'surface' || n.hold != null || offscreen(n)) continue
    const [x0, , x1] = SHAPES[n.s].box
    const stern = n.x - n.dir * -x0 * n.k, bow = n.x + n.dir * x1 * n.k
    const len = (10 + n.sp * 0.8) * S
    ctx.globalAlpha = 0.45 * n.fade
    ctx.beginPath()
    ctx.moveTo(stern, g.sea + 0.8)
    ctx.lineTo(stern - n.dir * len, g.sea + 0.8)
    ctx.moveTo(bow, g.sea)
    ctx.lineTo(bow + n.dir * 3 * S, g.sea - 2.5 * S)
    ctx.stroke()
    ctx.globalAlpha = 0.22 * n.fade
    ctx.beginPath()
    ctx.moveTo(stern - n.dir * 3 * S, g.sea + 2.8)
    ctx.lineTo(stern - n.dir * len * 0.6, g.sea + 2.8)
    ctx.stroke()
    if (n.s === 'magura') {
      ctx.globalAlpha = 0.18 * n.fade
      ctx.beginPath()
      ctx.moveTo(stern, g.sea)
      ctx.lineTo(stern - n.dir * 9 * S, g.sea - 5 * S)
      ctx.lineTo(stern - n.dir * 18 * S, g.sea)
      ctx.closePath()
      ctx.fill()
    }
  }
  for (const n of nodes) {
    if (!n.up) continue
    const mx = n.x + n.dir * 6.2 * n.k
    ctx.globalAlpha = 0.4 * n.fade
    ctx.beginPath()
    ctx.moveTo(mx - 6 * S, g.sea + 0.5)
    ctx.lineTo(mx + 6 * S, g.sea + 0.5)
    ctx.stroke()
  }
  for (const [x, y, t0] of mines) {
    ctx.save()
    ctx.globalAlpha = 0.8 * Math.max(0, 1 - (t - t0) / 40)
    ctx.translate(x, y)
    ctx.scale(S, S)
    drawShape(ctx, 'mine', inks.ground, t)
    ctx.restore()
  }
  ctx.globalAlpha = 1
}

function drawNodes(ctx: CanvasRenderingContext2D, nodes: Node[], g: Geo, p: Palette, t: number, inks: Record<Dom, Ink>, open: Open) {
  for (const n of nodes) {
    if (offscreen(n)) continue
    const c = p.dom[n.d]
    // out of contact: an AUV or the XLUUV deep with no gateway in range
    const lost = n.d === 'subsurface' && !n.host && !n.heard && !n.up
    ctx.globalAlpha = n.fade
    ctx.fillStyle = p.disc
    ctx.beginPath()
    ctx.arc(n.cx, n.cy, n.r, 0, Math.PI * 2)
    ctx.fill()
    ctx.strokeStyle = c
    ctx.lineWidth = 1.1
    ctx.globalAlpha = ((lost ? 0.45 : 0.62) + 0.38 * n.lit) * n.fade
    if (lost) ctx.setLineDash([3, 3.5])
    ctx.stroke()
    ctx.setLineDash([])
    ctx.save()
    ctx.translate(n.x, n.y)
    ctx.scale(n.dir * n.k, n.k)
    ctx.rotate(n.rot)
    ctx.globalAlpha = (lost ? 0.55 : 0.95) * n.fade
    drawShape(ctx, n.s, inks[n.d], t + n.ox, n.aim)
    ctx.restore()
    if (n.lit > 0.02) {
      ctx.globalAlpha = n.lit * 0.8 * n.fade
      ctx.strokeStyle = c
      ctx.lineWidth = 1
      ctx.beginPath()
      ctx.arc(n.cx, n.cy, n.r + 2 + (1 - n.lit) * 7, 0, Math.PI * 2)
      ctx.stroke()
    }
  }
  // a tag on one of each type: the catalog's short name
  ctx.font = `600 ${Math.round(9 * Math.min(1.2, g.S))}px ${p.font}`
  spaced(ctx, '0.08em')
  ctx.textBaseline = 'alphabetic'
  const tt = open.title
  for (const n of nodes) {
    if (!n.tag || offscreen(n)) continue
    const side = n.d === 'space'
    const x = side ? n.cx + n.r + 5 : n.cx
    const y = side ? n.cy + 3 : n.cy - n.r - 5
    if (tt && x > tt[0] && x < tt[2] && y > tt[1] && y < tt[3]) continue
    ctx.globalAlpha = 0.72 * n.fade
    ctx.fillStyle = p.dom[n.d]
    ctx.textAlign = side ? 'left' : 'center'
    ctx.fillText(SHAPES[n.s].tag, x, y)
  }
  ctx.globalAlpha = 1
}

/** whether a point is where the panels leave the scene open */
const visible = (o: Open, w: number, x: number, y: number) =>
  x > 0 && x < w && (y < o.top || x < o.left || x > o.right || (o.gap != null && x > o.gap[0] && x < o.gap[1]))

/** `onScene` hears each scene's name as it comes in (the menu shows it, faintly, in its top rail) */
export function SwarmBackdrop({ onScene }: { onScene?: (name: string) => void }) {
  const ref = useRef<HTMLCanvasElement>(null)
  const tell = useRef(onScene)
  useEffect(() => {
    tell.current = onScene
  }, [onScene])
  useEffect(() => {
    const canvas = ref.current
    const ctx = canvas?.getContext('2d')
    if (!canvas || !ctx) return
    const still = window.matchMedia('(prefers-reduced-motion: reduce)')
    const layer = document.createElement('canvas')
    const lctx = layer.getContext('2d')
    if (!lctx) return
    const p = palette(canvas)
    const inks = Object.fromEntries((Object.keys(p.dom) as Dom[]).map((d) => [d, { body: p.dom[d], cut: p.bg, lamp: p.lamp }])) as Record<Dom, Ink>
    let g: Geo | null = null
    let open: Open = { left: 0, right: 0, gap: null, top: 0, title: null }
    let nodes: Node[] = []
    let mines: [number, number, number][] = []
    const net: Net = { links: [], since: new Map(), ghosts: [], cutT: -99, burst: null }
    let sweeps: Sweep[] = []
    let sweepN = 0
    let burstN = 0
    let raf = 0
    let last = 0
    let clock = 0
    let nextSweep = 1.5
    let nextBurst = 7
    let dpr = 1
    // the opening scene, then another at random every so often
    let scene = 0
    let changed = false
    let nextScene = SCENE_S
    const ids = { n: 0 }

    /** to another scene: who is in both stays, who is not fades out, who is new fades in */
    const change = () => {
      if (!g) return
      let k = Math.floor(Math.random() * (SCENES.length - 1))
      if (k >= scene) k++
      scene = changed ? k : FIRST_CHANGE
      changed = true
      tell.current?.(SCENES[scene].name)
      const stay = nodes.filter((n) => n.goal === 1)
      const kept = new Map<Node, Node>()
      const fresh = seed(g, open, scene, ids)
      for (const n of fresh) {
        // a formation's members come in together, never one borrowed from the last scene
        const o = n.sw ? undefined : stay.find((x) => x.s === n.s && !x.sw && ![...kept.values()].includes(x))
        if (o) kept.set(n, o)
      }
      const held = new Set(kept.values())
      for (const o of stay) if (!held.has(o)) o.goal = 0
      for (const n of fresh) {
        if (kept.has(n)) continue
        if (n.host) n.host = kept.get(n.host) ?? n.host
        n.fade = 0
        n.tag = !nodes.some((x) => x.s === n.s && x.tag)
        nodes.push(n)
      }
    }

    /** this frame's links, against the last: what came up, what the terrain cut */
    const relink = (quiet: boolean) => {
      if (!g) return
      const { links, blocked } = network(nodes, g)
      const now = new Set(links.map((l) => l.key))
      for (const l of links) {
        if (net.since.has(l.key)) continue
        net.since.set(l.key, quiet ? -9 : clock)
        // an AUV back in contact
        if (!quiet && l.kind === 'acoustic') l.a.lit = 1
      }
      const prev = new Map(net.links.map((l) => [l.key, l]))
      for (const [k, l] of prev) {
        if (now.has(k)) continue
        net.since.delete(k)
        // a link the terrain cut: shown red, one at a time (the ridge cuts them often)
        const u = blocked.get(k)
        if (!quiet && u != null && clock - net.cutT > CUT_EVERY_S) {
          net.ghosts.push({ a: l.a, b: l.b, u, t0: clock })
          net.cutT = clock
        }
      }
      net.ghosts = net.ghosts.filter((gh) => clock - gh.t0 < GHOST_S)
      net.links = links
      // the ground station's dish follows the drone it is flying (the Puma, or the nearest)
      const gcs = nodes.find((n) => n.s === 'tracker')
      if (gcs) {
        const ctl = links.filter((l) => l.kind === 'control').map((l) => l.b)
        const to = ctl.find((n) => n.s === 'puma') ?? ctl[0]
        if (to) {
          const [dx, dy] = dishAt(gcs)
          const want = clamp(Math.atan2(to.cy - dy, to.cx - dx), -Math.PI, 0)
          gcs.aim += (want - gcs.aim) * (quiet ? 1 : 0.08)
        }
      }
    }
    const frame = () => {
      if (!g) return
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      ctx.clearRect(0, 0, g.w, g.h)
      ctx.drawImage(layer, 0, 0, g.w, g.h)
      for (const s of sweeps) drawSweep(ctx, s, clock, nodes, g, p)
      drawLinks(ctx, net, g, p, clock)
      drawTraffic(ctx, net, g, p, clock)
      drawWakes(ctx, nodes, mines, g, p, clock, inks)
      drawNodes(ctx, nodes, g, p, clock, inks, open)
    }
    const startSweep = () => {
      if (!g) return
      const kind = sweepN++ % 2 ? 'sonar' : 'radar'
      if (kind === 'radar') sweeps = [{ kind, t0: clock, x: g.summit[0], y: g.summit[1] - 4 }]
      else {
        const subs = nodes.filter((n) => n.d === 'subsurface' && !offscreen(n) && n.cx > 0 && n.cx < (g?.w ?? 0))
        const from = subs[Math.floor((clock * 7.3) % Math.max(1, subs.length))]
        if (from) sweeps = [{ kind, t0: clock, x: from.cx, y: from.cy }]
      }
    }
    /** now and then a burst of loss on a link that can be seen */
    const startBurst = () => {
      if (!g) return
      const geo = g
      const seen = net.links.filter((l) => {
        if (l.kind === 'tether') return false
        const [ax, ay] = end(l.a), [bx, by] = end(l.b)
        let k = 0
        for (let i = 1; i < 10; i++) if (visible(open, geo.w, ax + ((bx - ax) * i) / 10, ay + ((by - ay) * i) / 10)) k++
        return k >= 2
      })
      const pick = seen[burstN++ % Math.max(1, seen.length)]
      if (pick) net.burst = { key: pick.key, t0: clock }
    }
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick)
      if (!g || now - last < FRAME_MS) return
      const dt = Math.min(0.1, (now - last) / 1000)
      last = now
      clock += dt
      if (clock > nextScene) {
        change()
        nextScene = clock + SCENE_S
      }
      for (const n of nodes) n.fade = clamp(n.fade + ((n.goal ? 1 : -1) * dt) / FADE_S, 0, 1)
      nodes = nodes.filter((n) => n.goal || n.fade > 0)
      step(nodes, g, dt, clock, mines)
      relink(false)
      if (net.burst && clock - net.burst.t0 > BURST_S) {
        // recovered: it comes back as a fresh link
        net.since.set(net.burst.key, clock)
        net.burst = null
      }
      if (clock > nextSweep) {
        startSweep()
        nextSweep = clock + SWEEP_EVERY_S
      }
      if (clock > nextBurst) {
        startBurst()
        nextBurst = clock + 10 + 5 * hash(clock)
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
      g = geometry(w, h)
      open = measure(canvas, w)
      nodes = seed(g, open, scene, ids)
      mines = []
      net.links = []
      net.since.clear()
      net.ghosts = []
      net.burst = null
      // a still frame shows them already under way
      for (let k = 0; k < 150; k++) {
        clock += 1 / 30
        step(nodes, g, 1 / 30, clock, mines)
        relink(true)
      }
      nextBurst = clock + 6
      nextScene = clock + SCENE_S
      lctx.setTransform(dpr, 0, 0, dpr, 0, 0)
      drawStatic(lctx, g, p, open)
      frame()
    }
    const ro = new ResizeObserver(resize)
    ro.observe(canvas)
    resize()
    tell.current?.(SCENES[scene].name)
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

/** The backdrop's link styles, for the menu's top rail: quiet, and only where there is room. */
export function SwarmLegend() {
  return (
    <span className="mm-legend" aria-hidden="true">
      {(Object.keys(LINKS) as LinkKind[]).map((k) => (
        <span key={k}>
          <svg viewBox="0 0 18 6" width="18" height="6">
            <line x1="0" y1="3" x2="18" y2="3" stroke={`var(${LINKS[k].color})`} strokeWidth={Math.max(1.2, LINKS[k].width)} strokeDasharray={LINKS[k].dash.join(' ') || undefined} />
          </svg>
          {LINKS[k].label}
        </span>
      ))}
    </span>
  )
}
