// The link emulator's view of one vehicle's link (crates/murmur-linkemu), served by the bridge
// at /api/links/{uid} (crates/murmur-bridge/src/emu.rs). The fleet row only needs the brief
// that rides on each vehicle's link (`link.emu`); the details card polls the full view.

import { useEffect, useState } from 'react'

export interface EmuDir {
  pkts: number
  dropped: number
  loss: number | null
  by_cause: Record<string, number>
  /** mean one-way delay the emulator added, ms */
  latency_ms: number | null
}

export interface EmuSatcom {
  class: string
  mode: 'pending' | 'live' | 'static'
  static_reason: string | null
  up: boolean | null
  visible: number | null
  best_norad_id: number | null
  best_name: string | null
  best_elevation_deg: number | null
  slant_range_km: number | null
  latency_ms: number
  latency_source: 'space' | 'profile'
  handovers: number
  next_outage_in_s: number | null
  next_pass_in_s: number | null
}

export interface EmuLink {
  uid: number
  class: string
  profile: string
  /** the profile carrying the traffic now: `gated_profile` while the depth gate closes `profile` */
  active_profile?: string
  state: 'ok' | 'degraded' | 'down'
  cause: string | null
  blocked: string | null
  events: string[]
  depth_state: 'unknown' | 'surfaced' | 'submerged'
  los: { state: 'off' | 'unknown' | 'clear' | 'obstructed'; obstruction_m: number | null } | null
  satcom: EmuSatcom | null
  nominal: {
    loss: number
    latency_ms: number
    jitter_ms: number
    bandwidth_bps: number
    propagation_mps: number | null
    down_when: 'submerged' | 'surfaced' | null
    terrain: boolean
    gated_profile?: string
  } | null
  recent: { window_s: number; up: EmuDir; down: EmuDir }
  total: { up: EmuDir; down: EmuDir }
  emu_rtt_ms: number | null
}

export interface EmuAnswer {
  enabled: boolean
  ok: boolean
  age_s: number | null
  error: string | null
  link?: EmuLink
  detail?: string
}

/** What the emulator is doing to a link, in words. `downWhen` names the depth rule for "gate". */
export function emuCause(cause: string | null | undefined, downWhen?: string | null): string {
  switch (cause) {
    case null:
    case undefined:
      return ''
    case 'loss':
      return 'channel loss'
    case 'event_loss':
    case 'event':
      return 'scripted event'
    case 'jam':
      return 'jammer'
    case 'cut':
      return 'scripted cut'
    case 'range':
      return 'out of range'
    case 'terrain':
      return 'terrain blocks line of sight'
    case 'gate':
      return downWhen === 'surfaced' ? 'surfaced: acoustic works only submerged' : downWhen === 'submerged' ? 'submerged: no SATCOM under water' : 'wrong depth for this link'
    case 'queue':
      return 'queue full (bandwidth)'
    case 'no_sat':
      return 'no satellite in view'
    case 'handover':
      return 'satellite handover'
    case 'no_peer':
      return 'vehicle not heard yet'
    case 'socket':
      return 'socket error'
    default:
      return cause
  }
}

export function fmtBps(bps: number): string {
  if (!bps) return 'unlimited'
  return bps >= 1e6 ? `${(bps / 1e6).toFixed(bps % 1e6 ? 1 : 0)} Mbps` : bps >= 1e3 ? `${(bps / 1e3).toFixed(0)} kbps` : `${bps} bps`
}

export function fmtDuration(s: number): string {
  if (s < 90) return `${Math.round(s)} s`
  if (s < 5400) return `${Math.round(s / 60)} min`
  return `${(s / 3600).toFixed(1)} h`
}

/** Polls /api/links/{uid} while mounted. null until the first answer. */
export function useEmuLink(uid: number, everyMs = 2000): EmuAnswer | null {
  const [ans, setAns] = useState<{ uid: number; a: EmuAnswer } | null>(null)
  useEffect(() => {
    let live = true
    const load = async () => {
      try {
        const res = await fetch(`/api/links/${uid}`)
        const body = (await res.json()) as EmuAnswer
        if (live) setAns({ uid, a: body })
      } catch {
        if (live) setAns({ uid, a: { enabled: false, ok: false, age_s: null, error: 'bridge unreachable' } })
      }
    }
    void load()
    const t = window.setInterval(load, everyMs)
    return () => {
      live = false
      window.clearInterval(t)
    }
  }, [uid, everyMs])
  return ans && ans.uid === uid ? ans.a : null
}
