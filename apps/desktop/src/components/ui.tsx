import type { ReactNode } from "react";
import { ApiError, errorMessage } from "../api";
import { useT } from "../i18n";
import "../styles-pages.css";

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
  const t = useT();
  if (!error) return null;
  const e = ApiError.from(error);
  if (e.isMethodNotFound) return <NotAvailable />;
  return (
    <div className={e.isOffline ? "note" : "note note-error"} role="alert">
      <span>{e.isOffline ? t("common.offlineHint") : errorMessage(e)}</span>
      {e.kind === "rpc" && e.code !== null && <span className="muted small"> {t("common.errorCode", { code: e.code })}</span>}
      {onRetry && (
        <button type="button" className="btn btn-ghost btn-small" onClick={onRetry}>
          {t("common.retry")}
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

/**
 * First-load placeholder: a labelled status (announced once, politely) over a few skeleton
 * lines. The skeleton is decorative; the shimmer stops under reduced motion.
 */
export function Loading({ label, lines = 3 }: { label?: string; lines?: number }) {
  const t = useT();
  return (
    <div className="pg-loading" role="status" aria-live="polite">
      <span className="pg-loading-label">
        <span className="spinner" aria-hidden="true" />
        {label ?? t("common.loading")}
      </span>
      <div className="pg-loading" aria-hidden="true">
        {Array.from({ length: lines }, (_, i) => (
          <span key={i} className="pg-skeleton" />
        ))}
      </div>
    </div>
  );
}

/** Empty state with a next step: what's missing, what to do about it, and optionally a button. */
export function EmptyState({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="pg-empty">
      <p className="pg-empty-title">{title}</p>
      {children && <p className="pg-empty-body">{children}</p>}
      {action && <div className="pg-empty-action">{action}</div>}
    </div>
  );
}

export function Spinner({ label }: { label?: string }) {
  const t = useT();
  return <span className="spinner" role="status" aria-label={label ?? t("common.loading")} />;
}

/** The daemon doesn't implement this yet (JSON-RPC "method not found"). */
export function NotAvailable({ what }: { what?: string }) {
  const t = useT();
  return (
    <div className="note" role="status">
      <span>{what ? t("common.notAvailableWhat", { what }) : t("common.notAvailableSentence")}</span>
      <span className="small">{t("common.updateDaemon")}</span>
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
