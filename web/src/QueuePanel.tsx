import { type ReactNode, useState } from 'react'
import { intentAction, type Decisions, type Intent } from './decisions'
import { askSentence, type AskFleet } from './approvalText'
import { leftWords, queueShown, RECENT_MS, whyLine } from './queueWords'
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

interface Props {
  decisions: Decisions | null
  vehicles: Vehicle[]
  now: () => number
  /** fly to and highlight these vehicles (bridge uids) */
  onFocus: (uids: number[]) => void
  onClose: () => void
  /** the map's banners (App.tsx), shown under this drawer's header while it is open over the
   *  map's right edge, where they would otherwise sit over its top (bd murmur-76gv) */
  banners?: ReactNode
}

/** The orders that left the queue in the last ten minutes: how, and what the re-check found. */
function recentOf(decisions: Decisions | null, nowMs: number): Intent[] {
  return (decisions?.intents ?? []).filter((i) => {
    if (!outcomeOf(i)) return false
    const last = i.history[i.history.length - 1]?.t ?? 0
    return nowMs - last < RECENT_MS
  })
}

/** The Queue's button: only while an order is held or one just left (an empty queue is a button to
 *  nothing); the open drawer keeps its button so it can be closed. */
export function QueueButton({ decisions, open, onClick, now }: { decisions: Decisions | null; open: boolean; onClick: () => void; now: () => number }) {
  const n = decisions?.inbox.queued?.length ?? 0
  if (!queueShown(n, recentOf(decisions, now()).length, open)) return null
  const title = `${n} order${n === 1 ? '' : 's'} held for vehicles the link can't reach now`
  return (
    <button className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} aria-pressed={open} onClick={onClick} title={title} aria-label={`Queue: ${title}`}>
      Queue
      {n > 0 && <span className="ix-badge q-badge">{n}</span>}
    </button>
  )
}

function fleetOf(vehicles: Vehicle[]): AskFleet {
  const by = (u: string) => vehicles.find((x) => String(x.uid) === u)
  return {
    callsign: (u) => {
      const v = by(u)
      return v ? callsign(v) : `vehicle ${u}`
    },
    model: (u) => by(u)?.identity?.short ?? null,
    swarm: (u) => by(u)?.swarm ?? by(u)?.identity?.swarm ?? null,
  }
}

function names(vehicles: Vehicle[], uids: string[]) {
  const f = fleetOf(vehicles)
  return uids.map((u) => f.callsign(u))
}

/** Who authorized it: folded under the card, the vehicles are what the card is for. */
function who(i: Intent) {
  if (i.handed_over) return `the previous watch: handed over at the start by ${i.handed_over.fleet}`
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

export function QueuePanel({ decisions, vehicles, now, onFocus, onClose, banners }: Props) {
  const queued = decisions?.inbox.queued ?? []
  const [busy, setBusy] = useState<number | null>(null)
  const [notice, setNotice] = useState<{ text: string; tone: 'ok' | 'bad' } | null>(null)
  const nowMs = now()
  const recent = recentOf(decisions, nowMs)
  const fleet = fleetOf(vehicles)
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
        <button className="ghost close" onClick={onClose} aria-label="Close the queue">
          ×
        </button>
      </div>
      {banners && <div className="ix-banners">{banners}</div>}
      {notice && (
        <div className="ix-notices">
          <p className={notice.tone} onClick={() => setNotice(null)}>
            {notice.text}
          </p>
        </div>
      )}
      <div className="ix-scroll">
        {queued.map((i) => {
          const left = i.queue ? (i.queue.expires_t - nowMs) / 1000 : null
          const total = i.queue ? (i.queue.expires_t - i.queue.t) / 1000 : null
          const frac = left != null && total ? Math.max(0, Math.min(1, left / total)) : 0
          const reach = i.reach_now ?? i.queue?.reach ?? []
          const waiting = reach.filter((r) => !r.reachable)
          const heard = reach.filter((r) => r.reachable)
          const why = whyLine(i)
          return (
            <article key={i.id} className="ix-card q-card" tabIndex={0} onClick={() => onFocus(i.uids.map(Number).filter(Number.isFinite))}>
              <div className="ix-head">
                <span className="ix-kind" title={i.handed_over ? `the previous watch's order, handed over at the start by ${i.handed_over.fleet}` : undefined}>
                  {askSentence(i, fleet)}
                </span>
                <span className="ix-id">i{i.id}</span>
                {left != null && (
                  <span
                    className={`ix-count ${left < 60 ? 'urgent' : ''}`}
                    title={
                      i.queue?.inbox
                        ? 'a human approved it: when this runs out it goes back to the inbox for a new look'
                        : 'it expires when its wait runs out'
                    }
                  >
                    <b>{leftWords(left)}</b>
                    <span className="ix-count-bar">
                      <span style={{ width: `${frac * 100}%` }} />
                    </span>
                  </span>
                )}
              </div>
              <div className="q-for">
                waiting for <b>{names(vehicles, i.uids).join(', ')}</b>
              </div>
              {why && <div className="q-why">{why}</div>}
              {waiting.length > 0 && (
                <ul className="q-reach">
                  {waiting.map((r) => (
                    <li key={r.uid} className="wait">
                      <b>{names(vehicles, [r.uid])[0]}</b>: {r.detail}
                    </li>
                  ))}
                </ul>
              )}
              <details className="q-fold" onClick={(e) => e.stopPropagation()}>
                <summary>Details</summary>
                <p className="q-for">{who(i)}</p>
                {heard.length > 0 && (
                  <ul className="q-reach">
                    {heard.map((r) => (
                      <li key={r.uid} className="ok">
                        <b>{names(vehicles, [r.uid])[0]}</b> reachable: {r.detail}
                      </li>
                    ))}
                  </ul>
                )}
              </details>
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
            </h3>
            {recent.map((i) => {
              const o = outcomeOf(i)!
              return (
                <article key={i.id} className={`ix-card q-card q-done${o.tone === 'ok' ? '' : ` ${o.tone}`}`} tabIndex={0} onClick={() => onFocus(i.uids.map(Number).filter(Number.isFinite))}>
                  <div className="ix-head">
                    <span className="ix-kind">{askSentence(i, fleet, true)}</span>
                    <span className="ix-id">i{i.id}</span>
                    <span className={`ix-tag q-out ${o.tone}`}>{o.label}</span>
                  </div>
                  <div className="q-for">now {i.state.replace('_', ' ')}</div>
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
