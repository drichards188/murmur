import { useState } from 'react'
import { fmtDuration, intentAction, type Decisions, type Intent } from './decisions'
import type { Vehicle } from './types'
import { callsign } from './types'
import './queue.css'

/** The safe command queue (rules 5, murmur-5p4): orders that were approved but are held,
 *  not sent into a dead link, because a vehicle they are for can't be reached right now (a
 *  silent link, a satellite outage, an AUV whose acoustic modem can't carry a mission). Each
 *  says why it waits now and how long it may; the operator may cancel it. When its vehicles
 *  can be reached its checks run again on the world then: it is sent, comes back to the
 *  inbox (a human's picture changed), or is voided; past its wait it expires. The last such
 *  outcomes are listed too. */

const RECENT_MS = 10 * 60_000

interface Props {
  decisions: Decisions | null
  vehicles: Vehicle[]
  now: () => number
  /** fly to and highlight these vehicles (bridge uids) */
  onFocus: (uids: number[]) => void
  onClose: () => void
}

export function QueueButton({ decisions, open, onClick }: { decisions: Decisions | null; open: boolean; onClick: () => void }) {
  const n = decisions?.inbox.queued?.length ?? 0
  const title = `${n} order${n === 1 ? '' : 's'} held for vehicles the link can't reach now`
  return (
    <button className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} onClick={onClick} title={title} aria-label={`Queue: ${title}`}>
      Queue
      {n > 0 && <span className="ix-badge q-badge">{n}</span>}
    </button>
  )
}

function names(vehicles: Vehicle[], uids: string[]) {
  return uids.map((u) => {
    const v = vehicles.find((x) => String(x.uid) === u)
    return v ? callsign(v) : `vehicle ${u}`
  })
}

function what(i: Intent) {
  const p = i.params
  if (i.kind === 'orbit' && p.center) return `orbit ${Math.round(p.radius_m ?? 0)} m`
  if (i.kind === 'patrol' && p.route) return `patrol ${p.route.length} waypoints`
  return i.kind
}

function who(i: Intent) {
  const a = i.approval
  if (!a) return i.source.kind === 'agent' ? `auto (policy), proposed by agent ${i.source.id}` : 'auto (policy)'
  return a.at_submission ? `authorized by ${a.by} at submission` : `approved by ${a.by}`
}

const VERDICT_LABEL: Record<string, string> = {
  dispatch: 'sent after its re-check',
  inbox: 'back to the inbox',
  void: 'voided at its re-check',
}

/** An intent that left the queue: how, and what the re-check found. */
function outcomeOf(i: Intent): { label: string; tone: 'ok' | 'warn' | 'bad'; lines: string[] } | null {
  if (!i.queue || i.state === 'queued') return null
  const rc = i.queue.recheck
  if (i.state === 'expired') return { label: 'expired in the queue', tone: 'bad', lines: [i.history[i.history.length - 1]?.reason ?? ''] }
  if (rc) return { label: VERDICT_LABEL[rc.verdict] ?? rc.verdict, tone: rc.verdict === 'dispatch' ? 'ok' : rc.verdict === 'inbox' ? 'warn' : 'bad', lines: rc.changes }
  if (i.state === 'voided') return { label: 'cancelled or superseded', tone: 'bad', lines: [i.history[i.history.length - 1]?.reason ?? ''] }
  return null
}

export function QueuePanel({ decisions, vehicles, now, onFocus, onClose }: Props) {
  const queued = decisions?.inbox.queued ?? []
  const [busy, setBusy] = useState<number | null>(null)
  const [notice, setNotice] = useState<{ text: string; tone: 'ok' | 'bad' } | null>(null)
  const nowMs = now()
  const recent = (decisions?.intents ?? []).filter((i) => {
    if (!outcomeOf(i)) return false
    const last = i.history[i.history.length - 1]?.t ?? 0
    return nowMs - last < RECENT_MS
  })
  const cancel = async (i: Intent) => {
    setBusy(i.id)
    try {
      await intentAction(i.id, 'cancel', 'cancelled while queued')
      setNotice({ text: `i${i.id} cancelled: it will not be sent`, tone: 'ok' })
    } catch (e) {
      setNotice({ text: `i${i.id}: ${(e as Error).message}`, tone: 'bad' })
    } finally {
      setBusy(null)
    }
  }
  return (
    <aside className="ix q" aria-label="Queue">
      <div className="ix-top">
        <h2>Queue</h2>
        <span className="q-sub">orders held for unreachable vehicles</span>
        <button className="ghost close" onClick={onClose} aria-label="Close the queue">
          ×
        </button>
      </div>
      {notice && (
        <div className="ix-notices">
          <p className={notice.tone} onClick={() => setNotice(null)}>
            {notice.text}
          </p>
        </div>
      )}
      <div className="ix-scroll">
        <h3 className="ix-h">
          Waiting <span>{queued.length}</span>
          <small>re-checked on the world when they can be reached</small>
        </h3>
        {queued.length === 0 && <p className="empty muted">Nothing waits: every approved order reached its vehicles.</p>}
        {queued.map((i) => {
          const left = i.queue ? (i.queue.expires_t - nowMs) / 1000 : null
          const total = i.queue ? (i.queue.expires_t - i.queue.t) / 1000 : null
          const frac = left != null && total ? Math.max(0, Math.min(1, left / total)) : 0
          const reach = i.reach_now ?? i.queue?.reach ?? []
          return (
            <article key={i.id} className="ix-card q-card" tabIndex={0} onClick={() => onFocus(i.uids.map(Number).filter(Number.isFinite))}>
              <div className="ix-head">
                <span className="ix-kind">{what(i).toUpperCase()}</span>
                <span className="ix-id">i{i.id}</span>
                <span className="ix-tag q-tag">queued</span>
                {i.resumed && (
                  <span className="ix-tag warn" title={`carried over from ${i.resumed.from.log} (${i.resumed.from.was})`}>
                    after restart
                  </span>
                )}
                {left != null && (
                  <span className={`ix-count ${left < 60 ? 'urgent' : ''}`} title="it expires when its wait runs out">
                    <b>{fmtDuration(left)}</b>
                    <span className="ix-count-bar">
                      <span style={{ width: `${frac * 100}%` }} />
                    </span>
                  </span>
                )}
              </div>
              <div className="q-for">
                for <b>{names(vehicles, i.uids).join(', ')}</b> · {who(i)}
              </div>
              <ul className="q-reach">
                {reach.map((r) => (
                  <li key={r.uid} className={r.reachable ? 'ok' : 'wait'}>
                    <b>{names(vehicles, [r.uid])[0]}</b> {r.reachable ? 'reachable: ' : 'waiting: '}
                    {r.detail}
                  </li>
                ))}
              </ul>
              <div className="q-actions">
                <button
                  className="ghost"
                  disabled={busy === i.id}
                  onClick={(e) => {
                    e.stopPropagation()
                    void cancel(i)
                  }}
                >
                  Cancel order
                </button>
              </div>
            </article>
          )
        })}
        {recent.length > 0 && (
          <>
            <h3 className="ix-h">
              Left the queue <span>{recent.length}</span>
              <small>last 10 min</small>
            </h3>
            {recent.map((i) => {
              const o = outcomeOf(i)!
              return (
                <article key={i.id} className={`ix-card q-card q-done ${o.tone}`} tabIndex={0} onClick={() => onFocus(i.uids.map(Number).filter(Number.isFinite))}>
                  <div className="ix-head">
                    <span className="ix-kind">{what(i).toUpperCase()}</span>
                    <span className="ix-id">i{i.id}</span>
                    <span className={`ix-tag q-out ${o.tone}`}>{o.label}</span>
                  </div>
                  <div className="q-for">
                    for <b>{names(vehicles, i.uids).join(', ')}</b> · now {i.state.replace('_', ' ')}
                  </div>
                  {o.lines.filter(Boolean).length > 0 && (
                    <ul className="q-changes">
                      {o.lines.filter(Boolean).map((l, k) => (
                        <li key={k}>{l}</li>
                      ))}
                    </ul>
                  )}
                </article>
              )
            })}
          </>
        )}
      </div>
    </aside>
  )
}
