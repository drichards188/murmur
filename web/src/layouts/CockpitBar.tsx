// The cockpit layout's top bar (Cockpit.tsx; bd epic murmur-iobd, direction 1): one line, its pieces
// grouped by job with space between the groups and no rules between them. On the left what is
// happening: the menu and the brand; the run (who decides, the scenario, the newest thing to watch;
// a click opens the Situation panel); the glance (ours, ours in trouble, the contacts and which of
// them matter, each problem count a link to what it counts). On the right where to act and who acts:
// the Inbox, the one primary button; the other drawers; Views; the operator; the time, the quietest.
// Short of room (1366 px and a situation) the run's newest beat truncates first, then the glance
// folds into its problem counts, then a beat too short to read hides (the Situation panel has it),
// then the run's title truncates: the bar never takes a second row (useBarFold).
//
// Display only: every control is the console's own part, doing what it always did; the two links
// this adds (a hostile or suspect count) only show a track, as a click on its row does.
import { useLayoutEffect, useRef, useState, type RefObject } from 'react'
import { glanceWords, nextOf, runStanding, type GlancePiece } from '../topbar'
import type { BarFigures, ConsoleParts } from './parts'

/** The narrowest the run's beat is kept, truncated, before the glance folds to give it room (px). */
const MIN_BEAT = 72
/** Shorter than this, with the glance folded, the beat is hidden rather than shown as a stub (px)... */
const TINY_BEAT = 56
/** ...until there is room for this much of it again (px). */
const SHOW_BEAT = 112
/** Room to spare before a fold comes undone (px): it does not flick back and forth. */
const SPARE = 8

const FOLD = ['', 'glance', 'beat'] as const

export function CockpitBar({ parts }: { parts: ConsoleParts }) {
  const { bar, facts, act } = parts
  const ref = useRef<HTMLElement>(null)
  const level = useBarFold(ref, bar.run ? 'run' : '')
  const folded = level > 0
  const f = bar.figures
  const standing = f?.run ? runStanding(f.run) : null
  return (
    <header ref={ref} className="topbar cp-bar" data-fold={FOLD[level] || undefined}>
      <div className="cp-home">
        {bar.menu}
        {bar.brand}
      </div>
      {bar.run && (
        <div className="cp-run">
          {standing && (
            <span className={`cp-chip ${standing.tone}`} title={standing.title}>
              {standing.label}
            </span>
          )}
          {bar.run}
        </div>
      )}
      <div className={`cp-status${facts.live ? '' : ' stale'}`}>
        {bar.connection}
        {f ? (
          <>
            <Glance f={f} folded={folded} onTrack={act.showTrack} />
            {/* the whole line, unseen, for the width it would take (useGlanceFold) */}
            <span className="cp-measure-box" aria-hidden="true">
              <Glance f={f} folded={false} measure />
            </span>
          </>
        ) : (
          bar.glance
        )}
      </div>
      <nav className="cp-work" aria-label="Drawers over the map's right edge, one at a time">
        <span className="cp-inbox">{bar.inbox}</span>
        {bar.queue}
        {bar.groups}
        {bar.sitrep}
        {bar.officer}
      </nav>
      <div className="cp-ref">{bar.views}</div>
      <div className="cp-who">
        {bar.operator}
        {bar.clock}
      </div>
    </header>
  )
}

/** The glance line (topbar.ts `glanceWords`): "105 vehicles · 3 of ours in trouble · 2 contacts,
 *  1 hostile", or folded "3 of ours in trouble · 1 hostile". Each problem count is a link: the next
 *  of ours in trouble in the Fleet list, the next hostile or suspect contact on the map and in the
 *  Tracks list. `measure`: the same words as plain text, for the width alone. */
function Glance({ f, folded, onTrack, measure = false }: { f: BarFigures; folded: boolean; onTrack?: (id: string) => void; measure?: boolean }) {
  // which contact a click on a count showed last, so the next click shows the next
  const shown = useRef<{ hostile: string | null; suspect: string | null }>({ hostile: null, suspect: null })
  const showNext = (kind: 'hostile' | 'suspect') => {
    const id = nextOf(f[kind], shown.current[kind])
    shown.current[kind] = id
    if (id != null) onTrack?.(id)
  }
  const pieces = glanceWords(f.glance, f.trouble.n, folded)
  return (
    <span className={`cp-glance${measure ? ' cp-measure' : ''}`}>
      {pieces.map((p, k) => (
        <Piece key={k} p={p} f={f} measure={measure} onNext={showNext} />
      ))}
    </span>
  )
}

function Piece({ p, f, measure, onNext }: { p: GlancePiece; f: BarFigures; measure: boolean; onNext: (kind: 'hostile' | 'suspect') => void }) {
  if (p.kind === 'sep') return <span className="cp-sep">{p.text}</span>
  const words = (
    <>
      <b>{p.n}</b> {p.text}
    </>
  )
  if (p.kind === 'ours' || p.kind === 'contacts') {
    const title = p.kind === 'ours' ? `${p.n} of ours in the fleet` : `${p.n} contact${p.n === 1 ? '' : 's'} our sensors hold`
    return (
      <span className="cp-total" title={measure ? undefined : title}>
        {words}
      </span>
    )
  }
  const tone = p.kind === 'trouble' ? (f.trouble.tone ?? '') : p.kind === 'hostile' ? 'bad' : 'warn'
  if (measure) return <span className={`cp-link ${tone}`}>{words}</span>
  if (p.kind === 'trouble')
    return (
      <button type="button" className={`cp-link ${tone}`} onClick={f.showTrouble} title={f.trouble.says} aria-label={f.trouble.says}>
        {words}
      </button>
    )
  const kind = p.kind
  const says =
    p.n === 1
      ? `1 ${kind} contact. A click shows it on the map and in the Tracks list.`
      : `${p.n} ${kind} contacts. A click shows the next one on the map and in the Tracks list.`
  return (
    <button type="button" className={`cp-link ${tone}`} onClick={() => onNext(kind)} title={says} aria-label={says}>
      {words}
    </button>
  )
}

/** How far the bar has folded to keep to one row: 0 whole; 1 the glance folded into its problem
 *  counts; 2 that, and the run's beat hidden too, being too short to read (it is one click away, in
 *  the Situation panel). Measured rather than set by the window's width, so it holds whatever the
 *  bar carries (a situation's run or none, a long operator name, an Inbox with its time left).
 *  0 to 1 once the run's beat is cut below MIN_BEAT, the run's title is cut, or the bar runs out of
 *  room; 1 to 2 once the beat, so folded, still shows less than TINY_BEAT; back to 1 once there is
 *  room for SHOW_BEAT of it (or all of a shorter one); back to 0 once the room the glance gave up
 *  is free again (the gap before the drawer buttons, and what the beat has above MIN_BEAT). The
 *  whole line is measured from its unseen copy (`.cp-measure`), so a folded glance knows how wide
 *  it would be. The level stands on the bar (`data-fold`), where each pass reads it. `runKey`
 *  changes when the run comes or goes, to watch its pieces. */
function useBarFold(bar: RefObject<HTMLElement | null>, runKey: string): 0 | 1 | 2 {
  const [level, setLevel] = useState<0 | 1 | 2>(0)
  useLayoutEffect(() => {
    const el = bar.current
    if (!el) return
    let frame = 0
    const q = (s: string) => el.querySelector<HTMLElement>(s)
    const cut = (e: HTMLElement | null) => !!e && e.scrollWidth > e.clientWidth + 1
    const decide = () => {
      frame = 0
      const glance = q('.cp-glance:not(.cp-measure)')
      const whole = q('.cp-measure')
      const status = q('.cp-status')
      const work = q('.cp-work')
      if (!glance || !whole || !status || !work) return
      const now = el.dataset.fold === 'beat' ? 2 : el.dataset.fold === 'glance' ? 1 : 0
      // the run's pieces, as SituationPanel's chip draws them (its title, its newest beat)
      const beat = q('.cp-run .sit-chip-next')
      const title = q('.cp-run .sit-chip b')
      const beatW = beat?.clientWidth ?? 0
      const box = el.getBoundingClientRect()
      const cs = getComputedStyle(el)
      const last = el.lastElementChild?.getBoundingClientRect()
      const over = !!last && last.right > box.right - parseFloat(cs.paddingRight) + 1
      const squeezed = over || cut(title) || (cut(beat) && beatW < MIN_BEAT)
      // the room free before the drawer buttons
      const slack = work.getBoundingClientRect().left - status.getBoundingClientRect().right - (parseFloat(cs.columnGap) || 0)
      if (now === 0) {
        if (squeezed) setLevel(1)
      } else if (now === 1) {
        if (cut(beat) && beatW < TINY_BEAT) setLevel(2)
        else if (!squeezed && slack + Math.max(0, beatW - MIN_BEAT) >= whole.offsetWidth - glance.offsetWidth + SPARE) setLevel(0)
      } else if (!beat || slack >= Math.min(beat.scrollWidth, SHOW_BEAT) + SPARE) setLevel(1)
    }
    // after the layout settles, once a frame at most (a resize observer's own turn never sets state)
    const soon = () => {
      if (!frame) frame = requestAnimationFrame(decide)
    }
    const ro = new ResizeObserver(soon)
    ro.observe(el)
    for (const c of el.querySelectorAll<HTMLElement>(':scope > *, .cp-glance, .cp-run .sit-chip-next, .cp-run .sit-chip b')) ro.observe(c)
    soon()
    return () => {
      ro.disconnect()
      if (frame) cancelAnimationFrame(frame)
    }
  }, [bar, runKey])
  return level
}
