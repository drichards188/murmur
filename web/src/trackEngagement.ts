// What a track's card says of an order on that track (TracksPanel, Declaration.tsx): while one is in
// play, one line that opens its card in the Inbox and no Nominate button; once it ended, its last
// outcome and Nominate back as an ordinary button. Display only: it reads the intents the bridge
// already sends and decides nothing. Pure: trackEngagement.check.ts.

/** The states in which an engagement is in play (focus.ts reads the same set). */
const IN_PLAY = ['pending_approval', 'approved', 'dispatched', 'effected']

export interface TrackEngagement {
  id: number
  inPlay: boolean
  /** "Engagement i2 under way: REAPER 1 en route", "Engagement i2 assessed: persists" */
  line: string
}

interface IntentLike {
  id: number
  kind: string
  state: string
  uids: readonly string[]
  /** an engagement's nomination names its `track` */
  params: Record<string, unknown>
  engage?: { track?: string } | null
  engagement?: { snapshot?: { track_id?: string } | null; assessment?: { result: string } | null } | null
}

/** The track it is on: the approval's snapshot, the bridge's view, or the nomination's own. */
function targetOf(i: IntentLike): string | null {
  const p = i.params.track
  return i.engagement?.snapshot?.track_id ?? i.engage?.track ?? (typeof p === 'string' ? p : null)
}

const IN_PLAY_WORDS: Record<string, (who: string) => string> = {
  pending_approval: (who) => `waits for signatures (${who})`,
  approved: (who) => `approved: ${who} sets off`,
  dispatched: (who) => `under way: ${who} en route`,
  effected: (who) => `under way: released by ${who}, being assessed`,
}

const ENDED_WORDS: Record<string, string> = {
  done: 'ended',
  aborted: 'aborted',
  voided: 'voided',
  rejected: 'rejected',
  expired: 'expired unsigned',
  failed: 'failed',
}

/** The one on `track` the card speaks of: the newest in play, else the newest that ended
 *  (`intents` newest first, as the bridge sends them); null when there is none. */
export function trackEngagement(intents: readonly IntentLike[] | null | undefined, track: string, nameOf: (uid: string) => string): TrackEngagement | null {
  const on = (intents ?? []).filter((i) => i.kind === 'engage' && targetOf(i) === track)
  const who = (i: IntentLike) => (i.uids.length ? i.uids.map(nameOf).join(', ') : 'its platform')
  const live = on.find((i) => IN_PLAY.includes(i.state))
  if (live) {
    const words = IN_PLAY_WORDS[live.state] ?? ((w: string) => `in play (${w})`)
    return { id: live.id, inPlay: true, line: `Engagement i${live.id} ${words(who(live))}` }
  }
  const last = on.find((i) => ENDED_WORDS[i.state] != null)
  if (!last) return null
  const a = last.engagement?.assessment
  return { id: last.id, inPlay: false, line: a ? `Engagement i${last.id} assessed: ${a.result}` : `Engagement i${last.id} ${ENDED_WORDS[last.state]}` }
}
