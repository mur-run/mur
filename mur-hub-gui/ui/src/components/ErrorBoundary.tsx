import React from "react";

interface State {
  error: Error | null;
}

/**
 * Catches render/runtime errors anywhere in the React tree and shows the error
 * instead of letting the whole window go blank (white-screen). Also surfaces the
 * message + stack so a crash is diagnosable rather than silent.
 */
export class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  State
> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    // Visible in the webview console; helps diagnose otherwise-silent unmounts.
    console.error("Hub UI crashed:", error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    return (
      <div style={{ padding: 24, color: "#e6e6e6", fontFamily: "ui-monospace, monospace", overflow: "auto", height: "100vh" }}>
        <div style={{ fontSize: 16, fontWeight: 600, marginBottom: 12, color: "#ff6b6b" }}>
          Something went wrong in the Hub UI
        </div>
        <div style={{ marginBottom: 12 }}>{error.message}</div>
        <pre style={{ whiteSpace: "pre-wrap", fontSize: 12, opacity: 0.8 }}>{error.stack}</pre>
        <button
          className="toolbar-btn"
          style={{ marginTop: 12 }}
          onClick={() => this.setState({ error: null })}
        >
          Try again
        </button>
      </div>
    );
  }
}
