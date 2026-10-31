import type { ReactNode } from "react";
import { ApiError } from "../api";
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

/**
 * What an error means to the user: the text to show (and announce), and the technical text kept
 * behind "Details".
 * - busy / locked, timed out, version mismatch, failed start: a translated sentence.
 * - any other daemon (rpc) error: the daemon's own message stays visible, since it usually says
 *   what to fix (a bad path, a duplicate name); its code goes under Details, never number-formatted.
 * - anything else (shell io errors, script errors): "Something went wrong", raw text under Details.
 */
export function describeError(e: ApiError, t: ReturnType<typeof useT>): { headline: string; detail: string | null } {
  const raw = e.message.trim();
  const code = e.kind === "rpc" && e.code !== null ? t("common.errorCode", { code: String(e.code) }) : "";
  const detail = [raw, code].filter(Boolean).join(" ") || null;
  if (e.kind === "rpc" || e.kind === "protocol" || e.kind === "spawn") {
    if (e.isBusy || /\b(locked|busy)\b/i.test(raw)) return { headline: t("common.error.busy"), detail };
    if (/timed? ?out|timeout|deadline/i.test(raw)) return { headline: t("common.error.timeout"), detail };
  }
  if (e.kind === "protocol") return { headline: t("common.error.protocol"), detail };
  if (e.kind === "rpc" && raw) return { headline: raw, detail: code || null };
  return { headline: t("common.error.generic"), detail };
}

/**
 * Inline error: what went wrong (a translated sentence for known cases, else the daemon's own
 * message), an optional Retry, and the technical text behind "Details". Offline errors are
 * handled app-wide, so they render as a quiet hint here.
 */
export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const t = useT();
  if (!error) return null;
  const e = ApiError.from(error);
  if (e.isMethodNotFound) return <NotAvailable />;
  if (e.isOffline) {
    return (
      <div className="note" role="alert">
        <span>{t("common.offlineHint")}</span>
        {onRetry && (
          <button type="button" className="btn btn-small" onClick={onRetry}>
            {t("common.retry")}
          </button>
        )}
      </div>
    );
  }
  // App code throws plain `Error`s with an already translated sentence (e.g. Plugins' approve);
  // show those as they are. Runtime errors (TypeError, …) and shell errors keep the generic line.
  const own = error instanceof Error && !(error instanceof ApiError) && error.name === "Error" && error.message.trim();
  const { headline, detail } = own ? { headline: error.message.trim(), detail: null } : describeError(e, t);
  return (
    <div className="note note-error" role="alert">
      <span className="note-text" dir="auto">
        {headline}
      </span>
      {onRetry && (
        <button type="button" className="btn btn-small" onClick={onRetry}>
          {t("common.retry")}
        </button>
      )}
      {detail && (
        <details className="note-details">
          <summary>{t("common.details")}</summary>
          <p dir="auto">{detail}</p>
        </details>
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
