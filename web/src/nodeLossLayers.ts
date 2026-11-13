// Rules 30 (docs/NODE-LOSS.md sections 3 and 5): where each silent vehicle of ours can be, as an
// engagement's friendly keep-out counts it (the bridge's `GET /api/nodes`, stage 2's regions),
// through MapView's `layers` prop, at the surface and under the vehicle symbols. Each part of a
// region (its orders' corridors and circles, its failsafe's way home, a drift, its top speed's
// reach) is its padded outline; a vehicle presumed lost is drawn warmer. Dim, don't paint
// (docs/research/saturation-display.md R1): out of focus only the outline, faint; in focus (selected,
// details open) or presumed lost, filled, and the reach that caps it dashed. A vehicle confirmed lost
// has no region: it is out of every keep-out. Display only: the engine's checks decide.
//
//   node-region-fill     the parts, faintly filled
//   node-region-outline  their outlines, and the reach dashed
import { PathLayer, PolygonLayer } from '@deck.gl/layers'
import { regionRings } from './nodeLoss'
import type { NodeRow } from './useNodes'

type RGBA = [number, number, number, number]
type LL = [number, number]
const SILENT: RGBA = [154, 166, 178, 255]
const PRESUMED: RGBA = [255, 140, 90, 255]

function a(c: RGBA, k: number): RGBA {
  return [c[0], c[1], c[2], Math.round(c[3] * k)]
}

interface Path {
  path: LL[]
  color: RGBA
  width: number
}

export function buildNodeRegionLayers(rows: Iterable<NodeRow>, focus: ReadonlySet<number>) {
  const fills: { ring: LL[]; color: RGBA }[] = []
  const paths: Path[] = []
  for (const n of rows) {
    if (!n.region || n.state === 'lost') continue
    const color = n.state === 'presumed_lost' ? PRESUMED : SILENT
    const near = focus.has(n.uid)
    const r = regionRings(n.region)
    for (const p of r.parts) {
      if (near || n.state === 'presumed_lost') fills.push({ ring: p.ring, color: a(color, near ? 0.12 : 0.06) })
      paths.push({ path: p.ring, color: a(color, near ? 0.9 : 0.45), width: near ? 1.8 : 1 })
    }
    if (near && r.reach) {
      for (let k = 0; k + 1 < r.reach.length; k += 2) paths.push({ path: [r.reach[k], r.reach[k + 1]], color: a(color, 0.5), width: 1 })
    }
  }
  if (!paths.length) return []
  const onTop = { depthCompare: 'always' as const, depthWriteEnabled: false }
  return [
    new PolygonLayer<{ ring: LL[]; color: RGBA }>({
      id: 'node-region-fill',
      data: fills,
      getPolygon: (f) => f.ring,
      stroked: false,
      filled: true,
      getFillColor: (f) => f.color,
      parameters: onTop,
    }),
    new PathLayer<Path>({
      id: 'node-region-outline',
      data: paths,
      getPath: (p) => p.path,
      getColor: (p) => p.color,
      getWidth: (p) => p.width,
      widthUnits: 'pixels',
      parameters: onTop,
    }),
  ]
}
