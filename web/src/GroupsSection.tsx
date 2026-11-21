import { useState } from 'react'
import type { GroupsApi } from './useGroups'
import { defaultName, groupOfSelection, lookOf, nameProblem, nextColor, strength, toAdd, toSeparate, NAME_MAX, type OperatorGroup } from './groups'
import { GROUP_PALETTE } from './swarmsModel'
import './groups.css'

/** The operator's own groups: made, renamed, recoloured and deleted at run
 *  time, shared through the bridge. A click selects the members, so the order form commands the
 *  group in one click; nothing here sends an order or decides anything. */

interface Props {
  api: GroupsApi
  selected: ReadonlySet<number>
  /** the uids in the fleet now (a member that is not is "away": it may have left) */
  present: ReadonlySet<number>
  nameOf: (uid: number) => string
  /** select exactly these vehicles (a group's members) */
  onSelect: (uids: number[]) => void
}

function Swatches({ value, onPick, label }: { value: string | null; onPick: (color: string) => void; label: string }) {
  return (
    <span className="og-swatches" role="radiogroup" aria-label={label}>
      {GROUP_PALETTE.map((p) => (
        <button key={p.color} type="button" role="radio" aria-checked={value?.toLowerCase() === p.color} className={`og-swatch ${value?.toLowerCase() === p.color ? 'on' : ''}`} style={{ background: p.color }} title={p.color} onClick={() => onPick(p.color)} />
      ))}
    </span>
  )
}

/** The form that makes a group: a name, a colour, and the selection as its first members. */
function NewGroup({ api, selected, onDone }: { api: GroupsApi; selected: ReadonlySet<number>; onDone: (g: OperatorGroup | null) => void }) {
  const [name, setName] = useState(() => defaultName(api.groups))
  const [color, setColor] = useState(() => nextColor(api.groups))
  const [fromSel, setFromSel] = useState(selected.size > 0)
  const [busy, setBusy] = useState(false)
  const [said, setSaid] = useState<string | null>(null)
  const problem = nameProblem(name)
  const go = async () => {
    if (problem) return
    setBusy(true)
    const made = await api.create(name, fromSel ? [...selected].sort((a, b) => a - b) : [], color)
    setBusy(false)
    if (!made) return
    if (made.dropped.length) setSaid(`${made.dropped.length} not in the fleet, left out.`)
    onDone(made.group)
  }
  return (
    <form
      className="og-new"
      onSubmit={(e) => {
        e.preventDefault()
        void go()
      }}
    >
      <label>
        <span>Name</span>
        <input value={name} maxLength={NAME_MAX} onChange={(e) => setName(e.target.value)} autoFocus aria-invalid={problem != null} />
      </label>
      <div className="og-line">
        <span className="muted">Colour</span>
        <Swatches value={color} onPick={setColor} label="Group colour" />
      </div>
      <label className="og-check">
        <input type="checkbox" checked={fromSel} disabled={selected.size === 0} onChange={(e) => setFromSel(e.target.checked)} />
        start with the {selected.size} selected vehicle{selected.size === 1 ? '' : 's'}
      </label>
      {problem && <p className="error">{problem}</p>}
      {said && <p className="muted">{said}</p>}
      <div className="og-line">
        <button type="submit" className="primary" disabled={busy || problem != null}>
          {busy ? 'Making...' : 'Make group'}
        </button>
        <button type="button" className="ghost" onClick={() => onDone(null)}>
          Cancel
        </button>
      </div>
    </form>
  )
}

/** One group, opened: rename, recolour, the selection in and out, delete, and its members one by one. */
function GroupBody({ g, index, props }: { g: OperatorGroup; index: number; props: Props }) {
  const { api, selected, nameOf, present } = props
  const [name, setName] = useState(g.name)
  const [confirm, setConfirm] = useState(false)
  const [all, setAll] = useState(false)
  const add = toAdd(g, selected)
  const out = toSeparate(g, selected)
  const problem = nameProblem(name)
  const shown = all ? g.members : g.members.slice(0, 12)
  const away = strength(g, present).away
  return (
    <div className="og-body">
      <form
        className="og-line"
        onSubmit={(e) => {
          e.preventDefault()
          if (!problem && name.trim() !== g.name) void api.patch(g.id, { name })
        }}
      >
        <input value={name} maxLength={NAME_MAX} onChange={(e) => setName(e.target.value)} aria-label="Group name" aria-invalid={problem != null} />
        <button type="submit" className="ghost" disabled={problem != null || name.trim() === g.name}>
          Rename
        </button>
      </form>
      <div className="og-line">
        <span className="muted">Colour</span>
        <Swatches value={lookOf(g, index).color} onPick={(color) => void api.patch(g.id, { color })} label={`${g.name} colour`} />
      </div>
      <div className="og-line og-acts">
        <button className="ghost" disabled={add.length === 0} onClick={() => void api.patch(g.id, { add })} title="Put the selected vehicles that are not in it into it">
          Add selection{add.length ? ` (${add.length})` : ''}
        </button>
        <button className="ghost" disabled={out.length === 0} onClick={() => void api.patch(g.id, { remove: out })} title="Take the selected members out of it (they stay in the fleet and in any other group)">
          Remove selection{out.length ? ` (${out.length})` : ''}
        </button>
        <button
          className="ghost"
          disabled={out.length === 0}
          onClick={async () => {
            // separate: the selected members leave this group and form a new one
            const made = await api.create(defaultName(api.groups), out, nextColor(api.groups))
            if (made) void api.patch(g.id, { remove: out })
          }}
          title="Take the selected members out of it and make them a group of their own"
        >
          Separate into new group
        </button>
      </div>
      {away > 0 && <p className="muted">{away} member{away === 1 ? ' is' : 's are'} not in the fleet now.</p>}
      <ul className="og-members" aria-label={`${g.name} members`}>
        {shown.map((u) => (
          <li key={u} className={present.has(u) ? '' : 'away'}>
            <span>{nameOf(u)}</span>
            <button className="ghost og-x" aria-label={`Remove ${nameOf(u)} from ${g.name}`} title="Remove from this group" onClick={() => void api.patch(g.id, { remove: [u] })}>
              ×
            </button>
          </li>
        ))}
      </ul>
      {g.members.length > 12 && (
        <button className="ghost og-more" onClick={() => setAll(!all)}>
          {all ? 'Show fewer' : `Show all ${g.members.length}`}
        </button>
      )}
      <div className="og-line og-danger">
        {confirm ? (
          <>
            <span className="muted">Delete {g.name}? Its vehicles and orders are not touched.</span>
            <button className="ghost bad" onClick={() => void api.remove(g.id)}>
              Delete
            </button>
            <button className="ghost" onClick={() => setConfirm(false)}>
              Keep
            </button>
          </>
        ) : (
          <button className="ghost" onClick={() => setConfirm(true)}>
            Delete group
          </button>
        )}
      </div>
    </div>
  )
}

export function GroupsSection(props: Props) {
  const { api, selected, present, onSelect } = props
  const [making, setMaking] = useState(false)
  const [open, setOpen] = useState<string | null>(null)
  const exact = groupOfSelection(api.groups, selected)
  return (
    <section className="sw-group og" aria-label="Your groups">
      <h3 className="ix-h">
        Your groups <span>{api.groups.length}</span>
        <small>made by you, shared, never an order</small>
      </h3>
      {api.status === 'missing' && (
        <p className="empty sw-missing" role="status">
          This bridge has no groups of your own (<code>/api/groups</code>).
        </p>
      )}
      {api.error && api.status !== 'missing' && <p className="error">{api.error}</p>}
      {api.status !== 'missing' && !making && (
        <button className="ghost og-make" onClick={() => setMaking(true)}>
          New group{selected.size ? ` from ${selected.size} selected` : ''}
        </button>
      )}
      {making && (
        <NewGroup
          api={api}
          selected={selected}
          onDone={(g) => {
            setMaking(false)
            if (g) setOpen(g.id)
          }}
        />
      )}
      {api.status === 'ok' && api.groups.length === 0 && !making && <p className="empty muted">No groups yet. Select vehicles, then New group.</p>}
      {api.groups.map((g, i) => {
        const look = lookOf(g, i)
        const st = strength(g, present)
        const isOpen = open === g.id
        return (
          <div key={g.id} className={`og-item ${exact?.id === g.id ? 'sel' : ''}`}>
            <div className="og-row">
              <button className="og-pick" aria-pressed={exact?.id === g.id} onClick={() => onSelect(g.members.filter((u) => present.has(u)))} title={`Select the ${st.here} of ${g.name} in the fleet, for one order`}>
                <span className="og-chip" style={{ background: look.color }} aria-hidden />
                <b className="og-name">{g.name}</b>
                <span className="og-n">
                  {st.here}
                  {st.away > 0 ? <span className="dim">/{g.members.length}</span> : null}
                </span>
                <span className="og-badge" title="Your own group: the fleet's swarms are fixed by its file">yours</span>
              </button>
              <button className="ghost og-edit" aria-expanded={isOpen} aria-label={`${isOpen ? 'Close' : 'Edit'} ${g.name}`} onClick={() => setOpen(isOpen ? null : g.id)}>
                {isOpen ? '▾' : '▸'}
              </button>
            </div>
            {isOpen && <GroupBody g={g} index={i} props={props} />}
          </div>
        )
      })}
    </section>
  )
}

/** The fleet list's "add to group": the selection into a group of yours, or into a new one. */
export function AddToGroup({ api, selected }: { api: GroupsApi; selected: ReadonlySet<number> }) {
  const [making, setMaking] = useState(false)
  if (api.status === 'missing' || selected.size === 0) return null
  if (making) {
    return (
      <div className="og-addnew">
        <NewGroup api={api} selected={selected} onDone={() => setMaking(false)} />
      </div>
    )
  }
  return (
    <select
      className="og-add"
      aria-label={`Add the ${selected.size} selected to a group`}
      value=""
      onChange={(e) => {
        const v = e.target.value
        if (v === '__new') setMaking(true)
        else if (v) void api.patch(v, { add: toAdd(api.groups.find((g) => g.id === v)!, selected) })
      }}
    >
      <option value="">Add {selected.size} to group...</option>
      {api.groups.map((g) => (
        <option key={g.id} value={g.id} disabled={toAdd(g, selected).length === 0}>
          {g.name} ({toAdd(g, selected).length} new)
        </option>
      ))}
      <option value="__new">New group...</option>
    </select>
  )
}
