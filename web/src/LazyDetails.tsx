import { type ReactNode, useState } from 'react'

/** A `<details>` whose body is built only while it is open. A closed `<details>` still has its
 *  children in the DOM, and React renders and diffs them on every update: a hundred members' link
 *  bars and share rows are some 360 KB of markup nobody has asked to see (bd murmur-yfp6). */
export function LazyDetails({ className, summary, children, onClick }: { className?: string; summary: ReactNode; children: ReactNode; onClick?: (e: React.MouseEvent<HTMLDetailsElement>) => void }) {
  const [open, setOpen] = useState(false)
  return (
    <details className={className} onClick={onClick} onToggle={(e) => setOpen(e.currentTarget.open)}>
      <summary>{summary}</summary>
      {open ? children : null}
    </details>
  )
}
