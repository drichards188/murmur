// The rail layout's left edge (Rail.tsx): one rail holding the Fleet list, the Tracks list and the
// command log as tabs, one list at a time. The tabs stand in a strip that never folds (the activity
// bar's way): each says on its face how many its list holds and, with a mark, how many in it want a
// look (railTabs.ts); a click on the tab in front, or the fold button, folds the lists away and the
// map takes their room. Every list stays mounted while out of sight, so its scroll, its folds and a
// half-made order are where the person left them. The nomination form docks over the lists, as it
// docked over Cockpit's rail, and drags out over the map. Display only.
import { type KeyboardEvent, type ReactNode, useRef } from 'react'
import { clickTab, offerOrder, RAIL_TABS, type RailState, type RailTab, tabAfterKey, type TabFace } from './railTabs'

interface Props {
  state: RailState
  set: (f: (s: RailState) => RailState) => void
  /** the tabs' faces, in RAIL_TABS order */
  faces: readonly TabFace[]
  /** each tab's list (parts.fleet, parts.tracks, parts.command) */
  lists: Readonly<Record<RailTab, ReactNode>>
  /** how many of ours are selected (the order form commands them) */
  selected: number
  /** parts.nominate: the last child of the positioned dock over the lists */
  nominate: ReactNode
}

export function RailLists({ state, set, faces, lists, selected, nominate }: Props) {
  const tabs = useRef<HTMLDivElement>(null)
  const offer = offerOrder(state, selected)
  // the arrows move between the tabs; Enter or Space acts as a click on the one focused
  const onKey = (e: KeyboardEvent<HTMLButtonElement>, id: RailTab) => {
    const next = tabAfterKey(id, e.key)
    if (!next) return
    e.preventDefault()
    tabs.current?.querySelector<HTMLElement>(`[data-rail-tab="${next}"]`)?.focus()
  }
  const toOrders = () => set((s) => ({ ...s, tab: 'command', open: true }))
  return (
    <aside className={`rl ${state.open ? 'open' : 'shut'}`} aria-label="Fleet, tracks and command log">
      <div className="rl-tabs" ref={tabs} role="tablist" aria-orientation="vertical" aria-label="Lists, one at a time">
        {faces.map((f) => {
          const front = state.tab === f.id
          const ready = f.id === 'command' && offer
          const says = [`${f.label}: ${f.counts}`, f.mark?.says, ready ? `${selected} selected: the order form is here` : null]
            .filter((t): t is string => !!t)
            .map((t) => t.replace(/\.+$/, ''))
            .join('. ')
          return (
            <button
              key={f.id}
              type="button"
              role="tab"
              id={`rl-tab-${f.id}`}
              data-rail-tab={f.id}
              aria-selected={state.open && front}
              aria-controls={`rl-list-${f.id}`}
              tabIndex={front ? 0 : -1}
              className={`rl-tab${front ? ' front' : ''}${ready ? ' ready' : ''}`}
              title={`${says}.${state.open && front ? ' A click hides the lists.' : ''}`}
              onClick={() => set((s) => clickTab(s, f.id))}
              onKeyDown={(e) => onKey(e, f.id)}
            >
              <span className={`rl-count${f.count === 0 ? ' none' : ''}`}>{f.count ?? ''}</span>
              <span className="rl-label">{f.label}</span>
              {f.mark && (
                <span className={`ix-badge rl-mark ${f.mark.tone}`} aria-label={f.mark.says}>
                  {f.mark.n}
                </span>
              )}
            </button>
          )
        })}
        <span className="rl-fill" />
        <button
          type="button"
          className="rl-fold"
          aria-expanded={state.open}
          aria-label={state.open ? 'Hide the lists' : 'Show the lists'}
          title={state.open ? 'Hide the lists: the map takes their room (the tabs stay)' : `Show the ${faces.find((f) => f.id === state.tab)?.label ?? ''} list`}
          onClick={() => set((s) => ({ ...s, open: !s.open }))}
        >
          <span aria-hidden="true">{state.open ? '‹' : '›'}</span>
        </button>
      </div>
      <div className="rl-body">
        {RAIL_TABS.map((id) => (
          <div key={id} id={`rl-list-${id}`} role="tabpanel" aria-labelledby={`rl-tab-${id}`} className={`rl-list rl-list-${id}${state.tab === id ? ' front' : ''}`}>
            {lists[id]}
          </div>
        ))}
        {offer && (
          <button type="button" className="rl-go" onClick={toOrders} title="Show the order form in the Command tab: it commands the vehicles selected">
            Give an order to {selected === 1 ? 'the vehicle selected' : `the ${selected} selected`}
            <span aria-hidden="true">{'›'}</span>
          </button>
        )}
      </div>
      {/* the nomination form's layer: docked over the lists, dragged out over the map */}
      <div className="rl-dock">{nominate}</div>
    </aside>
  )
}
