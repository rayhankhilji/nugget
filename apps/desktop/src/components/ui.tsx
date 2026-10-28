import type { ReactNode } from "react";
import { ApiError, errorMessage, NOT_AVAILABLE } from "../api";

export function PageHeader({ title, description, actions }: { title: string; description?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="page-header">
      <div>
        <h1>{title}</h1>
        {description && <p className="muted">{description}</p>}
      </div>
      {actions && <div className="page-actions">{actions}</div>}
    </header>
  );
}

export function Badge({ tone = "neutral", children }: { tone?: "neutral" | "accent" | "good" | "warn" | "bad"; children: ReactNode }) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}

/** Inline error. Offline errors are handled app-wide, so they render as a quiet hint here. */
export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (!error) return null;
  const e = ApiError.from(error);
  if (e.isMethodNotFound) return <NotAvailable />;
  return (
    <div className={e.isOffline ? "note" : "note note-error"} role="alert">
      <span>{e.isOffline ? "Nugget isn't reachable right now." : errorMessage(e)}</span>
      {e.kind === "rpc" && e.code !== null && <span className="muted small"> (code {e.code})</span>}
      {onRetry && (
        <button type="button" className="btn btn-ghost btn-small" onClick={onRetry}>
          Retry
        </button>
      )}
    </div>
  );
}

export function Empty({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <p className="empty-title">{title}</p>
      {children && <p className="muted">{children}</p>}
    </div>
  );
}

export function Spinner({ label = "Loading" }: { label?: string }) {
  return <span className="spinner" role="status" aria-label={label} />;
}

/** The daemon doesn't implement this yet (JSON-RPC "method not found"). */
export function NotAvailable({ what }: { what?: string }) {
  return (
    <div className="note" role="status">
      <span>
        {what ? `${what}: ` : ""}
        {NOT_AVAILABLE}.
      </span>
      <span className="small">Update the Nugget background service to use this.</span>
    </div>
  );
}

export function ProgressBar({ done, total, label }: { done: number; total: number | null | undefined; label: string }) {
  if (total && total > 0) {
    return <progress aria-label={label} max={total} value={Math.min(done, total)} />;
  }
  // Unknown total: indeterminate.
  return <progress aria-label={label} />;
}
