// The group strip (bd murmur-4nik): every group in one row at the map's foot, each its colour, code,
// name, its state as an icon and a word, what it does and its strength against its minimum. In the
// manner of a strategy game's control-group bar, but a view, not a command: a click centres the map
// on the group and lights it up for a moment; it selects nothing and sends nothing (orders go
// through the order form and the engine). Folds away to one button, remembered in this browser.
import { type CSSProperties, useState } from 'react'
import { GROUP_STATE, type GroupMark, type GroupState, stripParts } from './swarmsModel'
import './swarms.css'

const KEY = 'murmur.groupStrip'

function savedOpen(): boolean {
  try {
    return localStorage.getItem(KEY) !== 'folded'
  } catch {
    return true
  }
}

interface Props {
  marks: GroupMark[]
  /** each group's state now (swarmsModel groupState) */
  states: ReadonlyMap<string, GroupState>
  selected: string | null
  hovered: string | null
  onHover: (id: string | null) => void
  /** centre the map on the group and light it up */
  onShow: (id: string) => void
  /** a drawer is open over the map's right edge: the strip keeps clear of it */
  drawer?: boolean
}

export function GroupStrip({ marks, states, selected, hovered, onHover, onShow, drawer = false }: Props) {
  const [open, setOpen] = useState(savedOpen)
  const toggle = () => {
    setOpen((o) => {
      try {
        localStorage.setItem(KEY, o ? 'folded' : 'open')
      } catch {
        // no storage: the choice lasts this page
      }
      return !o
    })
  }
  if (!marks.length) return null
  return (
    <nav className={`gstrip${open ? '' : ' folded'}${drawer ? ' drawer' : ''}`} aria-label="Groups: a view; a click centres the map on one">
      <button className="gstrip-toggle" onClick={toggle} aria-expanded={open} title={open ? 'Fold the group strip' : 'Show the group strip: each group, its state and strength'}>
        Groups <span aria-hidden>{open ? '▾' : '▸'}</span>
        {!open && <span className="gstrip-n">{marks.length}</span>}
      </button>
      {open && (
        <ul className="gstrip-list">
          {marks.map((m) => {
            const st = GROUP_STATE[states.get(m.id) ?? 'idle']
            const style = { '--g': m.color, '--ink': m.ink } as CSSProperties
            return (
              <li key={m.id}>
                <button
                  className={`gstrip-item ${st.tone}${selected === m.id ? ' sel' : ''}${hovered === m.id ? ' hov' : ''}`}
                  style={style}
                  onClick={() => onShow(m.id)}
                  onMouseEnter={() => onHover(m.id)}
                  onMouseLeave={() => onHover(null)}
                  title={`${m.id} · ${m.name}: ${st.word}; ${m.doing}, ${m.parts.map((p) => p.text).join(', ')}. A click centres the map on it.`}
                >
                  <span className="gstrip-top">
                    <span className="gchip-code">{m.code}</span>
                    <b>{m.id}</b>
                    <span className={`gstrip-state ${st.tone}`}>
                      <i aria-hidden>{st.icon}</i> {st.word}
                    </span>
                  </span>
                  <span className="gstrip-line">
                    {m.doing}
                    {stripParts(m.parts).map((p, i) => (
                      <span key={i} className={p.tone ?? ''}>
                        {' · '}
                        {p.text}
                      </span>
                    ))}
                  </span>
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </nav>
  )
}
