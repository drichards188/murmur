import { useEffect, useRef, useState } from 'react'

/** One reference view the menu turns on and off. */
export interface ViewEntry {
  key: string
  label: string
  /** what it shows, for the hover */
  title: string
  on: boolean
  onToggle: () => void
  /** why this run cannot show it ("no space segment in this run"): listed, greyed, not hidden */
  off?: string | null
}

/** The top bar's reference views (the cutdown's ledger 1 section 1: Weather, Space with its globe,
 *  Links, Platforms) behind one quiet button: each one click away, none at the glance. Every view is
 *  listed: one this run cannot show is greyed with the reason beside it, never left out (the owner,
 *  2026-11-24: a view that vanished read as a lost feature). A view that is on is ticked, and the
 *  button says how many are on. Escape or a click outside closes it. */
export function ViewsMenu({ entries }: { entries: ViewEntry[] }) {
  const [open, setOpen] = useState(false)
  const box = useRef<HTMLSpanElement>(null)
  useEffect(() => {
    if (!open) return
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false)
    }
    const esc = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false)
    window.addEventListener('mousedown', away)
    window.addEventListener('keydown', esc)
    return () => {
      window.removeEventListener('mousedown', away)
      window.removeEventListener('keydown', esc)
    }
  }, [open])
  const on = entries.filter((e) => e.on).length
  return (
    <span className="views" ref={box}>
      <button className={`ghost platforms-btn ${open ? 'on' : ''}`} aria-expanded={open} aria-haspopup="menu" onClick={() => setOpen((o) => !o)} title="Reference views: weather, space, link coverage, the platforms">
        Views{on > 0 && <span className="views-n">{on}</span>}
      </button>
      {open && (
        <span className="views-menu" role="menu">
          {entries.map((e) => (
            <button
              key={e.key}
              role="menuitemcheckbox"
              aria-checked={e.on}
              className={e.on ? 'on' : ''}
              title={e.off ? `${e.title}. Not available: ${e.off}` : e.title}
              disabled={!!e.off && !e.on}
              onClick={() => {
                e.onToggle()
                setOpen(false)
              }}
            >
              <span className="views-tick" aria-hidden="true">{e.on ? '✓' : ''}</span>
              {e.label}
              {e.off && <span className="views-off">{e.off}</span>}
            </button>
          ))}
        </span>
      )}
    </span>
  )
}
