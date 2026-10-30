// The weather layer's toggles (bd murmur-vr17), remembered per viewer: a convenience only, and
// every effect on when storage is unavailable or holds nothing.
import type { Effects } from './weatherGl'

const KEY = 'murmur.weather.effects'
export const ALL_EFFECTS: Effects = { wind: true, fog: true, rain: true, cloud: true }

export function savedEffects(): Effects {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? 'null') as Partial<Effects> | null
    if (!v || typeof v !== 'object') return ALL_EFFECTS
    return { wind: v.wind !== false, fog: v.fog !== false, rain: v.rain !== false, cloud: v.cloud !== false }
  } catch {
    return ALL_EFFECTS
  }
}

export function saveEffects(fx: Effects) {
  try {
    localStorage.setItem(KEY, JSON.stringify(fx))
  } catch {
    // storage unavailable: the toggles still work for this session
  }
}
