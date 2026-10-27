/** The main menu backdrop's nodes (SwarmBackdrop.tsx): a silhouette for each platform type in
 *  sim/platforms.toml and for each kind of satellite the space segment knows (space.ts), and a
 *  crewed fighter that leads the loyal-wingman scene's CCAs (the one shape no catalog type has), drawn
 *  side on as a person would recognise it. Each faces +x (the backdrop mirrors it), y down, in
 *  CSS px at scale 1, with its origin at its centre (aircraft, satellites, the deep), on the
 *  ground (ground vehicles) or on the waterline (boats). It is filled in its domain's colour,
 *  its details cut in the background's, with a halo of the background round it so the links
 *  behind it don't run into it. */

export type Shape =
  | 'rq4'
  | 'mq9b'
  | 'mq9b_armed'
  | 'cca'
  | 'fighter'
  | 'fvr90'
  | 'puma'
  | 'switchblade'
  | 'x10'
  | 'fpv'
  | 'lta'
  | 'ugv_logistics'
  | 'ugv_armed'
  | 'ugv_mines'
  | 'ugv_recon'
  | 'tracker'
  | 'mine'
  | 'magura'
  | 'corsair'
  | 'seahunter'
  | 'saildrone'
  | 'buoy'
  | 'bluerov'
  | 'remus'
  | 'orca'
  | 'sat_optical'
  | 'sat_sar'
  | 'sat_satcom'
  | 'sat_leo'
  | 'sat_gnss'
  | 'iss'

/** The networks the backdrop draws between its nodes, each in its own style (the legend in the
 *  menu's top rail reads them too): `color` is a CSS custom property of the menu. */
export type LinkKind = 'mesh' | 'datalink' | 'control' | 'satcom' | 'acoustic' | 'tether' | 'lost'
export const LINKS: Record<LinkKind, { label: string; color: string; dash: number[]; width: number; alpha: number }> = {
  mesh: { label: 'Mesh radio', color: '--mm-link', dash: [], width: 0.9, alpha: 0.4 },
  datalink: { label: 'Data link', color: '--dom-air', dash: [7, 3], width: 0.9, alpha: 0.45 },
  control: { label: 'Drone control', color: '--dom-ground', dash: [3, 2], width: 0.9, alpha: 0.42 },
  satcom: { label: 'SATCOM', color: '--dom-space', dash: [1.5, 4], width: 1, alpha: 0.5 },
  acoustic: { label: 'Acoustic', color: '--dom-subsurface', dash: [1, 3.5], width: 1.3, alpha: 0.6 },
  tether: { label: 'Tether', color: '--mm-tether', dash: [], width: 1.2, alpha: 0.75 },
  lost: { label: 'Link lost', color: '--bad', dash: [3, 3], width: 1.1, alpha: 0.8 },
}

/** body: filled; soft: a translucent fill (a far fin); glow: fainter still (a light, an exhaust);
 *  line: a stroke (a mast, a boom); thin: a faint stroke; cut and cutline: the background's colour
 *  over the body (wheels, windows, panel cells) */
type Kind = 'body' | 'soft' | 'glow' | 'line' | 'thin' | 'cut' | 'cutline'
interface Part {
  d: string
  k: Kind
  w?: number
}
export interface Def {
  /** the tag beside it: the catalog's short name (a satellite: what it does) */
  tag: string
  /** its extent round the origin: x0, y0, x1, y1 */
  box: [number, number, number, number]
  parts: Part[]
  /** rotors seen edge on: x, y, half span */
  rotors?: [number, number, number][]
  /** propellers seen edge on: x, y, half span */
  props?: [number, number, number][]
  /** a light that blinks */
  light?: [number, number]
  /** a dish on a pan head that the backdrop aims: its pivot */
  dish?: [number, number]
}

// ---- path helpers --------------------------------------------------------------------------------

const f = (v: number) => +v.toFixed(2)
const rect = (x0: number, y0: number, x1: number, y1: number) => `M${x0} ${y0}H${x1}V${y1}H${x0}Z`
const ellipse = (cx: number, cy: number, rx: number, ry: number) => `M${cx + rx} ${cy}A${rx} ${ry} 0 1 0 ${cx - rx} ${cy}A${rx} ${ry} 0 1 0 ${cx + rx} ${cy}Z`
const circle = (cx: number, cy: number, r: number) => ellipse(cx, cy, r, r)
const lines = (xs: number[], y0: number, y1: number) => xs.map((x) => `M${x} ${y0}V${y1}`).join('')
const rows = (ys: number[], x0: number, x1: number) => ys.map((y) => `M${x0} ${y}H${x1}`).join('')
const body = (d: string): Part => ({ d, k: 'body' })
const cut = (d: string): Part => ({ d, k: 'cut' })
const cutline = (d: string, w = 0.7): Part => ({ d, k: 'cutline', w })
const line = (d: string, w = 1): Part => ({ d, k: 'line', w })

/** a solar panel: its cells cut into it */
function panel(x0: number, y0: number, x1: number, y1: number, cols: number, rowsN = 2): Part[] {
  const xs = Array.from({ length: cols - 1 }, (_, i) => f(x0 + ((x1 - x0) * (i + 1)) / cols))
  const ys = Array.from({ length: rowsN - 1 }, (_, i) => f(y0 + ((y1 - y0) * (i + 1)) / rowsN))
  return [body(rect(x0, y0, x1, y1)), cutline(lines(xs, y0, y1) + rows(ys, x0, x1), 0.55)]
}

/** a dish seen side on, opening towards `ang` (rad, y down), centred on (cx, cy) */
function dishPath(cx: number, cy: number, r: number, ang: number) {
  const c = Math.cos(ang), s = Math.sin(ang)
  const p = (x: number, y: number) => `${f(cx + x * c - y * s)} ${f(cy + x * s + y * c)}`
  return `M${p(0.1 * r, -r)}Q${p(-0.62 * r, 0)} ${p(0.1 * r, r)}L${p(0.34 * r, 0.9 * r)}Q${p(-0.28 * r, 0)} ${p(0.34 * r, -0.9 * r)}Z`
}

// ---- the shapes ----------------------------------------------------------------------------------

const TRACKS = 'M-10.5 0L10.5 0C12.2 0 13 -1.2 13 -2.6L11.6 -5.4L-11.6 -5.4L-13 -2.6C-13 -1.2 -12.2 0 -10.5 0Z'
const tracked = (): Part[] => [
  body(TRACKS),
  cut([-9.4, -4.7, 0, 4.7, 9.4].map((x) => circle(x, -2.6, 1.7)).join('')),
  body('M-11.6 -5.6L-11.6 -9.4L6.5 -9.4L11.6 -6.4L11.6 -5.6Z'),
  cutline('M-11.4 -5.5H11.4', 0.9),
]

export const SHAPES: Record<Shape, Def> = {
  // ---- air: aircraft at their centre ----
  rq4: {
    tag: 'RQ-4',
    box: [-21.5, -9.6, 21, 2.8],
    parts: [
      body('M21 0.4C21 -2.6 18.5 -5.2 13 -5.4C8.5 -5.5 5.5 -3.2 1 -2.6L-17 -1.4L-21 -1.1L-21 0.7L-17 1L9 2.6C16 2.8 21 2.2 21 0.4Z'),
      body('M-2.6 -2.5L-2.6 -5.6L-12.6 -5.4C-15.2 -5.3 -16.6 -4.2 -16 -3.4L-14.6 -1.6Z'),
      body('M-13.4 -1.8L-18.8 -9.6L-21.4 -9.6L-19.6 -1.3Z'),
      cutline('M-2.6 -5.2V-2.8', 1),
      cutline('M9 0.9L-6 0.2', 0.6),
    ],
  },
  mq9b: {
    tag: 'MQ-9B',
    box: [-17.4, -7.8, 17.5, 5.8],
    parts: [
      body('M17.5 0.6C17.5 -1.8 15.5 -3.6 12 -3.7C9 -3.8 7 -2.4 4 -2L-14 -1L-15.4 -0.6L-15.4 0.8L-14 1.2L9 2.5C14 2.8 17.5 2.4 17.5 0.6Z'),
      body(circle(9.6, 3.8, 1.9)),
      body('M-9 -1.3L-15 -7.8L-16.8 -7.8L-14.2 -1Z'),
      body('M-10 1.2L-14 5.8L-15.4 5.8L-13.8 1Z'),
      cut(circle(10.4, 4.1, 0.7)),
      cutline('M7 0.6L-5 0.1', 0.6),
    ],
    props: [[-16.4, 0, 5.6]],
  },
  mq9b_armed: {
    tag: 'MQ-9B',
    box: [-17.4, -7.8, 17.5, 6.8],
    parts: [
      body('M17.5 0.6C17.5 -1.8 15.5 -3.6 12 -3.7C9 -3.8 7 -2.4 4 -2L-14 -1L-15.4 -0.6L-15.4 0.8L-14 1.2L9 2.5C14 2.8 17.5 2.4 17.5 0.6Z'),
      body(circle(9.6, 3.8, 1.9)),
      body('M-9 -1.3L-15 -7.8L-16.8 -7.8L-14.2 -1Z'),
      body('M-10 1.2L-14 5.8L-15.4 5.8L-13.8 1Z'),
      line('M1.6 1.6V3.4', 0.9),
      body('M6 3.4L-2.8 3.4L-3.6 4.7L6 4.7L7 4.05Z'),
      body('M4.4 5.5L-4.4 5.5L-5.2 6.8L4.4 6.8L5.4 6.15Z'),
      cut(circle(10.4, 4.1, 0.7)),
      cutline('M7 0.6L-5 0.1', 0.6),
    ],
    props: [[-16.4, 0, 5.6]],
  },
  cca: {
    tag: 'CCA',
    box: [-21, -10, 16.6, 2.6],
    parts: [
      { d: 'M-9.6 -3.6L-15.2 -8.8L-17.2 -8.8L-15 -2.4Z', k: 'soft' },
      body('M16.6 0.6L7 -1.6L1.4 -2.6L-2 -4.2L-9 -4.2L-14 -2.4L-16 -1.6L-16 1.4L-12 2.2L4 2.4Z'),
      body('M-7.4 -4L-13.4 -10L-16 -10L-13.4 -2.6Z'),
      { d: 'M-16 -1.1L-21 0.1L-16 1.1Z', k: 'glow' },
      cutline('M-1.8 -4L0.8 -2.7', 1),
      cutline('M-16 -1.2V1.1', 1.2),
      cutline('M6 0.9L-10 0.9', 0.6),
    ],
  },
  fighter: {
    // crewed, leading its CCAs: a bubble canopy, a side intake, twin canted tails
    tag: 'LEAD',
    box: [-25, -10.4, 22, 2.6],
    parts: [
      { d: 'M-9.4 -3.2L-14.6 -9L-16.8 -9L-16.2 -2.6Z', k: 'soft' },
      body('M22 0.4L14 -1.6L9 -2.6L-4 -3.2L-14 -3L-19 -1.8L-20 -1.2L-20 1.4L-14 2L2 2.6L12 1.8Z'),
      body('M13 -1.9C11.4 -6.8 5.2 -7.6 0.6 -3Z'),
      cut('M11.7 -2.3C10.3 -6 5.6 -6.5 2.2 -3.1Z'),
      body('M-11 -3L-16.5 -10.4L-19 -10.4L-18.6 -2.2Z'),
      body('M-13 1L-21.5 2.4L-22 1.2L-15 0.2Z'),
      cut('M3.2 -1.6L-1 -1.6L-1 0.8L2 0.8Z'),
      { d: 'M-20 -0.8L-25 0.2L-20 1.2Z', k: 'glow' },
      cutline('M8 0.9L-11 0.9', 0.6),
    ],
  },
  fvr90: {
    tag: 'VTOL',
    box: [-19.2, -10, 17, 3],
    parts: [
      body('M11.5 0.5C11.5 -2.5 9 -3.6 6 -3.6L-6 -3C-8 -2.8 -8.5 -1.5 -8.5 0C-8.5 1.5 -8 2.6 -6 2.8L6 3C9.5 3 11.5 2.5 11.5 0.5Z'),
      line('M13.6 -4.8H-16.4', 1.3),
      line('M11 -4.8V-6.8M-11 -4.8V-6.8', 1.1),
      body('M-14 -5L-17.4 -10L-19.2 -10L-17.2 -4.4Z'),
    ],
    rotors: [
      [11, -7.3, 6],
      [-11, -7.3, 6],
    ],
    props: [[-9.4, 0, 3.6]],
  },
  puma: {
    tag: 'PUMA',
    box: [-10.8, -5, 10.8, 3.8],
    parts: [
      body('M9.5 0.4C9.5 -1.4 8 -2 6 -2L-7 -0.9L-9.5 -0.6L-9.5 0.6L-7 0.9L6 2.2C8 2.2 9.5 1.8 9.5 0.4Z'),
      body('M5.6 -4.8C6.8 -4.8 7 -3.6 5.6 -3.5L-3 -3.6L-3 -4.3Z'),
      body('M3.6 -3.6L1.6 -1.5L-1 -1.2L0 -3.6Z'),
      body('M-6 -0.8L-9.4 -5L-10.8 -5L-9.6 -0.6Z'),
      body(circle(2.2, 2.6, 1.2)),
    ],
    props: [[10.2, 0.3, 3.6]],
  },
  switchblade: {
    tag: 'LM',
    box: [-10.6, -7.6, 10, 7.6],
    parts: [
      body('M10 0C10 -1.6 8.8 -2 7 -2L-8 -2L-9.6 -1.2L-9.6 1.2L-8 2L7 2C8.8 2 10 1.6 10 0Z'),
      body('M3.6 -1.8L0 -7.6L-1.4 -7.6L1.2 -1.8Z'),
      body('M3.6 1.8L0 7.6L-1.4 7.6L1.2 1.8Z'),
      body('M-4.4 -1.8L-7.4 -6.6L-8.8 -6.6L-6.8 -1.8Z'),
      body('M-4.4 1.8L-7.4 6.6L-8.8 6.6L-6.8 1.8Z'),
      cut(circle(8.2, 0, 0.9)),
      cutline('M5 -1.9V1.9', 0.8),
    ],
    props: [[-10.4, 0, 3.6]],
  },
  x10: {
    tag: 'QUAD',
    box: [-13.6, -6.8, 13.6, 4.8],
    parts: [
      line('M-3 -2.4L-9 -3.6M3 -2.4L9 -3.6', 1.4),
      body('M-4.6 -2C-4.6 -3.8 -3 -4.4 0 -4.4C3 -4.4 4.6 -3.8 4.6 -2L4.6 0.6C4.6 1.6 3.6 2 2 2L-2 2C-3.6 2 -4.6 1.6 -4.6 0.6Z'),
      body(rect(7.8, -5.4, 10.2, -3) + rect(-10.2, -5.4, -7.8, -3)),
      body(circle(4.2, 2.9, 1.7)),
      line('M-2.6 2L-3.8 4.8M1.2 2L1.8 4.8', 0.9),
      cut(circle(4.9, 3, 0.65)),
    ],
    rotors: [
      [9, -5.9, 4.6],
      [-9, -5.9, 4.6],
    ],
  },
  fpv: {
    tag: 'FPV',
    box: [-11.4, -5.4, 11.4, 4],
    parts: [
      line('M-7 -1.4H7', 1.3),
      body(rect(6, -3.4, 8, -1.4) + rect(-8, -3.4, -6, -1.4)),
      body(rect(-4.2, -4.2, 0.6, -1.6)),
      body('M1.4 -1.6L1.8 -4.4L4.6 -4L4.2 -1.6Z'),
      body('M-5.4 -0.4L2.6 -0.4L10.2 1.4L2.6 3.2L-5.4 3.2Z'),
      body('M-5.4 -0.6L-8.4 -1.8L-8.4 4L-5.4 3.4Z'),
      cut(circle(3.4, -2.8, 0.7)),
      cutline('M-2.6 -0.6V3.4M0.6 -0.6V3.4', 0.7),
    ],
    rotors: [
      [7, -3.9, 4.4],
      [-7, -3.9, 4.4],
    ],
  },
  lta: {
    tag: 'LTA',
    box: [-20, -11, 20, 9.6],
    parts: [
      { d: 'M-11 -1.4L-20 -2L-20 0L-11 -0.6Z', k: 'soft' },
      body('M-9.6 -5L-16.2 -11L-18.8 -11L-17.4 -2.6Z'),
      body('M-9.6 3L-16.2 9L-18.8 9L-17.4 0.6Z'),
      body('M20 -1C20 -5.5 14 -7.8 5 -7.8C-5 -7.8 -13 -5 -17 -1C-13 3 -5 5.8 5 5.8C14 5.8 20 3.5 20 -1Z'),
      body('M8.8 5.2L-0.6 5.2L-0.6 8C-0.6 9 0 9.6 1 9.6L7.4 9.6C8.4 9.6 8.8 9 8.8 8Z'),
      cutline('M18.6 -3.6C10 -6.2 -2 -6 -11.6 -3.4M18.6 1.6C10 4.2 -2 4 -11.6 1.4', 0.6),
      cutline('M0.6 7.4H7.4', 0.9),
    ],
    props: [[-1.6, 7.4, 2]],
  },

  // ---- ground: standing on the ground ----
  ugv_logistics: {
    tag: 'UGV-L',
    box: [-13, -15, 13, 0],
    parts: [
      ...tracked(),
      body(rect(-10, -15, -3, -9.4) + rect(-2, -13.4, 5, -9.4)),
      cutline('M-10 -15L-3 -9.6M-3 -15L-10 -9.6M-2 -13.4L5 -9.6M5 -13.4L-2 -9.6', 0.6),
      cutline('M-10.6 -9.5H5.6', 0.8),
    ],
  },
  ugv_armed: {
    tag: 'UGV-A',
    box: [-13, -17.6, 18.6, 0],
    parts: [
      ...tracked(),
      body('M-5 -9.4L-5 -12.6L-2.6 -14.6L3.6 -14.6L5.6 -12.6L5.6 -9.4Z'),
      line('M4.5 -12.4H17', 1.6),
      body(rect(16.4, -13.4, 18.6, -11.4)),
      body(rect(-3.4, -17.6, 0.8, -14.6)),
      cutline('M0.6 -16.9V-15.3', 0.9),
      cutline('M-5 -9.5H5.6', 0.8),
    ],
  },
  ugv_mines: {
    tag: 'UGV-M',
    box: [-13, -17, 13, 0],
    parts: [
      ...tracked(),
      body(rect(-11.2, -17, 2.6, -9.4)),
      cut([-8.6, -4.5, -0.4].flatMap((x) => [circle(x, -14.6, 1.15), circle(x, -11.8, 1.15)]).join('')),
      cutline('M-11.2 -9.5H2.6', 0.8),
    ],
  },
  ugv_recon: {
    tag: 'UGV-R',
    box: [-7.6, -17.8, 7.6, 0],
    parts: [
      body(circle(-4.6, -2.7, 2.7) + circle(4.6, -2.7, 2.7)),
      body('M-7.4 -3.8L-7.4 -7.2L4.6 -7.2L7.6 -5.2L7.6 -3.8Z'),
      line('M-3 -7.2V-14.6', 1.2),
      line('M5.4 -7.2L6.8 -13', 0.7),
      body(rect(-5.8, -17.8, 0.8, -14.6)),
      cut(circle(-4.6, -2.7, 1) + circle(4.6, -2.7, 1) + circle(-0.8, -16.2, 0.9)),
      cutline('M-7.2 -3.9H7.4', 0.8),
    ],
  },
  tracker: {
    tag: 'ANT',
    box: [-6, -17, 6, 0],
    parts: [line('M0 -7.5L-5.5 0M0 -7.5L5.5 0M0 -7.5L0.6 0', 1.1), body(rect(-2, -10.4, 2, -7.5))],
    dish: [0, -11.4],
  },
  mine: {
    tag: '',
    box: [-2.4, -1.8, 2.4, 0],
    parts: [body('M-2.4 0C-2.4 -1.8 2.4 -1.8 2.4 0Z')],
  },

  // ---- surface: on the waterline ----
  magura: {
    tag: 'USV-S',
    box: [-11, -8.6, 11.5, 1.7],
    parts: [
      body('M11.5 -3.8L-10.6 -2.4L-11 1L-5 1.7L4 1.4C7.5 0.8 10.2 -1.4 11.5 -3.8Z'),
      body('M-6 -2.8L-4.8 -4.8L1.4 -5L3.6 -3.3Z'),
      line('M-3.6 -4.8L-4.2 -8.6', 0.7),
      cutline('M9.6 -2.6L-10.4 -1.2', 0.7),
    ],
  },
  corsair: {
    tag: 'USV-M',
    box: [-14, -19, 14, 2],
    parts: [
      body('M14 -5.6L-13.4 -3.8L-14 -3.8L-14 1L8 2C11 1.4 13 -1.8 14 -5.6Z'),
      body('M-6.5 -4.2L-6.5 -9.4L2 -9.4L5.6 -4.8Z'),
      line('M-2.4 -9.4V-16.6', 1),
      line('M-5 -14.2H0.2', 1.2),
      body(circle(-2.4, -17.6, 1.5)),
      line('M-13.2 -3.8L-11.2 -10.2L-9.2 -4', 1),
      cutline('M-5.2 -7.8H2.4', 1.2),
    ],
  },
  seahunter: {
    tag: 'MUSV',
    box: [-19.6, -20.6, 20, 1.8],
    parts: [
      body('M20 -5L19 1.8L-14 1.6L-19.6 -0.2L-19.6 -3.4L2 -3.8Z'),
      body('M-3 -1.6L-17.2 -1.3L-18.6 0.4L-15.6 1.8L-4 1.7C-2.4 1.2 -2.2 -0.4 -3 -1.6Z'),
      body('M-4 -3.8L-4 -9.6L7.4 -9.6L11.6 -4.4Z'),
      line('M1.6 -9.6V-19.2', 1.2),
      line('M-1.6 -16.6H4.8M0 -13H3.2', 1),
      body(rect(-0.6, -20.6, 3.8, -19.2)),
      cutline('M-3 -1.6L-17.2 -1.3L-18.6 0.4L-15.6 1.8L-4 1.7C-2.4 1.2 -2.2 -0.4 -3 -1.6Z', 0.8),
      cutline('M-2.6 -8.1H7.6', 1.2),
    ],
  },
  saildrone: {
    tag: 'SAIL',
    box: [-11, -24, 11, 9],
    parts: [
      body('M11 -1.6L-10 -1.2L-11 0.8L7 1.2C9.5 0.8 10.6 -0.4 11 -1.6Z'),
      body('M1.6 1L0.6 9L-2.6 9L-1.6 1Z'),
      body('M2.8 -1.4L2.2 -22.6C2 -23.8 -0.4 -24 -1.8 -22.8L-3 -1.4Z'),
      line('M-2.4 -19.4H-8.4', 0.8),
      body(rect(-9.8, -22, -8.2, -16.8)),
      cutline('M2.6 -8H-2.8M2.4 -15H-2.4', 0.6),
    ],
  },
  buoy: {
    tag: 'BUOY',
    box: [-4.6, -13.8, 4.6, 12],
    parts: [
      { d: 'M0 3V12', k: 'thin', w: 0.7 },
      line('M0 -3V-11.4', 1.1),
      body('M-4.6 -1.4C-4.6 -3.6 4.6 -3.6 4.6 -1.4L4 1.6C2 3.4 -2 3.4 -4 1.6Z'),
      body('M-4.2 -7.4L-0.4 -9.2L-0.4 -7.4L-4.2 -5.6Z'),
      cutline('M-4.5 -0.8H4.5', 0.8),
    ],
    light: [0, -12.6],
  },

  // ---- subsurface: at their centre ----
  bluerov: {
    tag: 'ROV',
    box: [-8, -4.6, 11.2, 4.6],
    parts: [
      { d: 'M10 0L30 -7L30 7Z', k: 'glow' },
      body('M-8 -3Q-8 -4.6 -6.4 -4.6L6.4 -4.6Q8 -4.6 8 -3L8 3Q8 4.6 6.4 4.6L-6.4 4.6Q-8 4.6 -8 3Z'),
      body('M8 -3.2A3.2 3.2 0 0 1 8 3.2Z'),
      cut(circle(-4.6, 1.9, 1.5) + circle(1.6, 1.9, 1.5)),
      cutline('M-7.6 -1.2H7.6', 0.9),
      cutline('M8 -3.2V3.2', 0.7),
    ],
  },
  remus: {
    tag: 'AUV',
    box: [-12.4, -6.6, 13, 6.6],
    parts: [
      body('M13 0C13 -2.3 11.2 -3 9 -3L-7 -3L-11.6 -1.1L-11.6 1.1L-7 3L9 3C11.2 3 13 2.3 13 0Z'),
      body('M-7.6 -2.6L-10.4 -6.6L-11.8 -6.6L-11 -1.2Z'),
      body('M-7.6 2.6L-10.4 6.6L-11.8 6.6L-11 1.2Z'),
      line('M3 -3V-5', 0.8),
      cutline('M6 -2.9V2.9M-2 -2.9V2.9', 0.7),
    ],
    props: [[-12.4, 0, 2.8]],
  },
  orca: {
    tag: 'XLUUV',
    box: [-21.4, -13.4, 19, 10.2],
    parts: [
      { d: 'M-14 -0.6L-21.4 -0.9L-21.4 0.9L-14 0.6Z', k: 'soft' },
      body('M19 0C19 -3.6 16.6 -4.8 13 -4.8L-12 -4.8L-19 -1.6L-19 1.6L-12 4.8L13 4.8C16.6 4.8 19 3.6 19 0Z'),
      body('M9.4 -4.6L8.2 -10.6L3 -10.6L1.6 -4.6Z'),
      line('M6.2 -10.6V-13.4', 0.8),
      body('M-13.6 -4.2L-17.4 -10.2L-19.4 -10.2L-18.2 -2.6Z'),
      body('M-13.6 4.2L-17.4 10.2L-19.4 10.2L-18.2 2.6Z'),
      cutline('M-5 -4.7V4.7M11 -4.7V4.7', 0.7),
    ],
    props: [[-20.2, 0, 3.4]],
  },

  // ---- space: at their centre, the Earth below ----
  sat_optical: {
    tag: 'EO SAT',
    box: [-12.6, -4.6, 12.6, 6.8],
    parts: [
      line('M-3.6 -0.5H-5.6M3.6 -0.5H5.6', 0.8),
      body(rect(-3.6, -4.6, 3.6, 3.6)),
      body(rect(-2.4, 3.6, 2.4, 6.8)),
      ...panel(-12.6, -3.2, -5.6, 2.2, 3),
      ...panel(5.6, -3.2, 12.6, 2.2, 3),
      cut(rect(-1.6, 5.9, 1.6, 6.8)),
      cutline('M-3.6 -1.6H3.6', 0.6),
    ],
  },
  sat_sar: {
    tag: 'SAR SAT',
    box: [-14, -12.6, 14, 2.2],
    parts: [
      line('M0 -7V-8.6', 0.8),
      body(rect(-3.2, -7, 3.2, -1.4)),
      ...panel(-6.4, -12.6, 6.4, -8.6, 4),
      body(rect(-14, -0.6, 14, 2.2)),
      cutline(lines([-10.5, -7, -3.5, 0, 3.5, 7, 10.5], -0.6, 2.2), 0.6),
      cutline('M-3.2 -4.2H3.2', 0.6),
    ],
  },
  sat_satcom: {
    tag: 'SATCOM',
    box: [-20, -5.2, 20, 10.4],
    parts: [
      line('M-4.6 0H-7.4M4.6 0H7.4M-2.6 5.2L-5.4 7.8M2.6 5.2L5.4 7.8', 0.9),
      body(rect(-4.6, -5.2, 4.6, 5.2)),
      ...panel(-20, -3, -7.4, 3, 4),
      ...panel(7.4, -3, 20, 3, 4),
      // two dishes, turned towards the Earth: rim, bowl and feed
      body(ellipse(-6, 8, 4, 2.4) + ellipse(6, 8, 4, 2.4)),
      cut(ellipse(-6, 7.7, 2.8, 1.5) + ellipse(6, 7.7, 2.8, 1.5)),
      body(circle(-6, 7.8, 0.75) + circle(6, 7.8, 0.75)),
      cutline('M-4.6 -1.8H4.6M-4.6 1.8H4.6', 0.6),
    ],
  },
  sat_leo: {
    tag: 'LEO SATCOM',
    box: [-14, -4.4, 14, 4.4],
    parts: [
      line('M-4 0H-1.6', 0.9),
      body(rect(-14, -1.5, -4, 1.5)),
      ...panel(-1.6, -4.4, 14, 4.4, 6),
      cutline(lines([-11.5, -9, -6.5], -1.5, 1.5), 0.6),
    ],
  },
  sat_gnss: {
    tag: 'GNSS',
    box: [-15.6, -4.6, 15.6, 8.4],
    parts: [
      line('M-4.2 0H-6.2M4.2 0H6.2', 0.9),
      body(rect(-4.2, -4.6, 4.2, 4.6)),
      ...panel(-15.6, -2.6, -6.2, 2.6, 3),
      ...panel(6.2, -2.6, 15.6, 2.6, 3),
      body(rect(-3.6, 4.6, 3.6, 5.6)),
      line(lines([-2.6, -0.9, 0.9, 2.6], 5.6, 8.4), 0.9),
      cutline('M-4.2 -1.4H4.2', 0.6),
    ],
  },
  iss: {
    tag: 'ISS',
    box: [-22, -11.4, 22, 11.4],
    parts: [
      body(rect(-22, -0.9, 22, 0.9)),
      ...[-20, -14.6, 14.6, 20].flatMap((x) => [
        body(rect(x - 1.7, -11.4, x + 1.7, -1.4) + rect(x - 1.7, 1.4, x + 1.7, 11.4)),
        cutline(rows([-9.4, -7.4, -5.4, -3.4, 3.4, 5.4, 7.4, 9.4], x - 1.7, x + 1.7) + `M${x} -11.4V-1.4M${x} 1.4V11.4`, 0.5),
      ]),
      { d: rect(-8.6, -6.4, -6, -1.4) + rect(6, 1.4, 8.6, 6.4), k: 'soft' },
      body(rect(-1.8, -5.8, 1.8, 5.8) + rect(-4.6, -1.8, 4.6, 1.8)),
      cutline('M-1.8 -3H1.8M-1.8 3H1.8', 0.5),
    ],
  },
}

// ---- drawing -------------------------------------------------------------------------------------

export interface Ink {
  body: string
  /** the background: details and the halo */
  cut: string
  /** a light when it is on */
  lamp: string
}

/** The scale that fits shape `s` inside a ring of radius `r` (its box's corners on 94 % of the
 *  radius), and its box's centre at scale 1. */
export function fit(s: Shape, r: number) {
  const [x0, y0, x1, y1] = SHAPES[s].box
  return { k: (0.94 * r) / Math.hypot((x1 - x0) / 2, (y1 - y0) / 2), cx: (x0 + x1) / 2, cy: (y0 + y1) / 2 }
}

const HALO = 2.4
const compiled = new Map<Shape, { p: Path2D; k: Kind; w: number }[]>()
let dish: Path2D | null = null

function parts(s: Shape) {
  let c = compiled.get(s)
  if (!c) {
    c = SHAPES[s].parts.map((p) => ({ p: new Path2D(p.d), k: p.k, w: p.w ?? 1 }))
    compiled.set(s, c)
  }
  return c
}

/** Draws shape `s` at the origin of `ctx`'s transform, at its global alpha: `t` (s) turns its
 *  rotors and blinks its light, `aim` (rad, y down) points its dish. */
export function drawShape(ctx: CanvasRenderingContext2D, s: Shape, ink: Ink, t: number, aim = -Math.PI / 4) {
  const def = SHAPES[s]
  const ps = parts(s)
  const a = ctx.globalAlpha
  ctx.lineCap = 'round'
  ctx.lineJoin = 'round'
  ctx.strokeStyle = ink.cut
  for (const p of ps) {
    if (p.k !== 'body' && p.k !== 'line') continue
    ctx.lineWidth = (p.k === 'line' ? p.w : 0) + HALO
    ctx.stroke(p.p)
  }
  for (const p of ps) {
    ctx.globalAlpha = p.k === 'soft' ? a * 0.42 : p.k === 'glow' ? a * 0.13 : p.k === 'thin' ? a * 0.5 : a
    const col = p.k === 'cut' || p.k === 'cutline' ? ink.cut : ink.body
    if (p.k === 'body' || p.k === 'soft' || p.k === 'glow' || p.k === 'cut') {
      ctx.fillStyle = col
      ctx.fill(p.p)
    } else {
      ctx.strokeStyle = col
      ctx.lineWidth = p.w
      ctx.stroke(p.p)
    }
  }
  ctx.fillStyle = ink.body
  ctx.strokeStyle = ink.body
  ctx.lineWidth = 1
  for (const [x, y, r] of def.rotors ?? []) {
    ctx.globalAlpha = a * 0.3
    ctx.beginPath()
    ctx.ellipse(x, y, r, 0.9, 0, 0, Math.PI * 2)
    ctx.fill()
    const b = r * Math.abs(Math.cos(t * 7 + x))
    ctx.globalAlpha = a
    ctx.beginPath()
    ctx.moveTo(x - b, y)
    ctx.lineTo(x + b, y)
    ctx.stroke()
  }
  for (const [x, y, r] of def.props ?? []) {
    ctx.globalAlpha = a * 0.3
    ctx.beginPath()
    ctx.ellipse(x, y, 0.9, r, 0, 0, Math.PI * 2)
    ctx.fill()
    const b = r * Math.abs(Math.cos(t * 6 + y))
    ctx.globalAlpha = a
    ctx.beginPath()
    ctx.moveTo(x, y - b)
    ctx.lineTo(x, y + b)
    ctx.stroke()
  }
  if (def.light) {
    const [x, y] = def.light
    const on = t % 1.8 < 0.3
    ctx.globalAlpha = on ? a * 0.3 : a * 0.6
    ctx.fillStyle = on ? ink.lamp : ink.body
    ctx.beginPath()
    ctx.arc(x, y, on ? 4.2 : 1.3, 0, Math.PI * 2)
    ctx.fill()
    if (on) {
      ctx.globalAlpha = a
      ctx.beginPath()
      ctx.arc(x, y, 1.6, 0, Math.PI * 2)
      ctx.fill()
    }
  }
  if (def.dish) {
    dish ??= new Path2D(dishPath(0, 0, 6, 0))
    ctx.save()
    ctx.translate(def.dish[0], def.dish[1])
    ctx.rotate(aim)
    ctx.globalAlpha = a
    ctx.strokeStyle = ink.cut
    ctx.lineWidth = HALO
    ctx.stroke(dish)
    ctx.fillStyle = ink.body
    ctx.fill(dish)
    ctx.strokeStyle = ink.body
    ctx.lineWidth = 0.8
    ctx.beginPath()
    ctx.moveTo(-2.4, 0)
    ctx.lineTo(6.4, 0)
    ctx.stroke()
    ctx.beginPath()
    ctx.arc(6.6, 0, 1.1, 0, Math.PI * 2)
    ctx.fill()
    ctx.restore()
  }
  ctx.globalAlpha = a
}
