// The groups' chips over the map (bd murmur-4nik): one per group over its members, naming it, what
// it does and its strength against its minimum, or one per cluster tile far out naming the groups
// in it. HTML over the map's canvas, so over every layer (never under a symbol or crossed by a
// stem) and crisp; laid out (which chips, where) with the label budget when the camera stops or the
// data changes (MapView planNow, groupScreen.ts), moved with the camera on every frame the map draws
// (`place`, a few dozen projections). A chip is a label, not a handle: a click selects the group's
// members in the console, as the Fleet list and the Swarms panel do, and opens its card; orders go
// through the order form and the engine as always.
import { type CSSProperties, type RefObject, useImperativeHandle, useLayoutEffect, useRef } from 'react'
import type { MapScreen } from './aloft'
import { cellChipText, chipName, type CellChip } from './groupScreen'
import type { GroupMarks } from './swarmsModel'
import './swarms.css'

/** The groups as the map shows them, and what a chip does. */
export interface GroupsOnMap {
  marks: GroupMarks
  /** the group the selection is exactly, if any */
  selected: string | null
  /** the group whose chip (or card) is under the pointer: its members highlighted */
  hovered: string | null
  onHover: (id: string | null) => void
  /** select the group's members (client side) and open its card */
  onSelect: (id: string) => void
  /** who is missing and why, for a chip's hover (missingLines) */
  detail: (id: string) => string[]
}

export interface ChipSpot {
  /** `g:<id>` a group's chip, `gc:<x>:<y>` a cluster tile's */
  key: string
  /** the group (a tile's: its largest) */
  id: string
  /** the chip's centre is where xAt is drawn, its bottom `lift` px above where yAt is */
  xAt: [number, number, number]
  yAt: [number, number, number]
  lift: number
  /** px of the lift that is stacking (over another group's chip): a leader line down that far */
  leader?: number
  cell?: CellChip
}

export interface ChipPlacer {
  place(screen: MapScreen): void
}

interface Props {
  spots: ChipSpot[]
  groups: GroupsOnMap
  /** set to the function that moves the chips to this camera (MapView calls it on every frame) */
  placer: RefObject<ChipPlacer | null>
  screen: () => MapScreen
}

export function GroupChips({ spots, groups, placer, screen }: Props) {
  const els = useRef(new Map<string, HTMLElement>())
  const spotsRef = useRef(spots)
  // each chip's width, measured once a layout, and the overlay's: a chip is kept inside the map
  const widths = useRef(new Map<string, number>())
  const box = useRef<HTMLDivElement>(null)
  // the mover MapView calls on every frame
  useImperativeHandle(
    placer,
    () => ({
      place(s: MapScreen) {
        const W = box.current?.clientWidth ?? 0
        for (const spot of spotsRef.current) {
          const el = els.current.get(spot.key)
          if (!el) continue
          const p = s.at(spot.xAt[0], spot.xAt[1], spot.xAt[2])
          const q = s.at(spot.yAt[0], spot.yAt[1], spot.yAt[2])
          if (!p || !q) {
            el.style.visibility = 'hidden'
            continue
          }
          const half = (widths.current.get(spot.key) ?? 0) / 2
          const x = W > 2 * half + 8 ? Math.min(Math.max(p[0], half + 4), W - half - 4) : p[0]
          el.style.visibility = ''
          el.style.transform = `translate(${Math.round(x)}px, ${Math.round(q[1] - spot.lift)}px) translate(-50%, -100%)`
        }
      },
    }),
    [],
  )
  // a new layout placed at once, not a frame later
  useLayoutEffect(() => {
    spotsRef.current = spots
    widths.current.clear()
    for (const [key, el] of els.current) widths.current.set(key, el.offsetWidth)
    placer.current?.place(screen())
  })
  const { marks, selected, hovered } = groups
  const ref = (key: string) => (el: HTMLElement | null) => {
    if (el) els.current.set(key, el)
    else els.current.delete(key)
  }
  return (
    <div className="gchips" ref={box}>
      {/* the most important last, so on top: a stacked chip's leader runs under the chips over it */}
      {[...spots].reverse().map((spot) => {
        const m = marks.byId.get(spot.id)
        if (!m) return null
        const style = { '--g': m.color, '--ink': m.ink, '--leader': `${spot.leader ?? 0}px` } as CSSProperties
        const hov = hovered === m.id
        const cls = `gchip${spot.cell ? ' cell' : ''}${spot.leader ? ' led' : ''} ${m.tone}${m.suspended ? ' suspended' : ''}${selected === m.id ? ' sel' : ''}${hov ? ' hov' : ''}`
        const enter = () => groups.onHover(m.id)
        const leave = () => groups.onHover(null)
        if (spot.cell) {
          const { head, rest } = cellChipText(spot.cell, (id) => marks.byId.get(id)?.code ?? id)
          const label = `${spot.cell.total} vehicles here: ${spot.cell.groups.map(([id, n]) => `${n} of ${id}`).join(', ')}${spot.cell.none ? `, ${spot.cell.none} of no group` : ''}. Select every member of ${m.id}.`
          return (
            <button key={spot.key} ref={ref(spot.key)} className={cls} style={style} onMouseEnter={enter} onMouseLeave={leave} onFocus={enter} onBlur={leave} onClick={() => groups.onSelect(m.id)} aria-label={label} title={label}>
              <span className="gchip-top">
                <span className="gchip-code">{m.code}</span>
                <b>{head}</b>
              </span>
              {rest && <span className="gchip-status">{rest}</span>}
            </button>
          )
        }
        const status = [m.doing, ...m.parts.map((p) => p.text)].join(', ')
        const missing = hov ? groups.detail(m.id) : []
        return (
          <button
            key={spot.key}
            ref={ref(spot.key)}
            className={cls}
            style={style}
            onMouseEnter={enter}
            onMouseLeave={leave}
            onFocus={enter}
            onBlur={leave}
            onClick={() => groups.onSelect(m.id)}
            aria-label={`${m.id} ${m.name}: ${status}. Select its ${m.members.length} members and open its card.`}
          >
            <span className="gchip-top">
              <span className="gchip-code">{m.code}</span>
              <b>{m.id}</b>
              {m.name && <span className="gchip-name">{chipName(m.name)}</span>}
            </span>
            <span className="gchip-status">
              <span className="gchip-doing">{m.doing}</span>
              {m.parts.map((p, i) => (
                <span key={i} className={p.tone ?? ''}>
                  <i aria-hidden> · </i>
                  {p.text}
                </span>
              ))}
            </span>
            {hov && (
              <span className="gchip-tip" role="tooltip">
                <b>
                  {m.id} · {m.name}
                </b>
                <span>{m.members.length} members · {status}</span>
                {missing.length ? missing.map((l) => <span key={l} className="warn">{l}</span>) : <span className="dim">None missing.</span>}
                <span className="dim">Click: select its {m.members.length} members and open its card.</span>
              </span>
            )}
          </button>
        )
      })}
    </div>
  )
}
