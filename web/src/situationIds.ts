/** The scripted ids a running situation names in its status (`GET /api/situation`, display
 *  only) and the badge each gets: the Situation panel keeps this up to date, and the Inbox shows
 *  the badge on a card from one of them ("situation setup (scripted)"), so a script's order is
 *  never taken for a person's or an AI's. Empty when no situation runs. */
let badges: Record<string, string> = {}

export function setScriptedBadges(next: Record<string, string> | undefined) {
  badges = next ?? {}
}

export function scriptedBadge(id: string): string | undefined {
  return badges[id]
}
