// The swarm summary's feed (GET /api/swarms) and the map's swarm outlines (swarmsModel.ts has the
// types and the pure helpers). The outlines are deck.gl layers at sea level over the terrain and
// the water (as orderLayers.ts: a MapLibre line would drape on the seabed), built from one summary
// and so rebuilt about once a second when a new one comes, never per frame.
import { PolygonLayer, TextLayer } from '@deck.gl/layers'
import { useEffect, useState } from 'react'
import type { MapScreen } from './aloft'
import { ALL_FULL, textWidth, type LabelCandidate, type LabelPlan } from './declutter'
import { ALERT_LABEL } from './decisions'
import type { Outline, OutlineLabel, SwarmsDoc } from './swarmsModel'

export type SwarmsStatus = 'off' | 'loading' | 'ok' | 'missing' | 'error'

export interface SwarmsFeed {
  doc: SwarmsDoc | null
  status: SwarmsStatus
  error: string | null
}

const POLL_MS = 1000

/** GET /api/swarms about once a second while `on`. A 404 is `missing` (a bridge without swarms:
 *  the panel says so plainly and nothing stands in for the data); another failure keeps the last
 *  summary and says why. */
export function useSwarms(on: boolean): SwarmsFeed {
  const [feed, setFeed] = useState<SwarmsFeed>(OFF)
  useEffect(() => {
    if (!on) return
    let stop = false
    let timer: number | undefined
    const tick = async () => {
      try {
        const res = await fetch('/api/swarms')
        if (stop) return
        if (res.status === 404) setFeed({ doc: null, status: 'missing', error: 'the bridge has no /api/swarms' })
        else if (!res.ok) setFeed((f) => ({ ...f, status: f.doc ? 'ok' : 'error', error: `bridge returned ${res.status}` }))
        else {
          const doc = (await res.json()) as SwarmsDoc
          if (!stop) setFeed({ doc: Array.isArray(doc?.swarms) ? doc : { t: 0, swarms: [] }, status: 'ok', error: null })
        }
      } catch (e) {
        if (!stop) setFeed((f) => ({ ...f, status: f.doc ? 'ok' : 'error', error: (e as Error).message }))
      } finally {
        if (!stop) timer = window.setTimeout(() => void tick(), POLL_MS)
      }
    }
    void tick()
    return () => {
      stop = true
      window.clearTimeout(timer)
      setFeed(OFF)
    }
  }, [on])
  if (!on) return OFF
  return feed.status === 'off' ? LOADING : feed
}

const OFF: SwarmsFeed = { doc: null, status: 'off', error: null }
const LOADING: SwarmsFeed = { doc: null, status: 'loading', error: null }

/** A swarm alert's kind as the Inbox names a member alert of it ("LOST LINK"). */
export function alertLabel(kind: string): string {
  return (ALERT_LABEL as Record<string, string>)[kind] ?? kind.replace(/_/g, ' ').toUpperCase()
}

const TEXT_OUTLINE: [number, number, number, number] = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
// over the scene: an outline at sea level is never hidden by the terrain or the seabed
const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }

/** The groups' names for the label budget (declutter.ts, R2): aggregates, priority 2, after what
 *  is in focus and before our vehicles and the tracks; the selected group's always. Names that
 *  ran together ("SEARCH-8 SEARCH-7 ... SEARCH-1") now take turns. */
export function swarmLabelCandidates(outlines: { labels: OutlineLabel[] }, selected: string | null, screen: MapScreen): LabelCandidate[] {
  return outlines.labels.map((l) => {
    const w = textWidth(l.text, 11) / 2
    const box: [number, number, number, number] = [-w, -8, w, 8]
    return { key: `s:${l.id}`, at: screen.at(l.at[0], l.at[1], 0), short: box, full: box, priority: l.id === selected ? 1 : 2, force: l.id === selected }
  })
}

/** deck.gl layers for the outlines (MapView's `layers` prop), from one summary's outlines
 *  (swarmOutlines: memoise them on it) and the label plan. */
export function buildSwarmLayers(shapes: { outlines: Outline[]; labels: OutlineLabel[] }, selected: string | null, plan: LabelPlan = ALL_FULL) {
  const { outlines } = shapes
  const labels = shapes.labels.filter((l) => plan.level(`s:${l.id}`) !== 'none')
  if (!outlines.length && !labels.length) return []
  return [
    new PolygonLayer<Outline>({
      id: 'swarm-hulls',
      data: outlines,
      getPolygon: (o) => o.ring,
      getLineColor: (o) => o.line,
      getFillColor: (o) => o.fill,
      getLineWidth: (o) => (o.id === selected ? 2.5 : 1.5),
      lineWidthUnits: 'pixels',
      stroked: true,
      filled: true,
      pickable: false,
      updateTriggers: { getLineWidth: selected },
      parameters: onTop,
    }),
    new TextLayer<OutlineLabel>({
      id: 'swarm-labels',
      data: labels,
      getPosition: (l) => l.at,
      getText: (l) => l.text,
      getSize: 11,
      getColor: (l) => l.color,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'center',
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: TEXT_OUTLINE,
      // the other layers' settings, so deck.gl's font atlas cache keeps one entry for all
      fontSettings: { sdf: true, fontSize: 48, buffer: 6 },
      characterSet: 'auto',
      parameters: onTop,
    }),
  ]
}
