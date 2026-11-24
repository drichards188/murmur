import { scriptedBadge } from './situationIds'

/** Who asks, on a card and a waiting row: the AI labelled as the AI, a script marked as one, a
 *  person by their id (principle 2: the AI's proposals are labelled as the AI's). */
export function Proposer({ source, what = 'proposed' }: { source: { kind: 'human' | 'agent' | 'handover'; id: string }; what?: 'proposed' | 'nominated' }) {
  const badge = scriptedBadge(source.id)
  if (badge) {
    return (
      <span className="ix-src scripted" title={`${source.id}: a situation's script, not a person and not the AI`}>
        {badge} {source.id}
      </span>
    )
  }
  const title = source.kind === 'agent' ? `${what} by an AI agent` : source.kind === 'handover' ? "the previous watch's order, handed over at the start by this fleet file" : `${what} by a human operator`
  return (
    <span className={`ix-src ${source.kind}`} title={title}>
      {source.kind === 'agent' ? `AI ${source.id}` : source.kind === 'handover' ? 'previous watch' : source.id}
    </span>
  )
}
