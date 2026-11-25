import { useEffect, useState } from 'react'
import { LINKS } from './catalogLabels'
import { awayText, FAILSAFE_WORDS, linkLine } from './detailsText'
import { BASIS_LABEL, estimateAt, failsafeAt, flatBearing, flatDistance, regionText } from './estimate'
import { flagNode } from './ident'
import { evidenceLine, NO_NODE_LOSS, type NodeLossView, regionLines, STANDING_LABEL, standingOf } from './nodeLoss'
import './nodeLoss.css'
import type { NodeRow } from './useNodes'
import { orderCard } from './orderCard'
import { doingNow, latestOrders, opWord, opWordIsBad, orderOver, type OrderIntent } from './orderWords'
import { usePoll } from './space'
import { conditionsText, type ReachAnswer, sensorLines } from './sensorReach'
import { type EmuDir, emuCause, fmtDuration, useEmuLink } from './linkEmu'
import { symbolSvg } from './symbols'
import { orbitRange, usePlatformLimits } from './taskDefaults'
import type { Estimate, Tasking, Vehicle } from './types'
import { callsign, domainOf, isStale } from './types'
import { exceptionOf, type ExceptionContext, NO_EXCEPTIONS } from './vehicleException'
import { useFloatBar } from './floating'
import { FloatGrip } from './FloatingPanel'

interface Props {
  v: Vehicle
  taskings: Tasking[]
  /** the intents the taskings carry out: what it is doing, in the words of its order */
  intents?: readonly OrderIntent[]
  /** the map's sensor reach answer (`GET /api/sensing/reach`, the rings'): its Sensors lines; the
   *  details ask for this vehicle's own while the rings' answer does not hold it */
  reach?: ReachAnswer | null
  /** what its exception slot is made from (vehicleException.ts): the same slot as its fleet row */
  exceptions?: ExceptionContext
  /** bridge clock, ms (useBridgeClock): the estimate of a silent vehicle advances with it */
  now: () => number
  selected: boolean
  onToggleSelect: () => void
  onClose: () => void
  /** the camera is following this vehicle; onFollow resumes after the operator panned away */
  following: boolean
  onFollow: () => void
  /** armed platforms: open the engagement nomination form with this platform (Phase E) */
  onEngage?: () => void
  /** rules 30: our vehicles' standing (the review's; a run before rules 30 has none) and this
   *  vehicle's row of `GET /api/nodes` (its region as the keep-out counts it) */
  nodeLoss?: NodeLossView
  nodeRow?: NodeRow
  /** open the Review at this vehicle's card (`N-<uid>` presumed or flagged, `L-<uid>` lost) */
  onReview?: (id: string) => void
  /** one of ours by its uid as a person names it (its callsign): the evidence's sensors */
  nameOf?: (uid: string) => string
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

/** The evidence behind a section's bottom line, one click away (requirements.md: a fold). */
function Fold({ summary, children }: { summary: string; children: React.ReactNode }) {
  return (
    <details className="vd-fold">
      <summary>{summary}</summary>
      {children}
    </details>
  )
}

const pct = (x: number | null | undefined, digits = 0) => (x == null ? '–' : `${(x * 100).toFixed(digits)}%`)

/** Rules 24 (bd murmur-lbw8.1): one weapon of an armed platform's loadout, as /api/loadouts
 *  serves it (the engine's rounds left). */
interface Weapon {
  name: string
  kind: string
  count: number
  left: number
  inert: boolean
  /** rules 26: a gun's burst and the seconds between bursts */
  burst_rounds?: number | null
  burst_interval_s?: number | null
  min_range_m: number
  max_range_m: number
  speed_mps: number
  targets: string[]
}

/** The platform's loadout with each weapon's rounds left (polled from /api/loadouts while the
 *  details are open). Nothing for a vehicle without one. */
function LoadoutItems({ uid }: { uid: number }) {
  const [weapons, setWeapons] = useState<{ uid: number; w: Weapon[] } | null>(null)
  useEffect(() => {
    let live = true
    const load = async () => {
      try {
        const res = await fetch('/api/loadouts')
        const body = (await res.json()) as { vehicles?: { uid: number; weapons: Weapon[] }[] }
        const mine = body.vehicles?.find((x) => x.uid === uid)
        if (live) setWeapons({ uid, w: mine?.weapons ?? [] })
      } catch {
        if (live) setWeapons({ uid, w: [] })
      }
    }
    void load()
    const t = window.setInterval(load, 5000)
    return () => {
      live = false
      window.clearInterval(t)
    }
  }, [uid])
  const ws = weapons && weapons.uid === uid ? weapons.w : []
  if (!ws.length) return null
  return (
    <Item k="Loadout" title="Rounds left of each weapon (the engine's count: each release spends one round, each burst of a gun its rounds, each mine laid one)">
      {ws.map((w, i) => (
        <span key={w.name} className={w.inert ? 'muted' : w.left === 0 ? 'bad' : ''}
          title={`${w.kind}, ${fmt(w.min_range_m)} to ${fmt(w.max_range_m, 0, ' m')} at ${fmt(w.speed_mps, 0, ' m/s')}; for ${w.targets.join(', ')} targets${w.burst_rounds ? `; bursts of ${w.burst_rounds} every ${fmt(w.burst_interval_s ?? 0, 0, ' s')}` : ''}${w.inert ? '; inert until its stage' : ''}`}>
          {i > 0 ? ' · ' : ''}
          {w.name} {w.left}/{w.count}
          {w.inert ? ' (inert)' : ''}
        </span>
      ))}
    </Item>
  )
}

/** Each sensor's reach now where the vehicle is, against its clear day's (R4), from the rings'
 *  answer, or this vehicle's own while that one does not hold it. Nothing without reaches (the
 *  legacy sensing model, or before the first sensing step). */
function SensorItems({ uid, shared }: { uid: number; shared?: ReachAnswer | null }) {
  const has = shared?.vehicles.some((x) => x.uid === uid) ?? false
  const own = usePoll<ReachAnswer>(has ? null : `/api/sensing/reach?uids=${uid}`, 3000)
  const answer = has ? shared : own.data
  const r = answer?.available ? answer.vehicles.find((x) => x.uid === uid) : undefined
  if (!r) return null
  const lines = sensorLines(r)
  if (!lines.length) return null
  return (
    <Item k="Sensors" title={`Each sensor's reach now, where it is (${conditionsText(r.conditions)}), against a reference contact; the rings on the map show it. Decisions read the same reach: an identify order's standoff, a strike assessment's cover.`}>
      {answer?.stale && <span className="muted">stale · </span>}
      {lines.map((l) => (
        <span key={l.name} className={`vd-sensor${l.low ? ' warn' : ''}`}>
          <b>{l.name}</b> {l.text}
        </span>
      ))}
    </Item>
  )
}

function dirLine(d: EmuDir) {
  return d.pkts ? `${pct(d.loss)} loss, ${fmt(d.latency_ms, 0, ' ms')}` : 'no packets'
}

const fmtM = (m: number) => (m >= 1000 ? `${(m / 1000).toFixed(1)} km` : `${Math.round(m)} m`)

/** Its link (R5): one line, "SATCOM, good" when nothing is wrong, else what failed (silent, down
 *  and why, degraded, measured loss, heard late); under it, only while so, what is done to it, the
 *  terrain in the way, a sub's depth gate, the satellite gap and the next outage. The numbers are
 *  in a fold (the bridge's measures; the link emulator's view from /api/links/{uid} when one runs).
 *  The depth gate only for a vehicle that dives: an aircraft's SATCOM profile has the gate too, and
 *  its details said "Depth: surfaced" (bd murmur-iobd.8). */
function LinkSection({ v, dives, spaceClass }: { v: Vehicle; dives: boolean; spaceClass?: string }) {
  const ans = useEmuLink(v.uid)
  const l = ans?.enabled && ans.ok ? (ans.link ?? null) : null
  const stale = isStale(v)
  // the emulator's cause in words; with none named, what it drops the most packets for
  const drops: Record<string, number> = {}
  if (l) for (const d of [l.recent.up, l.recent.down]) for (const [k, c] of Object.entries(d.by_cause)) drops[k] = (drops[k] ?? 0) + c
  const topDrop = Object.entries(drops).sort((a, b) => b[1] - a[1])[0]?.[0] ?? null
  const emu = l
    ? { state: l.state, cause: emuCause(l.cause ?? (l.state !== 'ok' ? topDrop : null), l.nominal?.down_when) }
    : v.link.emu
      ? { state: v.link.emu.state, cause: emuCause(v.link.emu.cause) }
      : null
  const line = linkLine({ type: v.identity ? (LINKS[v.identity.link] ?? v.identity.link) : 'Link', stale, sinceMs: v.link.since_last_ms, expectedMs: v.link.expected_interval_ms ?? null, lossPct: v.link.loss_pct, emu })
  const sat = l?.satcom
  const n = l?.nominal
  return (
    <section>
      <h4>Link</h4>
      <p className={line.tone}>{line.text}</p>
      <dl>
        {l && l.events.length > 0 && (
          <Item k="Events">
            <b className="warn">{l.events.join(', ')}</b>
          </Item>
        )}
        {l?.los?.state === 'obstructed' && (
          <Item k="Terrain" title="Line of sight to the ground station carrying the link, over tidewater's DEM">
            <b className="bad">blocked by {fmt(l.los.obstruction_m, 0, ' m')} of terrain</b>
          </Item>
        )}
        {dives && l && n?.down_when && (
          <Item k="Depth">
            {l.depth_state} ({l.profile} {n.down_when === 'surfaced' ? 'needs the vehicle submerged' : 'needs the vehicle surfaced'}
            {n.gated_profile ? `; otherwise ${n.gated_profile}` : ''})
            {l.active_profile && l.active_profile !== l.profile ? `, now on ${l.active_profile}` : ''}
          </Item>
        )}
        {sat?.mode === 'live' &&
          (sat.up ? (
            sat.next_outage_in_s != null && <Item k="Satellite">next outage in {fmtDuration(sat.next_outage_in_s)}</Item>
          ) : (
            <Item k="Satellite">
              <b className="bad">
                no {sat.class} satellite in view{sat.next_pass_in_s != null ? `; next pass in ${fmtDuration(sat.next_pass_in_s)}` : ''}
              </b>
            </Item>
          ))}
      </dl>
      <Fold summary="Numbers">
        <dl>
          <Item k="Last heard">
            {stale ? `${Math.floor((v.link.since_last_ms ?? 0) / 1000)} s ago` : fmt(v.link.since_last_ms, 0, ' ms ago')}
            {(v.link.expected_interval_ms ?? 0) > 1000 ? ` (low-rate link: reports every ${Math.round((v.link.expected_interval_ms ?? 0) / 1000)} s)` : ''}
          </Item>
          <Item k="Loss / RTT" title="Measured by the bridge: MAVLink sequence gaps over 10 s, TIMESYNC round trip">
            {fmt(v.link.loss_pct, 0, '%')} · {fmt(v.link.rtt_ms, 0, ' ms')}
            {(v.link.ack_timeout_ms ?? 0) > 1000 ? ` · ACK wait ${((v.link.ack_timeout_ms ?? 0) / 1000).toFixed(1)} s` : ''}
          </Item>
          {spaceClass && <Item k="Class">{spaceClass}</Item>}
          {l && (
            <Item k="Now" title={`Over the last ${Math.round(l.recent.window_s)} s: uplink (vehicle to bridge), downlink; delay is the emulator's added one-way latency`}>
              up {dirLine(l.recent.up)} · down {dirLine(l.recent.down)}
              {l.emu_rtt_ms != null ? ` · RTT ${fmt(l.emu_rtt_ms, 0, ' ms')}` : ''}
            </Item>
          )}
          {l?.station && (
            <Item k="Station" title="The ground station carrying this link now: with several, the one giving the best path (range, terrain)">
              {l.station}
              {l.station_range_m != null ? ` · ${fmtM(l.station_range_m)}` : ''}
            </Item>
          )}
          {ans?.enabled && !ans.ok && <Item k="Emulator">{ans.error ? `no answer (${ans.error})` : (ans.detail ?? 'waiting')}</Item>}
        </dl>
      </Fold>
    </section>
  )
}

/** Where a silent vehicle probably is (bridge `estimate`, murmur-4y9), in words: how far and which
 *  way from its last report, what it is probably doing, and what its lost-link failsafe will do;
 *  how sure, the other hypothesis and the engine's own words in a fold. `silent`: say how long it
 *  has been silent (the Standing section says it under rules 30). */
function EstimateItems({ v, e, nowMs, silent }: { v: Vehicle; e: Estimate; nowMs: number; silent: boolean }) {
  if (!v.position) return null
  const task = estimateAt(e, nowMs)
  const fs = e.failsafe
  const branch = failsafeAt(e, nowMs)
  // once the timer has fired the branch leads (the vehicle is probably on it, if it lost us
  // too); the task model stays as the other hypothesis
  const lead = branch ?? task
  const at = v.position
  const where = (p: [number, number]) => awayText(flatDistance(at, p), flatBearing(at, p))
  return (
    <>
      <dl>
        {silent && (
          <Item k="Silent">
            <b className="bad">{fmtDuration((v.link.since_last_ms ?? 0) / 1000)}</b>
          </Item>
        )}
        <Item k="Probably" title="The bridge's motion model for this silence, evaluated now">
          {where(lead.position)}
        </Item>
        <Item k="Basis" title="What the estimate rests on: the lost-link failsafe once its timer fired, else in order the current task, the mode, the last velocity, else stationary">
          {branch && fs ? <b className="warn">{FAILSAFE_WORDS[fs.action] ?? fs.action} on its lost-link failsafe, since {fmtDuration(nowMs / 1000 - fs.fires_t)}, if it lost us too</b> : BASIS_LABEL[e.basis]}
        </Item>
        {!branch && fs && (
          <Item k="Lost link" title="The platform's GCS failsafe timer for its link profile and what its firmware does when it fires">
            {FAILSAFE_WORDS[fs.action] ?? fs.action} in {fmtDuration(fs.fires_t - nowMs / 1000)} if it lost us too
          </Item>
        )}
      </dl>
      <Fold summary="How sure">
        <dl>
          <Item k="Uncertainty" title={`Grows with the silence (platform top speed ${Math.round(e.reach_mps)} m/s, capped at ${fmtM(e.cap_m)}); about two sigma`}>
            {regionText(lead.region)}
          </Item>
          {branch && (
            <Item k="If it kept its task" title="The other hypothesis: the vehicle never heard the failsafe (it still hears us) and went on with what it was doing">
              {BASIS_LABEL[e.basis]}, {where(task.position)}, {regionText(task.region)}
            </Item>
          )}
          <Item k="Engine">{branch && fs ? fs.detail : e.detail}</Item>
          {!branch && fs && <Item k="Failsafe">{fs.detail}</Item>}
          {e.note && <Item k="Note">{e.note}</Item>}
        </dl>
      </Fold>
    </>
  )
}

/** Rules 30 (docs/NODE-LOSS.md section 5): the vehicle's standing while it is not reporting, its
 *  consequence in a clause, the evidence and the region every engagement keep-out counts it in
 *  folded, and a way to put a silent one on the Review by hand, saying why (any operator; it decides
 *  nothing). Confirming and revoking are the Review card's (an operator with engagement authority
 *  confirms), never here. */
function StandingSection({ v, nodes, row, now, onReview, nameOf }: { v: Vehicle; nodes: NodeLossView; row?: NodeRow; now: () => number; onReview?: (id: string) => void; nameOf?: (uid: string) => string }) {
  const standing = standingOf(nodes, v.uid, isStale(v))
  const [note, setNote] = useState('')
  const [flagging, setFlagging] = useState(false)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState<string | null>(null)
  if (!standing) return null
  const loss = nodes.review.get(v.uid)
  const lost = nodes.lost.get(v.uid)
  const ago = (t: number) => `${fmtDuration(Math.max(0, now() / 1000 - t))} ago`
  const silentFor = (v.link.since_last_ms ?? 0) / 1000
  const where = regionLines(row?.region?.parts ?? [])
  const flag = async () => {
    setBusy(true)
    setErr(null)
    try {
      await flagNode(v.uid, note)
      setFlagging(false)
      setNote('')
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <section className="vd-standing" aria-label={`Standing: ${STANDING_LABEL[standing]}`}>
      <h4>Standing</h4>
      <p>
        <span className={`nl-tag ${standing === 'silent' ? 'silent' : standing === 'lost' ? 'lost' : ''}`}>{STANDING_LABEL[standing]}</span>{' '}
        {standing === 'lost' && lost ? (
          <>
            confirmed by {lost.by} {ago(lost.t)}
            {lost.note ? `: ${lost.note}` : ''}
          </>
        ) : (
          <span className="muted">silent for {fmtDuration(silentFor)}</span>
        )}
      </p>
      {standing === 'lost' ? (
        <>
          <p className="muted">It takes no order and is out of every keep-out; any report from it revokes the loss.</p>
          {(lost?.from || lost?.not_held) && (
            <Fold summary="Carried over">
              <p className="muted">
                {lost.from ? `Carried over from ${lost.from}.` : ''}
                {lost.not_held ? ` It does not hold the vehicle under this uid now: ${lost.not_held}.` : ''}
              </p>
            </Fold>
          )}
        </>
      ) : (
        <>
          <p className="muted">{standing === 'presumed_lost' ? 'A flag that decides nothing until it is confirmed in the Review.' : 'Silence alone presumes nothing.'}</p>
          {loss && loss.evidence.length > 0 && (
            <Fold summary="Evidence">
              <ul className="nl-lines" aria-label="Evidence">
                {loss.evidence.map((e, i) => (
                  <li key={i}>{evidenceLine(e, ago(e.t), nameOf)}</li>
                ))}
              </ul>
            </Fold>
          )}
          {(where.length > 0 || row) && (
            <Fold summary="Where the keep-outs count it">
              {where.length > 0 ? (
                <ul className="nl-lines" aria-label="Where the keep-outs count it">
                  {where.map((l) => (
                    <li key={l}>{l}</li>
                  ))}
                </ul>
              ) : (
                <p className="muted">Its region is not placed (no position reported).</p>
              )}
            </Fold>
          )}
        </>
      )}
      <div className="nl-actions">
        {(loss || lost) && onReview && (
          <button className="ghost mini" onClick={() => onReview(lost ? `L-${v.uid}` : `N-${v.uid}`)}>
            {lost ? 'Review: revoke...' : 'Review: confirm or not...'}
          </button>
        )}
        {standing === 'silent' && !loss && !flagging && (
          <button className="ghost mini" onClick={() => setFlagging(true)} title="Put it on the Review list for an operator with engagement authority to confirm it lost or not (any operator; it decides nothing)">
            Flag as possibly lost...
          </button>
        )}
        {flagging && (
          <>
            <input className="ix-note" value={note} maxLength={300} onChange={(e) => setNote(e.target.value)} placeholder="Why: a crew's report, a picture from outside" aria-label="Why it may be lost" disabled={busy} />
            <button className="ghost mini" disabled={busy || !note.trim()} onClick={() => void flag()}>
              Flag
            </button>
            <button className="ghost mini" disabled={busy} onClick={() => setFlagging(false)}>
              Back
            </button>
          </>
        )}
        {err && <span className="bad">{err}</span>}
      </div>
    </section>
  )
}

/** The details once the vehicle's whole report has come. A vehicle that has only ever been out of
 *  view at scale is known by lite snapshots alone (push.rs: identity, a coarse position, mode,
 *  link; no messages, battery or GPS), and the details read fields a lite one does not carry: it
 *  crashed them when opened from the fleet list or the Sitrep. Opening it pins it, so its full
 *  report comes with the next frame; until then the panel says so. */
export function VehicleDetailsWhenFull(p: Props) {
  const bar = useFloatBar()
  if (Array.isArray(p.v.statustext)) return <VehicleDetails {...p} />
  return (
    <aside className="vdetails" aria-label={`Details for ${callsign(p.v)}`}>
      <header className="vd-head" {...bar}>
        <FloatGrip />
        <div className="vd-title">
          <div className="vd-name">{callsign(p.v)}</div>
          <div className="muted">Out of view until now: its full report comes with the next update.</div>
        </div>
        <button className="ghost mini" onClick={p.onClose} aria-label="Close details">
          ✕
        </button>
      </header>
    </aside>
  )
}

/** One vehicle, one click from its fleet row (ledger 1 section 14): what it is and its one
 *  exception, its standing while silent, Now (what it is doing in the words of its order, height,
 *  speed, battery, each sensor's reach now), where a silent one probably is, its link, its order and
 *  the platform (loadout first, speeds, endurance, what it can be ordered to do). Selecting for
 *  tasking is a separate control. */
export function VehicleDetails({ v, taskings, intents, reach, exceptions = NO_EXCEPTIONS, now, selected, onToggleSelect, onClose, following, onFollow, onEngage, nodeLoss = NO_NODE_LOSS, nodeRow, onReview, nameOf }: Props) {
  const limits = usePlatformLimits()
  const p = v.identity ? limits?.get(v.identity.platform) : undefined
  const stale = isStale(v)
  // rules 30: confirmed lost by an operator: out of every keep-out, it takes no order, and what
  // it last reported is no longer where it is (no estimate of where it went, no tasking)
  const lost = nodeLoss.lost.has(v.uid)
  const domain = domainOf(v)
  const orbit = orbitRange([v], limits)
  const intentById = new Map((intents ?? []).map((i) => [i.id, i]))
  const order = latestOrders(taskings, intentById).get(v.uid)
  const name = (uid: number | string) => nameOf?.(String(uid)) ?? `vehicle ${uid}`
  const x = exceptionOf(v, exceptions, emuCause)
  const card = order ? orderCard(order.tasking, order.intent, order.parent, name) : null
  const opW = order ? opWord(order.op, order.tasking.task) : null
  const overW = order ? orderOver(order.intent?.kind, order.intent?.state) : null
  const word = overW && opW === 'running' ? overW : opW

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])
  const bar = useFloatBar()

  return (
    <aside className="vdetails" aria-label={`Details for ${callsign(v)}`}>
      <header className="vd-head" {...bar}>
        <FloatGrip />
        <span className="vd-sym" dangerouslySetInnerHTML={{ __html: symbolSvg(v, { stale, size: 22, labels: false }).svg }} />
        <div className="vd-title">
          <div className="vd-name">
            {callsign(v)} {v.identity && <span className="vrow-short">{v.identity.short}</span>}
            {v.identity?.armed ? <span className="vrow-weapons">WEAPONS</span> : null}
          </div>
          {v.identity && (
            <div className="muted">
              {v.identity.name} · {v.identity.role}
            </div>
          )}
          {x && (
            <span className={`vrow-exc ${x.tone}`} title={x.title} data-exception={x.kind}>
              {x.text}
            </span>
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
      <div className="vd-body">

      {v.identity?.armed && onEngage && !lost && (
        <button className="vd-select eg-engage-btn" onClick={onEngage} title="Nominate an engagement with this armed platform on a declared-hostile track (two operators with engagement authority decide)">
          Engage...
        </button>
      )}
      {!lost && (
        <button className={`vd-select ${selected ? 'on' : ''}`} onClick={onToggleSelect}>
          {selected ? '✓ Selected for tasking' : 'Select for tasking'}
        </button>
      )}

      <StandingSection key={v.uid} v={v} nodes={nodeLoss} row={nodeRow} now={now} onReview={onReview} nameOf={nameOf} />

      <section className={lost ? 'vd-last-report' : undefined}>
        <h4>{lost ? 'Last report' : 'Now'}</h4>
        {lost && (
          <p className="muted">
            What it reported last, {fmtDuration((v.link.since_last_ms ?? 0) / 1000)} ago, before it was confirmed lost: none of it is current.
          </p>
        )}
        <dl>
          <Item k="Doing">{doingNow(v, order, name, { cannotMove: exceptions.damage.get(v.uid)?.mobility, silent: isStale(v) })}</Item>
          {domain === 'subsurface' ? (
            <Item k="Depth">{v.alt_msl == null ? '–' : `${Math.max(0, -v.alt_msl).toFixed(1)} m`}</Item>
          ) : domain === 'air' ? (
            <Item k="Altitude" title="above mean sea level">
              {fmt(v.alt_msl, 0, ' m')}
            </Item>
          ) : null}
          <Item k="Speed" title="over the ground">
            {fmt(v.groundspeed, 1, ' m/s')}
          </Item>
          <Item k="Battery">{fmt(v.battery_pct, 0, '%')}</Item>
          <SensorItems uid={v.uid} shared={reach} />
        </dl>
      </section>

      {stale && v.estimate && !lost && (
        <section>
          <h4>Estimate</h4>
          <EstimateItems v={v} e={v.estimate} nowMs={now()} silent={standingOf(nodeLoss, v.uid, stale) == null} />
        </section>
      )}

      <LinkSection v={v} dives={domain === 'subsurface'} spaceClass={p?.space_class} />

      <section>
        <h4>Task</h4>
        {order && card ? (
          <>
            <p>
              {order.tasking.params.look ? `IDENTIFY ${order.tasking.params.look}` : card.label} <span className="muted" title={card.idTitle}>{card.id}</span> · <b className={word && opWordIsBad(word) ? 'bad' : ''}>{word}</b>
            </p>
            {(order.op.detail || order.op.progress) && (
              <Fold summary="Detail">
                <dl>
                  {order.op.detail && <Item k="Detail">{order.op.detail}</Item>}
                  {order.op.progress && order.op.progress !== order.op.detail && <Item k="Progress">{order.op.progress}</Item>}
                </dl>
              </Fold>
            )}
          </>
        ) : (
          <p className="muted">No task sent this session.</p>
        )}
      </section>

      {p && (
        <section>
          <h4>Platform</h4>
          <dl>
            <LoadoutItems uid={v.uid} />
            <Item k="Speed">
              {fmt(p.cruise_mps, 0, ' m/s cruise')}
              {p.max_mps ? ` · ${fmt(p.max_mps, 0, ' max')}` : ''}
            </Item>
            {p.endurance_h != null && <Item k="Endurance">{fmt(p.endurance_h, p.endurance_h < 10 ? 1 : 0, ' h')}</Item>}
            <Item k="Can do">
              {p.tasks.length ? p.tasks.join(', ') : 'nothing (telemetry only)'}
              {p.tasks.includes('orbit') ? ` · orbit radius ≥ ${orbit.min} m` : ''}
            </Item>
          </dl>
        </section>
      )}
      </div>
    </aside>
  )
}
