// What a person alone is holding up: the bridge's `blocking` (crates/murmur-bridge/src/decision/
// blocking.rs), on the inbox, the review and the watch officer's MCP inbox. Each item is a review
// item (a rule declaration nobody confirmed, a flag on an act against ours with nothing declared)
// on which a nomination with an armed platform fails and nothing else: every other check passes.
// Display only: the bridge never logs it and no decision reads it. The console shows such an item
// as urgent wherever it appears (the strip across the top, first in the Inbox's Review, on the
// track's card, beside an officer's note that names it), and never decides it: every way in only
// points to the card where a person decides, with its own evidence and note field.

export interface BlockingPlatform {
  uid: number
  callsign: string | null
}

export interface Blocking {
  /** the review item a person acts on: `D-<n>` or `F-<n>` */
  id: string
  kind: 'declaration' | 'flag'
  track: string
  holds_up: 'nomination'
  /** the armed platforms whose nomination passes every other check, nearest first */
  platforms: BlockingPlatform[]
  /** when the item was raised, bridge seconds */
  raised_t: number
  /** since when it has held up the nomination, bridge seconds */
  since_t: number
  act?: string
  /** what waits for a person, in a line */
  waits_for: string
  /** what a person's act does to it */
  unblocks: string
  who: string
  /** "Holding up: REAPER 1's nomination on T-0008. Every other check passes." */
  text: string
  /** the failing check's own words */
  failing: string
}

/** The items to show, the one waiting longest first, one per review item (an item on several
 *  tracks shows its first). */
export function waitingItems(list: readonly Blocking[] | undefined | null): Blocking[] {
  const seen = new Set<string>()
  return [...(list ?? [])]
    .sort((a, b) => a.since_t - b.since_t || a.id.localeCompare(b.id))
    .filter((b) => (seen.has(b.id) ? false : (seen.add(b.id), true)))
}

/** The item holding something up under review item `id`, if any. */
export function holdingFor(list: readonly Blocking[] | undefined | null, id: string): Blocking | undefined {
  return (list ?? []).find((b) => b.id === id)
}

/** The item holding up a nomination on `track` by its declaration, if any. */
export function holdingOnTrack(list: readonly Blocking[] | undefined | null, track: string): Blocking | undefined {
  return (list ?? []).find((b) => b.track === track && b.kind === 'declaration')
}

/** How long it has waited, "0:41" or "1:02:03", from the bridge clock (ms). */
export function waited(b: Pick<Blocking, 'since_t'>, nowMs: number): string {
  const n = Math.max(0, Math.round(nowMs / 1000 - b.since_t))
  const h = Math.floor(n / 3600)
  const m = Math.floor((n % 3600) / 60)
  const s = String(n % 60).padStart(2, '0')
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${s}` : `${m}:${s}`
}

/** What the person does, in two or three words, for a button or a heading. */
export function actWords(b: Pick<Blocking, 'kind' | 'track'>): string {
  return b.kind === 'declaration' ? `confirm or revoke ${b.track}'s declaration` : `decide on ${b.track}`
}

/** The strip's line: the item, what waits for a person, what it holds up. */
export function stripLine(b: Blocking): string {
  return `${b.id}: ${b.waits_for}. ${b.text}`
}

/** The items a note names that hold something up now: by id as a word in its text ("D-1",
 *  never "D-12" for "D-1"), or among the ids its writer attached (`blocking`: the watch
 *  officer's driver attaches what held something up when its turn ended). */
export function namedBlocking(note: { text: string; blocking?: readonly string[] | null }, list: readonly Blocking[] | undefined | null): Blocking[] {
  const attached = new Set(note.blocking ?? [])
  return waitingItems(list).filter((b) => attached.has(b.id) || new RegExp(`(^|[^A-Za-z0-9-])${b.id}(?![0-9])`).test(note.text))
}

// ---- what waits on a person without holding anything up -----------------------------------------

/** A review item waiting on the person that holds nothing up (or not yet): a rule declaration
 *  awaiting confirmation, an open flag. The calm standing pointer under the top bar
 *  (docs/research/saturation-display.md R4) shows each, pending, not urgent: it only opens the
 *  item's card in the Review, where the person decides. */
export interface Pending {
  /** the review item: `D-<n>` or `F-<n>` */
  id: string
  kind: 'declaration' | 'flag'
  tracks: string[]
  /** when it was raised, bridge seconds */
  t: number
  /** a declaration's affiliation */
  affiliation?: string
  /** a flag's kind (ident.ts FLAG_LABEL words it) */
  flag?: string
}

interface ReviewLike {
  declarations?: readonly { id: string; track: string; t: number; affiliation: string }[]
  flags?: readonly { id: string; tracks: string[]; t: number; flag_kind: string; status: { status: string } | string }[]
}

/** The review items waiting on the person, the oldest first: every rule declaration awaiting
 *  confirmation and every open flag, except those holding up a nomination now (the urgent strip
 *  has them). */
export function pendingItems(review: ReviewLike | null | undefined, blocking: readonly Pick<Blocking, 'id'>[] | null | undefined): Pending[] {
  const urgent = new Set((blocking ?? []).map((b) => b.id))
  const out: Pending[] = []
  for (const d of review?.declarations ?? []) if (!urgent.has(d.id)) out.push({ id: d.id, kind: 'declaration', tracks: [d.track], t: d.t, affiliation: d.affiliation })
  for (const f of review?.flags ?? []) {
    const status = typeof f.status === 'string' ? f.status : f.status?.status
    if (status !== 'open' || urgent.has(f.id)) continue
    out.push({ id: f.id, kind: 'flag', tracks: f.tracks, t: f.t, flag: f.flag_kind })
  }
  return out.sort((a, b) => a.t - b.t || a.id.localeCompare(b.id))
}

/** How long ago, in words for a line: "40 s", "2 min", "1 h 05 min". */
export function agoWords(t: number, nowMs: number): string {
  const s = Math.max(0, Math.round(nowMs / 1000 - t))
  if (s < 90) return `${s} s`
  const m = Math.floor(s / 60)
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')} min`
}
