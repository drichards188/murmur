import { Component, type ErrorInfo, type ReactNode } from 'react'

interface Props {
  /** what failed, for the banner: "Map", "Fleet list", ... */
  name: string
  children: ReactNode
  /** extra class on the banner (placement: the map's banner floats, a panel's fills its slot) */
  className?: string
}

interface State {
  error: Error | null
  /** bumped by Retry: a fresh key remounts the children from scratch */
  attempt: number
}

/** One crash stays in its own part of the console: a compact banner with Retry replaces the
 *  part that threw, and the rest keeps working (murmur-s8w). */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, attempt: 0 }

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(`[murmur] ${this.props.name} crashed:`, error, info.componentStack)
  }

  retry = () => this.setState((s) => ({ error: null, attempt: s.attempt + 1 }))

  render() {
    const { error, attempt } = this.state
    if (error) {
      return (
        <div className={`crash ${this.props.className ?? ''}`} role="alert">
          <span className="crash-what">{this.props.name} stopped</span>
          <span className="crash-msg" title={error.stack ?? error.message}>
            {error.message || String(error)}
          </span>
          <button className="ghost mini" onClick={this.retry}>
            Retry
          </button>
        </div>
      )
    }
    return <Fallthrough key={attempt}>{this.props.children}</Fallthrough>
  }
}

function Fallthrough({ children }: { children: ReactNode }) {
  return <>{children}</>
}
