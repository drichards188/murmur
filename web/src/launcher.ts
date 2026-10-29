import { useCallback, useEffect, useState } from 'react'
import type { Domain } from './types'

/** The launcher (`make start`, eval/launcher.py): Murmur's main menu. It serves this console and
 *  starts and stops one scenario at a time (the open world, what `make goldengate` runs, or a
 *  situation, what `make situation` runs); the console asks it under /launcher, which Vite
 *  proxies only when the launcher runs it. Without one (a stack started with `make up` or `make
 *  goldengate`) the probe finds nothing and the console is what it always was. */

export type Stage = 'waiting' | 'starting' | 'bridge' | 'ready' | 'running' | 'ended' | 'stopping' | 'stopped' | 'failed'
export type OperatorMode = 'live' | 'autoplay'
export type AiMode = 'claude' | 'codex' | 'playbook' | 'none'

/** A scenario of the menu: the open world, or a situation (sim/situations/*.toml); never a
 *  situation's director's notes. */
export interface Scenario {
  id: string
  kind: 'open_world' | 'situation'
  default: boolean
  /** the guided tutorial (a situation with `tutorial = true`): offered apart, at the top of the
   *  menu, started with its own modes in one click; the console walks through it (Tutorial.tsx) */
  tutorial?: boolean
  title: string
  brief: string
  /** a headline and a few bullets for the brief (a situation's `[summary]`), when it has one */
  summary?: { headline: string; bullets: string[] } | null
  runnable: boolean
  /** why it can't start from the menu (headless only, a draft) */
  why_not: string | null
  vehicles?: number
  /** how many of them run in ArduPilot SITL (the rest are stand-ins) */
  sitl?: number
  platform_types?: number
  /** how many of them in each domain (air, ground, surface, subsurface) */
  domains?: Record<Domain, number>
  /** how long vehicles usually take to be ready (s) */
  typical_s?: [number, number]
  contacts?: number
  /** the tidewater scenario, or "none" */
  tidewater?: string
  at?: string
  /** situation time (s); null: the open world runs until it is ended */
  duration_s?: number | null
  rate?: number
  /** the situation's own defaults, and what it allows */
  operator?: OperatorMode
  ai?: AiMode
  operators?: OperatorMode[]
  ais?: AiMode[]
  orders?: number
  steps?: number
  watch?: number
  /** the same from a terminal */
  cli?: string
}

/** One launch: its stages as the launcher saw them, and its end. */
export interface Launch {
  id: string
  scenario: string
  kind: 'open_world' | 'situation'
  title: string
  options: { operator?: OperatorMode; ai?: AiMode; ai_label?: string }
  cli: string
  stage: Stage
  detail: string
  /** why it failed */
  error: string | null
  /** what was still held after it ended (ports, processes) */
  leftovers: string[]
  started_t: number
  stage_t: number
  history: { stage: Stage; t: number }[]
  ready: { ready: number; of: number } | null
  typical_s: [number, number]
  /** its command's output */
  log: string
  bridge_port: number
  /** a situation's run (its id and logs), once the runner reports */
  run: { id?: string; audit_log?: string; situation_log?: string } | null
  /** the bridge answers: the console can show it */
  stack_up: boolean
}

export interface LauncherState {
  launcher: { console_port: number; api_port: number }
  scenarios: Scenario[]
  /** whether the AI CLIs are installed here (the runner checks their login itself) */
  ai_cli: Record<string, boolean>
  current: Launch | null
  /** the launcher's clock (s) */
  now: number
}

export interface StartRequest {
  scenario: string
  operator?: OperatorMode
  ai?: AiMode
  /** end the running scenario first */
  replace?: boolean
}

const ACTIVE: Stage[] = ['waiting', 'starting', 'bridge', 'ready', 'running', 'ended', 'stopping']
/** a launch still under way or still up (anything but stopped or failed) */
export const isActive = (l: Launch | null | undefined): l is Launch => !!l && ACTIVE.includes(l.stage)
/** a launch on its way up or down: the menu polls faster */
const moving = (l: Launch | null | undefined) => !!l && ['waiting', 'starting', 'bridge', 'ready', 'stopping'].includes(l.stage)

/** The launcher's state, or null when no launcher answers (without one, Vite serves its own
 *  index.html for the path, or nothing). */
export async function probeLauncher(timeoutMs = 2500): Promise<LauncherState | null> {
  const ctl = new AbortController()
  const timer = window.setTimeout(() => ctl.abort(), timeoutMs)
  try {
    const res = await fetch('/launcher/api/state', { signal: ctl.signal, cache: 'no-store' })
    if (!res.ok || !(res.headers.get('content-type') ?? '').includes('application/json')) return null
    const body = await res.json()
    return body && Array.isArray(body.scenarios) ? (body as LauncherState) : null
  } catch {
    return null
  } finally {
    window.clearTimeout(timer)
  }
}

async function post(path: 'start' | 'end', body: object): Promise<Launch | null> {
  const res = await fetch(`/launcher/api/${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
  const out = await res.json().catch(() => ({}))
  if (!res.ok) throw new Error(out.detail ?? `the launcher answered ${res.status}`)
  return out.current ?? null
}

export const startScenario = (req: StartRequest) => post('start', req)
export const endScenario = () => post('end', {})

/** The launcher's state, polled: every second while a launch comes up or goes down, every 3 s
 *  otherwise. `received` is this browser's clock (ms) when it came; `lost` says the launcher
 *  stopped answering (the last state stays). */
export function useLauncher(initial: LauncherState) {
  const [state, setState] = useState(initial)
  const [received, setReceived] = useState(() => Date.now())
  const [lost, setLost] = useState(false)
  // bumped to look again at once (after a start or an end)
  const [kick, setKick] = useState(0)
  useEffect(() => {
    let stop = false
    let timer: number | undefined
    const poll = async () => {
      const s = await probeLauncher()
      if (stop) return
      if (s) {
        setState(s)
        setReceived(Date.now())
      }
      setLost(!s)
      timer = window.setTimeout(poll, s && !moving(s.current) ? 3000 : 1000)
    }
    timer = window.setTimeout(poll, kick ? 0 : 1000)
    return () => {
      stop = true
      window.clearTimeout(timer)
    }
  }, [kick])
  /** a launch the launcher just answered with, shown before the next look */
  const update = useCallback((current: Launch | null) => {
    if (current) setState((s) => ({ ...s, current }))
    setKick((n) => n + 1)
  }, [])
  return { state, received, lost, update }
}
