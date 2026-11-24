// The map's labels in a person's words (ledger 1 of the console cutdown, bd murmur-iobd): what a
// label says, never a raw id where a name exists, nothing ticking that no decision reads. Display
// only. Pure, no imports: mapWords.check.ts runs it under node.

/** "47 s", "4 min", "2 h 05 min": how long, in words for a label. */
export function spanWords(s: number): string {
  const n = Math.max(0, Math.round(s))
  if (n < 90) return `${n} s`
  const m = Math.floor(n / 60)
  return m < 120 ? `${m} min` : `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')} min`
}

// ---- our silent vehicles' estimates ----------------------------------------------------------

/** What a silent vehicle is probably doing, by the bridge's motion model (`Estimate.basis`). */
export const PROBABLY: Record<string, string> = {
  orbit: 'on its orbit',
  patrol: 'on its patrol route',
  hold: 'holding',
  rtl: 'returning home',
  velocity: 'on its last course',
  stationary: 'where it was',
  drift: 'drifting with the water',
}

/** What its lost-link failsafe does once its timer has fired (`FailsafeBranch.action`). */
export const FAILSAFE_DOES: Record<string, string> = {
  rtl: 'returning (lost-link RTL)',
  surface: 'surfacing (lost-link failsafe)',
  disarm: 'disarmed (lost-link failsafe)',
}

/** A silent vehicle's estimate label. The leading hypothesis says what it is probably doing and how
 *  long it has been silent ("probably returning (lost-link RTL); silent 47 s"); the task model,
 *  once the failsafe leads, what it would be doing had it kept its task. */
export function estimateWords(h: { failsafe: string } | { basis: string; second: boolean }, silentS: number): string {
  if ('failsafe' in h) return `probably ${FAILSAFE_DOES[h.failsafe] ?? h.failsafe}; silent ${spanWords(silentS)}`
  const doing = PROBABLY[h.basis] ?? h.basis
  return h.second ? `if it kept its task: ${doing}` : `probably ${doing}; silent ${spanWords(silentS)}`
}

// ---- contacts --------------------------------------------------------------------------------

/** A track's one label: its kind in words, qualified when our sensors hold less than its kind
 *  ("small fast boat", "small air contact (detected)", "vessel (recognised)"), "unclassified"
 *  without one; with its height or depth when given (in focus only: the caller decides). */
export function trackWords(kindWords: string | null, level: string | null | undefined, height?: string | null): string {
  const kind = kindWords ?? 'unclassified'
  const qualified = kindWords != null && level === 'detect' ? `${kind} (detected)` : kindWords != null && level === 'recognise' ? `${kind} (recognised)` : kind
  return height ? `${qualified} · ${height}` : qualified
}

/** A lost contact's label: "lost 4 min" (its possible area's ring says the rest). */
export function lostWords(outOfViewS: number): string {
  return `lost ${spanWords(outOfViewS)}`
}

// ---- orders ----------------------------------------------------------------------------------

/** An order held in the queue: "queued orbit: link down, 2 min left". */
export function queuedWords(kind: string, why: string, leftS: number | null): string {
  return `queued ${kind}: ${why}${leftS == null ? '' : `, ${spanWords(leftS)} left`}`
}

/** An order carried over a restart, waiting for its vehicle: "resumed patrol: waiting for the
 *  vehicle". */
export function resumedWords(kind: string): string {
  return `resumed ${kind}: waiting for the vehicle`
}

/** An order whose start went out unconfirmed and is no longer in force: a person decides.
 *  "MAY BE RUNNING orbit: its start went out unconfirmed (voided)". */
export function mayBeRunningWords(kind: string, state: string): string {
  return `MAY BE RUNNING ${kind}: its start went out unconfirmed (${state.replace(/_/g, ' ')})`
}

/** A space collection in force: "collect SAT-2 10:00Z", "collect SAT-2 10:00Z (area)". */
export function collectWords(satellite: string, at: string, scene: boolean): string {
  return `collect ${satellite} ${at}${scene ? '' : ' (area)'}`
}

// ---- engagements -----------------------------------------------------------------------------

export interface EngageWords {
  /** the platform's callsign; absent for the nomination form's draft */
  platform?: string
  track?: string
  phase: 'draft' | 'pending' | 'running' | 'effected' | 'ended'
  /** ms to the planned effect (negative once it is due) */
  effectInMs: number
  /** the engine has released (the effect is recorded) */
  released: boolean
  /** released and landed (a munition still in flight has not struck yet) */
  struck: boolean
  assessment?: 'persists' | 'ceased' | 'unknown'
  /** the intent's state, for one that ended */
  state?: string
}

/** Who on what: "REAPER 1 on T-0008", "nomination on T-0008", "the nomination". */
export function engageWho(w: Pick<EngageWords, 'platform' | 'track' | 'phase'>): string {
  const who = w.platform ?? (w.phase === 'draft' ? 'nomination' : 'engagement')
  return w.track ? `${who} on ${w.track}` : w.phase === 'draft' ? 'the nomination' : who
}

/** An engagement's label in full: "REAPER 1 on T-0008: effect in 44 s", "...: released",
 *  "T-0008 struck: assessing", "T-0008 still detected", "T-0008 no longer detected",
 *  "T-0008: outcome unknown", "REAPER 1 on T-0008: aborted". */
export function engageWords(w: EngageWords): string {
  const who = engageWho(w)
  const target = w.track ?? 'the target'
  if (w.assessment === 'persists') return `${target} still detected`
  if (w.assessment === 'ceased') return `${target} no longer detected`
  if (w.assessment === 'unknown') return `${target}: outcome unknown`
  if (w.struck) return `${target} struck: assessing`
  if (w.released) return `${who}: released`
  if (w.phase === 'ended') return w.state ? `${who}: ${w.state.replace(/_/g, ' ')}` : who
  const s = Math.round(w.effectInMs / 1000)
  return s > 0 ? `${who}: effect in ${spanWords(s)}` : `${who}: effect due`
}
