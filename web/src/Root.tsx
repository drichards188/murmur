import { useEffect, useRef, useState } from 'react'
import App from './App'
import { type LauncherState, probeLauncher, useLauncher } from './launcher'
import { Menu } from './Menu'

/** What the page shows first. With the launcher (`make start`) the main menu, and the console on
 *  the scenario it runs; without one (`make up`, `make goldengate`), the console as it always was. */
export function Root() {
  // undefined: still asking (a moment: the launcher is local)
  const [probe, setProbe] = useState<LauncherState | null | undefined>(undefined)
  useEffect(() => {
    void probeLauncher().then(setProbe)
  }, [])
  if (probe === undefined) return <div className="boot" />
  if (probe === null) return <App />
  return <Launched initial={probe} />
}

function Launched({ initial }: { initial: LauncherState }) {
  const { state, received, lost, update } = useLauncher(initial)
  const cur = state.current
  const up = !!cur?.stack_up
  // the menu over the console: always while nothing is up, and when the console's Menu asks
  const [menu, setMenu] = useState(!up)
  // a launch that reaches running opens the console on it, once
  const opened = useRef(new Set<string>())
  const runningId = cur?.stage === 'running' ? cur.id : null
  useEffect(() => {
    if (runningId && !opened.current.has(runningId)) {
      opened.current.add(runningId)
      setMenu(false)
    }
  }, [runningId])
  return (
    <>
      {/* a new launch is a new console: nothing of the last scenario's picture carries over */}
      {up && cur && <App key={cur.id} onMenu={() => setMenu(true)} />}
      {(menu || !up) && <Menu state={state} received={received} lost={lost} onUpdate={update} onReturn={up ? () => setMenu(false) : undefined} />}
    </>
  )
}
