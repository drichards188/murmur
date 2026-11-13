import { useEffect, useState } from 'react'
import type { NodeRegion, NodeStanding } from './nodeLoss'

/** Rules 30: one of ours not reporting, as `GET /api/nodes` serves it: its standing, since when
 *  (bridge seconds), and where the engagement keep-out counts it (null for one confirmed lost,
 *  which is out of every keep-out, or one never placed). As of the bridge's last tick. */
export interface NodeRow {
  uid: number
  state: NodeStanding
  since_t: number | null
  silent_s?: number
  region: NodeRegion | null
  by?: string
  t?: number
}

const EMPTY: ReadonlyMap<number, NodeRow> = new Map()

/** `GET /api/nodes` every 2 s while `on` (a run under rules 30 with any of ours not reporting):
 *  the map's SILENT regions and the details' standing. Empty otherwise (a run before rules 30
 *  serves no such route). Read-only. */
export function useNodes(on: boolean): ReadonlyMap<number, NodeRow> {
  const [rows, setRows] = useState<ReadonlyMap<number, NodeRow>>(EMPTY)
  useEffect(() => {
    if (!on) return
    let live = true
    const load = async () => {
      try {
        const res = await fetch('/api/nodes')
        const body = res.ok ? ((await res.json()) as { nodes?: NodeRow[] }) : { nodes: [] }
        if (live) setRows(new Map((body.nodes ?? []).map((n) => [n.uid, n])))
      } catch {
        // the bridge unreachable: keep what we had (the app shows the bridge as down)
      }
    }
    void load()
    const t = window.setInterval(load, 2000)
    return () => {
      live = false
      window.clearInterval(t)
    }
  }, [on])
  return on ? rows : EMPTY
}
