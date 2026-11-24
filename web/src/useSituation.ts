import { useState } from 'react'
import type { SituationStatus } from './SituationPanel'
import { usePoll } from './space'
import { type ClockAnchor, situationTime, type SituationTime } from './situationText'

interface Envelope {
  situation: SituationStatus | null
  received_t: number | null
  /** how many statuses the runner has posted (a new one every 2 s while it runs) */
  posts?: number
}

export interface Situation {
  /** the runner's last status; null where no situation runs, undefined before the bridge answers */
  s: SituationStatus | null | undefined
  /** its clock now (situationText.ts); null with no situation */
  time: SituationTime | null
}

/** A situation runner's status (`make situation`, `sim/situations/`; GET /api/situation), asked
 *  once for the whole console: the top bar's clock, the Situation panel and the tutorial's card.
 *  The clock runs on from when this console received each new status (`now`: the bridge's clock as
 *  this console keeps it, ms), never from the runner's host clock against this one's. Read at each
 *  render of its caller (App renders twice a second with the bridge's clock). Display only: never
 *  a decision input. */
export function useSituation(now: () => number): Situation {
  const { data } = usePoll<Envelope>('/api/situation', 2000)
  const s = data == null ? undefined : (data.situation ?? null)
  const posts = data?.posts ?? 0
  const nowS = now() / 1000
  // anchored when a new post comes (state adjusted while rendering when a prop it follows changes)
  const [anchor, setAnchor] = useState<(ClockAnchor & { posts: number }) | null>(null)
  if (s && (anchor == null || anchor.posts !== posts)) setAnchor({ posts, elapsed: s.clock.elapsed_s, at: nowS })
  const time = s ? situationTime(s.clock, s.phase, anchor && anchor.posts === posts ? anchor : null, nowS) : null
  return { s, time }
}
