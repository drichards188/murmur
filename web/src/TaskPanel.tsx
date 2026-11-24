import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { EMPTY_ROUTE, planDrawing, routeDrawing, type LinkRouteDrawing } from './coverage'
import { DryRun } from './DryRun'
import { useDryRun } from './useDryRun'
import type { Draft } from './MapView'
import type { OpState, TaskKind, Tasking, Vehicle } from './types'
import { callsign, opEnding } from './types'
import { BlufReason } from './Bluf'
import { Lazy } from './Lazy'
import { reasonBluf } from './bluf'
import { TaskResult, type TaskOutcome } from './TaskResult'
import { tierLabel, type Intent, type SpaceRuns } from './decisions'
import { scenarioNow } from './decisions'
import { SpaceRunCard } from './SpaceRuns'
import { sendTask, TaskRefused, type TaskRequest } from './useBridge'
import { availableTasks, orbitRange, usePlatformLimits } from './taskDefaults'
import { OptionLine } from './IdentifyForm'
import { sendIdentify, useIdentifyOptions } from './identify'
import { OPS_FOLD, orderSummary, STUCK_S } from './exceptions'
import { useGroupOrders } from './policy'
import { defaultMin, excludedMarks, memberClasses, reforms, slotMarks, SPREAD_WORDS, SPREADS, type GroupSpec, type Spread } from './groupOrders'
import { drawnHeight } from './map3d'
import './command.css'
import { sameFleet } from './stableFleet'
import { routing, selectionLine, sendWords } from './commandModel'
import { oneCardPerOrder, orderCard } from './orderCard'
import { opWord, opWordIsBad, orderOver } from './orderWords'

interface Props {
  vehicles: Vehicle[]
  /** Keep names in completed commands after a spent vehicle leaves the live fleet. */
  historyVehicles?: Vehicle[]
  /** the name a selection that is exactly a group goes by (one of the operator's own groups, else
   *  a fleet swarm's id; App.tsx): the order carries it as its `group.swarm` label */
  groupLabel?: string | null
  selected: Set<number>
  draft: Draft
  setDraft: (d: Draft) => void
  taskings: Tasking[]
  /** intents waiting in the inbox, and a way to open one (or its Decisions entry) */
  pendingIds?: Set<number>
  onInbox?: (intentId: number) => void
  /** satellite collects and covers, and the bridge clock (for their countdowns) */
  space?: SpaceRuns
  now?: () => number
  /** the drafted order's predicted link, for the map (the dry run's `link_coverage`) */
  onLinkRoute?: (d: LinkRouteDrawing | null) => void
  /** the run's latest intents: how an identify order's look ended (rules 21) */
  intents?: Intent[]
}

/** The task buttons' kinds: the task layer's, and (rules 21) identify, one vehicle sent to
 *  identify the contact clicked on the map. */
type PanelKind = TaskKind | 'identify'

const TASKS: { kind: PanelKind; label: string; hint: string }[] = [
  { kind: 'orbit', label: 'Orbit', hint: 'Click the map to set the orbit center.' },
  { kind: 'patrol', label: 'Patrol', hint: 'Click the map to add waypoints; the route loops.' },
  { kind: 'transit', label: 'Transit', hint: 'Click the map to add points: it flies the route once and holds at its end.' },
  { kind: 'hold', label: 'Hold', hint: 'Planes loiter in place, boats hold position.' },
  { kind: 'rtl', label: 'Return', hint: 'Each returns to where it launched.' },
  { kind: 'track', label: 'Track', hint: 'Select one antenna tracker and the one vehicle it should point at.' },
  { kind: 'identify', label: 'Identify', hint: "Click a ringed contact on the map: the vehicle orbits it at its camera's standoff until it is identified, then goes back to what it was doing." },
]

interface CardProps {
  t: Tasking
  /** what the card's content rests on (see `sigs`): equal signatures, same card */
  sig: string
  label: string
  /** the order's number (`i2`, orderCard.ts) and what it is */
  num: string
  numTitle: string
  /** how an identify order's look ended */
  ended: string | null
  /** the order is over though its task goes on (an engagement after its release: orderOver) */
  over?: string | null
  nameOf: (uid: number) => Vehicle | undefined
  /** an order to more than OPS_FOLD vehicles: summarised unless open */
  open: boolean
  onOpen: (id: number) => void
  /** the bridge clock, s, at the minute (what makes an op stuck) */
  minute: number
}

/** A summarised order (R6): its vehicles counted by their state word (opWord: starting, running,
 *  done...; colour only on a failure), and every exception (failed, superseded, may be running,
 *  back to the queue, stuck) listed, grouped by reason with the vehicles named; "show all" lists
 *  them one by one as before. */
function OrderSummary({ t, nameOf, minute, onOpen }: Pick<CardProps, 't' | 'nameOf' | 'minute' | 'onOpen'>) {
  const s = orderSummary(t.ops, minute)
  const name = (uid: number) => {
    const v = nameOf(uid)
    return v ? callsign(v) : String(uid)
  }
  // the states counted in lifecycle order, as their words (sent, starting and acked are one: starting)
  const words = new Map<string, number>()
  for (const [state, n] of s.counts) {
    const w = opWord({ state: state as OpState, detail: '' }, t.task)
    words.set(w, (words.get(w) ?? 0) + n)
  }
  return (
    <>
      <p className="tcard-sum">
        {[...words].map(([w, n]) => (
          <span key={w} className={opWordIsBad(w) ? 'bad' : undefined}>
            {n} {w}
          </span>
        ))}
      </p>
      {s.exceptions.map((e) => {
        const ending = e.state === 'stuck' ? `stuck over ${STUCK_S} s` : (opEnding({ state: e.state, detail: e.detail }) ?? e.state)
        const why = reasonBluf(e.detail, {}).headline || e.detail
        return (
          <div key={`${e.state}|${e.detail}`} className={`tcard-exc ${e.state}`}>
            <span className="tcard-exc-head">
              <b>{ending.toUpperCase()}</b> <span className="muted">×{e.uids.length}</span> {why}
            </span>
            <span className="tcard-names">{e.uids.map(name).join(', ')}</span>
          </div>
        )
      })}
      <button className="ghost mini tcard-all" onClick={() => onOpen(t.id)}>
        show all {t.ops.length}
      </button>
    </>
  )
}

/** One command log card: the order, its number, and each vehicle's op in it in one word (and on a
 *  failure, why, the bridge's own words a click away). A tasking to a swarm has a hundred ops, so
 *  it is built only while it is near the log's view (Lazy) and only re-rendered when its ops
 *  changed. */
const TaskingCard = memo(
  function TaskingCard({ t, label, num, numTitle, ended, over, nameOf, open, onOpen, minute }: CardProps) {
    const summarised = t.ops.length > OPS_FOLD && !open
    return (
      <Lazy as="section" className="tcard" estimate={summarised ? 90 : 36 + t.ops.length * 26}>
        {() => (
        <>
        <div className="tcard-head">
          <strong>{label}</strong>
          <span className="muted" title={numTitle}>
            {num}
          </span>
          {t.ops.length > OPS_FOLD && <span className="muted">{t.ops.length} vehicles</span>}
          {t.ops.length > OPS_FOLD && open && (
            <button className="ghost mini tcard-fold" onClick={() => onOpen(t.id)}>
              summarise
            </button>
          )}
        </div>
        {ended && (
          <BlufReason
            className="tcard-ended"
            text={ended}
            ctx={{ who: t.ops[0] ? (() => { const v = nameOf(t.ops[0].uid); return v ? callsign(v) : undefined })() : undefined }}
          />
        )}
        {summarised && <OrderSummary t={t} nameOf={nameOf} minute={minute} onOpen={onOpen} />}
        {!summarised && t.ops.map((o) => {
          const v = nameOf(o.uid)
          const w = opWord(o, t.task)
          const word = over && w === 'running' ? over : w
          const bad = opWordIsBad(word)
          const phrase = reasonBluf(o.detail, { who: v ? callsign(v) : undefined }).headline
          return (
            <div key={o.id} className={`op ${o.state}`}>
              <span className="op-name" title={v?.identity ? `${v.identity.name} · ${v.identity.role}` : undefined}>
                {v ? callsign(v) : o.uid}
              </span>
              <span className="op-detail">
                <b className={`op-word${bad ? ' bad' : ''}`}>{word}</b>
                {phrase && phrase.toLowerCase() !== word ? `: ${phrase}` : ''}
                {bad && o.detail && phrase !== o.detail && (
                  <details className="bluf-more" onClick={(e) => e.stopPropagation()}>
                    <summary>More</summary>
                    <div className="bluf-detail">{o.detail}</div>
                  </details>
                )}
              </span>
            </div>
          )
        })}
        </>
        )}
      </Lazy>
    )
  },
  (a, b) =>
    a.sig === b.sig && a.label === b.label && a.num === b.num && a.numTitle === b.numTitle && a.ended === b.ended && a.over === b.over && a.nameOf === b.nameOf && a.t.id === b.t.id &&
    a.open === b.open && a.onOpen === b.onOpen && a.minute === b.minute,
)

export function TaskPanel({ vehicles, historyVehicles = vehicles, groupLabel = null, selected, draft, setDraft, taskings, pendingIds, onInbox, space, now, onLinkRoute, intents }: Props) {
  const runs = space?.runs ?? []
  const scen = now ? scenarioNow(space, now) : null
  // rules 21: an identify order's taskings (its orbit each time it moved with its track, then the
  // order it went back to) as one card, the newest, saying how its look ended
  const identify = new Map<number, { track: string; newest: number; ended: string | null }>()
  for (const t of taskings) {
    if (t.intent_id == null || !t.params.look) continue
    if (!identify.has(t.intent_id)) identify.set(t.intent_id, { track: t.params.look, newest: t.id, ended: null })
  }
  for (const t of taskings) {
    const e = t.intent_id != null ? identify.get(t.intent_id) : undefined
    if (e && t.id > e.newest) e.newest = t.id
  }
  for (const i of intents ?? []) {
    const e = identify.get(i.id)
    const last = i.history[i.history.length - 1]
    if (e && i.kind === 'identify' && last && (i.state === 'done' || i.state === 'failed' || /identified|not identified|lost contact|not in the picture/.test(last.reason))) e.ended = last.reason
  }
  // a card names the order it carries out, by what it is ("ENGAGE T-0008 with REAPER 1", not the
  // orbit the task layer flies it as), and numbers it as the Inbox and the map do (`i2`, not the
  // task layer's `#2`); a group order's names the group it went to ("ORBIT · North picket"), a
  // member's its group order's (orderCard.ts)
  const intentById = useMemo(() => new Map((intents ?? []).map((i) => [i.id, i])), [intents])
  const cardOf = (t: Tasking) => {
    const i = t.intent_id != null ? intentById.get(t.intent_id) : undefined
    const parent = i?.parent != null ? intentById.get(i.parent) : undefined
    const c = orderCard(t, i, parent, (uid) => {
      const v = nameOf(Number(uid))
      return v ? callsign(v) : `vehicle ${uid}`
    })
    const e = t.intent_id != null ? identify.get(t.intent_id) : undefined
    if (e) c.label = t.params.look ? `IDENTIFY ${e.track}` : `IDENTIFY ${e.track}, then ${t.task.toUpperCase()}`
    return c
  }
  // one card per order: an identify's newest tasking, a group order's members as one (orderCard.ts)
  const shownTaskings = oneCardPerOrder(
    taskings.filter((t) => !identify.has(t.intent_id ?? -1) || identify.get(t.intent_id ?? -1)?.newest === t.id),
    (id) => intentById.get(id),
  )
  // R6: an order to many vehicles is one card with its vehicles counted, exceptions listed; the
  // person opens one to list its vehicles (a view setting of this console)
  const [openCards, setOpenCards] = useState<ReadonlySet<number>>(() => new Set())
  const toggleCard = useCallback(
    (id: number) =>
      setOpenCards((prev) => {
        const next = new Set(prev)
        if (next.has(id)) next.delete(id)
        else next.add(id)
        return next
      }),
    [],
  )
  const cardOpen = (id: number) => openCards.has(id)
  // what makes an op stuck, read at the minute (the cards re-render no more often for it)
  const minute = Math.floor((now ? now() : Date.now()) / 60_000) * 60
  const [kind, setKind] = useState<PanelKind>('orbit')
  // rules 34: an order to many is one group order; what the person sets of how its members share
  // it (the engine fills the rest), cleared with the task or the selection
  const groupOrders = useGroupOrders()
  const [grp, setGrp] = useState<GroupSpec>({})
  // rules 21: what sending an identify order said (sent, or waiting in the Inbox)
  const [idSent, setIdSent] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<TaskOutcome | null>(null)
  // the selected vehicles by uid (all an order form looks up), not the fleet's thousands each render
  const byUid = new Map(vehicles.filter((v) => selected.has(v.uid)).map((v) => [v.uid, v]))
  // the fleet as the dry run reads it (names only): the same list while it is the same fleet
  const fleetRef = useRef(vehicles)
  if (!sameFleet(fleetRef.current, vehicles)) fleetRef.current = vehicles
  // the command log's cards look vehicles up by uid for their names: a lookup that is the same
  // function every render (so a card is not re-rendered for it), over the latest vehicles, its
  // map built only when a card is
  const historyRef = useRef({ vehicles: historyVehicles, by: null as Map<number, Vehicle> | null })
  if (historyRef.current.vehicles !== historyVehicles) historyRef.current = { vehicles: historyVehicles, by: null }
  const nameOf = useCallback((uid: number) => {
    const h = historyRef.current
    h.by ??= new Map(h.vehicles.map((v) => [v.uid, v]))
    return h.by.get(uid)
  }, [])
  // a card is re-rendered only when what it shows changed: the bridge sends the task list whole
  // each time, so identity says nothing; `sigs` is each tasking's ops' state and words
  const sigs = useMemo(() => new Map(taskings.map((t) => [t, t.ops.map((o) => `${o.id}:${o.uid}:${o.state}:${o.detail}:${o.progress}`).join('|')])), [taskings])
  const sel = [...selected].sort((a, b) => a - b)
  const limits = usePlatformLimits()
  const selVehicles = sel.map((s) => byUid.get(s)).filter((v): v is Vehicle => v != null)
  const orbit = orbitRange(selVehicles, limits)
  const offered = availableTasks(selVehicles, limits)
  const selKey = sel.join(',')
  // whether the selection can take `k`: identify sends exactly one vehicle (its camera's reach is
  // the bridge's to judge, below)
  const can = (k: PanelKind) =>
    k === 'identify'
      ? sel.length === 1
      : // rules 34: whatever patrols transits
        k === 'transit'
        ? groupOrders && (offered == null || offered.has('patrol'))
        : offered == null || offered.has(k)
  // rules 21: the identify target's answer for the one vehicle selected (why it can't go, or its
  // orbit, time and link)
  const idTrack = kind === 'identify' && sel.length === 1 ? (draft.track ?? null) : null
  const { options: idOptions, error: idError } = useIdentifyOptions(idTrack, idTrack ? sel : undefined)
  const idOption = idOptions?.find((o) => o.uid === sel[0]) ?? null
  // the map's ring on the target is the orbit the vehicle would fly
  const standoff = idOption?.standoff_m ?? null
  useEffect(() => {
    if (kind === 'identify' && standoff != null && standoff !== draft.radius) setDraft({ ...draft, radius: standoff })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind, standoff])

  // a new selection gets that platform's own defaults: its typical orbit, and a task it can do;
  // none selected, no order to draw, so nothing of one stays on the map
  useEffect(() => {
    if (!sel.length) {
      setDraft({ ...draft, center: null, route: [], track: null, note: null })
      return
    }
    setDraft({ ...draft, radius: orbit.value, track: null, note: null })
    setGrp({})
    if (!can(kind)) {
      const next = TASKS.find((t) => can(t.kind))?.kind
      if (next) choose(next)
    }
    // only when the selection (or the catalog) changes, not on every draft edit
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selKey, limits])

  const choose = (k: PanelKind) => {
    setKind(k)
    setError(null)
    setIdSent(null)
    setGrp({})
    setDraft({ ...draft, mode: k === 'orbit' || k === 'patrol' || k === 'transit' || k === 'identify' ? k : null, track: null, note: null })
  }
  // rules 34: a group order's options, for a kind that spreads over more than one vehicle
  const spreads = SPREADS[kind] ?? []
  const grouped = groupOrders && sel.length > 1 && spreads.length > 0
  const spread: Spread | undefined = grp.spread ?? spreads[0]
  const lanesOk = spread !== 'lanes' || ((grp.lanes ?? 0) >= 2 && (grp.lane_spacing_m ?? 0) >= 10)
  const groupSpec: GroupSpec | null = useMemo(() => {
    if (!grouped) return null
    const g: GroupSpec = {}
    if (grp.spread && grp.spread !== spreads[0]) g.spread = grp.spread
    if (grp.min_members != null) g.min_members = grp.min_members
    if (grp.wait_s != null) g.wait_s = grp.wait_s
    if (groupLabel) g.swarm = groupLabel
    if (grp.alt_m != null) g.alt_m = grp.alt_m
    if (grp.reform === false && reforms(kind)) g.reform = false
    if (g.spread === 'lanes') {
      g.lanes = grp.lanes
      g.lane_spacing_m = grp.lane_spacing_m
    }
    return Object.keys(g).length ? g : null
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [grouped, kind, grp, groupLabel])

  const trackers = sel.filter((s) => byUid.get(s)?.kind === 'tracker')
  const others = sel.filter((s) => byUid.get(s)?.kind !== 'tracker')
  const trackPair = trackers.length === 1 && others.length === 1

  const radiusOk = draft.radius >= orbit.min
  const ready =
    sel.length > 0 &&
    can(kind) &&
    (kind !== 'orbit' || radiusOk) &&
    lanesOk &&
    (kind === 'orbit'
      ? draft.center != null
      : kind === 'patrol'
        ? draft.route.length >= 2
        : kind === 'transit'
          ? draft.route.length >= 1
        : kind === 'track'
          ? trackPair
          : kind === 'identify'
            ? draft.track != null && idOption?.passed === true
            : true)

  /** the order as drafted, once it is complete: what Send sends and the dry run asks about */
  const request = useMemo<TaskRequest | null>(
    () =>
      ready
        ? {
            task: kind,
            uids: kind === 'track' ? trackers : sel,
            ...(kind === 'track' ? { target: others[0] } : {}),
            ...(kind === 'orbit' ? { center: draft.center!, radius: draft.radius } : {}),
            ...(kind === 'patrol' || kind === 'transit' ? { route: draft.route } : {}),
            ...(kind === 'identify' ? { track: draft.track! } : {}),
            ...(groupSpec ? { group: groupSpec } : {}),
          }
        : null,
    // the selection by its key; the rest as drafted
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [ready, kind, selKey, draft.center, draft.radius, draft.route, draft.track, groupSpec],
  )
  // the dry run (checks, policy, predicted link) of the order while it stands; information only
  const dry = useDryRun(busy ? null : request, vehicles)
  const linkRoute = useMemo(() => {
    const a = dry.answer
    // rules 34: a group's predicted link is along the group's own task, not each member's way to its
    // share, so its legs are not drawn (the slots and each member's way to its own are)
    const link = a?.link_coverage && dry.order && !a.group ? routeDrawing(a.link_coverage, dry.order, dry.at, a.routes) : EMPTY_ROUTE
    if (!a) return link
    // rules 22: the planned route, and where a refusal meets what refuses it
    const name = (uid: string | null) => {
      const v = uid == null ? null : byUid.get(Number(uid))
      return v ? callsign(v) : null
    }
    const plan = planDrawing(a.routes, a.checks.filter((c) => c.outcome === 'fail'), name)
    // rules 34: a group order's layout, each member's slot, and the members it leaves out where
    // they are (bd murmur-4nik: ghosts before it is sent)
    const lines = a.group ? memberClasses(a.uids ?? [], a.checks) : []
    const slots = a.group
      ? slotMarks(
          a.group,
          lines,
          (uid) => byUid.get(Number(uid))?.position ?? null,
          (uid) => name(uid) ?? uid,
        )
      : []
    const excluded = excludedMarks(
      lines,
      (uid) => {
        const v = byUid.get(Number(uid))
        return v?.position ? [v.position[0], v.position[1], drawnHeight(v)] : null
      },
      (uid) => name(uid) ?? uid,
    )
    if (!plan.routed?.length && !plan.marks.length && !slots.length && !excluded.length) return link
    return { ...link, routed: plan.routed, waypoints: plan.waypoints, marks: [...link.marks, ...plan.marks], slots, excluded }
    // byUid follows the vehicles; the answer is what changes the drawing
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dry.answer, dry.order, dry.at])
  useEffect(() => {
    onLinkRoute?.(linkRoute.lines.length || linkRoute.dots.length || linkRoute.marks.length || linkRoute.routed?.length || linkRoute.slots?.length ? linkRoute : null)
  }, [linkRoute, onLinkRoute])
  useEffect(() => () => onLinkRoute?.(null), [onLinkRoute])

  // whether the dry run is out, and where it says the order goes (read from its answer, never
  // guessed): on the Send button, the routing's reason in its title
  const route = dry.answer && request ? routing(dry.answer, tierLabel) : null
  const sendRoute = sendWords({ complete: request != null, loading: dry.loading, routing: route })
  const kindWord = (TASKS.find((t) => t.kind === kind)?.label ?? kind).toUpperCase()

  const submit = async () => {
    if (!request) return
    // rules 21: an identify order goes as an intent with its track (the task layer's route takes none)
    if (kind === 'identify' && draft.track) {
      setBusy(true)
      setError(null)
      try {
        setIdSent(await sendIdentify(sel[0], draft.track, callsign(byUid.get(sel[0]) ?? { uid: sel[0], kind: null })))
        setDraft({ ...draft, track: null, note: null })
      } catch (e) {
        setError((e as Error).message)
      } finally {
        setBusy(false)
      }
      return
    }
    setBusy(true)
    setError(null)
    setOutcome(null)
    try {
      const answer = await sendTask(request)
      if (answer.status === 202 && answer.intent_id != null && answer.state === 'queued') {
        setOutcome({ kind: 'queued', intentId: answer.intent_id, detail: answer.detail ?? '' })
      } else if (answer.status === 202 && answer.intent_id != null) {
        setOutcome({ kind: 'wait', intentId: answer.intent_id, detail: answer.detail ?? '', expires_t: answer.expires_t ?? null })
      } else if (answer.authorization && answer.intent_id != null) {
        setOutcome({ kind: 'authorized', intentId: answer.intent_id, authorization: answer.authorization })
      }
      setDraft({ ...draft, center: null, route: [] })
    } catch (e) {
      // a refusal keeps the draft, so it can be adjusted and sent again
      if (e instanceof TaskRefused) setOutcome({ kind: 'refused', intent: e.intent, detail: e.message })
      else setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <aside className="panel tasking">
      {/* the order form only while vehicles are selected (an order's outcome stays until dismissed) */}
      {(sel.length > 0 || outcome) && (
        <>
          <header className="panel-head">
            <h2>Task</h2>
            <span className="muted" title={sel.length > 6 ? sel.map((s) => callsign(byUid.get(s) ?? { uid: s, kind: null })).join(', ') : undefined}>
              {sel.length > 6 ? selectionLine(selVehicles, groupLabel) : sel.map((s) => callsign(byUid.get(s) ?? { uid: s, kind: null })).join(' · ')}
            </span>
          </header>
          <div className="taskform">
            {sel.length > 0 && (
              <>
              <div className="seg wide">
                {TASKS.filter((t) => t.kind !== 'transit' || groupOrders).map((t) => (
                  <button
                    key={t.kind}
                    className={kind === t.kind ? 'on' : ''}
                    disabled={!can(t.kind)}
                    title={
                      can(t.kind)
                        ? undefined
                        : t.kind === 'identify'
                          ? 'Identify sends exactly one vehicle: select one'
                          : 'Not every selected vehicle can do this'
                    }
                    onClick={() => choose(t.kind)}
                  >
                    {t.label}
                  </button>
                ))}
              </div>
              <p className="hint">
                {kind === 'identify' && draft.track
                  ? `Target ${draft.track}: click another ringed contact to change it.`
                  : TASKS.find((t) => t.kind === kind)!.hint}
              </p>
              {/* a click that picked nothing: why, where it can't be missed */}
              {kind === 'identify' && draft.note && <p className="error">{draft.note}</p>}
              {idTrack && (
                <p className={`hint ${idOption && !idOption.passed ? 'error' : ''}`}>
                  {idOption ? <OptionLine o={idOption} name={false} /> : idError ? null : 'Asking the bridge...'}
                </p>
              )}
              {idError && <p className="error">{idError}</p>}
              {kind === 'identify' && idSent && <p className="muted">{idSent}. Follow it in the command log.</p>}
              {kind === 'orbit' && (
                <label className="slider">
                  <span>Radius</span>
                  <input
                    type="range"
                    min={orbit.min}
                    max={orbit.max}
                    step={orbit.step}
                    value={Math.min(orbit.max, Math.max(orbit.min, draft.radius))}
                    onChange={(e) => setDraft({ ...draft, radius: Number(e.target.value) })}
                  />
                  <output>{draft.radius >= 10000 ? `${(draft.radius / 1000).toFixed(1)} km` : `${draft.radius} m`}</output>
                </label>
              )}
              {kind === 'orbit' && orbit.limitedBy && (
                <p className={`hint ${radiusOk ? '' : 'error'}`}>
                  Minimum {orbit.min.toLocaleString('en-US')} m: {sel.length === 1 ? 'its' : `${orbit.limitedBy}'s`} turning circle
                </p>
              )}
              {(kind === 'patrol' || kind === 'transit') && (
                <div className="row">
                  <span className="muted">
                    {draft.route.length} {kind === 'transit' ? 'point' : 'waypoint'}
                    {draft.route.length === 1 ? '' : 's'}
                  </span>
                  <button className="ghost" disabled={!draft.route.length} onClick={() => setDraft({ ...draft, route: draft.route.slice(0, -1) })}>
                    Undo
                  </button>
                  <button className="ghost" disabled={!draft.route.length} onClick={() => setDraft({ ...draft, route: [] })}>
                    Clear
                  </button>
                </div>
              )}
              {grouped && (
                <fieldset className="grp" aria-label="Group order">
                  <legend>One group order to {groupLabel ? `${groupLabel} (${sel.length})` : sel.length}</legend>
                  {spreads.length > 1 && (
                    <div className="seg">
                      {spreads.map((s) => (
                        <button key={s} className={spread === s ? 'on' : ''} title={SPREAD_WORDS[s]} onClick={() => setGrp({ ...grp, spread: s, ...(s === 'lanes' ? { lanes: grp.lanes ?? 2, lane_spacing_m: grp.lane_spacing_m ?? 200 } : {}) })}>
                          {s}
                        </button>
                      ))}
                    </div>
                  )}
                  {spread && <p className="hint">{SPREAD_WORDS[spread]}</p>}
                  <div className="grp-row">
                    <label>
                      <span>Minimum</span>
                      <input
                        type="number"
                        min={1}
                        max={sel.length}
                        placeholder={String(defaultMin(kind, sel.length))}
                        value={grp.min_members ?? ''}
                        onChange={(e) => setGrp({ ...grp, min_members: e.target.value === '' ? undefined : Math.max(1, Math.min(sel.length, Math.round(Number(e.target.value)))) })}
                      />
                    </label>
                  </div>
                  {/* the engine's defaults fit most orders: how long it waits for a member, its lowest layer */}
                  <details className="grp-more">
                    <summary>More group settings</summary>
                    <div className="grp-row">
                      <label title="how long the order waits for a member not heard yet to join it, s (empty: the engine's default)">
                        <span>Wait</span>
                        <input
                          type="number"
                          min={0}
                          max={3600}
                          placeholder="auto"
                          value={grp.wait_s ?? ''}
                          onChange={(e) => setGrp({ ...grp, wait_s: e.target.value === '' ? undefined : Math.max(0, Math.min(3600, Math.round(Number(e.target.value)))) })}
                        />
                        <span className="muted">s</span>
                      </label>
                      {kind !== 'hold' && kind !== 'rtl' && (
                        <label title="the lowest layer, m above home (a depth below the surface for a sub); empty: each type's own cruise altitude">
                          <span>Layer base</span>
                          <input
                            type="number"
                            min={0}
                            max={10000}
                            placeholder="type's own"
                            value={grp.alt_m ?? ''}
                            onChange={(e) => setGrp({ ...grp, alt_m: e.target.value === '' ? undefined : Math.max(0, Number(e.target.value)) })}
                          />
                          <span className="muted">m</span>
                        </label>
                      )}
                    </div>
                  </details>
                  {spread === 'lanes' && (
                    <div className="grp-row">
                      <label>
                        <span>Lanes</span>
                        <input type="number" min={2} max={50} value={grp.lanes ?? 2} onChange={(e) => setGrp({ ...grp, lanes: Math.round(Number(e.target.value)) })} />
                      </label>
                      <label>
                        <span>apart</span>
                        <input type="number" min={10} max={10000} value={grp.lane_spacing_m ?? 200} onChange={(e) => setGrp({ ...grp, lane_spacing_m: Number(e.target.value) })} />
                        <span className="muted">m</span>
                      </label>
                    </div>
                  )}
                  {reforms(kind) ? (
                    <label className="grp-check">
                      <input type="checkbox" checked={grp.reform ?? true} onChange={(e) => setGrp({ ...grp, reform: e.target.checked })} />
                      re-forms itself when members drop (inside these limits; below the minimum it stops and asks)
                    </label>
                  ) : (
                    <p className="hint">{kind === 'transit' ? 'A transit does not re-form: a gap a member leaves stays open.' : 'Each member by its type; nothing re-forms.'}</p>
                  )}
                </fieldset>
              )}
              <button className="primary" disabled={!ready || busy} onClick={submit} title={route?.text} aria-live="polite">
                {kind === 'track'
                  ? trackPair
                    ? `Point ${callsign(byUid.get(trackers[0])!)} at ${callsign(byUid.get(others[0])!)}`
                    : 'Select 1 tracker + 1 vehicle'
                  : kind === 'identify'
                    ? draft.track
                      ? `Send IDENTIFY ${draft.track} to ${callsign(byUid.get(sel[0]) ?? { uid: sel[0], kind: null })}${sendRoute ? ` (${sendRoute})` : ''}`
                      : 'Click a contact first'
                    : `${grouped ? `Send ${kindWord} to ${groupLabel ?? `${sel.length} vehicles`} as one group` : `Send ${kindWord} to ${sel.length} vehicle${sel.length > 1 ? 's' : ''}`}${sendRoute ? ` (${sendRoute})` : ''}`}
              </button>
              </>
            )}
            {error && <p className="error">{error}</p>}
            {request && !outcome && <DryRun st={dry} vehicles={fleetRef.current} />}
            {outcome && (
              <TaskResult
                outcome={outcome}
                vehicles={vehicles}
                stillPending={outcome.kind === 'wait' && (pendingIds?.has(outcome.intentId) ?? false)}
                onInbox={(id) => onInbox?.(id)}
                onDismiss={() => setOutcome(null)}
              />
            )}
          </div>
        </>
      )}

      <header className="panel-head sub">
        <h2>Command log</h2>
      </header>
      <div className="log">
        {taskings.length === 0 && runs.length === 0 && <p className="empty">Orders you send appear here</p>}
        {space &&
          scen != null &&
          runs.map((r) => <SpaceRunCard key={`s${r.intent_id}`} run={r} scen={scen} onOpen={onInbox} cancellable={space.cancellable ?? false} />)}
        {shownTaskings.map((t) => {
          const e = identify.get(t.intent_id ?? -1)
          const c = cardOf(t)
          return (
            <TaskingCard
              key={t.id}
              t={t}
              sig={sigs.get(t) ?? t.ops.map((o) => `${o.id}:${o.uid}:${o.state}:${o.detail}:${o.progress}`).join('|')}
              label={c.label}
              num={c.id}
              numTitle={c.idTitle}
              ended={e?.ended ?? null}
              over={(() => { const i = t.intent_id != null ? intentById.get(t.intent_id) : undefined; return orderOver(i?.kind, i?.state) })()}
              nameOf={nameOf}
              open={cardOpen(t.id)}
              onOpen={toggleCard}
              minute={minute}
            />
          )
        })}
      </div>
    </aside>
  )
}
