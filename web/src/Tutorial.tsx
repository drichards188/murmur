import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react'
import type { Check, Decisions } from './decisions'
import type { SituationStatus } from './SituationPanel'
import { kindLabel, type TracksMessage } from './tracks'
import { coachPlace, CONSOLE_OPERATOR, type Facts, type IdentityCheck, resumeAt, stepScene, STEPS, stepDone, type StepId, tutorialFacts } from './tutorial'
import type { LngLat } from './types'
import './tutorial.css'

/** The tutorial's coach card (sim/situations/tutorial.toml): shown only while the running
 *  situation says it is the tutorial (`modes.tutorial` in `GET /api/situation`). It walks the
 *  person through the engagement chain as it happens: a card at the map's bottom left, non-modal
 *  (the map and every panel stay usable), a ring around the part of the console each step needs
 *  (the `data-tutorial` attributes), and a step that moves on by itself when the bridge shows it
 *  done (tutorial.ts), or with Next for a step that is only read. Display only: it reads what the
 *  console reads and never acts for the person. Its place in the walk is kept per run in this
 *  browser (a reload picks it up), wrapped against a storage that fails. */

interface Props {
  /** the situation runner's status (useSituation.ts), null where none runs */
  situation: SituationStatus | null
  decisions: Decisions | null
  tracks: TracksMessage | null
  /** the bridge's clock, ms */
  now: () => number
  /** the launcher's main menu, when the console has one */
  onMenu?: () => void
  /** the nomination form's latest dry run on its target, while it is open */
  formChecks?: { track: string; checks: Check[] } | null
  /** where each of ours is now, by callsign, for the step's scene */
  placeOf?: (callsign: string) => LngLat | null
  /** frame these points on the map (once as a step comes up: the card keeps its scene in view) */
  onFit?: (points: LngLat[]) => void
}

/** how long a done step shows its tick before the walk moves on (ms) */
const ADVANCE_MS = 1400

interface Saved {
  idx: number
  hidden: boolean
  closed: boolean
  /** situation time (s) at the assessment, for the wrap-up */
  doneAt?: number
}

function load(key: string | null): Saved | null {
  if (!key) return null
  try {
    const v = JSON.parse(window.localStorage.getItem(key) ?? 'null')
    return v && typeof v.idx === 'number' ? (v as Saved) : null
  } catch {
    return null
  }
}

function save(key: string | null, v: Saved) {
  if (!key) return
  try {
    window.localStorage.setItem(key, JSON.stringify(v))
  } catch {
    // no storage: a reload starts the walk again where the bridge says the person is
  }
}

const mmss = (s: number) => {
  const n = Math.max(0, Math.round(s))
  return `${Math.floor(n / 60)}:${String(n % 60).padStart(2, '0')}`
}

const q = (target: string, extra = '') => `[data-tutorial="${target}"]${extra}`

const n2 = (x: number) => x.toFixed(2)

/** What our sensors hold of the boat, in the words of its level: the form's target-identity check. */
function heldAs(T: string, c: IdentityCheck) {
  const k = kindLabel(c.kind)
  switch (c.level) {
    case 'detect':
      return `only detected ${T} as a ${k} (its size class) at ${n2(c.confidence)}`
    case 'recognise':
      return `only recognised ${T}'s family (${k}) at ${n2(c.confidence)}`
    case 'identify':
      return `identified ${T} as a ${k}, but only at ${n2(c.confidence)}`
    default:
      return `hold ${T} as ${k} at ${n2(c.confidence)}`
  }
}

/** What each step says, and which parts of the console it rings (the first one shown wins). */
function stepView(
  id: StepId,
  f: Facts,
  sawIdentityBlock = false,
): { title: string; body: ReactNode; note?: ReactNode; targets: string[] } {
  const T = f.boat?.id ?? 'the boat'
  const row = f.boat ? [q('track-row', `[data-track-id="${f.boat.id}"]`)] : []
  const N = f.nomination ? `i${f.nomination.id}` : 'your nomination'
  switch (id) {
    case 'welcome':
      return {
        title: 'Welcome to the watch',
        body: (
          <>
            <p>
              You are <b>{CONSOLE_OPERATOR}</b>, the operator at this console. In about 8 minutes you take one engagement through Murmur&apos;s decision chain:
            </p>
            <ol>
              <li>a boat attacks one of ours (missiles, then a machine gun): you see when and how we know, and the engine declares the boat hostile;</li>
              <li>you confirm that declaration: the threat designation;</li>
              <li>you nominate an engagement with an armed drone;</li>
              <li>two other operators sign it (you cannot sign your own);</li>
              <li>the drone releases its missile, and our sensors assess the result.</li>
            </ol>
            <p>
              This card moves on by itself when something happens. <b>Next</b> and <b>Back</b> step through the parts you only read. The map stays live underneath: pan and zoom
              as you like.
            </p>
            <p className="tut-small">
              Nothing here is real: simulated vehicles and weapons. Two training aids, in this tutorial only: our missile is set to destroy the boat, and the
              boat&apos;s strikes on HUNTER 1 are drawn and recorded but do it no harm (elsewhere they can stop or sink it). The two signers are scripts, labeled
              scripted.
            </p>
          </>
        ),
        targets: [],
      }
    case 'map':
      return {
        title: 'Your fleet',
        body: (
          <>
            <p>Two of ours, listed on the left and drawn on the map:</p>
            <ul>
              <li>
                <b>HUNTER 1</b>, a sensor ship (radar and a camera) patrolling the channel just west of the Golden Gate Bridge;
              </li>
              <li>
                <b>REAPER 1</b>, an armed drone loitering at 7,600 m off Lands End, west of the bridge.
              </li>
            </ul>
            <p>REAPER 1 has no orders. Nothing scripted may order an armed platform: every order for one waits for a person.</p>
          </>
        ),
        targets: [q('fleet')],
      }
    case 'tracks':
      return {
        title: 'Contacts: the Tracks list',
        body: (
          <>
            <p>
              On the right, <b>Tracks</b> lists every contact our sensors hold: what it is, why it is shown so, and which of ours see it. A ship broadcasting AIS shows as
              neutral; a contact without AIS stays unknown until a sensor classifies it.
            </p>
            {f.boat ? (
              <p>
                Find <b>{T}</b>: a small fast boat without AIS, holding mid-strait 1.5 km west of the bridge. Click its row to open its card.
              </p>
            ) : (
              <p>A small fast boat without AIS is coming in west of the bridge. It appears here within a minute.</p>
            )}
          </>
        ),
        targets: [...row, q('tracks')],
      }
    case 'act':
      return {
        title: 'Watch for the attack',
        body: (
          <>
            <p>
              Intelligence warned of an attack by small craft, which are known to carry ATGM-class missiles (some 250 m/s: from a kilometre one strikes in about 4 s)
              and heavy machine guns. This boat fires its missiles first, then bursts of its machine gun. Three things can tell us it fired:
            </p>
            <ol>
              <li>
                <b>the launch</b>: a camera with the boat in view sees the flash, a missile&apos;s or a gun&apos;s muzzle flash. HUNTER 1&apos;s ring camera always has it
                in view, so by day it sees nearly every one and places the shooter;
              </li>
              <li>
                <b>the missile in flight</b>, a small fast object: REAPER 1&apos;s radar plots it about two flights in five, HUNTER 1&apos;s camera about one in three, mostly
                in its last second. HUNTER 1&apos;s navigation radar never does: a missile coming nose-on is smaller than the sea clutter around it. A gun&apos;s rounds are
                never seen in flight;
              </li>
              <li>
                <b>the strike</b>: HUNTER 1 knows it was hit (here the hit is drawn and recorded but does it no harm, a training aid).
              </li>
            </ol>
            <p>
              The earliest cue is when we know; from it to the strike is our warning. The map and the banner show what we know, stage by stage, as a crew learns it: a{' '}
              <b>launch</b> (where the camera placed the shooter, the bearing from HUNTER 1: no type, speed or target yet); a <b>weapon</b> once fixes measure a speed no
              boat makes (its type unknown); its <b>class</b> only on named evidence: the boat identified as a kind known to carry missiles and the camera telling the
              launch as a missile&apos;s (from a kilometre, about 19 times in 20), or a speed fitting one class alone (some 250 m/s fits an ATGM and a rocket, and names
              neither). A muzzle flash is gunfire at once. The estimated flight is drawn dashed, with a countdown, only once its target and timing have a basis; its real
              fixes are solid marks. The exact weapon is never named: nothing here could tell it.
            </p>
            <p>
              The launch the camera saw is the act&apos;s observation: attributed to one track at 0.9 or more, the engine declares that track hostile by rule. Speed,
              closeness or a missing AIS never declare anything.
            </p>
          </>
        ),
        note: f.declared ? undefined : f.lastActNot ? (
          <>
            An act came, but it was {f.lastActNot}. On an unsure attribution the engine declares nothing. The boat fires again within 90 s: the next launch is another
            chance.
          </>
        ) : f.attack && !f.attack.launch && f.attack.struck ? (
          <>No sensor saw that launch, so no shooter was placed and the rule could declare nothing. The boat fires again within 90 s.</>
        ) : (
          <>Watching. In this tutorial the boat fires about every 90 s.</>
        ),
        targets: [...row, q('weapon-status'), q('tracks')],
      }
    case 'confirm':
      return {
        title: 'Confirm the declaration',
        body: (
          <>
            <p>
              The engine declared <b>{T}</b> hostile by rule{f.declarationAct ? ` on act ${f.declarationAct}` : ''}. Nothing can be nominated on it until an operator
              confirms it: that confirmation is your threat designation.
            </p>
            <p>
              Open the <b>Inbox</b> (top bar). Under <b>Needs you</b>, read {T}&apos;s card: the act and whom it was against, which sensors saw it, the attribution against the 0.9
              threshold. If it holds, click <i>Confirm hostile…</i>, then <i>Confirm {T} hostile</i>. (The track&apos;s card in Tracks has the same Confirm.)
            </p>
            <p className="tut-small">If it does not hold, revoke it: the next act may declare it again.</p>
          </>
        ),
        note: !f.confirmed && !f.declared ? <>No declaration stands on {T} now. The next act may declare it again.</> : undefined,
        targets: [q('review-declaration', f.boat ? `[data-track-id="${f.boat.id}"]` : ''), q('confirm-declaration'), q('inbox-button')],
      }
    case 'nominate':
      return {
        title: 'Nominate an engagement',
        body: (
          <>
            <p>
              In <b>Tracks</b>, open {T}&apos;s card and click <i>Nominate an engagement…</i>: the form opens on the right as a checklist.
            </p>
            <ol>
              <li>Target: {T}, confirmed.</li>
              <li>
                Platform: choose <b>REAPER 1</b>. Nothing is chosen for you; each line says how far it is and whether its checks pass.
              </li>
              <li>Effect: a radius (75 m is ample for a small boat), a window, and what it does if stopped before the effect (a fixed wing can only return home).</li>
              <li>Checks: every one must pass. If a keep-out blocks, the form names what is in the way and what would clear it.</li>
              <li>Rationale: a line for the signers: why this target, this platform, now.</li>
            </ol>
            <p>
              Then <b>Nominate</b>. The map draws the plan: REAPER 1&apos;s path, the effect point and the keep-out ring.
            </p>
          </>
        ),
        note:
          f.identity && !f.identity.passed ? (
            // the form blocks on the target's identity: what it means, and what clears it
            <>
              The form blocks on <b>target identity</b>: our sensors have {heldAs(T, f.identity)}, and the policy needs {n2(f.identity.needed)} to nominate. Each
              look adds evidence: wait a few seconds for more looks, or send one of ours for a closer look (<i>identify…</i> on {T}&apos;s track card). The check
              clears by itself; then nominate.
            </>
          ) : f.nominationEnded ? (
            <>
              {N} was {f.nominationEnded.state}
              {f.nominationEnded.reason ? `: ${f.nominationEnded.reason}` : ''}. Nominate again from {T}&apos;s card.
            </>
          ) : f.identity?.passed && sawIdentityBlock ? (
            <>
              Target identity clears: {T} held as {kindLabel(f.identity.kind)} at {n2(f.identity.confidence)} (the policy needs {n2(f.identity.needed)}). You can
              nominate.
            </>
          ) : undefined,
        targets: [q('engage-nominate'), q('nominate'), ...row, q('tracks')],
      }
    case 'sign':
      return {
        title: 'Two signatures, neither yours',
        body: (
          <>
            <p>
              {N} waits in the Inbox. An engagement needs two operators with engagement authority, and never its proposer. You proposed it, so you cannot sign it: the
              two-person rule (the card says so, and the engine would refuse it).
            </p>
            <p>
              Here <b>op-2</b> and <b>op-3</b> sign: scripts standing in for two colleagues, labeled scripted on the card. Each first asks the engine for the picture it
              would sign, then signs that plan, and only while the live checks pass.
            </p>
          </>
        ),
        note: f.nominationEnded ? (
          <>
            {N} was {f.nominationEnded.state}
            {f.nominationEnded.reason ? `: ${f.nominationEnded.reason}` : ''}. Use Back to nominate again.
          </>
        ) : undefined,
        targets: [q('engage-card', f.nomination ? `[data-intent-id="${f.nomination.id}"]` : ''), q('inbox-button')],
      }
    case 'strike':
      return {
        title: 'Dispatch, release, flight',
        body: (
          <>
            <p>Both signatures are in: REAPER 1 is dispatched toward the effect point.</p>
            <p>
              The engine releases its missile only when REAPER 1 is within the missile&apos;s range of the point (some 2 to 2.5 km out), the strike falls inside the
              plan&apos;s time, and every keep-out still passes, judged at the moment of the strike. The missile then flies for about 18 s.
            </p>
            <p>
              Until the release, any operator may stop it with <i>Abort</i> on the card.
            </p>
          </>
        ),
        note: f.nominationEnded ? (
          <>
            {N} was {f.nominationEnded.state}
            {f.nominationEnded.reason ? `: ${f.nominationEnded.reason}` : ''}. REAPER 1 takes its fallback. Use Back to nominate again.
          </>
        ) : f.released ? undefined : (
          <>REAPER 1 is on its way: the card counts down to the effect.</>
        ),
        targets: [q('engage-run', f.nomination ? `[data-intent-id="${f.nomination.id}"]` : ''), q('inbox-button')],
      }
    case 'assess':
      return {
        title: 'Impact and assessment',
        body: (
          <>
            <p>
              The missile struck at its impact. In this tutorial every strike on the boat destroys it, so the chain ends cleanly. In other situations a
              game-level damage model draws each strike (a published hit chance, then the damage), and a weapon can miss or only stop a boat rather than sink it.
            </p>
            <p>Our sensors now look at the point, and the engine assesses the result from what they see, never from the simulation:</p>
            <ul>
              <li>
                <b>ceased</b>: no target detected where expected (what you should see here);
              </li>
              <li>
                <b>persists</b>: the target is still detected; another nomination may follow;
              </li>
              <li>
                <b>unknown</b>: none of ours covers the point.
              </li>
            </ul>
          </>
        ),
        note: f.assessment ? undefined : <>Waiting for our sensors: about 15 s.</>,
        targets: [q('impact-status'), q('engage-run', f.nomination ? `[data-intent-id="${f.nomination.id}"]` : '')],
      }
    case 'done':
      return { title: 'Done: the chain end to end', body: null, targets: [] }
  }
}

/** A ring around the first of `targets` on the page now (redrawn as panels move and open). */
function Ring({ targets }: { targets: string[] }) {
  const [box, setBox] = useState<{ top: number; left: number; width: number; height: number } | null>(null)
  const key = targets.join('|')
  useEffect(() => {
    if (!targets.length) {
      setBox(null)
      return
    }
    const find = () => {
      for (const sel of targets) {
        for (const el of document.querySelectorAll<HTMLElement>(sel)) {
          const r = el.getBoundingClientRect()
          if (r.width > 0 && r.height > 0) {
            setBox((b) => (b && b.top === r.top && b.left === r.left && b.width === r.width && b.height === r.height ? b : { top: r.top, left: r.left, width: r.width, height: r.height }))
            return
          }
        }
      }
      setBox(null)
    }
    find()
    const id = window.setInterval(find, 300)
    return () => window.clearInterval(id)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])
  if (!box) return null
  const pad = 4
  return <div className="tut-ring" aria-hidden="true" style={{ top: box.top - pad, left: box.left - pad, width: box.width + 2 * pad, height: box.height + 2 * pad }} />
}

/** The coach card's place (tutorial.ts coachPlace) above the weather bar (WeatherLayer.tsx's
 *  `.weather-legend`, over the same corner of the map), measured while the card shows and again
 *  whenever the bar or the map changes size (the bar folds and opens; the window resizes). */
function useCoachPlace(on: boolean): { bottom: number; maxHeight: number } | null {
  const [place, setPlace] = useState<{ bottom: number; maxHeight: number } | null>(null)
  useEffect(() => {
    if (!on) return
    let raf = 0
    const measure = () => {
      raf = 0
      const bar = document.querySelector<HTMLElement>('.weather-legend')
      const map = document.querySelector<HTMLElement>('.mapwrap')
      if (!map) return
      const m = map.getBoundingClientRect()
      const next = coachPlace(m.height, bar && bar.offsetParent ? bar.getBoundingClientRect().top - m.top : null)
      setPlace((p) => (p && p.bottom === next.bottom && p.maxHeight === next.maxHeight ? p : next))
    }
    const later = () => {
      if (!raf) raf = requestAnimationFrame(measure)
    }
    const ro = new ResizeObserver(later)
    const watch = () => {
      ro.disconnect()
      for (const sel of ['.weather-legend', '.mapwrap']) {
        const el = document.querySelector(sel)
        if (el) ro.observe(el)
      }
      later()
    }
    watch()
    // the bar is made again when the weather layer's state changes: watch the new one
    const mo = new MutationObserver(watch)
    const map = document.querySelector('.mapwrap')
    if (map) mo.observe(map, { childList: true })
    return () => {
      ro.disconnect()
      mo.disconnect()
      cancelAnimationFrame(raf)
    }
  }, [on])
  return place
}

export function Tutorial({ situation: s, decisions, tracks, now, onMenu, formChecks = null, placeOf, onFit }: Props) {
  const on = !!s?.modes?.tutorial
  const runKey = on && s?.run?.id ? `murmur.tutorial.${s.run.id}` : null
  const nowMs = now()
  const f = useMemo(
    () => tutorialFacts(tracks?.tracks ?? [], decisions?.inbox.review, decisions?.inbox.acts, decisions?.intents ?? [], nowMs, decisions?.inbox.pending, formChecks, tracks?.weapons ?? []),
    // the bridge clock moves every render; the facts that read it (the impact) are re-read each second
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tracks, decisions, formChecks, Math.floor(nowMs / 1000)],
  )
  // the form blocked on the target's identity at some point on this console: say so when it clears
  const [sawIdentityBlock, setSawIdentityBlock] = useState(false)
  if (f.identity && !f.identity.passed && !sawIdentityBlock) setSawIdentityBlock(true)
  const [saved, setSaved] = useState<Saved | null>(null)
  // a step reached with Back does not move on by itself (the person is re-reading it)
  const [cameBack, setCameBack] = useState(false)
  // when this console showed the step now on the card (this browser's clock, ms)
  const [shownAt, setShownAt] = useState<{ idx: number; at: number } | null>(null)
  // the walk's place, once this run is known: what this browser kept, else where the bridge says
  if (runKey && saved == null) {
    setSaved(load(runKey) ?? { idx: resumeAt(f), hidden: false, closed: false })
  }
  useEffect(() => {
    if (saved) save(runKey, saved)
  }, [runKey, saved])
  const idx = Math.min(saved?.idx ?? 0, STEPS.length - 1)
  const step = STEPS[idx]
  const done = stepDone(step.id, f)
  // the step's scene kept in view (tutorial.ts stepScene): framed once as the step comes up, while
  // the card shows; a scene not on the map yet (the boat not yet seen) is framed when it is
  const framed = useRef<number | null>(null)
  const boatAt = f.boat?.position ?? null
  useEffect(() => {
    if (!on || !saved || saved.closed || saved.hidden || framed.current === idx) return
    const scene = stepScene(step.id)
    const pts = [...(scene.boat && boatAt ? [boatAt] : []), ...scene.ours.flatMap((c) => {
      const p = placeOf?.(c)
      return p ? [p] : []
    })]
    if (!pts.length) return
    framed.current = idx
    onFit?.(pts)
  })
  if (saved && shownAt?.idx !== idx) setShownAt({ idx, at: Date.now() })
  const elapsed = s?.clock?.elapsed_s ?? null
  // back on a step that is not done (a nomination to make again): it moves on by itself again
  useEffect(() => {
    if (cameBack && !done) setCameBack(false)
  }, [cameBack, done])
  // a waiting step the bridge shows done: a moment with its tick, then the next
  useEffect(() => {
    if (!saved || step.kind !== 'wait' || !done || cameBack || shownAt?.idx !== idx) return
    const wait = Math.max(ADVANCE_MS, (step.minShowS ?? 0) * 1000 - (Date.now() - shownAt.at))
    const id = window.setTimeout(() => setSaved((v) => (v && v.idx === idx ? { ...v, idx: idx + 1, doneAt: step.id === 'assess' ? (elapsed ?? undefined) : v.doneAt } : v)), wait)
    return () => window.clearTimeout(id)
  }, [saved, step, done, cameBack, idx, elapsed, shownAt])
  // above the weather bar, never under it (bd murmur-iobd.11)
  const place = useCoachPlace(on && !!saved && !saved.closed)
  if (!on || !saved || saved.closed) return null
  const go = (to: number, back: boolean) => {
    setCameBack(back)
    setSaved({ ...saved, idx: Math.max(0, Math.min(STEPS.length - 1, to)), doneAt: step.id === 'assess' && !back ? (saved.doneAt ?? elapsed ?? undefined) : saved.doneAt })
  }
  if (saved.hidden)
    return (
      <button className="tut-chip" style={place ? { bottom: place.bottom } : undefined} onClick={() => setSaved({ ...saved, hidden: false })} title="Show the tutorial's coach card">
        TUTORIAL <b>{idx + 1}</b>/{STEPS.length}
      </button>
    )
  const v = stepView(step.id, f, sawIdentityBlock)
  const last = step.id === 'done'
  return (
    <>
      <Ring targets={v.targets} />
      <aside className="tut" aria-label="Tutorial" aria-live="polite" style={place ? { bottom: place.bottom, maxHeight: place.maxHeight } : undefined}>
        <header className="tut-top">
          <span className="tut-k">Tutorial</span>
          <span className="tut-n">
            step {idx + 1} of {STEPS.length}
          </span>
          <button className="ghost close" onClick={() => setSaved({ ...saved, hidden: true })} aria-label="Hide the coach card" title="Hide (the chip at the bottom left brings it back)">
            –
          </button>
        </header>
        <h2 className="tut-title">
          {v.title}
          {step.kind === 'wait' && done && (
            <span className="tut-tick" role="status">
              done
            </span>
          )}
        </h2>
        <div className="tut-body">
          {last ? (
            <>
              <p>
                You took one engagement through the chain{saved.doneAt != null ? `, from the start to the assessment in ${mmss(saved.doneAt)}` : ''}: an observed act, the
                engine&apos;s declaration, your confirmation, your nomination, two signatures that were not yours, the release and the assessment.
              </p>
              <p>
                Every one of them is recorded, with what the engine checked at each, and can be re-checked from the record after the run.
              </p>
              <p>What next:</p>
              <ul>
                <li>
                  <b>Harbor watch</b>, with you live: an AI watch officer proposes, you decide.
                </li>
                <li>
                  <b>Hostile act to engagement</b> on autoplay: the same chain, an AI nominating.
                </li>
                <li>
                  <b>The open world</b>: 22 vehicles at the Gate, nothing scripted.
                </li>
              </ul>
            </>
          ) : (
            v.body
          )}
          {v.note && <p className="tut-note">{v.note}</p>}
        </div>
        <footer className="tut-go">
          <button className="ghost" disabled={idx === 0} onClick={() => go(idx - 1, true)}>
            Back
          </button>
          {last ? (
            <>
              {onMenu && (
                <button className="primary" onClick={onMenu}>
                  Back to the menu
                </button>
              )}
              <button className="ghost" onClick={() => setSaved({ ...saved, closed: true })}>
                Close the tutorial
              </button>
            </>
          ) : (
            <button className="primary" disabled={step.kind === 'wait' && !done} onClick={() => go(idx + 1, false)} title={step.kind === 'wait' && !done ? 'Moves on by itself when it happens' : undefined}>
              Next
            </button>
          )}
          {step.kind === 'wait' && !done && <span className="tut-wait">moves on by itself when it happens</span>}
        </footer>
      </aside>
    </>
  )
}
