import { useRef, useState, type Dispatch, type SetStateAction } from 'react'
import { composeKey, countTone, insertAt, sendState } from './officer'
import { textLength, TEXT_MAX } from './tasking'
import type { TaskingApi } from './useTasking'
import './dock.css'

/** Something the box can put in at the cursor: the selection ("PICKET (60)", a few callsigns) or the
 *  selected track; `text` null with `why` when it can't (too many to name). */
export interface ComposeInsert {
  key: string
  label: string
  text: string | null
  why?: string
}

/** The message box to the watch officer, pinned at the top of its drawer (TaskingPanel's `compose`):
 *  Enter sends, Shift+Enter is a new line, the count runs to the bridge's cap, and Send waits, saying
 *  why, while no officer reads the tasking. The draft is the caller's (App), so it outlives the
 *  drawer: switching to the Inbox to decide and back keeps what was half written. A request the
 *  officer carries out with the orders it may propose: sending it is never an approval or a
 *  decision, and those orders wait where they always wait (officer.ts, tasking.ts). */
export function OfficerCompose({ api, draft, setDraft, inserts = [] }: { api: TaskingApi; draft: string; setDraft: Dispatch<SetStateAction<string>>; inserts?: ComposeInsert[] }) {
  const [busy, setBusy] = useState(false)
  const box = useRef<HTMLTextAreaElement>(null)
  if (api.status === 'missing') return null
  const state = sendState(draft, api.status, api.reader, busy)
  const n = textLength(draft)
  const send = async () => {
    if (!state.ok) return
    const sent = draft
    setBusy(true)
    const item = await api.post(sent)
    setBusy(false)
    // only what went: anything written while it was on its way stays
    if (item) setDraft((now) => (now === sent ? '' : now))
    box.current?.focus()
  }
  const insert = (add: string) => {
    const ta = box.current
    const at = insertAt(draft, ta?.selectionStart ?? draft.length, ta?.selectionEnd ?? draft.length, add)
    setDraft(at.text)
    window.requestAnimationFrame(() => {
      box.current?.focus()
      box.current?.setSelectionRange(at.caret, at.caret)
    })
  }
  return (
    <form
      className="oc"
      aria-label="Message to the watch officer"
      onSubmit={(e) => {
        e.preventDefault()
        void send()
      }}
    >
      <textarea
        ref={box}
        className="oc-text"
        rows={3}
        value={draft}
        placeholder="What should the officer do? e.g. Split the picket in two groups, one patrolling each side of the bridge."
        aria-label="Message to the watch officer"
        aria-describedby="oc-count oc-keys oc-why"
        aria-invalid={n > TEXT_MAX}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (composeKey({ key: e.key, shiftKey: e.shiftKey, altKey: e.altKey, isComposing: e.nativeEvent.isComposing }) !== 'send') return
          e.preventDefault()
          void send()
        }}
      />
      <div className="oc-bar">
        {inserts.length > 0 && (
          <div className="oc-insert" role="group" aria-label="Insert at the cursor">
            {inserts.map((c) => (
              <button
                key={c.key}
                type="button"
                className="oc-chip"
                disabled={!c.text}
                title={c.text ? `Insert "${c.text}" at the cursor` : c.why}
                // keep the box's caret where it is
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => c.text && insert(c.text)}
              >
                + {c.label}
              </button>
            ))}
          </div>
        )}
        <span id="oc-count" className={`oc-count ${countTone(draft)}`}>
          {n}/{TEXT_MAX}
        </span>
        <button type="submit" className="oc-send" disabled={!state.ok}>
          {busy ? 'Sending…' : 'Send'}
        </button>
      </div>
      <p id="oc-keys" className="oc-keys">
        Enter sends · Shift+Enter new line · a request, not an approval
      </p>
      <p id="oc-why" className="oc-why" role="status">
        {state.why ?? api.error ?? ''}
      </p>
    </form>
  )
}
