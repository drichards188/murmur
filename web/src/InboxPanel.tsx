import { type ReactNode, useEffect, useMemo, useState } from 'react'
import type { Alert, Check, Decisions, Intent } from './decisions'
import {
  ALERT_LABEL,
  StepUpNeeded,
  stepsUp,
  useAlertMemory,
  useOperator,
  verdict,
  writeOffRestore,
  writeOffTarget,
  unspendPlatform,
  unspendTarget,
} from './decisions'
import { Bluf, BlufReason } from './Bluf'
import { RefusalFold } from './RefusalFold'
import { alertParts } from './bluf'
import { waitingItems } from './blocking'
import { EngageCard, RunCard } from './EngageCard'
import { leftText, soonestLeftS, urgentInbox } from './inboxBadge'
import { needsYou } from './needsYou'
import { alertRank, alertSort, engagementRank, inboxOrder, RANK, shownEngagements } from './inboxOrder'
import { agoWhen, plainText } from './plainText'
import { RationaleText } from './Rationale'
import { reviewEntries } from './reviewEntries'
import { ApprovalCard } from './ApprovalCard'
import { askFleet, nameOf } from './approvalFleet'
import { Proposer } from './Proposer'
import { askSentence } from './approvalText'
import { doingWords, latestOrders, type OrderIntent } from './orderWords'
import { isEngage } from './engage'
import { fmtWorldZ } from './scenarioClock'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import { alertLabel } from './swarms'
import { causeLines, foldedInto, type SwarmAlert, unfolded } from './swarmsModel'
import type { Tasking, Vehicle } from './types'
import type { Track } from './tracks'
import './inbox.css'

export type InboxTab = 'inbox' | 'decisions'

/** What the operator is looking at: an intent (`i2`) or an alert (`link:3`). */
export type InboxSelection = { kind: 'intent'; id: number } | { kind: 'alert'; id: string } | null

interface Props {
  decisions: Decisions | null
  vehicles: Vehicle[]
  now: () => number
  tab: InboxTab
  setTab: (t: InboxTab) => void
  selection: InboxSelection
  /** fly to and highlight: the vehicles (bridge uids) and the intent whose preview to frame */
  onFocus: (sel: InboxSelection, uids: number[], intent: Intent | null) => void
  onClose: () => void
  /** rules 9: open a track in the tracks drawer (from a review item) */
  onTrack?: (id: string) => void
  /** murmur-8xe: an engagement card's Replace (opens the nomination form on its plan) */
  onReplace?: (i: Intent) => void
  /** a notice from elsewhere in the console to show with the inbox's own (once per `n`) */
  notice?: Notice | null
  /** bring a review item into view (the strip's or the Situation panel's link), once per `n` */
  focus?: { id: string; n: number } | null
  /** the map's banners (App.tsx), shown under this drawer's header while it is open over the
   *  map's right edge, where they would otherwise sit over its top (bd murmur-76gv) */
  banners?: ReactNode
  /** an order's line saying what request it was proposed for (Awaiting.tsx's TurnCause), or nothing */
  cause?: (i: Intent) => ReactNode
  /** the task layer's orders, for what each vehicle does now (orderWords.ts) */
  taskings?: Tasking[]
  /** the track picture, for a declaration's AIS and its kind in words */
  tracks?: Track[]
}

// ---- top bar button -------------------------------------------------------------------------

/** The Inbox's button: one number, what needs the person (needsYou.ts, the same count as the
 *  drawer's "Needs you" tab and list), red only when something urgent is in it and grey for the routine
 *  (inboxBadge.ts), and beside it the time left on the item that expires soonest once that is
 *  under a few minutes; its title says what it counts and what it leaves out. Each item counts
 *  once: a review item holding up a nomination is one review item, not also a second badge. */
export function InboxButton({ decisions, open, onClick, hiddenAlerts, settled, now }: { decisions: Decisions | null; open: boolean; onClick: () => void; hiddenAlerts: Set<string>; settled: ReadonlyMap<string, string>; now: () => number }) {
  const n = needsYou(decisions?.inbox, hiddenAlerts, new Set(settled.keys()), shownEngagements(decisions?.intents ?? []))
  const urgent = urgentInbox(decisions?.inbox, hiddenAlerts)
  const left = soonestLeftS(decisions?.inbox.pending ?? [], now())
  const says = `${n.says}${left != null ? ` The soonest expires in ${leftText(left)}.` : ''}`
  return (
    <button data-tutorial="inbox-button" className={`ghost platforms-btn ix-btn ${open ? 'on' : ''}`} aria-pressed={open} onClick={onClick} title={says} aria-label={`Inbox: ${says}`}>
      Inbox
      {n.total > 0 && <span className={`ix-badge ${urgent ? 'urgent' : 'routine'}`}>{n.total}</span>}
      {left != null && <span className={`ix-left${urgent ? ' urgent' : ''}`}>{leftText(left)}</span>}
    </button>
  )
}

// ---- helpers ----------------------------------------------------------------------------------

const OUTCOME_MARK: Record<Check['outcome'], string> = { pass: 'PASS', fail: 'FAIL', unverified: 'UNVERIFIED', not_evaluated: 'NOT EVALUATED' }

/** The browser-only caveat, on the Acknowledge button's title where a person who asks sees it. */
const ACK_TITLE = 'Kept in this browser only: not shared with other operators or consoles, and the bridge does not record it.'

// ---- an alert -----------------------------------------------------------------------------------

function AlertCard({
  alert: a,
  now,
  selected,
  muted,
  mem,
  onFocus,
  name,
}: {
  alert: Alert
  now: () => number
  selected: boolean
  muted: boolean
  mem: ReturnType<typeof useAlertMemory>
  onFocus: () => void
  /** one of ours by its uid, for a detail that names one by uid */
  name: (uid: string) => string
}) {
  // rules 12: a restore that will never be confirmed (the vehicle lost or spent, another one
  // under its uid, an earlier log gone) is written off by an operator, saying why; rules 15: a
  // platform spent by its own effect that survived is un-spent by one with engagement authority
  const target = writeOffTarget(a)
  const spent = unspendTarget(a)
  const [noting, setNoting] = useState<null | 'write_off' | 'unspend'>(null)
  const [note, setNote] = useState('')
  const [writeErr, setWriteErr] = useState<string | null>(null)
  // rules 15: an un-spend makes an armed platform taskable again: a credentials run re-enters
  // the credential (the password, or a passkey made for this un-spend)
  const unspendStepsUp = stepsUp()
  const [step, setStep] = useStepUp()
  const [stepUpError, setStepUpError] = useState(false)
  const [busy, setBusy] = useState(false)
  const submitNote = async () => {
    setWriteErr(null)
    setStepUpError(false)
    setBusy(true)
    try {
      if (noting === 'unspend' && spent) {
        const proof = unspendStepsUp ? await stepUpProof(step, { act: 'unspend', uid: spent.uid }) : {}
        setStep({ password: '' })
        await unspendPlatform(spent.uid, note.trim(), proof)
      } else if (target) {
        await writeOffRestore(target, note.trim())
      }
      setNoting(null)
    } catch (e) {
      setStepUpError(e instanceof StepUpNeeded)
      setWriteErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const who = a.callsign ?? `vehicle ${a.uid}`
  const age = Math.max(0, now() / 1000 - a.since_t)
  return (
    <article className={`ix-alert ${a.severity} ${selected ? 'sel' : ''} ${muted ? 'muted-card' : ''}`} tabIndex={0} onClick={onFocus} onKeyDown={(e) => e.key === 'Enter' && e.target === e.currentTarget && onFocus()}>
      <header className="ix-head">
        <strong className="ix-akind">{ALERT_LABEL[a.kind] ?? alertLabel(a.kind)}</strong>
        <span className="ix-who">{who}</span>
        <span className="ix-age" title="since">since {fmtWorldZ(a.since_t * 1000)}</span>
      </header>
      {/* the consequence first, the bridge's detail with its epoch stamps in words under More */}
      <Bluf
        className="ix-adetail"
        parts={alertParts({ kind: a.kind, detail: plainText(a.detail, { nameOf: name, when: agoWhen(now()) }) }, `${Math.max(1, Math.round(age / 60))} min`, who)}
      />
      {a.intent_id != null && <p className="ix-link"><span className="ix-id">i{a.intent_id}</span></p>}
      <footer className="ix-actions" onClick={(e) => e.stopPropagation()}>
        {muted ? (
          <>
            <span className="muted ix-muted">acknowledged</span>
            <button className="ghost" onClick={() => mem.wake(a)}>
              Restore
            </button>
          </>
        ) : (noting === 'write_off' && target) || (noting === 'unspend' && spent) ? (
          <>
            <input
              className="ix-note"
              placeholder={noting === 'unspend' ? 'Why: what shows it survived' : 'Why: lost, spent, another vehicle, log gone'}
              value={note}
              onChange={(e) => setNote(e.target.value)}
              aria-label={noting === 'unspend' ? 'Why the platform is un-spent' : 'Why the restore is written off'}
            />
            <button
              className="ghost"
              disabled={busy || !note.trim() || (noting === 'unspend' && unspendStepsUp && !stepUpReady(step))}
              onClick={() => void submitNote()}
            >
              {noting === 'unspend' ? 'Un-spend' : 'Write off'}
            </button>
            <button className="ghost" onClick={() => setNoting(null)}>
              Back
            </button>
            {noting === 'unspend' && unspendStepsUp && (
              <StepUpField
                passwordLabel="Consequential: your password to un-spend"
                purpose="to un-spend this platform"
                ariaLabel="Your password, to un-spend"
                value={step}
                onChange={setStep}
                busy={busy}
                bad={stepUpError}
                onEnter={() => note.trim() && void submitNote()}
              />
            )}
            {writeErr && <span className="muted ix-muted">{writeErr}</span>}
          </>
        ) : (
          <>
            <button className="ghost" onClick={onFocus}>
              Fly to
            </button>
            <span className="spacer" />
            {target && (
              <button className="ghost" onClick={() => setNoting('write_off')}>
                Write off
              </button>
            )}
            {spent && (
              <button className="ghost" onClick={() => setNoting('unspend')}>
                Un-spend
              </button>
            )}
            <button className="ghost" title={ACK_TITLE} onClick={() => mem.ack(a)}>
              Acknowledge
            </button>
          </>
        )}
      </footer>
    </article>
  )
}

// ---- a group alert ------------------------------------------------------------------------------

/** One card for a group's members' alerts of one kind (the bridge folds them: `swarm_alerts`),
 *  instead of one card per member; opened, it lists the members and each one's own alert. A view:
 *  acknowledging it is this browser's, as for any alert. */
function SwarmAlertCard({ alert: a, members, vehicles, now, selected, onFocus }: {
  alert: SwarmAlert
  members: Map<number, Alert[]>
  vehicles: Vehicle[]
  now: () => number
  selected: boolean
  onFocus: () => void
}) {
  const [open, setOpen] = useState(false)
  const causes = causeLines(a.causes)
  const uids = a.uids.length ? a.uids : [...members.keys()]
  return (
    <article className={`ix-alert sw-card ${a.severity} ${selected ? 'sel' : ''}`} tabIndex={0} onClick={onFocus} onKeyDown={(e) => e.key === 'Enter' && e.target === e.currentTarget && onFocus()}>
      <header className="ix-head">
        <strong className="ix-akind">{alertLabel(a.kind)}</strong>
        <span className="ix-who">
          group <b>{a.swarm}</b>
        </span>
        <span className="ix-age" title="since">since {fmtWorldZ(a.since_t * 1000)}</span>
      </header>
      <p className="sw-count">
        <b>{a.count}</b> of {a.of} members
      </p>
      {causes.length > 0 && (
        <ul className="sw-causes">
          {causes.map((c, k) => (
            <li key={k}>{c}</li>
          ))}
        </ul>
      )}
      {open && (
        <ul className="sw-members" onClick={(e) => e.stopPropagation()}>
          {uids.map((u) => (
            <li key={u}>
              <b>{nameOf(vehicles, String(u))}</b>
              <span>{(members.get(u) ?? []).map((m) => plainText(m.detail, { nameOf: (x) => nameOf(vehicles, x), when: agoWhen(now()) })).join('; ') || 'no member alert of its own'}</span>
            </li>
          ))}
        </ul>
      )}
      <footer className="ix-actions" onClick={(e) => e.stopPropagation()}>
        <button className="ghost" onClick={onFocus}>
          Select and fly to
        </button>
        <span className="spacer" />
        <button className="ghost" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
          {open ? 'Hide members' : `Show ${uids.length} members`}
        </button>
      </footer>
    </article>
  )
}

// ---- decisions list -----------------------------------------------------------------------------

/** Who decided an order, in plain words (the route chip's replacement). */
function routeWords(i: Intent): { text: string; tone: string } {
  const v = verdict(i)
  const a = i.approval
  if (v.route === 'auto') return { text: 'sent at once', tone: 'auto' }
  if (v.route === 'rules') return { text: 'stopped by a check', tone: 'rules' }
  if (a) return a.by === i.source.id ? { text: `self-approved by ${a.by}`, tone: 'self' } : { text: `approved by ${a.by}`, tone: 'human' }
  if (v.decided === 'rejected') return { text: 'rejected by a person', tone: 'human' }
  if (v.decided === 'expired') return { text: 'expired undecided', tone: 'human' }
  return { text: 'waiting for a person', tone: 'human' }
}

function DecisionRow({ intent: i, vehicles, now }: { intent: Intent; vehicles: Vehicle[]; now: () => number }) {
  const [open, setOpen] = useState(false)
  const v = verdict(i)
  const names = i.uids.length ? i.uids.map((u) => nameOf(vehicles, u)).join(', ') : 'no vehicles'
  const t0 = i.created_t
  const approver = i.approval
  const route = routeWords(i)
  const ask = askSentence({ kind: i.kind, params: i.params as Record<string, unknown>, uids: i.uids, group: i.group }, askFleet(vehicles), true)
  return (
    <li className={`ix-dec ${open ? 'open' : ''}`}>
      <button className="ix-dec-row" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="ix-dec-top">
          <strong className="ix-dec-ask">{ask}</strong>
          <span className="ix-id">i{i.id}</span>
          <span className={`ix-state ${i.state}`}>{i.state.replace('_', ' ')}</span>
        </span>
        <span className="ix-dec-sub">
          <span className={`ix-route ${route.tone}`}>{route.text}</span>
          <Proposer source={i.source} />
          <span className="right">{fmtWorldZ(t0)}</span>
        </span>
      </button>
      {open && (
        <div className="ix-dec-body">
          {v.wait_s != null && <p className="ix-note-shown">A person decided it in {Math.round(v.wait_s)} s.</p>}
          {i.rationale && (
            <div className="ix-rationale">
              <RationaleText rationale={i.rationale} source={i.source} />
            </div>
          )}
          <ol className="ix-trail">
            {i.history.map((h, k) => (
              <li key={k}>
                <span className="ix-t">+{Math.round((h.t - t0) / 1000)} s</span>
                <b>{h.state.replace('_', ' ')}</b>
                <BlufReason className="compact" text={plainText(h.reason, { nameOf: (u) => nameOf(vehicles, u), when: agoWhen(now()) })} ctx={{ who: names }} />
              </li>
            ))}
          </ol>
          {approver?.note && <p className="ix-note-shown">Note from {approver.by}: {approver.note}</p>}
          {i.uids.length > 1 && i.checks.some((c) => c.uid == null) && <RefusalFold checks={i.checks} uids={i.uids} vehicles={vehicles} />}
          {!(i.uids.length > 1 && i.checks.some((c) => c.uid == null)) && i.checks.some((c) => c.outcome !== 'pass') && (
            <ul className="ix-dec-checks">
              {i.checks
                .filter((c) => c.outcome !== 'pass')
                .map((c, k) => (
                  <li key={k} className={c.outcome}>
                    {OUTCOME_MARK[c.outcome]} {c.rule}: {plainText(c.detail, { nameOf: (u) => nameOf(vehicles, u), when: agoWhen(now()) })}
                  </li>
                ))}
            </ul>
          )}
        </div>
      )}
    </li>
  )
}

function DecisionsList({ intents, vehicles, now }: { intents: Intent[]; vehicles: Vehicle[]; now: () => number }) {
  if (!intents.length) return <p className="empty">No orders yet.</p>
  return (
    <ul className="ix-decs">
      {intents.map((i) => (
        <DecisionRow key={i.id} intent={i} vehicles={vehicles} now={now} />
      ))}
    </ul>
  )
}

// ---- the panel --------------------------------------------------------------------------------------

interface Notice {
  n: number
  text: string
  tone: 'ok' | 'bad'
}

const NO_INTENTS: Intent[] = []

/** An Inbox item with where it sorts (inboxOrder.ts) and its card. */
interface Item {
  key: string
  rank: number
  sort: number | null
  node: ReactNode
}

export function InboxPanel({ banners, decisions, vehicles, now, tab, setTab, selection, onFocus, onClose, onTrack, onReplace, notice, focus, mem, hidden, settled, cause, taskings, tracks }: Props & { mem: ReturnType<typeof useAlertMemory>; hidden: Set<string>; settled: ReadonlyMap<string, string> }) {
  // a card chosen from outside this drawer (a line at the head of another, the task panel's link)
  // is brought into view; one clicked here already is (`nearest` leaves it be)
  const selId = selection?.kind === 'intent' ? selection.id : null
  useEffect(() => {
    if (selId != null) document.querySelector(`.ix [data-intent-id="${selId}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [selId, tab])
  const [notices, setNotices] = useState<Notice[]>(notice ? [notice] : [])
  const [noticeSeen, setNoticeSeen] = useState(notice?.n ?? null)
  if (notice && notice.n !== noticeSeen) {
    setNoticeSeen(notice.n)
    setNotices((ns) => [...ns, notice])
  }
  const [operator] = useOperator()
  const pending = decisions?.inbox.pending ?? []
  const intents = decisions?.intents ?? NO_INTENTS
  const allAlerts = decisions?.inbox.alerts ?? []
  // a member alert folded into a group alert is shown inside that group's card, not on its own
  const alerts = unfolded(allAlerts)
  const swarmAlerts = decisions?.inbox.swarm_alerts ?? []
  const blocking = decisions?.inbox.blocking
  // what needs the person, counted as the Inbox button counts it (needsYou.ts)
  const engagements = shownEngagements(intents)
  const need = needsYou(decisions?.inbox, hidden, new Set(settled.keys()), engagements)
  const name = (uid: string) => nameOf(vehicles, uid)
  const done = (text: string, tone: 'ok' | 'bad') => setNotices((ns) => [...ns, { n: Date.now() + Math.random(), text, tone }])

  useEffect(() => {
    if (!notices.length) return
    const id = window.setTimeout(() => setNotices((ns) => ns.slice(1)), 9000)
    return () => window.clearTimeout(id)
  }, [notices])

  // what each of ours is doing, in the words of its order
  const orders = useMemo(() => latestOrders(taskings ?? [], new Map<number, OrderIntent>(intents.map((i) => [i.id, i]))), [taskings, intents])
  const doing = (uid: string) => {
    const v = vehicles.find((x) => String(x.uid) === uid)
    return v ? doingWords(v, orders.get(v.uid), (u) => nameOf(vehicles, String(u))) : 'no report'
  }
  const uidsOf = (i: Intent) => i.uids.map(Number).filter((n) => Number.isFinite(n))
  const selectedIntent = (i: Intent) => selection?.kind === 'intent' && selection.id === i.id
  const left = (i: Intent) => (i.expires_t != null ? (i.expires_t - now()) / 1000 : null)

  const items: Item[] = []
  for (const i of pending) {
    if (isEngage(i)) {
      const until = i.engagement?.preview?.window?.until
      const t = until != null && i.expires_t != null ? Math.min(until, i.expires_t) : (until ?? i.expires_t)
      items.push({
        key: `i${i.id}`,
        rank: RANK.signing,
        sort: t != null ? (t - now()) / 1000 : null,
        node: <EngageCard key={`i${i.id}`} intent={i} vehicles={vehicles} now={now} selected={selectedIntent(i)} onFocus={() => onFocus({ kind: 'intent', id: i.id }, uidsOf(i), i)} onDone={done} onReplace={onReplace} />,
      })
    } else {
      items.push({
        key: `i${i.id}`,
        rank: RANK.approval,
        sort: left(i),
        node: (
          <ApprovalCard
            key={`i${i.id}`}
            intent={i}
            engaged={intents.filter((e) => isEngage(e) && e.id !== i.id && ['pending_approval', 'approved', 'dispatched'].includes(e.state) && e.uids.some((u) => i.uids.includes(u)))}
            vehicles={vehicles}
            now={now}
            selected={selectedIntent(i)}
            onFocus={() => onFocus({ kind: 'intent', id: i.id }, uidsOf(i), i)}
            onDone={done}
            cause={cause?.(i)}
            doing={doing}
          />
        ),
      })
    }
  }
  for (const i of engagements) {
    items.push({
      key: `i${i.id}`,
      rank: engagementRank(i),
      sort: null,
      node: <RunCard key={`i${i.id}`} intent={i} vehicles={vehicles} now={now} selected={selectedIntent(i)} onFocus={() => onFocus({ kind: 'intent', id: i.id }, uidsOf(i), i)} onDone={done} onTrack={onTrack} />,
    })
  }
  for (const e of reviewEntries(
    { review: decisions?.inbox.review, acts: decisions?.inbox.acts, now, onDone: done, onTrack, blocking, focus, nameOf: name, settled, tracks, vehicles },
    operator,
  ))
    items.push(e)
  for (const a of alerts) {
    const muted = hidden.has(a.id)
    items.push({
      key: a.id,
      rank: muted ? RANK.acknowledged : alertRank(a.kind, a.severity),
      sort: alertSort(a.severity, a.since_t),
      node: <AlertCard key={a.id} alert={a} now={now} selected={selection?.kind === 'alert' && selection.id === a.id} muted={muted} mem={mem} name={name} onFocus={() => onFocus({ kind: 'alert', id: a.id }, [a.uid], null)} />,
    })
  }
  for (const a of swarmAlerts) {
    items.push({
      key: a.id,
      rank: alertRank(a.kind, a.severity),
      sort: alertSort(a.severity, a.since_t),
      node: <SwarmAlertCard key={a.id} alert={a} members={foldedInto(allAlerts, a.id)} vehicles={vehicles} now={now} selected={selection?.kind === 'alert' && selection.id === a.id} onFocus={() => onFocus({ kind: 'alert', id: a.id }, a.uids, null)} />,
    })
  }
  const { needs, info } = inboxOrder(items)
  const urgentWaiting = waitingItems(blocking).length > 0
  // the policy's route, said once for every approval below (not on each card)
  const routes = [...new Set(pending.filter((i) => !isEngage(i)).map((i) => i.policy?.reason).filter((r): r is string => !!r))]

  return (
    <aside className="ix" aria-label="Inbox">
      <header className="ix-top">
        <h2>Inbox</h2>
        <div className="seg">
          <button className={tab === 'inbox' ? 'on' : ''} onClick={() => setTab('inbox')} title={need.says}>
            Needs you{need.total ? ` (${need.total})` : ''}
          </button>
          <button className={tab === 'decisions' ? 'on' : ''} onClick={() => setTab('decisions')}>
            Decisions
          </button>
        </div>
        <button className="ghost close" onClick={onClose} aria-label="Close inbox">
          Close
        </button>
      </header>
      {banners && <div className="ix-banners">{banners}</div>}
      {notices.length > 0 && (
        <div className="ix-notices" role="status">
          {notices.map((n) => (
            <p key={n.n} className={n.tone} onClick={() => setNotices((ns) => ns.filter((x) => x !== n))}>
              {n.text}
            </p>
          ))}
        </div>
      )}

      <div className="ix-scroll">
        {tab === 'decisions' ? (
          <DecisionsList intents={intents} vehicles={vehicles} now={now} />
        ) : (
          <>
            {/* one list, by consequence, then time left: what asks a person first, a review item that
                alone holds up a nomination ahead of the rest (keyed, so a card that moves keeps what
                it has open or typed) */}
            <h3 className={`ix-h${urgentWaiting ? ' ix-h-urgent' : ''}`}>
              Needs you <span>{needs.length}</span>
            </h3>
            {routes.length > 0 && <p className="ix-route-note">A person decides these: {routes.join('; ')}.</p>}
            {needs.length === 0 && <p className="empty">Nothing needs you</p>}
            {needs.map((x) => x.node)}
            {info.length > 0 && (
              <>
                <h3 className="ix-h ix-h-quiet">
                  For information <span>{info.length}</span>
                </h3>
                {info.map((x) => x.node)}
              </>
            )}
          </>
        )}
      </div>
    </aside>
  )
}
