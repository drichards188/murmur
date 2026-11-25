// The rail layout's right edge (Rail.tsx): only Needs you, the Inbox's own content (parts.inbox:
// every card as the Inbox shows it, the banners at its head), beside the map rather than over it.
// It folds to a strip that keeps the count (red when something in it is urgent); its Close folds it
// there. A drawer from the top bar (Queue, Groups, Sitrep, Officer) lies over it while open, one at
// a time as before, and gives the edge back when closed. Display only: a decision is made on its
// card, as in the Inbox.
import type { MouseEvent, ReactNode } from 'react'
import type { NeedsYou } from '../needsYou'

interface Props {
  /** the dock's drawer while one other than the Inbox is open (dock.drawer), else null */
  drawer: ReactNode
  /** whether Needs you shows (else its strip) */
  shown: boolean
  /** parts.inbox */
  inbox: ReactNode
  needs: NeedsYou
  urgent: boolean
  onFold: () => void
  onUnfold: () => void
}

export function RailNeeds({ drawer, shown, inbox, needs, urgent, onFold, onUnfold }: Props) {
  if (drawer) return <div className="rn drawer">{drawer}</div>
  if (shown) {
    // the Inbox's Close folds Needs you to its strip (its own handler closes a dock it is not in)
    const fold = (e: MouseEvent) => {
      if (e.target instanceof Element && e.target.closest('.ix-top .close')) onFold()
    }
    return (
      <div className="rn open" onClickCapture={fold}>
        {inbox}
      </div>
    )
  }
  return (
    <div className="rn shut">
      <button type="button" className="rn-strip" onClick={onUnfold} title={`${needs.says} A click shows Needs you.`} aria-label={`Show Needs you. ${needs.says}`} data-urgent={urgent || undefined}>
        {needs.total > 0 && <span className={`ix-badge ${urgent ? 'urgent' : 'routine'}`}>{needs.total}</span>}
        <span className="rn-strip-label">Needs you</span>
      </button>
    </div>
  )
}
