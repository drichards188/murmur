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

// whether the weather legend is folded to its title row, remembered the same way
const COLLAPSED_KEY = 'murmur.weather.legendCollapsed'

export function savedLegendCollapsed(): boolean {
  try {
    return localStorage.getItem(COLLAPSED_KEY) === '1'
  } catch {
    return false
  }
}

export function saveLegendCollapsed(collapsed: boolean) {
  try {
    localStorage.setItem(COLLAPSED_KEY, collapsed ? '1' : '0')
  } catch {
    // storage unavailable: it still folds for this session
  }
}
