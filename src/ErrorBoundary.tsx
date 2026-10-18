import { Component } from 'react';
import type { ErrorInfo, ReactNode } from 'react';

// A rendering error shows a way out instead of a blank window.
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error(error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="app">
        <div className="content">
          <div className="empty-state">
            Une erreur d’affichage est survenue.
            <div className="error-text">{this.state.error.message}</div>
            <button className="btn" onClick={() => window.location.reload()}>Recharger</button>
          </div>
        </div>
      </div>
    );
  }
}
