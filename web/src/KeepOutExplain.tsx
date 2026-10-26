import { fmtZ } from './decisions'
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
      Accept {l.track} as lost for this engagement, out of view since its last fix at {fmtZ(l.since)} (engagement authority; logged under your session with the
      nomination; it covers this loss only)
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
            <b>Accept as behaving now</b>: {b.track}, {courseText(b.velocity_mps, b.velocity_sigma_mps)}, its estimate at {fmtZ(b.estimate_t)} (engagement authority;
            logged under your session with the nomination; for this engagement only)
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
  ex,
  acceptLost,
  onAcceptLost,
  acceptBehaving,
  onAcceptBehaving,
}: { i: KeepOutItem; ex: KeepOutExplanation } & Pick<Props, 'acceptLost' | 'onAcceptLost' | 'acceptBehaving' | 'onAcceptBehaving'>) {
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
      {i.altitude && <div className="ko-alt">{i.altitude}</div>}
      {i.blocks && (
        <ul className="ko-clears">
          {ex.move_within && <li>{clearsText(i, ex)}</li>}
          {i.wait && (
            <li>
              {i.wait.why} <span className="ko-at">(at {fmtZ(i.wait.nominate_t)})</span>
            </li>
          )}
          {i.not_by_waiting && <li>{i.not_by_waiting}</li>}
          {i.other && <li>{i.other}</li>}
        </ul>
      )}
      {i.blocks && i.lost && onAcceptLost && <AcceptLost l={i.lost} on={!!acceptLost?.[i.lost.track]} onToggle={onAcceptLost} />}
      {i.blocks && i.behaving && <AcceptBehaving b={i.behaving} on={!!acceptBehaving?.[i.behaving.track]} onToggle={onAcceptBehaving} />}
    </div>
  )
}

/** The platform's hints on the picture now: closer (within), farther or later windows. */
function Placement({ mw, name, t }: { mw: MoveWithin; name: string; t: number }) {
  if (!mw.possible) {
    return (
      <p className="ko-hint">
        <b>Where</b>
        <span>{mw.why}</span>
      </p>
    )
  }
  const later = mw.windows.filter((w) => w.from_t > mw.earliest_t)
  return (
    <>
      {mw.within_m != null && mw.effect_by_t != null && (
        <p className="ko-hint move">
          <b>Closer</b>
          <span>
            {name} within {fmtDist(mw.within_m)} of {mw.holding ? 'the target' : 'the marked point'}
            {mw.now_m != null ? ` (now ${fmtDist(mw.now_m)})` : ''}: every keep-out holds for an effect up to {span(mw.effect_by_t - t)} from now, on the
            picture now.
          </span>
        </p>
      )}
      {mw.within_m == null && (
        <p className="ko-hint">
          <b>Closer</b>
          <span>No closer position clears it, on the picture now.</span>
        </p>
      )}
      {later.length > 0 && (
        <p className="ko-hint move">
          <b>{mw.holding ? 'Farther' : 'Later'}</b>
          <span>
            {mw.holding ? `${name} ` : ''}
            {later.map((w) => windowText(w, t)).join('; or ')}: every keep-out holds, on the picture now
            {mw.holding ? '' : ' (with the target moving, the platform’s distance alone does not set the effect time)'}.
          </span>
        </p>
      )}
    </>
  )
}

export function KeepOutExplain({ ex, name, now, acceptLost, onAcceptLost, acceptBehaving, onAcceptBehaving }: Props) {
  if (!ex.items.length) return null
  const blocking = ex.items.filter((i) => i.blocks)
  const near = ex.items.filter((i) => !i.blocks)
  const mw = ex.move_within
  return (
    <section className="eg-sec ko" aria-label="Why the keep-out blocks">
      <span className="ix-label">
        Keep-out
        <em>{ex.blocked ? `blocked by ${ex.blocking}` : `holds; ${near.length} near`}</em>
      </span>
      <p className="ko-lead">
        The effect, {Math.round(ex.effect_radius_m)} m at {span(ex.effect_t - now())} from now, needs {fmtDist(ex.clearance_m)} clear of every track and
        vehicle of ours at the effect time.
      </p>
      {blocking.map((i) => (
        <Item key={`${i.subject}${i.id}`} i={i} ex={ex} acceptLost={acceptLost} onAcceptLost={onAcceptLost} acceptBehaving={acceptBehaving} onAcceptBehaving={onAcceptBehaving} />
      ))}
      {near.length > 0 && (
        <details className="ko-near" open={!ex.blocked}>
          <summary>Nearly blocking ({near.length})</summary>
          {near.map((i) => (
            <Item key={`${i.subject}${i.id}`} i={i} ex={ex} />
          ))}
        </details>
      )}
      {ex.omitted > 0 && <p className="ko-foot">{ex.omitted} more not listed.</p>}
      {ex.blocked && (
        <div className="ko-hints">
          {mw && <Placement mw={mw} name={name} t={ex.t} />}
          <p className="ko-hint">
            <b>Wait</b>
            {ex.wait ? (
              <span>
                {ex.wait.why} <span className="ko-at">(at {fmtZ(ex.wait.nominate_t)})</span>
              </span>
            ) : (
              <span>{ex.wait_none}</span>
            )}
          </p>
        </div>
      )}
      <p className="ko-foot">{ex.assumptions} Read-only: the checks decide, and nothing here relaxes a keep-out.</p>
    </section>
  )
}
