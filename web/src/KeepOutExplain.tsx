import { fmtZ } from './decisions'
import { fmtDist, KEEP_OUT_KIND_LABEL, type EffectWindow, type KeepOutExplanation, type KeepOutItem, type MoveWithin } from './engage'

/** Why a keep-out blocks the chosen option of the nomination form, and what would clear it
 *  (murmur-12e): the bridge's read-only explanation, from the functions the checks read on
 *  the same snapshot. It says, never decides: the checks decide, and nothing here relaxes a
 *  keep-out. The map draws the same regions (engageLayers). */

interface Props {
  ex: KeepOutExplanation
  /** the platform's name as the form shows it */
  name: string
  now: () => number
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
  return 'No effect time a nomination allows clears it, on the picture now.'
}

function Item({ i, ex }: { i: KeepOutItem; ex: KeepOutExplanation }) {
  const kind = i.region ? KEEP_OUT_KIND_LABEL[i.region.kind] : 'cannot be placed'
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

export function KeepOutExplain({ ex, name, now }: Props) {
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
        <Item key={`${i.subject}${i.id}`} i={i} ex={ex} />
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
