import { Fragment, type KeyboardEvent, type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import { stepsUpAct, useAuth } from './auth'
import { ChecksFold } from './Bluf'
import type { Check, Intent } from './decisions'
import { intentAction, StepUpNeeded, useOperator, useOperators } from './decisions'
import { DeclarationBlock } from './Declaration'
import {
  acceptBehavingBody,
  acceptLostBody,
  engageOptions,
  engageParams,
  FALLBACK_LABEL,
  nominate,
  type BehavingInfo,
  type EngageOption,
  type EngagePreview,
  type Fallback,
  type KeepOutExplanation,
  type KeepOutLost,
} from './engage'
import './engage.css'
import {
  blocks,
  checksLine,
  checkTitle,
  clearText,
  aisText,
  declFacts,
  dist,
  evidenceBullets,
  fallbackNote,
  fallbacksFor,
  nominateReady,
  platformLine,
  radiusFor,
  span,
  type Step,
  stepOf,
  targetState,
  whyText,
  windowFor,
} from './engageSteps'
import { KeepOutExplain } from './KeepOutExplain'
import { StepUpField } from './StepUp'
import { stepUpProof, stepUpReady, useStepUp } from './stepUpState'
import type { Track, TracksMessage } from './tracks'
import { kindWithLevel } from './tracks'
import { callsign, type Vehicle, vehicleNamer } from './types'

/** The nomination form (Phase E) as a checklist: 1 the target (its declaration, confirmed here
 *  when the policy needs it), 2 the platform (unranked, each with its one line), 3 the effect
 *  (radius, window, fallback, in plain words, drawn on the map), 4 the checks in one line (the
 *  keep-out explanation behind it), 5 a rationale, then Nominate with its consequence. Every check
 *  the bridge's dry run fails (`POST /api/engage/options`: the checks and the preview on the world
 *  now, nothing created) is shown at the step that can clear it and keeps Nominate disabled, so
 *  nothing fails at the end that a step could have shown (engageSteps.ts). Platforms are listed by
 *  uid, never ranked, and nothing is chosen for the operator: neither the target (unless the form
 *  was opened from it) nor the platform (unless opened from it). Nominating puts it in the inbox,
 *  where two operators with engagement authority, never the proposer, decide it. The form sits in
 *  the right rail, over the tracks and the task panel, so the map and the inbox stay clear.
 *
 *  Murmur-8xe, replace mode: opened by Replace on a pending nomination's card, or by "Replace iN"
 *  on a platform iN holds, the form takes iN's plan and, on submit, rejects iN and then nominates
 *  this plan in its place (two acts, in that order; nothing new in the engine). Every dry run
 *  passes `replaces`, so the checks read the platform as free of iN; they still gate the submit
 *  exactly as for any nomination. */

interface Props {
  tracks: TracksMessage | null
  vehicles: Vehicle[]
  now: () => number
  /** what opened the form: a track (its card), a platform (an armed vehicle's details), or a
   *  pending nomination to replace (its card's Replace) */
  initial: { track?: string; uid?: number; replaces?: number }
  /** every intent the console knows (the bridge's view): a nomination being replaced, or one
   *  that holds a platform, is found here */
  intents: Intent[]
  /** the selected option's preview, to draw on the map */
  onPreview: (p: EngagePreview | null) => void
  /** murmur-12e: the selected option's keep-out explanation, to draw its regions on the map */
  onKeepOuts: (ex: KeepOutExplanation | null) => void
  /** `replaced`: the nomination this one replaced (rejected just before) */
  onNominated: (intentId: number, replaced?: number) => void
  onClose: () => void
}

const FALLBACKS: Fallback[] = ['return', 'loiter', 'abort']

type StepState = 'done' | 'blocked' | 'todo' | 'optional'

const STATE_WORD: Record<StepState, string> = { done: 'ready', blocked: 'blocked', todo: 'to do', optional: 'optional' }

function StepBox({ n, title, state, status, note, children }: { n: number; title: string; state: StepState; status?: string; note?: string; children: ReactNode }) {
  return (
    <li className={`eg-step ${state}`}>
      <div className="eg-step-head">
        <span className="eg-num" aria-hidden="true">
          {state === 'done' ? '✓' : n}
        </span>
        <h3>
          <span className="eg-sr">Step {n}: </span>
          {title}
        </h3>
        {note && <span className="eg-step-note">{note}</span>}
        <span className="eg-step-status">{status ?? STATE_WORD[state]}</span>
      </div>
      <div className="eg-step-body">{children}</div>
    </li>
  )
}

/** A failing check at its step: what, why, and what would clear it. */
function Blocker({
  c,
  o,
  platform,
  now,
  needsConfirm,
  action,
  headless,
}: {
  c: Check
  o: EngageOption | null
  platform?: string
  now: number
  needsConfirm?: boolean
  action?: ReactNode
  /** the what and why are already on the line above (a platform's row): only what clears it */
  headless?: boolean
}) {
  const clear = clearText(c, o, { platform, now }, needsConfirm)
  if (headless && !clear && !action) return null
  return (
    <div className="eg-blocker">
      {!headless && (
        <p className="eg-blocker-head">
          {checkTitle(c.rule)}: {whyText(c, o)}
        </p>
      )}
      {clear && <p className="eg-blocker-clear">To clear: {clear}</p>}
      {action}
    </div>
  )
}

/** The checks list behind the one line: blocking first, each in plain words with the rule's code. */
function CheckRows({ checks }: { checks: Check[] }) {
  const order: Step[] = ['target', 'platform', 'effect', 'checks']
  const sorted = [...checks].sort((a, b) => Number(blocks(b)) - Number(blocks(a)) || order.indexOf(stepOf(a.rule)) - order.indexOf(stepOf(b.rule)))
  return (
    <div className="eg-checks">
      {sorted.map((c, k) => (
        <div key={k} className={`ix-check ${c.outcome}`}>
          <span className="ix-mark">{c.outcome === 'pass' ? 'PASS' : c.outcome === 'fail' ? 'BLOCKS' : c.outcome.replace('_', ' ').toUpperCase()}</span>
          <span className="ix-text">
            <span className="ix-rule">
              {checkTitle(c.rule)} <code className="eg-code">{c.rule}</code>
            </span>
            <span className="ix-detail">{c.detail}</span>
          </span>
        </div>
      ))}
    </div>
  )
}

/** A nomination's window as the form sets it: its length from the nomination, to 30 s. */
function windowOf(i: Intent): number {
  const w = engageParams(i).window
  if (!w) return 600
  return Math.max(30, Math.round((w.until - (w.from ?? i.created_t)) / 30_000) * 30)
}

/** Murmur-8xe: the id of the engagement a platform_free check says holds the platform. */
function holderOf(c: Check): number | null {
  if (c.rule !== 'platform_free' || !blocks(c)) return null
  const m = /engagement i(\d+) in play/.exec(c.detail)
  return m ? Number(m[1]) : null
}

/** "by rule, not confirmed", "by rule, confirmed by op-1", "by op-1": a declaration's few words. */
function declShort(t: Track) {
  const d = t.declared
  if (!d) return 'not declared'
  if (d.rule) return d.confirmed_by ? `declared by rule, confirmed by ${d.confirmed_by}` : 'declared by rule, not confirmed'
  return `declared by ${d.by}`
}

export function EngagePanel({ tracks, vehicles, now, initial, intents, onPreview, onKeepOuts, onNominated, onClose }: Props) {
  const [operator] = useOperator()
  const auth = useAuth()
  const ops = useOperators()
  // rules 19: only an operator with engagement authority may accept a live track as behaving
  // now (the bridge refuses the whole nomination otherwise): nobody else is offered the control
  const mayAcceptBehaving = auth.auth === 'credentials' ? auth.engage === true : ops.engage.includes(operator)
  const hostile = useMemo(() => (tracks?.tracks ?? []).filter((t) => t.affiliation === 'hostile' && t.declared).sort((a, b) => a.id.localeCompare(b.id)), [tracks])
  // murmur-8xe: opened to replace a pending nomination: its plan, to change
  const [base] = useState(() => (initial.replaces != null ? (intents.find((x) => x.id === initial.replaces) ?? null) : null))
  // nothing chosen for the operator: the target is the one the form was opened from, or none
  const [track, setTrack] = useState<string>(base ? engageParams(base).track : (initial.track ?? ''))
  const [radius, setRadius] = useState(base ? Math.round(engageParams(base).effect_radius_m) : 75)
  const [windowS, setWindowS] = useState(base ? windowOf(base) : 600)
  const [fallback, setFallback] = useState<Fallback>(base ? engageParams(base).fallback : 'return')
  const [options, setOptions] = useState<EngageOption[] | null>(null)
  const [uid, setUid] = useState<number | null>(base ? Number(base.uids[0]) : (initial.uid ?? null))
  const [rationale, setRationale] = useState(base?.rationale ?? '')
  // murmur-8xe: the pending nomination this one replaces (rejected on submit, just before it is
  // nominated), the one rejected here when the nomination after it was refused, and a one-line
  // note when replace mode ended on its own
  const [replaces, setReplaces] = useState<number | null>(base ? base.id : null)
  const [rejectedHere, setRejectedHere] = useState<number | null>(null)
  const [replaceNote, setReplaceNote] = useState<string | null>(null)
  // the reject this form sent, while it is under way (its own state change is not "ended on its own")
  const rejecting = useRef<number | null>(null)
  const old = replaces != null ? (intents.find((x) => x.id === replaces) ?? null) : null
  const takePlanOf = (i: Intent) => {
    // the operator's target stays; the rest is the nomination's
    const p = engageParams(i)
    setUid(Number(i.uids[0]))
    setRadius(Math.round(p.effect_radius_m))
    setWindowS(windowOf(i))
    setFallback(p.fallback)
    setRationale(i.rationale ?? '')
    setReplaces(i.id)
    setRejectedHere(null)
    setReplaceNote(null)
    setRefused(null)
  }
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // why the bridge refused the nomination itself, and the step its check belongs to (kept until
  // the next try: the dry run's refresh clears only its own error)
  const [refused, setRefused] = useState<{ text: string; step: Step | null } | null>(null)
  const [tick, setTick] = useState(0)
  // rules 16: the lost contacts the operator accepts with the nomination (by track id)
  const [acceptLost, setAcceptLost] = useState<Record<string, KeepOutLost>>({})
  const onAcceptLost = (l: KeepOutLost, on: boolean) =>
    setAcceptLost((cur) => {
      const next = { ...cur }
      if (on) next[l.track] = l
      else delete next[l.track]
      return next
    })
  // rules 19: the live tracks the operator accepts as behaving now with the nomination (by
  // track id): the engine logs each one's estimate at the nomination
  const [acceptBehaving, setAcceptBehaving] = useState<Record<string, BehavingInfo>>({})
  const onAcceptBehaving = (b: BehavingInfo, on: boolean) =>
    setAcceptBehaving((cur) => {
      const next = { ...cur }
      if (on) next[b.track] = b
      else delete next[b.track]
      return next
    })
  // what was accepted goes with the target and the platform it was shown for: a new choice
  // starts with none ticked
  useEffect(() => {
    setAcceptLost({})
    setAcceptBehaving({})
  }, [track, uid])
  const lostIds = Object.keys(acceptLost)
  // (none sent for an operator without engagement authority, say after switching operator)
  const behavingIds = mayAcceptBehaving ? Object.keys(acceptBehaving) : []
  // "T-0007 as lost and T-0002 as behaving now", for the step-up's label
  const acceptWhat = [lostIds.length ? `${lostIds.join(', ')} as lost` : '', behavingIds.length ? `${behavingIds.join(', ')} as behaving now` : '']
    .filter(Boolean)
    .join(' and ')
  // rules 8 and 10: in a credentials run whose policy lists accept_unknown, accepting a lost
  // contact (rules 19: or a live track as behaving now) takes the operator's step-up (the
  // password again, or their passkey); the bridge refuses the nomination without it
  const accepting = lostIds.length > 0 || behavingIds.length > 0
  const stepUp = accepting && stepsUpAct('accept_unknown')
  const [step, setStep] = useStepUp()
  const [stepBad, setStepBad] = useState(false)

  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => e.key === 'Escape' && onClose()
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  // the dry run, again when the parameters change and every few seconds (the picture moves)
  useEffect(() => {
    if (!track) {
      setOptions(null)
      return
    }
    let gone = false
    const id = window.setTimeout(() => {
      engageOptions(track, radius, windowS, fallback, replaces)
        .then((o) => {
          if (gone) return
          setOptions(o)
          setError(null)
        })
        .catch((e) => !gone && setError((e as Error).message))
    }, 250)
    const again = window.setTimeout(() => setTick((n) => n + 1), 4000)
    return () => {
      gone = true
      window.clearTimeout(id)
      window.clearTimeout(again)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [track, radius, windowS, fallback, replaces, tick])

  // murmur-8xe: the nomination being replaced ended on its own (expired, rejected or signed by
  // someone else): this is a plain nomination now, said in one line
  const oldGone =
    replaces != null &&
    rejecting.current !== replaces &&
    (old == null ? intents.length > 0 : old.state !== 'pending_approval' || (old.expires_t != null && old.expires_t <= now()))
  useEffect(() => {
    if (!oldGone || replaces == null) return
    const last = old?.history[old.history.length - 1]
    const why = old?.state === 'pending_approval' ? 'its window closed' : (last?.reason ?? old?.state ?? 'it is gone')
    setReplaces(null)
    setReplaceNote(`i${replaces} no longer waits for signatures (${why}): submitting now makes a new nomination and rejects nothing.`)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [oldGone])

  const chosen = options?.find((o) => o.uid === uid) ?? null
  // rules 19: the ticked tracks that accepting would not clear, on the latest dry run
  const behavingShort = behavingIds.filter((id) => {
    const b = chosen?.keep_outs?.items.find((x) => x.behaving?.track === id)?.behaving ?? acceptBehaving[id]
    return !b.clears
  })
  useEffect(() => {
    onPreview(chosen?.preview ?? null)
  }, [chosen, onPreview])
  useEffect(() => () => onPreview(null), [onPreview])
  useEffect(() => {
    onKeepOuts(chosen?.keep_outs ?? null)
  }, [chosen, onKeepOuts])
  useEffect(() => () => onKeepOuts(null), [onKeepOuts])

  const nowMs = now()
  const t = (tracks?.tracks ?? []).find((x) => x.id === track) ?? null
  const targets = t && !hostile.some((h) => h.id === t.id) ? [t, ...hostile] : hostile
  const decl = t ? declFacts(t) : null
  const target = targetState(decl, options, !!track)
  const needsConfirm = target.kind === 'confirm'
  const vehicleOf = (u: number) => vehicles.find((x) => x.uid === u)
  const nameOf = (u: number) => {
    const v = vehicleOf(u)
    return v ? callsign(v) : `vehicle ${u}`
  }
  // the fallbacks the chosen platform can be given (its own lost-link action): the others are off,
  // and a choice it can't take moves to one it can, the note saying why
  const chosenKind = uid != null ? vehicleOf(uid)?.kind : null
  const canFall = fallbacksFor(chosenKind)
  const fallNote = uid != null ? fallbackNote(nameOf(uid), chosenKind) : null
  useEffect(() => {
    if (canFall?.length && !canFall.includes(fallback)) setFallback(canFall[0])
    // only when the platform changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [uid, chosenKind])
  const modelOf = (u: number) => {
    const v = vehicleOf(u)
    return v?.identity?.short ?? v?.identity?.platform ?? ''
  }
  const platform = chosen ? nameOf(chosen.uid) : undefined
  const acc = { lost: acceptLost, behaving: mayAcceptBehaving ? acceptBehaving : {} }
  const ready = nominateReady({ target, option: chosen, acc, stepUpOk: !stepUp || stepUpReady(step) })
  const canNominate = ready.ok && !busy

  // the effect step's own checks: the target's accuracy against the radius (the same on every
  // option) and the fallback the chosen platform must hold
  const effectFails = (chosen ?? options?.[0])?.checks.filter((c) => blocks(c) && stepOf(c.rule) === 'effect' && (chosen || c.rule === 'target_uncertainty')) ?? []
  const line = chosen ? checksLine(chosen.checks, chosen, { platform, now: nowMs }, needsConfirm) : null
  // the chosen platform's own checks that block, said under the list with what would clear them
  const platformFails = chosen?.checks.filter((c) => blocks(c) && stepOf(c.rule) === 'platform') ?? []

  // murmur-8xe: what replacing means, and what changed from the plan replaced
  const oldP = old ? engageParams(old) : null
  const oldSigs = old?.engage?.signed?.length ?? 0
  const mayStill = old ? (old.engage?.engage_operators ?? ops.engage).filter((x) => x !== operator) : []
  const oldAccepted = [...(old?.engage?.accepted_lost ?? []).map((a) => `${a.track} as lost`), ...(old?.engage?.accepted_behaving ?? []).map((a) => `${a.track} as behaving now`)]
  const changed: string[] = []
  if (old && oldP) {
    const oldUid = Number(old.uids[0])
    if (track !== oldP.track) changed.push(`target ${oldP.track} → ${track || 'none'}`)
    if (uid !== oldUid) changed.push(`platform ${nameOf(oldUid)} → ${uid != null ? nameOf(uid) : 'none'}`)
    if (radius !== Math.round(oldP.effect_radius_m)) changed.push(`radius ${Math.round(oldP.effect_radius_m)} → ${radius} m`)
    if (windowS !== windowOf(old)) changed.push(`window ${span(windowOf(old) * 1000)} → ${span(windowS * 1000)}`)
    if (fallback !== oldP.fallback) changed.push(`fallback ${FALLBACK_LABEL[oldP.fallback]} → ${FALLBACK_LABEL[fallback]}`)
    if (rationale.trim() !== (old.rationale ?? '').trim()) changed.push('rationale edited')
  }
  // a platform held by another engagement: a pending one can be replaced from here, one signed
  // and under way only aborted from its card
  const holderAction = (o: EngageOption) => {
    const id = o.checks.map(holderOf).find((x) => x != null) ?? null
    if (id == null || id === replaces) return null
    const h = intents.find((x) => x.id === id)
    if (h?.state === 'pending_approval')
      return (
        <div className="eg-pick-act">
          <button type="button" className="dc-btn" disabled={busy} onClick={() => takePlanOf(h)} title={`Take i${id}'s plan here; submitting rejects i${id} and nominates this one in its place`}>
            Replace i{id}
          </button>
          <span className="eg-muted">takes its plan; i{id} is rejected only when you submit</span>
        </div>
      )
    if (h && (h.state === 'approved' || h.state === 'dispatched'))
      return <p className="eg-pick-act eg-muted">i{id} is signed and under way: only Abort on its card stops it.</p>
    return null
  }

  const submit = async () => {
    if (!track || uid == null || !canNominate) return
    if (stepUp && !stepUpReady(step)) {
      setStepBad(true)
      return
    }
    setBusy(true)
    setRefused(null)
    // murmur-8xe: replacing, the old nomination is rejected first, and nothing is nominated if
    // that fails
    const replacing = old?.state === 'pending_approval' ? old.id : null
    try {
      const list = Object.values(acceptLost)
      const proof = stepUp
        ? await stepUpProof(step, {
            act: 'accept_unknown',
            intent: null,
            ...(list.length ? { accept_lost: acceptLostBody(list) } : {}),
            ...(behavingIds.length ? { accept_behaving: acceptBehavingBody(behavingIds) } : {}),
          })
        : undefined
      if (replacing != null) {
        rejecting.current = replacing
        try {
          await intentAction(replacing, 'reject', `Replaced by a new nomination from ${operator}`)
        } catch (e) {
          rejecting.current = null
          setRefused({ text: `i${replacing} could not be rejected: ${(e as Error).message}`, step: null })
          setBusy(false)
          return
        }
        // from here this is a plain nomination: if it is refused, the form keeps every value
        setReplaces(null)
        setRejectedHere(replacing)
        rejecting.current = null
      }
      const i = await nominate({ track, uid, radius, windowS, fallback, rationale, acceptLost: list, acceptBehaving: behavingIds, proof })
      setStepBad(false)
      onNominated(i.id, replacing ?? undefined)
    } catch (e) {
      setStepBad(e instanceof StepUpNeeded)
      const text = (e as Error).message
      const rule = /^([a-z_]+): /.exec(text)?.[1]
      setRefused({ text, step: rule ? stepOf(rule) : null })
      setTick((n) => n + 1)
    }
    setStep({ password: '' })
    setBusy(false)
  }
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
      e.preventDefault()
      void submit()
    }
  }
  const refusedAt = (s: Step) => refused?.step === s && <p className="eg-refused">Not nominated: {refused.text}</p>

  const title = replaces != null ? `Replace i${replaces}` : rejectedHere != null ? `Nominate an engagement (i${rejectedHere} rejected)` : 'Nominate an engagement'

  // ---- the steps' states ------------------------------------------------------------------------
  const s1: StepState = target.kind === 'ready' ? 'done' : target.kind === 'confirm' || target.kind === 'blocked' ? 'blocked' : 'todo'
  const s1Status = target.kind === 'confirm' ? 'needs a confirmation' : target.kind === 'checking' ? 'checking' : target.kind === 'none' ? 'choose one' : undefined
  const pLine = chosen ? platformLine(chosen, nowMs) : null
  const s2: StepState = !chosen ? 'todo' : pLine?.ok ? 'done' : 'blocked'
  const s3: StepState = effectFails.length ? 'blocked' : chosen ? 'done' : 'todo'
  const s4: StepState = !line ? 'todo' : line.tone === 'ok' || ready.onAcceptances ? 'done' : 'blocked'

  return (
    <aside className="eg-panel" aria-label={title} onKeyDown={onKeyDown}>
      <header className="eg-top">
        <h2>{title}</h2>
        <span className="eg-as">as {operator}</span>
        <button className="ghost mini close" onClick={onClose} aria-label="Close" title="Close (Esc)">
          ✕
        </button>
      </header>
      <p className="eg-sub">An abstract effect: a point, a radius, a time. Two operators with engagement authority decide it, never you.</p>
      {replaces != null && (
        <div className="eg-replace" role="note" aria-label={`Replacing i${replaces}`}>
          <p className="eg-replace-head">
            Submitting rejects i{replaces}
            {oldSigs > 0 ? ` and drops its ${oldSigs === 1 ? 'signature' : `${oldSigs} signatures`}` : ''}, then nominates this plan in its place.
          </p>
          <p>
            It needs fresh signatures. You become its proposer, so you cannot sign it. {mayStill.length ? `May sign: ${mayStill.join(', ')}.` : 'Nobody else listed may sign.'}
          </p>
          {oldAccepted.length > 0 && <p>What i{replaces} accepted ({oldAccepted.join(', ')}) does not carry over: accept again at step 4 if you still need it.</p>}
          <p className={`eg-replace-diff ${changed.length ? '' : 'same'}`} aria-live="polite">
            {changed.length ? (
              <>
                <span className="eg-k">Changed from i{replaces}</span> {changed.join('; ')}
              </>
            ) : (
              `Same plan as i${replaces}: it meets the checks i${replaces} meets now, so change something above if one blocks.`
            )}
          </p>
        </div>
      )}
      {replaces == null && rejectedHere != null && (
        <p className="eg-replace-note" role="status">
          i{rejectedHere} is rejected, and this plan was not nominated: fix what the refusal says and nominate it again.
        </p>
      )}
      {replaces == null && rejectedHere == null && replaceNote && (
        <p className="eg-replace-note" role="status">
          {replaceNote}
        </p>
      )}
      <ol className="eg-steps">
        <StepBox n={1} title="Target" state={s1} status={s1Status}>
          {targets.length === 0 ? (
            <p className="eg-muted">No track is declared hostile. A track is declared on its card in Tracks: by an operator, or by the engine's rule on a hostile act our sensors observed.</p>
          ) : (
            <div className="eg-picks" role="radiogroup" aria-label="Declared-hostile tracks">
              {targets.map((h) => (
                <label key={h.id} className={`eg-pick ${track === h.id ? 'on' : ''}`}>
                  <input type="radio" name="eg-target" checked={track === h.id} onChange={() => setTrack(h.id)} />
                  <span className="eg-pick-text">
                    <span className="eg-pick-name">
                      <b>{h.id}</b> {kindWithLevel(h.identity)}
                    </span>
                    <span className={`eg-pick-line ${h.declared?.rule && !h.declared.confirmed_by ? 'warn' : ''}`}>
                      {h.affiliation === 'hostile' ? 'hostile' : h.affiliation}, {declShort(h)}
                    </span>
                  </span>
                </label>
              ))}
            </div>
          )}
          {track && !t && <p className="eg-blocker-head">{track} is no longer in the picture: choose another target.</p>}
          {t && target.kind === 'confirm' && (
            <div className="eg-need">
              <p className="eg-need-head">The policy needs an operator's confirmation before a nomination. Read the evidence, then confirm or revoke.</p>
              <DeclarationBlock t={t} vehicles={vehicles} now={now} where="nominate" onChanged={() => setTick((n) => n + 1)} />
            </div>
          )}
          {t && target.kind === 'ready' && t.declared?.evidence && (
            <details className="bluf-more eg-evidence">
              <summary>Evidence</summary>
              <ul className="dc-evidence">
                {evidenceBullets(t.declared.evidence, vehicleNamer(vehicles), nowMs).map((b) => (
                  <li key={b}>{b}</li>
                ))}
                <li>{aisText(t)}</li>
              </ul>
            </details>
          )}
          {t && target.kind === 'blocked' && (
            <>
              {target.checks.length === 0 && <p className="eg-blocker-head">{t.id} is not declared hostile: an operator declares it on its card in Tracks.</p>}
              {target.checks.map((c) => (
                <Blocker key={c.rule} c={c} o={options?.[0] ?? null} now={nowMs} />
              ))}
            </>
          )}
          {refusedAt('target')}
        </StepBox>

        <StepBox n={2} title="Platform" state={s2} status={!chosen ? 'choose one' : undefined} note="by uid, not ranked">
          {!track ? (
            <p className="eg-muted">Listed once a target is chosen.</p>
          ) : !options ? (
            <p className="eg-muted">Checking every armed platform...</p>
          ) : !options.length ? (
            <p className="eg-muted">No armed platform in the fleet.</p>
          ) : (
            <>
              {uid == null && <p className="eg-muted">Nothing is chosen for you: pick one.</p>}
              <div className="eg-picks" role="radiogroup" aria-label="Armed platforms">
                {options.map((o) => {
                  const pl = platformLine(o, nowMs)
                  return (
                    <Fragment key={o.uid}>
                      <label className={`eg-pick ${uid === o.uid ? 'on' : ''}`}>
                        <input type="radio" name="eg-platform" checked={uid === o.uid} onChange={() => setUid(o.uid)} />
                        <span className="eg-pick-text">
                          <span className="eg-pick-name">
                            <b>{nameOf(o.uid)}</b> {modelOf(o.uid)}
                          </span>
                          <span className={`eg-pick-line ${pl.ok ? 'ok' : 'bad'}`}>
                            <span aria-hidden="true">{pl.ok ? '✓ ' : '✕ '}</span>
                            {pl.text}
                          </span>
                        </span>
                      </label>
                      {holderAction(o)}
                    </Fragment>
                  )
                })}
              </div>
              {platformFails.map((c, k) => {
                const w = windowFor(c)
                return (
                  <Blocker
                    key={`${c.rule}${c.uid ?? ''}`}
                    c={c}
                    o={chosen}
                    platform={platform}
                    now={nowMs}
                    headless={k === 0}
                    action={
                      w != null && w <= 3600 && w !== windowS ? (
                        <button type="button" className="dc-btn" onClick={() => setWindowS(w)}>
                          Use a {span(w * 1000)} window
                        </button>
                      ) : undefined
                    }
                  />
                )
              })}
            </>
          )}
          {refusedAt('platform')}
        </StepBox>

        <StepBox n={3} title="Effect" state={s3}>
          <div className="eg-row">
            <label>
              Radius
              <span className="eg-unit">
                <input type="number" min={10} max={250} step={5} value={radius} onChange={(e) => setRadius(Math.max(1, Number(e.target.value) || 0))} />m
              </span>
            </label>
            <label>
              Window
              <span className="eg-unit">
                <input type="number" min={0.5} max={60} step={0.5} value={windowS / 60} onChange={(e) => setWindowS(Math.max(30, Math.round((Number(e.target.value) || 0) * 60)))} />
                min
              </span>
            </label>
          </div>
          <fieldset className="eg-fallbacks">
            <legend>If it stops before the effect (an abort, a void, a lost link)</legend>
            {FALLBACKS.map((f) => {
              const off = canFall != null && !canFall.includes(f)
              return (
                <label key={f} className={off ? 'off' : undefined} title={off && fallNote ? fallNote : undefined}>
                  <input type="radio" name="fallback" checked={fallback === f} disabled={off} onChange={() => setFallback(f)} />
                  {FALLBACK_LABEL[f]}
                </label>
              )
            })}
            {fallNote && <p className="eg-fallback-note">{fallNote}</p>}
          </fieldset>
          {effectFails.map((c) => {
            const r = radiusFor(c)
            return (
              <Blocker
                key={c.rule}
                c={c}
                o={chosen}
                now={nowMs}
                action={
                  r != null && r <= 250 && r !== radius ? (
                    <button type="button" className="dc-btn" onClick={() => setRadius(r)}>
                      Use {r} m
                    </button>
                  ) : undefined
                }
              />
            )
          })}
          {chosen?.preview && (
            <div className="eg-plain">
              <span className="eg-plain-head">If signed now (drawn on the map)</span>
              <ul>
                <li>
                  {platform} sets off {span(chosen.preview.start_t - chosen.preview.t)} after the last signature and flies {dist(chosen.preview.distance_m)} at {Math.round(chosen.preview.cruise_mps)} m/s
                </li>
                <li>
                  The effect about {span(chosen.preview.effect_t - nowMs)} from now, {Math.round(chosen.preview.effect_radius_m)} m around the marked point
                </li>
                <li>It must come within the window, {span(windowS * 1000)} from the nomination</li>
              </ul>
            </div>
          )}
          {refusedAt('effect')}
        </StepBox>

        <StepBox n={4} title="Checks" state={s4} status={!line ? 'after step 2' : undefined}>
          {!chosen || !line ? (
            <p className="eg-muted">They run once a platform is chosen.</p>
          ) : (
            <ChecksFold
              checks={chosen.checks}
              line={{ text: line.text, tone: line.tone === 'ok' ? 'ok' : ready.onAcceptances ? 'warn' : 'bad' }}
              sub={line.clear ? `To clear: ${line.clear}` : undefined}
              open={line.tone === 'bad' && !!chosen.keep_outs?.blocked}
            >
              <CheckRows checks={chosen.checks} />
              {chosen.keep_outs && (
                <KeepOutExplain
                  ex={chosen.keep_outs}
                  name={nameOf(chosen.uid)}
                  now={now}
                  acceptLost={acceptLost}
                  onAcceptLost={onAcceptLost}
                  acceptBehaving={acceptBehaving}
                  onAcceptBehaving={mayAcceptBehaving ? onAcceptBehaving : undefined}
                />
              )}
            </ChecksFold>
          )}
          {(lostIds.length > 0 || behavingIds.length > 0) && (
            <ul className="eg-accepts" role="status">
              {lostIds.length > 0 && (
                <li>
                  You accept {lostIds.join(', ')} as lost for this engagement: logged under your session with the nomination and shown to every signer; the checks leave{' '}
                  {lostIds.length > 1 ? 'them' : 'it'} out while out of view since that fix.
                </li>
              )}
              {behavingIds.length > 0 && (
                <li>
                  You accept {behavingIds.join(', ')} as behaving now: logged with the estimate then and shown to every signer; the keep-out counts{' '}
                  {behavingIds.length > 1 ? 'each' : 'it'} along that course only, and the acceptance lapses on its own if it departs from it or goes out of view.
                  {behavingShort.length > 0 ? ` Accepting ${behavingShort.join(', ')} would not clear the keep-out.` : ''}
                </li>
              )}
              {ready.onAcceptances && <li>Your acceptances cover every blocker. The dry run above does not count them; the engine checks with them when you nominate.</li>}
            </ul>
          )}
          {refusedAt('checks')}
        </StepBox>

        <StepBox n={5} title="Rationale" state="optional">
          <label className="eg-rationale">
            <span className="eg-muted">Why this target, this platform, now. The signers read it. Bottom line first.</span>
            <textarea rows={2} value={rationale} maxLength={500} onChange={(e) => setRationale(e.target.value)} aria-label="Rationale (optional; the signers read it)" />
          </label>
        </StepBox>
      </ol>

      <footer className="eg-go">
        {stepUp && (
          <StepUpField
            passwordLabel={`Your password, to accept ${acceptWhat}`}
            purpose={`to accept ${acceptWhat} for this engagement`}
            ariaLabel="Your password, to accept for this engagement"
            value={step}
            onChange={(s) => {
              setStep(s)
              setStepBad(false)
              setRefused(null)
            }}
            busy={busy}
            bad={stepBad}
            onEnter={() => void submit()}
          />
        )}
        <p className="eg-consequence">
          {replaces != null ? `Rejects i${replaces}, then creates` : 'Creates'} a nomination{t && platform ? ` of ${t.id} with ${platform}` : ''} in the Inbox
          {replaces != null ? ' in its place' : ''}. Nothing moves until two operators with engagement authority, neither of them you, sign it.
        </p>
        {!ready.ok && <p className="eg-missing">Not yet: {ready.missing.join('; ')}.</p>}
        <div className="eg-go-row">
          <button className="eg-nominate" disabled={!canNominate} onClick={() => void submit()} title="Ctrl+Enter">
            {replaces != null ? `Replace i${replaces}` : t ? `Nominate ${t.id}` : 'Nominate'}
          </button>
          <button className="dc-btn quiet" onClick={onClose} title="Esc">
            Cancel
          </button>
          <span className="eg-keys" aria-hidden="true">
            Ctrl+Enter
          </span>
        </div>
        {refused && !refused.step && <p className="eg-refused">Not nominated: {refused.text}</p>}
        {error && <p className="error ix-error">{error}</p>}
      </footer>
    </aside>
  )
}
