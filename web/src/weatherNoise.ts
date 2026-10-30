// The texture that gives the animated fog and cloud shadows their shape (bd murmur-vr17). Pure:
// weatherMap.ts uploads it and its shader samples it, drifting with each region's wind.
//
// Only the shape is invented; how much of a region it covers is the sampler's value: a cloud
// shadow covers the share of the texture whose noise is above `coverThreshold(cover)`, which
// is that cover's share exactly over one tile of the texture, and fog's mean opacity is the
// visibility's (`fogAlpha`) wherever it is drawn.

/** texels a side; the texture repeats */
export const NOISE_SIZE = 128

/** A tileable fractal value noise in [0, 1], NOISE_SIZE square, row-major. Deterministic. */
export function makeNoise(seed = 7): Float32Array {
  const n = NOISE_SIZE
  const out = new Float32Array(n * n)
  let amp = 1, total = 0
  for (let octave = 0, cells = 4; octave < 4; octave++, cells *= 2, amp *= 0.5) {
    // a lattice of random values, cells by cells, wrapping
    const lattice = new Float32Array(cells * cells)
    let s = (seed * 7919 + octave * 104729) >>> 0
    for (let i = 0; i < lattice.length; i++) {
      // xorshift32
      s ^= s << 13; s >>>= 0
      s ^= s >>> 17
      s ^= s << 5; s >>>= 0
      lattice[i] = s / 0xffffffff
    }
    total += amp
    for (let y = 0; y < n; y++) {
      const fy = (y / n) * cells, y0 = Math.floor(fy), ty = smooth(fy - y0)
      for (let x = 0; x < n; x++) {
        const fx = (x / n) * cells, x0 = Math.floor(fx), tx = smooth(fx - x0)
        const at = (i: number, j: number) => lattice[((j % cells) * cells) + (i % cells)]
        const a = at(x0, y0), b = at(x0 + 1, y0), c = at(x0, y0 + 1), d = at(x0 + 1, y0 + 1)
        out[y * n + x] += amp * (a + (b - a) * tx + (c - a) * ty + (a - b - c + d) * tx * ty)
      }
    }
  }
  for (let i = 0; i < out.length; i++) out[i] /= total
  // stretch to [0, 1] so a threshold has the whole range to work with
  let lo = Infinity, hi = -Infinity
  for (const v of out) {
    lo = Math.min(lo, v)
    hi = Math.max(hi, v)
  }
  for (let i = 0; i < out.length; i++) out[i] = (out[i] - lo) / (hi - lo)
  return out
}

function smooth(t: number) {
  return t * t * (3 - 2 * t)
}

/** The texture's values sorted: its distribution, for thresholds. */
export function noiseQuantiles(noise: Float32Array): Float32Array {
  return Float32Array.from(noise).sort()
}

/** The threshold above which `cover` (0..1) of the texture lies: a cloud shadow drawn where the
 *  noise is above it covers that share of the ground. 2 for none (nothing is above it), -1 for
 *  all. */
export function coverThreshold(sorted: Float32Array, cover: number | null): number {
  if (cover == null || !Number.isFinite(cover) || cover <= 0) return 2
  if (cover >= 1) return -1
  const i = Math.min(sorted.length - 1, Math.max(0, Math.floor((1 - cover) * sorted.length)))
  return sorted[i]
}

/** The share of the texture a threshold covers (what coverThreshold inverts). */
export function coveredShare(noise: Float32Array, threshold: number): number {
  let k = 0
  for (const v of noise) if (v >= threshold) k++
  return k / noise.length
}

const FOG_M = 50
const CLEAR_M = 20_000
/** the fog's mean opacity at its thickest */
export const FOG_MAX = 0.62

/** The fog's mean opacity for a visibility: none at 20 km or more (or unknown), FOG_MAX at 50 m
 *  or less, rising on the logarithm and slowly at first, so that ordinary haze stays faint. */
export function fogAlpha(visibility_m: number | null): number {
  if (visibility_m == null || !Number.isFinite(visibility_m)) return 0
  const s = Math.max(0, Math.min(1, 1 - Math.log(Math.max(visibility_m, 1) / FOG_M) / Math.log(CLEAR_M / FOG_M)))
  return FOG_MAX * s ** 1.4
}
