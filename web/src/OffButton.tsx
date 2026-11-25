// A top bar button for a drawer or view this run has nothing in, or cannot show: listed, greyed,
// with the reason on hover, never left out (the owner, 2026-11-24: a control that vanished read as a
// lost feature). Display only.

/** A greyed, disabled top bar button with why it is off. */
export function OffButton({ label, off }: { label: string; off: string }) {
  return (
    <button className="ghost platforms-btn ix-btn is-off" disabled title={`${label}: ${off}`} aria-label={`${label}, not available: ${off}`}>
      {label}
    </button>
  )
}
