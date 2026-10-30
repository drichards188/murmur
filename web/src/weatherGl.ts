// The weather's surface effects in one MapLibre custom layer (bd murmur-vr17): the colour field,
// the cloud shadows, the fog and the archive's outline, drawn by one shader over a mesh that lies
// on the surface (sea level over the water, the ground over land: where the boats and every other
// surface drawing are, not the seabed MapLibre drapes on).
//
// The shader finds each pixel's region by the sampler's own rule (the nearest point by squared
// degrees of latitude plus squared degrees of longitude scaled by the cosine of the latitude, the
// first on a tie; inside the archive's bounds, else `outside`), as weatherField.ts nearestRegion
// does and the check holds against the sampler. Everything it draws there is that region's values:
// its field colour, its fog (the mean opacity fogAlpha gives its visibility), its cloud shadow
// (the share of the ground its cloud cover gives), each shape drifting with its own wind.
// `packEffects` is pure and checked; the rest is WebGL 2.
import type * as maplibregl from 'maplibre-gl'
import { weatherValue, type WeatherField } from './weather.ts'
import { MAX_REGIONS, rampColor, type WeatherRegions } from './weatherField.ts'
import { coverThreshold, fogAlpha, NOISE_SIZE } from './weatherNoise.ts'

export interface Effects {
  wind: boolean
  fog: boolean
  rain: boolean
  cloud: boolean
}

/** metres one tile of the noise spans: fog's shapes are a few hundred metres, clouds' km */
export const FOG_TILE_M = 2600
export const CLOUD_TILE_M = 11000

export interface Packed {
  n: number
  /** each point, degrees from the origin (lon, lat) */
  pts: Float32Array
  /** each region's field colour (straight RGBA 0..1), index 0 outside */
  col: Float32Array
  /** each region's fog opacity, cloud threshold (2 for none), field missing (1) or not, 0 */
  fx: Float32Array
  /** each region's drift: fog's and cloud's texture offsets, metres */
  off: Float32Array
}

/** Packs the regions for the shader. `offsets`: each region's drift so far (fog east, north,
 *  cloud east, north, metres), index 0 outside. `sorted`: the noise's distribution. */
export function packEffects(r: WeatherRegions, field: WeatherField | null, origin: [number, number], offsets: Float32Array, sorted: Float32Array): Packed {
  const n = Math.min(MAX_REGIONS, r.points.length)
  const pts = new Float32Array(MAX_REGIONS * 2)
  const col = new Float32Array((MAX_REGIONS + 1) * 4)
  const fx = new Float32Array((MAX_REGIONS + 1) * 4)
  const off = new Float32Array((MAX_REGIONS + 1) * 4)
  for (let i = 0; i < n; i++) {
    pts[2 * i] = r.points[i].lon - origin[0]
    pts[2 * i + 1] = r.points[i].lat - origin[1]
  }
  const values = [r.outside, ...r.points.slice(0, n).map((p) => p.values)]
  values.forEach((v, k) => {
    if (field) {
      const value = weatherValue(v, field)
      const c = rampColor(field, value)
      col.set([c[0] / 255, c[1] / 255, c[2] / 255, c[3]], 4 * k)
      fx[4 * k + 2] = value == null ? 1 : 0
    }
    fx[4 * k] = fogAlpha(v.visibility_m)
    fx[4 * k + 1] = coverThreshold(sorted, v.cloud_cover_pct == null ? null : v.cloud_cover_pct / 100)
    off.set(offsets.subarray(4 * k, 4 * k + 4), 4 * k)
  })
  return { n, pts, col, fx, off }
}

const VS = `#version 300 es
uniform mat4 u_matrix;
in vec3 a_pos;
in vec2 a_ll;
in vec2 a_m;
out vec2 v_ll;
out vec2 v_m;
void main() {
  v_ll = a_ll;
  v_m = a_m;
  gl_Position = u_matrix * vec4(a_pos, 1.0);
}`

const FS = `#version 300 es
precision highp float;
#define MAXR ${MAX_REGIONS}
in vec2 v_ll;
in vec2 v_m;
uniform float u_lat0;
uniform int u_n;
uniform vec2 u_pt[MAXR];
uniform vec4 u_col[MAXR + 1];
uniform vec4 u_fx[MAXR + 1];
uniform vec4 u_off[MAXR + 1];
uniform vec4 u_bounds;
uniform float u_hasBounds;
uniform vec4 u_on;
uniform sampler2D u_noise;
uniform vec2 u_tile;
out vec4 fragColor;

int region() {
  if (u_hasBounds < 0.5) return -1;
  if (v_ll.x < u_bounds.x || v_ll.y < u_bounds.y || v_ll.x > u_bounds.z || v_ll.y > u_bounds.w) return -1;
  float c = cos(radians(u_lat0 + v_ll.y));
  int best = 0;
  float bd = 1e30;
  for (int i = 0; i < MAXR; i++) {
    if (i >= u_n) break;
    vec2 d = u_pt[i] - v_ll;
    d.x *= c;
    float dd = dot(d, d);
    if (dd < bd) { bd = dd; best = i; }
  }
  return best;
}

void main() {
  int k = region() + 1;
  vec4 col = vec4(0.0);
  // the colour field (premultiplied out); a missing value faintly hatched, in screen pixels
  if (u_on.x > 0.5) {
    if (u_fx[k].z > 0.5) {
      float h = mod(gl_FragCoord.x + gl_FragCoord.y, 16.0) < 1.0 ? 0.08 : 0.015;
      col = vec4(vec3(0.62, 0.68, 0.76) * h, h);
    } else {
      vec4 c = u_col[k];
      col = vec4(c.rgb * c.a, c.a);
    }
  }
  // cloud shadows: dark where the noise is above the region's threshold
  if (u_on.z > 0.5 && u_fx[k].y < 1.5) {
    float n = texture(u_noise, (v_m + u_off[k].zw) / u_tile.y).r;
    float sh = smoothstep(u_fx[k].y - 0.035, u_fx[k].y + 0.035, n) * 0.2;
    col = col * (1.0 - sh) + vec4(0.0, 0.01, 0.03, 1.0) * sh;
  }
  // fog: two scales of the noise drifting together, its mean the region's opacity
  if (u_on.y > 0.5 && u_fx[k].x > 0.0) {
    vec2 p = (v_m + u_off[k].xy) / u_tile.x;
    float n = 0.62 * texture(u_noise, p).r + 0.38 * texture(u_noise, p * 2.3 + 0.37).r;
    float a = clamp(u_fx[k].x * (1.0 + 0.7 * (n - 0.5)), 0.0, 0.85);
    col = col * (1.0 - a) + vec4(vec3(0.87, 0.9, 0.93) * a, a);
  }
  // the archive's bounds: a faint dashed outline (where its model points end)
  if (u_on.w > 0.5 && u_hasBounds > 0.5) {
    vec2 f = max(fwidth(v_ll), vec2(1e-9));
    bool yin = v_ll.y >= u_bounds.y && v_ll.y <= u_bounds.w;
    bool xin = v_ll.x >= u_bounds.x && v_ll.x <= u_bounds.z;
    float ex = yin ? min(abs(v_ll.x - u_bounds.x), abs(v_ll.x - u_bounds.z)) / f.x : 1e9;
    float ey = xin ? min(abs(v_ll.y - u_bounds.y), abs(v_ll.y - u_bounds.w)) / f.y : 1e9;
    float line = (1.0 - smoothstep(0.6, 1.4, min(ex, ey))) * (mod(gl_FragCoord.x + gl_FragCoord.y, 12.0) < 7.0 ? 0.3 : 0.0);
    col = col * (1.0 - line) + vec4(vec3(0.72, 0.78, 0.86) * line, line);
  }
  fragColor = col;
}`

/** the mesh: GRID by GRID cells over the frame, each vertex on the surface */
const GRID = 40
const M_PER_DEG = 111_320

const mercX = (lon: number) => (lon + 180) / 360
const mercY = (lat: number) => (180 - (180 / Math.PI) * Math.log(Math.tan(Math.PI / 4 + (lat * Math.PI) / 360))) / 360

/** What the layer draws now; set before each repaint. */
export interface EffectsState {
  packed: Packed | null
  on: [number, number, number, number]
  bounds: [number, number, number, number] | null
}

export class EffectsLayer implements maplibregl.CustomLayerInterface {
  readonly id: string
  readonly type = 'custom' as const
  readonly renderingMode = '2d' as const
  state: EffectsState = { packed: null, on: [0, 0, 0, 0], bounds: null }
  private gl: WebGL2RenderingContext | null = null
  private program: WebGLProgram | null = null
  private vao: WebGLVertexArrayObject | null = null
  private vbo: WebGLBuffer | null = null
  private ibo: WebGLBuffer | null = null
  private tex: WebGLTexture | null = null
  private count = 0
  private loc: Record<string, WebGLUniformLocation | null> = {}
  /** the mesh's origin (lon, lat) and mercator origin */
  origin: [number, number] = [0, 0]
  private mOrigin: [number, number] = [0, 0]
  private readonly noise: Uint8Array
  private map: maplibregl.Map | null = null

  constructor(id: string, noise: Float32Array) {
    this.id = id
    this.noise = Uint8Array.from(noise, (v) => Math.round(v * 255))
  }

  onAdd(map: maplibregl.Map, gl: WebGLRenderingContext | WebGL2RenderingContext) {
    this.map = map
    const g = gl as WebGL2RenderingContext
    if (typeof g.createVertexArray !== 'function') return
    this.gl = g
    const sh = (type: number, src: string) => {
      const s = g.createShader(type)!
      g.shaderSource(s, src)
      g.compileShader(s)
      if (!g.getShaderParameter(s, g.COMPILE_STATUS)) console.warn('[murmur] weather shader', g.getShaderInfoLog(s))
      return s
    }
    const p = g.createProgram()!
    g.attachShader(p, sh(g.VERTEX_SHADER, VS))
    g.attachShader(p, sh(g.FRAGMENT_SHADER, FS))
    g.linkProgram(p)
    if (!g.getProgramParameter(p, g.LINK_STATUS)) {
      console.warn('[murmur] weather program', g.getProgramInfoLog(p))
      return
    }
    this.program = p
    for (const u of ['u_matrix', 'u_lat0', 'u_n', 'u_pt', 'u_col', 'u_fx', 'u_off', 'u_bounds', 'u_hasBounds', 'u_on', 'u_noise', 'u_tile']) this.loc[u] = g.getUniformLocation(p, u)
    this.vao = g.createVertexArray()
    this.vbo = g.createBuffer()
    this.ibo = g.createBuffer()
    g.bindVertexArray(this.vao)
    g.bindBuffer(g.ARRAY_BUFFER, this.vbo)
    const stride = 7 * 4
    const attr = (name: string, size: number, offset: number) => {
      const l = g.getAttribLocation(p, name)
      if (l < 0) return
      g.enableVertexAttribArray(l)
      g.vertexAttribPointer(l, size, g.FLOAT, false, stride, offset)
    }
    attr('a_pos', 3, 0)
    attr('a_ll', 2, 12)
    attr('a_m', 2, 20)
    g.bindBuffer(g.ELEMENT_ARRAY_BUFFER, this.ibo)
    g.bindVertexArray(null)
    this.tex = g.createTexture()
    g.bindTexture(g.TEXTURE_2D, this.tex)
    g.pixelStorei(g.UNPACK_ALIGNMENT, 1)
    g.texImage2D(g.TEXTURE_2D, 0, g.R8, NOISE_SIZE, NOISE_SIZE, 0, g.RED, g.UNSIGNED_BYTE, this.noise)
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MIN_FILTER, g.LINEAR)
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_MAG_FILTER, g.LINEAR)
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_S, g.REPEAT)
    g.texParameteri(g.TEXTURE_2D, g.TEXTURE_WRAP_T, g.REPEAT)
    g.bindTexture(g.TEXTURE_2D, null)
  }

  onRemove() {
    const g = this.gl
    if (!g) return
    g.deleteProgram(this.program)
    g.deleteVertexArray(this.vao)
    g.deleteBuffer(this.vbo)
    g.deleteBuffer(this.ibo)
    g.deleteTexture(this.tex)
    this.gl = null
  }

  /** Lays the mesh over `bbox` (w, s, e, n) on the surface `zOf` gives (rendered metres). */
  setMesh(bbox: [number, number, number, number], zOf: (lon: number, lat: number) => number) {
    const g = this.gl
    if (!g || !this.vbo) return
    const [w, s, e, n] = bbox
    this.origin = [w, n]
    this.mOrigin = [mercX(w), mercY(n)]
    const k = Math.cos((n * Math.PI) / 180)
    const v = new Float32Array((GRID + 1) * (GRID + 1) * 7)
    let o = 0
    for (let j = 0; j <= GRID; j++) {
      // rows even in Web Mercator, as the map is
      const my = this.mOrigin[1] + ((mercY(s) - this.mOrigin[1]) * j) / GRID
      const lat = (Math.atan(Math.sinh(Math.PI * (1 - 2 * my))) * 180) / Math.PI
      for (let i = 0; i <= GRID; i++) {
        const lon = w + ((e - w) * i) / GRID
        v[o++] = mercX(lon) - this.mOrigin[0]
        v[o++] = my - this.mOrigin[1]
        v[o++] = zOf(lon, lat)
        v[o++] = lon - w
        v[o++] = lat - n
        v[o++] = (lon - w) * M_PER_DEG * k
        v[o++] = (lat - n) * M_PER_DEG
      }
    }
    const idx = new Uint16Array(GRID * GRID * 6)
    o = 0
    for (let j = 0; j < GRID; j++) for (let i = 0; i < GRID; i++) {
      const a = j * (GRID + 1) + i, b = a + 1, c = a + GRID + 1, d = c + 1
      idx.set([a, c, b, b, c, d], o)
      o += 6
    }
    g.bindVertexArray(this.vao)
    g.bindBuffer(g.ARRAY_BUFFER, this.vbo)
    g.bufferData(g.ARRAY_BUFFER, v, g.DYNAMIC_DRAW)
    g.bindBuffer(g.ELEMENT_ARRAY_BUFFER, this.ibo)
    g.bufferData(g.ELEMENT_ARRAY_BUFFER, idx, g.DYNAMIC_DRAW)
    g.bindVertexArray(null)
    this.count = idx.length
  }

  render(gl: WebGLRenderingContext | WebGL2RenderingContext, args: maplibregl.CustomRenderMethodInput) {
    const g = gl as WebGL2RenderingContext
    const st = this.state
    if (!this.program || !st.packed || !this.count || st.on.every((x) => x === 0)) return
    const m = args.modelViewProjectionMatrix as unknown as ArrayLike<number>
    // the view-projection takes world pixels (x, y) and metres (z): from the mesh's mercator
    // offsets by the world's size, in double precision before it goes to the GPU
    const ws = (this.map?.transform as unknown as { worldSize: number } | undefined)?.worldSize ?? 0
    if (!ws) return
    const t = [ws, 0, 0, 0, 0, ws, 0, 0, 0, 0, 1, 0, this.mOrigin[0] * ws, this.mOrigin[1] * ws, 0, 1]
    const out = new Float32Array(16)
    for (let c = 0; c < 4; c++) for (let r = 0; r < 4; r++) {
      let sum = 0
      for (let k = 0; k < 4; k++) sum += m[k * 4 + r] * t[c * 4 + k]
      out[c * 4 + r] = sum
    }
    const p = st.packed
    g.useProgram(this.program)
    g.uniformMatrix4fv(this.loc.u_matrix, false, out)
    g.uniform1f(this.loc.u_lat0, this.origin[1])
    g.uniform1i(this.loc.u_n, p.n)
    g.uniform2fv(this.loc.u_pt, p.pts)
    g.uniform4fv(this.loc.u_col, p.col)
    g.uniform4fv(this.loc.u_fx, p.fx)
    g.uniform4fv(this.loc.u_off, p.off)
    const b = st.bounds
    g.uniform4f(this.loc.u_bounds, b ? b[0] - this.origin[0] : 0, b ? b[1] - this.origin[1] : 0, b ? b[2] - this.origin[0] : 0, b ? b[3] - this.origin[1] : 0)
    g.uniform1f(this.loc.u_hasBounds, b ? 1 : 0)
    g.uniform4f(this.loc.u_on, ...st.on)
    g.uniform2f(this.loc.u_tile, FOG_TILE_M, CLOUD_TILE_M)
    g.activeTexture(g.TEXTURE0)
    g.bindTexture(g.TEXTURE_2D, this.tex)
    g.uniform1i(this.loc.u_noise, 0)
    g.disable(g.DEPTH_TEST)
    g.disable(g.STENCIL_TEST)
    g.disable(g.CULL_FACE)
    g.enable(g.BLEND)
    g.blendFunc(g.ONE, g.ONE_MINUS_SRC_ALPHA)
    g.bindVertexArray(this.vao)
    g.drawElements(g.TRIANGLES, this.count, g.UNSIGNED_SHORT, 0)
    g.bindVertexArray(null)
    g.bindTexture(g.TEXTURE_2D, null)
  }
}
