// The cockpit layout (`?layout=cockpit`, the default; bd epic murmur-iobd, direction 1): map first,
// the console's structure as it was with the top bar redone. The bar across the top (CockpitBar.tsx:
// the run and the glance on the left, the Inbox and the other drawers, Views, the operator and the
// time on the right), the waiting strip under it while something holds up a step, then three
// columns: the Fleet list, the map with the dock's drawer over its right edge, and the rail (the
// Tracks list over the command log, the nomination form docking over both). The run's chip sits in
// the bar, not over the map: the map floats only the open Situation panel. index.css lays out the
// grid, cockpit.css the bar, each part's own CSS the rest (parts.ts).
import { CockpitBar } from './CockpitBar'
import type { LayoutProps } from './parts'
import './cockpit.css'

export function Cockpit({ parts }: LayoutProps) {
  return (
    <>
      <CockpitBar parts={parts} />
      {parts.waiting}
      {parts.fleet}
      {parts.map({ drawer: parts.dock.drawer, runChip: false })}
      <div className="rail">
        <div className="tk-slot">{parts.tracks}</div>
        {parts.command}
        {parts.nominate}
      </div>
    </>
  )
}
