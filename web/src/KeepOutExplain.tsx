import { fmtWorldZ } from './scenarioClock'
import { courseText, fmtDist, KEEP_OUT_KIND_LABEL, type BehavingInfo, type EffectWindow, type KeepOutExplanation, type KeepOutItem, type KeepOutLost, type MoveWithin } from './engage'

/** Why a keep-out blocks the chosen option of the nomination form, and what would clear it
 *  (murmur-12e): the bridge's read-only explanation, from the functions the checks read on
 *  the same snapshot. It says, never decides: the checks decide, and nothing here relaxes a
 *  keep-out. The map draws the same regions (engageLayers). */

interface Props {
  ex: KeepOutExplanation
  /** the platform's name as the form shows it */
  name: string
  now: () => number
  /** rules 16: the lost contacts the operator accepts with the nomination, by track id, and a
   *  toggle (absent: the explanation is read-only here) */
  acceptLost?: Record<string, KeepOutLost>
  onAcceptLost?: (l: KeepOutLost, on: boolean) => void
  /** rules 19: the live tracks the operator accepts as behaving now with the nomination, by
   *  track id, and a toggle (absent: read-only) */
  acceptBehaving?: Record<string, BehavingInfo>
  onAcceptBehaving?: (b: BehavingInfo, on: boolean) => void
}

/** "56 s", "3 min 36 s". */
function span(ms: number) {
  const s = Math.max(0, Math.round(ms / 1000))
  return s < 120 ? `${s} s` : `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, '0')} s`
}

/** A window in words: distances when the target holds, effect times otherwise. */
function windowText(w: EffectWindow, t: number) {
  const times = `effect ${span(w.from_t - t)} to ${span(w.until_t - t)} from now`
  return w.from_m != null && w.until_m != null ? `${fmtDist(w.from_m)} to ${fmtDist(w.until_m)} out (${times})` : times
}

function clearsText(i: KeepOutItem, ex: KeepOutExplanation) {
  if (i.fits_until_t != null) return `Its reach fits an effect up to ${span(i.fits_until_t - ex.t)} from now (a closer platform), on the picture now.`
  if (i.clear_windows.length) return `Clear for a later effect, on the picture now: ${i.clear_windows.map((w) => windowText(w, ex.t)).join('; or ')}.`
  return 'No effect time a nomination allows, on a 0.25 s grid, clears it, on the picture now.'
}

/** Rules 16: accept a lost contact for this engagement (an engagement decision: logged under
 *  the operator's session with the nomination; the bridge refuses it without engagement
 *  authority). It covers this loss only. */
function AcceptLost({ l, on, onToggle }: { l: KeepOutLost; on: boolean; onToggle: (l: KeepOutLost, on: boolean) => void }) {
  return (
    <label className="ko-accept">
      <input type="checkbox" checked={on} onChange={(e) => onToggle(l, e.target.checked)} />
      Accept {l.track} as lost, out of view since {fmtWorldZ(l.since)}
    </label>
  )
}

/** Rules 19: a live blocker as the operator would accept it "as behaving now" for this
 *  engagement: the engine's own words on what that assumes and whether the keep-out would then
 *  hold for it, and (when it may be accepted) the control. It covers this engagement only and
 *  lapses on its own when the track departs from that course or goes out of view. */
function AcceptBehaving({ b, on, onToggle }: { b: BehavingInfo; on: boolean; onToggle?: (b: BehavingInfo, on: boolean) => void }) {
  return (
    <div className={`ko-behaving ${b.acceptable ? '' : 'not'}`}>
      {b.acceptable && onToggle && (
        <label className="ko-accept">
          <input type="checkbox" checked={on} onChange={(e) => onToggle(b, e.target.checked)} />
          <span>
            <b>Accept as behaving now</b>: {b.track}, {courseText(b.velocity_mps, b.velocity_sigma_mps)}, its estimate at {fmtWorldZ(b.estimate_t)}
          </span>
        </label>
      )}
      <div className="ko-behaving-reason">{b.reason}</div>
      {b.acceptable && !onToggle && <div className="ko-behaving-reason">Only an operator with engagement authority may accept it as behaving now.</div>}
      {b.acceptable && !b.clears && (
        <div className="ko-behaving-short">
          Accepting it would not clear the keep-out{b.short_m != null && b.short_m > 0 ? `: it would still be short ${fmtDist(b.short_m)}` : ''}.
        </div>
      )}
    </div>
  )
}

function Item({
  i,
  acceptLost,
  onAcceptLost,
  acceptBehaving,
  onAcceptBehaving,
}: { i: KeepOutItem } & Pick<Props, 'acceptLost' | 'onAcceptLost' | 'acceptBehaving' | 'onAcceptBehaving'>) {
  const kind = i.subject === 'lost_contact' ? `lost: ${i.region ? KEEP_OUT_KIND_LABEL[i.region.kind] : 'its reach'}` : i.region ? KEEP_OUT_KIND_LABEL[i.region.kind] : 'cannot be placed'
  return (
    <div className={`ko-item ${i.blocks ? 'blocks' : 'near'}`}>
      <div className="ko-head">
        <b>{i.label}</b>
        <span className="ko-kind">{kind}</span>
        {i.short_m != null && (
          <span className="ko-num">{i.blocks ? `short ${fmtDist(i.short_m)}` : `clear by ${fmtDist(-i.short_m)}`}</span>
        )}
      </div>
      <div className="ko-reason">{i.reason}</div>
      {i.blocks && i.lost && onAcceptLost && <AcceptLost l={i.lost} on={!!acceptLost?.[i.lost.track]} onToggle={onAcceptLost} />}
      {i.blocks && i.behaving && <AcceptBehaving b={i.behaving} on={!!acceptBehaving?.[i.behaving.track]} onToggle={onAcceptBehaving} />}
    </div>
  )
}

/** What would clear each blocker, in one list: its own ways (a closer platform, a later window,
 *  waiting, or not by waiting), then the platform's placement on the picture now, then whether
 *  waiting helps at all. */
function clearing(ex: KeepOutExplanation, name: string, many: boolean): string[] {
  const out: string[] = []
  const mw = ex.move_within
  for (const i of ex.items.filter((x) => x.blocks)) {
    const who = many ? `${i.label}: ` : ''
    if (mw) out.push(`${who}${clearsText(i, ex)}`)
    if (i.wait) out.push(`${who}${i.wait.why} (at ${fmtWorldZ(i.wait.nominate_t)})`)
    if (i.not_by_waiting) out.push(`${who}${i.not_by_waiting}`)
    if (i.other) out.push(`${who}${i.other}`)
  }
  if (mw) out.push(...placement(mw, name, ex.t))
  out.push(ex.wait ? `Wait: ${ex.wait.why} (at ${fmtWorldZ(ex.wait.nominate_t)})` : ex.wait_none ? `Wait: ${ex.wait_none}` : '')
  return out.filter(Boolean)
}

/** The platform's placement on the picture now: closer (within), farther or later windows. */
function placement(mw: MoveWithin, name: string, t: number): string[] {
  if (!mw.possible) return [mw.why]
  const later = mw.windows.filter((w) => w.from_t > mw.earliest_t)
  const out: string[] = []
  if (mw.within_m != null && mw.effect_by_t != null) {
    out.push(`${name} within ${fmtDist(mw.within_m)} of ${mw.holding ? 'the target' : 'the marked point'}${mw.now_m != null ? ` (now ${fmtDist(mw.now_m)})` : ''}: every keep-out holds for an effect up to ${span(mw.effect_by_t - t)} from now.`)
  } else {
    out.push('No closer position clears it.')
  }
  if (later.length > 0) out.push(`${mw.holding ? `Farther: ${name} ` : 'Later: '}${later.map((w) => windowText(w, t)).join('; or ')}: every keep-out holds${mw.holding ? '' : ' (with the target moving, the platform\u2019s distance alone does not set the effect time)'}.`)
  return out
}

export function KeepOutExplain({ ex, name, now, acceptLost, onAcceptLost, acceptBehaving, onAcceptBehaving }: Props) {
  if (!ex.items.length) return null
  const blocking = ex.items.filter((i) => i.blocks)
  const near = ex.items.filter((i) => !i.blocks)
  const ways = ex.blocked ? clearing(ex, name, blocking.length > 1) : []
  const heights = ex.items.filter((i) => i.altitude)
  return (
    <section className="eg-sec ko" aria-label="Why the keep-out blocks">
      <span className="ix-label">
        Keep-out
        <em>{ex.blocked ? `blocked by ${ex.blocking}` : `holds; ${near.length} near`}</em>
      </span>
      {blocking.map((i) => (
        <Item key={`${i.subject}${i.id}`} i={i} acceptLost={acceptLost} onAcceptLost={onAcceptLost} acceptBehaving={acceptBehaving} onAcceptBehaving={onAcceptBehaving} />
      ))}
      {ways.length > 0 && (
        <div className="ko-hints">
          <span className="ix-label">What would clear it</span>
          <ul className="ko-clears">
            {ways.map((w, k) => (
              <li key={k}>{w}</li>
            ))}
          </ul>
        </div>
      )}
      {near.length > 0 && (
        <details className="ko-near" open={!ex.blocked}>
          <summary>Nearly blocking ({near.length})</summary>
          {near.map((i) => (
            <Item key={`${i.subject}${i.id}`} i={i} />
          ))}
        </details>
      )}
      {ex.omitted > 0 && <p className="ko-foot">{ex.omitted} more not listed.</p>}
      <details className="ko-near">
        <summary>Evidence</summary>
        <p className="ko-lead">
          The effect, {Math.round(ex.effect_radius_m)} m at {span(ex.effect_t - now())} from now, needs {fmtDist(ex.clearance_m)} clear of every track and vehicle of ours at the effect time.
        </p>
        {heights.map((i) => (
          <p key={`${i.subject}${i.id}`} className="ko-alt">
            {i.label}: {i.altitude}
          </p>
        ))}
        <p className="ko-foot">{ex.assumptions}</p>
      </details>
    </section>
  )
}
