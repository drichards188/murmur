import { useEffect } from 'react'
import { LINKS, PROTOCOLS, type Protocol } from './catalogLabels'
import { type EmuDir, emuCause, fmtBps, fmtDuration, useEmuLink } from './linkEmu'
import { symbolSvg } from './symbols'
import { orbitRange, usePlatformLimits } from './taskDefaults'
import type { Tasking, Vehicle } from './types'
import { callsign, DOMAIN_LABEL, domainOf, isStale } from './types'

interface Props {
  v: Vehicle
  taskings: Tasking[]
  selected: boolean
  onToggleSelect: () => void
  onClose: () => void
  /** the camera is following this vehicle; onFollow resumes after the operator panned away */
  following: boolean
  onFollow: () => void
}

function fmt(n: number | null | undefined, digits = 0, unit = '') {
  return n == null || Number.isNaN(n) ? '–' : `${n.toFixed(digits)}${unit}`
}

function Item({ k, children, title }: { k: string; children: React.ReactNode; title?: string }) {
  return (
    <>
      <dt title={title}>{k}</dt>
      <dd>{children}</dd>
    </>
  )
}

const pct = (x: number | null | undefined, digits = 0) => (x == null ? '–' : `${(x * 100).toFixed(digits)}%`)

function dirLine(d: EmuDir) {
  return d.pkts ? `${pct(d.loss)} loss, ${fmt(d.latency_ms, 0, ' ms')}` : 'no packets'
}

/** The link emulator's state for this vehicle's link (Link section rows), from the bridge's
 *  /api/links/{uid}. Nothing when no emulator runs. */
function EmuItems({ uid }: { uid: number }) {
  const ans = useEmuLink(uid)
  if (!ans || !ans.enabled) return null
  const l = ans.link
  if (!ans.ok || !l) {
    return <Item k="Emulator">{ans.error ? <span className="warn">no answer ({ans.error})</span> : ans.detail ?? 'waiting'}</Item>
  }
  const n = l.nominal
  const merged: Record<string, number> = {}
  for (const d of [l.recent.up, l.recent.down]) for (const [k, c] of Object.entries(d.by_cause)) merged[k] = (merged[k] ?? 0) + c
  const causes = Object.entries(merged).sort((a, b) => b[1] - a[1])
  const sat = l.satcom
  return (
    <>
      <Item k="Emulator" title="crates/murmur-linkemu: what the link model is doing to this link">
        <b className={l.state === 'down' ? 'bad' : l.state === 'degraded' ? 'warn' : ''}>{l.state === 'down' ? 'link down' : l.state}</b>
        {l.cause ? `: ${emuCause(l.cause, n?.down_when)}` : ''} · profile {l.profile}
        {l.active_profile && l.active_profile !== l.profile ? (
          <span title={`The ${l.profile} link is closed at this depth (${l.depth_state}); traffic goes over ${l.active_profile}`}> · now on {l.active_profile}</span>
        ) : null}
      </Item>
      {n && (
        <Item k="Profile" title="The profile's nominal numbers (sim/links.toml)">
          {pct(n.loss, 1)} loss · {fmt(n.latency_ms, 0)} ±{fmt(n.jitter_ms, 0, ' ms')}
          {n.propagation_mps ? ` + range/${n.propagation_mps} m/s` : ''} · {fmtBps(n.bandwidth_bps)}
        </Item>
      )}
      <Item k="Now" title={`Over the last ${Math.round(l.recent.window_s)} s: uplink (vehicle to bridge), downlink; delay is the emulator's added one-way latency`}>
        up {dirLine(l.recent.up)} · down {dirLine(l.recent.down)}
        {l.emu_rtt_ms != null ? ` · RTT ${fmt(l.emu_rtt_ms, 0, ' ms')}` : ''} <span className="muted">({Math.round(l.recent.window_s)} s)</span>
      </Item>
      {causes.length > 0 && (
        <Item k="Drops" title={`Packets dropped by cause, last ${Math.round(l.recent.window_s)} s`}>
          {causes.map(([k, c]) => `${emuCause(k, n?.down_when)} ${c}`).join(' · ')}
        </Item>
      )}
      {l.events.length > 0 && (
        <Item k="Events">
          <b className="warn">{l.events.join(', ')}</b>
        </Item>
      )}
      {l.los && l.los.state !== 'off' && (
        <Item k="Terrain" title="Line of sight to the ground station over tidewater's DEM">
          {l.los.state === 'obstructed' ? (
            <b className="bad">blocked by {fmt(l.los.obstruction_m, 0, ' m')} of terrain</b>
          ) : l.los.state === 'clear' ? (
            `clear by ${fmt(l.los.obstruction_m == null ? null : -l.los.obstruction_m, 0, ' m')}`
          ) : (
            'not known yet'
          )}
        </Item>
      )}
      {n?.down_when && (
        <Item k="Depth">
          {l.depth_state} ({l.profile} {n.down_when === 'surfaced' ? 'needs the vehicle submerged' : 'needs the vehicle surfaced'}
          {n.gated_profile ? `; otherwise ${n.gated_profile}` : ''})
        </Item>
      )}
      {sat && (
        <Item k="Satellite" title={sat.mode === 'static' ? `Static profile numbers: ${sat.static_reason ?? ''}` : 'Best satellite of the class by elevation (murmur-space, real orbits)'}>
          {sat.mode !== 'live' ? (
            <span className="muted">{sat.mode === 'static' ? `static numbers (${sat.static_reason ?? 'no space data'})` : 'computing'}</span>
          ) : sat.up ? (
            <>
              {sat.best_name ?? '?'} {sat.best_norad_id != null ? `(${sat.best_norad_id})` : ''} · el {fmt(sat.best_elevation_deg, 0, '°')} · {fmt(sat.latency_ms, 0, ' ms')} one way
              {sat.latency_source === 'profile' ? ' (profile)' : ''} · {sat.visible ?? 0} in view · {sat.handovers} handovers
              {sat.next_outage_in_s != null ? ` · next outage in ${fmtDuration(sat.next_outage_in_s)}` : ''}
            </>
          ) : (
            <b className="bad">
              no {sat.class} satellite in view{sat.next_pass_in_s != null ? `; next pass in ${fmtDuration(sat.next_pass_in_s)}` : ''}
            </b>
          )}
        </Item>
      )}
    </>
  )
}

/** Everything known about one vehicle: identity, live state, link, platform limits and its
 *  current task. Opened by clicking a fleet row; selecting for tasking is a separate control. */
export function VehicleDetails({ v, taskings, selected, onToggleSelect, onClose, following, onFollow }: Props) {
  const limits = usePlatformLimits()
  const p = v.identity ? limits?.get(v.identity.platform) : undefined
  const stale = isStale(v)
  const domain = domainOf(v)
  const orbit = orbitRange([v], limits)
  const op = taskings.flatMap((t) => t.ops.filter((o) => o.uid === v.uid).map((o) => ({ o, t }))).sort((a, b) => b.t.created - a.t.created)[0]

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  return (
    <aside className="vdetails" aria-label={`Details for ${callsign(v)}`}>
      <header className="vd-head">
        <span className="vd-sym" dangerouslySetInnerHTML={{ __html: symbolSvg(v, { stale, size: 22, labels: false }).svg }} />
        <div className="vd-title">
          <div className="vd-name">
            {callsign(v)} {v.identity && <span className="vrow-short">{v.identity.short}</span>}
            {v.identity?.armed ? <span className="vrow-weapons">WEAPONS</span> : null}
            {v.identity?.sim === 'standin' && <span className="vrow-standin">STAND-IN</span>}
          </div>
          {v.identity && (
            <div className="muted">
              {v.identity.name} · {v.identity.role}
            </div>
          )}
        </div>
        <button
          className={`ghost mini vd-follow ${following ? 'on' : ''}`}
          onClick={onFollow}
          title={following ? 'Camera follows this vehicle (pan the map to stop)' : 'Follow this vehicle with the camera'}
        >
          {following ? 'following' : 'follow'}
        </button>
        <button className="ghost mini" onClick={onClose} aria-label="Close details">
          ✕
        </button>
      </header>

      <button className={`vd-select ${selected ? 'on' : ''}`} onClick={onToggleSelect}>
        {selected ? '✓ Selected for tasking' : 'Select for tasking'}
      </button>

      <section>
        <h4>Now</h4>
        <dl>
          <Item k="Mode">
            {v.mode ?? '–'}
            {v.armed ? <em className="motors-on"> motors on</em> : null}
          </Item>
          <Item k="Navigation">
            {v.nav_ready ? 'ready' : 'not ready'}
            {v.nav_source === 'dead_reckoning' ? (
              <b title="No GNSS: position from inertial navigation and DVL, error growing with distance travelled"> on dead reckoning</b>
            ) : null}{' '}
            · GPS {v.gps_fix >= 3 ? '3D fix' : v.gps_fix === 2 ? '2D fix' : 'no fix'}{v.satellites ? `, ${v.satellites} sats` : ''}</Item>
          <Item k="Position">{v.position ? `${v.position[1].toFixed(5)}, ${v.position[0].toFixed(5)}` : '–'}</Item>
          {domain === 'subsurface' ? (
            <Item k="Depth">{v.alt_msl == null ? '–' : `${Math.max(0, -v.alt_msl).toFixed(1)} m`}</Item>
          ) : domain === 'air' ? (
            <Item k="Altitude">
              {fmt(v.alt_msl, 0, ' m MSL')} · {fmt(v.alt_rel, 0, ' m above home')}
            </Item>
          ) : null}
          <Item k="Speed">
            {fmt(v.groundspeed, 1, ' m/s ground')}
            {domain === 'air' && v.airspeed != null ? ` · ${fmt(v.airspeed, 1, ' m/s air')}` : ''}
          </Item>
          <Item k="Heading">{fmt(v.heading, 0, '°')}</Item>
          <Item k="Battery">{fmt(v.battery_pct, 0, '%')}</Item>
          <Item k="Domain">{DOMAIN_LABEL[domain]}</Item>
        </dl>
      </section>

      <section>
        <h4>Link</h4>
        <dl>
          <Item k="Type">{v.identity ? (LINKS[v.identity.link] ?? v.identity.link) : '–'}{p?.space_class ? ` (${p.space_class})` : ''}</Item>
          <Item k="Last message">{stale ? <b className="bad">{Math.floor((v.link.since_last_ms ?? 0) / 1000)} s ago</b> : fmt(v.link.since_last_ms, 0, ' ms ago')}
            {(v.link.expected_interval_ms ?? 0) > 1000 ? ` (low-rate link: reports every ${Math.round((v.link.expected_interval_ms ?? 0) / 1000)} s)` : ''}
          </Item>
          <Item k="Loss / RTT" title="Measured by the bridge: MAVLink sequence gaps over 10 s, TIMESYNC round trip">
            {fmt(v.link.loss_pct, 0, '%')} · {fmt(v.link.rtt_ms, 0, ' ms')}
          </Item>
          <EmuItems uid={v.uid} />
        </dl>
      </section>

      <section>
        <h4>Task</h4>
        {op ? (
          <dl>
            <Item k="Current">
              {op.o.task.toUpperCase()} #{op.t.id} · <b className={op.o.state === 'failed' || op.o.state === 'superseded' ? 'bad' : ''}>{op.o.state}</b>
            </Item>
            {op.o.detail && <Item k="Detail">{op.o.detail}</Item>}
            {op.o.progress && op.o.progress !== op.o.detail && <Item k="Progress">{op.o.progress}</Item>}
          </dl>
        ) : (
          <p className="muted">No task sent this session.</p>
        )}
        {p && (
          <p className="muted">
            Can do: {p.tasks.length ? p.tasks.join(', ') : 'nothing (telemetry only)'}
            {p.tasks.includes('orbit') ? ` · orbit radius ≥ ${orbit.min} m` : ''}
          </p>
        )}
      </section>

      {p && (
        <section>
          <h4>Platform</h4>
          <dl>
            <Item k="Runs as">{v.identity?.sim === 'sitl' ? 'ArduPilot SITL (real firmware)' : 'Stand-in (MAVLink kinematic model)'}</Item>
            <Item k="Speed">
              {fmt(p.cruise_mps, 0, ' m/s cruise')}
              {p.max_mps ? ` · ${fmt(p.max_mps, 0, ' max')}` : ''}
            </Item>
            {p.min_turn_radius_m != null && <Item k="Turn radius">{fmt(p.min_turn_radius_m, 0, ' m')}</Item>}
            {p.endurance_h != null && <Item k="Endurance">{fmt(p.endurance_h, p.endurance_h < 10 ? 1 : 0, ' h')}</Item>}
            {p.alt_m != null && <Item k="Typical alt.">{fmt(p.alt_m, 0, ' m')}</Item>}
            {p.protocol && (
              <Item k="Real protocol" title="How a C2 reaches the real platform">
                <span title={p.protocol_note}>{PROTOCOLS[p.protocol as Protocol] ?? p.protocol}</span>
              </Item>
            )}
          </dl>
          {p.authenticity && <p className="muted">Max authenticity: {p.authenticity}</p>}
          {p.authenticity_blocker && <p className="muted warn">Blocker: {p.authenticity_blocker}</p>}
        </section>
      )}

      {v.statustext.length > 0 && (
        <section>
          <h4>Messages</h4>
          <ul className="vd-msgs">
            {v.statustext
              .slice(-6)
              .reverse()
              .map((m, i) => (
                <li key={i} className={m.severity <= 3 ? 'bad' : m.severity <= 4 ? 'warn' : ''}>
                  {m.text}
                </li>
              ))}
          </ul>
        </section>
      )}
    </aside>
  )
}
