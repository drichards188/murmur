import { useEffect, useRef, useState } from 'react'
import { operatorFetch, operatorHeaders } from './auth'
import { getOperator, type Check, type PolicyDecision } from './decisions'
import type { OrderLink, OrderShape, VehicleAt } from './coverage'
import type { Vehicle } from './types'
import type { TaskRequest } from './useBridge'

/** `POST /api/intents/preview`: the checks and the policy now, nothing created or logged, and
 *  the predicted link along each vehicle's route (`link_coverage`, information only). */
export interface DryRunAnswer {
  checks: Check[]
  passed: boolean
  policy: PolicyDecision | null
  would_reach: string
  submission_authorizes?: boolean
  link_coverage?: OrderLink
}

export interface DryRunState {
  answer: DryRunAnswer | null
  /** the order and where its vehicles were when it was asked (the legs are drawn from these) */
  order: OrderShape | null
  at: Map<number, VehicleAt>
  loading: boolean
  error: string | null
}

const EMPTY: DryRunState = { answer: null, order: null, at: new Map(), loading: false, error: null }
/** asked again this long after each answer while the order stands: its vehicles move, and the
 *  transit with them */
const REFRESH_MS = 5000

/** The dry run of the order being drafted: asked shortly after it changes, then again a few
 *  seconds after each answer while it stands (never two at once; a changed order cancels the one
 *  in flight, and its answer goes with it). */
export function useDryRun(req: TaskRequest | null, vehicles: Vehicle[]): DryRunState {
  const key = req ? JSON.stringify(req) : null
  // the state of one order (its key): another order's reads as nothing yet
  const [st, setSt] = useState<DryRunState & { key: string | null }>({ ...EMPTY, key: null })
  const vehiclesRef = useRef(vehicles)
  useEffect(() => {
    vehiclesRef.current = vehicles
  }, [vehicles])
  useEffect(() => {
    if (!key) return
    const body = JSON.parse(key) as TaskRequest
    const mine = (s: DryRunState & { key: string | null }) => (s.key === key ? s : { ...EMPTY, key })
    let ctl: AbortController | null = null
    let timer = 0
    let gone = false
    const ask = async () => {
      ctl = new AbortController()
      // where the vehicles are as it is asked: the bridge lays the transit out from there
      const at = new Map<number, VehicleAt>()
      for (const u of body.uids) {
        const v = vehiclesRef.current.find((x) => x.uid === u)
        at.set(u, { position: v?.position ?? null, home: v?.home ?? null })
      }
      setSt((s) => ({ ...mine(s), loading: true }))
      try {
        const res = await operatorFetch('/api/intents/preview', {
          method: 'POST',
          headers: { 'content-type': 'application/json', ...operatorHeaders(getOperator()) },
          body: key,
          signal: ctl.signal,
        })
        const answer = await res.json().catch(() => ({}))
        if (!res.ok) throw new Error(answer.detail ?? `bridge returned ${res.status}`)
        if (gone) return
        const order: OrderShape = { task: body.task, center: body.center, radius: body.radius, route: body.route }
        setSt({ key, answer: answer as DryRunAnswer, order, at, loading: false, error: null })
      } catch (e) {
        if (gone) return
        setSt((s) => ({ ...mine(s), loading: false, error: (e as Error).message }))
      }
      timer = window.setTimeout(ask, REFRESH_MS)
    }
    timer = window.setTimeout(ask, 350)
    return () => {
      gone = true
      ctl?.abort()
      window.clearTimeout(timer)
    }
  }, [key])
  return key && st.key === key ? st : EMPTY
}
