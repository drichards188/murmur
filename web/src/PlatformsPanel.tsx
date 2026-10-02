import ms from 'milsymbol'
import { Fragment, useEffect, useState } from 'react'
import type { Domain } from './types'
import { LINKS, PROTOCOLS, type Protocol } from './catalogLabels'


export interface Platform {
  id: string
  name: string
  short: string
  role: string
  domain: Domain
  sim: 'sitl' | 'standin'
  link: string
  sidc: string
  callsign: string
  cruise_mps: number | null
  max_mps: number | null
  endurance_h: number | null
  alt_m: number | null
  min_turn_radius_m: number | null
  protocol?: Protocol
  protocol_note?: string
  authenticity?: string
  authenticity_blocker?: string
  tasks: string[]
  in_fleet: { uid: number; sysid: number | null; callsign: string }[]
}

const DOMAINS: { id: Domain; title: string }[] = [
  { id: 'air', title: 'Air' },
  { id: 'ground', title: 'Ground' },
  { id: 'surface', title: 'Water' },
  { id: 'subsurface', title: 'Subsurface' },
]



const icons = new Map<string, string>()
function icon(sidc: string) {
  // no text modifiers: the type column already names it, and labels shrink the symbol
  if (!icons.has(sidc)) icons.set(sidc, new ms.Symbol(sidc, { size: 22 }).asSVG())
  return icons.get(sidc)!
}

function speed(p: Platform) {
  if (p.cruise_mps == null) return '–'
  if (p.cruise_mps === 0) return 'drifts'
  return `${p.cruise_mps} m/s (${Math.round(p.cruise_mps * 1.944)} kt)`
}

function altitude(p: Platform) {
  if (p.alt_m == null) return '–'
  if (p.alt_m < 0) return `${-p.alt_m} m depth`
  if (p.alt_m === 0) return p.domain === 'ground' ? 'ground' : 'surface'
  return p.alt_m >= 1000 ? `${(p.alt_m / 1000).toFixed(1)} km` : `${p.alt_m} m`
}

function endurance(h: number | null) {
  if (h == null) return '–'
  if (h >= 24 * 60) return `~${Math.round(h / (24 * 30))} months`
  if (h >= 48) return `~${Math.round(h / 24)} days`
  if (h >= 1) return `${h} h`
  return `${Math.round(h * 60)} min`
}

interface Props {
  onClose: () => void
  onFocus: (uid: number) => void
}

export function PlatformsPanel({ onClose, onFocus }: Props) {
  const [platforms, setPlatforms] = useState<Platform[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [open, setOpen] = useState<Set<string>>(new Set())
  const toggle = (id: string) =>
    setOpen((o) => {
      const n = new Set(o)
      if (!n.delete(id)) n.add(id)
      return n
    })

  useEffect(() => {
    fetch('/api/platforms')
      .then((r) => (r.ok ? r.json() : Promise.reject(new Error(`bridge returned ${r.status}`))))
      .then((d) => setPlatforms(d.platforms))
      .catch((e) => setError(String(e.message ?? e)))
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  const fielded = platforms?.filter((p) => p.in_fleet.length).length ?? 0

  return (
    <div className="platforms" role="dialog" aria-label="Platform capabilities">
      <header className="platforms-head">
        <h2>Platforms</h2>
        {platforms && (
          <span className="muted">
            {platforms.length} types · {fielded} in the current fleet · figures are approximate, see sim/platforms.toml
          </span>
        )}
        <button className="ghost close" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>
      {error && <p className="error">{error}</p>}
      {!platforms && !error && <p className="empty">Loading…</p>}
      {platforms && (
        <div className="platforms-body">
          <table>
            <thead>
              <tr>
                <th />
                <th>Type</th>
                <th>Sim</th>
                <th>Cruise</th>
                <th>Alt / depth</th>
                <th>Endurance</th>
                <th>Link</th>
                <th>Tasks</th>
                <th>In fleet</th>
              </tr>
            </thead>
            {DOMAINS.map(({ id, title }) => {
              const rows = platforms.filter((p) => p.domain === id)
              if (!rows.length) return null
              return (
                <tbody key={id}>
                  <tr className="domain">
                    <td colSpan={9}>
                      {title} <span>{rows.length}</span>
                    </td>
                  </tr>
                  {rows.map((p) => (
                    <Fragment key={p.id}>
                    <tr className={p.in_fleet.length ? '' : 'absent'}>
                      <td className="sym" dangerouslySetInnerHTML={{ __html: icon(p.sidc) }} />
                      <td>
                        <div className="pname">
                          {p.name} <span className="vrow-short">{p.short}</span>
                        </div>
                        <div className="muted">{p.role}</div>
                        {p.protocol && (
                          <div className="protorow">
                            <span className={`prototag ${p.protocol}`} title={p.protocol_note}>
                              {PROTOCOLS[p.protocol] ?? p.protocol}
                            </span>
                            {p.authenticity && (
                              <button className="ghost moretoggle" onClick={() => toggle(p.id)} aria-expanded={open.has(p.id)}>
                                {open.has(p.id) ? 'hide authenticity' : 'authenticity'}
                              </button>
                            )}
                          </div>
                        )}
                      </td>
                      <td>
                        <span className={`simtag ${p.sim}`}>{p.sim === 'sitl' ? 'ArduPilot' : 'stand-in'}</span>
                      </td>
                      <td className="num">{speed(p)}</td>
                      <td className="num">
                        {altitude(p)}
                        {p.min_turn_radius_m ? <div className="muted">turn ≥ {p.min_turn_radius_m} m</div> : null}
                      </td>
                      <td className="num">{endurance(p.endurance_h)}</td>
                      <td>{LINKS[p.link] ?? p.link}</td>
                      <td className="tasks">
                        {p.tasks.length ? (
                          p.tasks.map((t) => (
                            <span key={t} className="taskchip">
                              {t}
                            </span>
                          ))
                        ) : (
                          <span className="muted">none (no propulsion)</span>
                        )}
                      </td>
                      <td>
                        {p.in_fleet.length ? (
                          p.in_fleet.map((m) => (
                            <button key={m.uid} className="cschip" onClick={() => onFocus(m.uid)}>
                              {m.callsign}
                            </button>
                          ))
                        ) : (
                          <span className="muted">not in this fleet</span>
                        )}
                      </td>
                    </tr>
                    {open.has(p.id) && (
                      <tr className={`detail${p.in_fleet.length ? '' : ' absent'}`}>
                        <td />
                        <td colSpan={8}>
                          <div>
                            <span className="dlabel">Max authenticity</span> {p.authenticity}
                          </div>
                          {p.authenticity_blocker && (
                            <div>
                              <span className="dlabel blocker">Blocker</span> {p.authenticity_blocker}
                            </div>
                          )}
                          {p.protocol_note && (
                            <div className="muted">
                              <span className="dlabel">Protocol</span> {p.protocol_note}
                            </div>
                          )}
                        </td>
                      </tr>
                    )}
                    </Fragment>
                  ))}
                </tbody>
              )
            })}
          </table>
        </div>
      )}
    </div>
  )
}
