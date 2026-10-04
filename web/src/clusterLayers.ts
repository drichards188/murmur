// Cluster markers (murmur-and): when the view holds more vehicles than the bridge sends in
// full (zoomed out over a large fleet), the bridge sends counts per map tile instead
// (`clusters` in the state and delta messages, push.rs) and the console draws one marker per
// tile here: a disc sized by the count, the count in it, the split by domain under it, and a
// red line for tracks of concern (hostile, suspect, unknown) in the tile. Clicking a marker
// zooms in on it (MapView). The pinned vehicles (selected, followed, in the inbox) still
// come in full and are drawn as symbols on top.
import { ScatterplotLayer, TextLayer } from '@deck.gl/layers'
import type { Cluster, Clusters, LngLat } from './types'

/** layer ids that stand for a cluster under the pointer */
export const CLUSTER_PICK_LAYERS = ['clusters']

type RGBA = [number, number, number, number]
type XYZ = [number, number, number]
const FILL: RGBA = [20, 30, 42, 225]
const RING: RGBA = [127, 184, 255, 200]
const RING_LOST: RGBA = [255, 93, 93, 220]
const TEXT: RGBA = [215, 221, 228, 255]
const DIM: RGBA = [215, 221, 228, 200]
const HOT: RGBA = [255, 120, 120, 255]
const OUTLINE: RGBA = [11, 15, 20, 255]
const FONT = 'IBM Plex Mono, ui-monospace, monospace'
const FONT_SETTINGS = { sdf: true, fontSize: 48, buffer: 6 }

/** marker radius in pixels: 16 for one vehicle, about 34 for a thousand */
export function clusterRadius(c: Cluster) {
  return 16 + 6 * Math.log10(Math.max(1, c.n))
}

/** "A412 G130 W88 S40", only the domains present */
export function domainLine(c: Cluster) {
  const names = ['A', 'G', 'W', 'S']
  return c.d.flatMap((n, i) => (n ? [`${names[i]}${n}`] : [])).join(' ')
}

/** "2 hostile 1 suspect 5 unk" for the tracks in the tile that need a look; '' otherwise */
export function trackLine(c: Cluster) {
  const [hostile, suspect, unknown] = c.tk
  const parts: string[] = []
  if (hostile) parts.push(`${hostile} hostile`)
  if (suspect) parts.push(`${suspect} suspect`)
  if (unknown) parts.push(`${unknown} unk`)
  return parts.join(' ')
}

export function clusterCenter(c: Cluster): LngLat {
  return c.c
}

export function buildClusterLayers(clusters: Clusters | null | undefined) {
  const cells = clusters?.cells ?? []
  const at = (c: Cluster): XYZ => [c.c[0], c.c[1], 0]
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  // a new tile zoom is a new set of cells: everything is rebuilt
  const trigger = { getPosition: clusters?.z ?? 0 }
  return [
    new ScatterplotLayer<Cluster>({
      id: 'clusters',
      data: cells,
      pickable: true,
      getPosition: at,
      getRadius: clusterRadius,
      radiusUnits: 'pixels',
      billboard: true,
      stroked: true,
      filled: true,
      getFillColor: FILL,
      getLineColor: (c) => (c.lost ? RING_LOST : RING),
      getLineWidth: 2,
      lineWidthUnits: 'pixels',
      updateTriggers: trigger,
      parameters: onTop,
    }),
    new TextLayer<Cluster>({
      id: 'cluster-counts',
      data: cells,
      getPosition: at,
      getText: (c) => String(c.n),
      getSize: 14,
      getColor: TEXT,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'center',
      fontFamily: FONT,
      fontWeight: 700,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: trigger,
      parameters: onTop,
    }),
    new TextLayer<Cluster>({
      id: 'cluster-domains',
      data: cells.filter((c) => c.n > 0),
      getPosition: at,
      getText: domainLine,
      getSize: 10,
      getColor: DIM,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (c) => [0, clusterRadius(c) + 3],
      fontFamily: FONT,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: trigger,
      parameters: onTop,
    }),
    new TextLayer<Cluster>({
      id: 'cluster-tracks',
      data: cells.filter((c) => trackLine(c)),
      getPosition: at,
      getText: trackLine,
      getSize: 10,
      getColor: HOT,
      getTextAnchor: 'middle',
      getAlignmentBaseline: 'top',
      getPixelOffset: (c) => [0, clusterRadius(c) + (c.n > 0 ? 15 : 3)],
      fontFamily: FONT,
      fontWeight: 600,
      outlineWidth: 3,
      outlineColor: OUTLINE,
      fontSettings: FONT_SETTINGS,
      characterSet: 'auto',
      updateTriggers: trigger,
      parameters: onTop,
    }),
  ]
}
