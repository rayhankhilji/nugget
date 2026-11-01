import type { ModelFailReason, ModelProgress } from "../api";
import { formatBytes } from "../format";
import type { MessageKey, TFunction } from "../i18n";

// KK1: plain-language model download failures, shared by the Models page and onboarding.

const FAIL_KEY: Partial<Record<ModelFailReason, MessageKey>> = {
  offline: "models.fail.offline",
  captive_portal: "models.fail.captivePortal",
  stalled: "models.fail.stalled",
  interrupted: "models.fail.interrupted",
  disk_full: "models.fail.diskFull",
  integrity: "models.fail.integrity",
  cancelled: "models.fail.cancelled",
};

/** The user cancelled this download (the daemon's final progress says so). */
export function isCancelled(p: ModelProgress | undefined): boolean {
  return p?.phase === "failed" && p.reason === "cancelled";
}

/**
 * A short, localised message for a failed install: by `reason`, else the daemon's own
 * plain-language text (an older or newer daemon), else `fallback`.
 */
export function failureMessage(t: TFunction, p: ModelProgress | undefined, fallback: string): string {
  if (!p || p.phase !== "failed") return fallback;
  if (p.reason === "http_error") return t("models.fail.httpError", { status: String(p.http_status ?? "?") });
  const key = p.reason ? FAIL_KEY[p.reason] : undefined;
  if (key) return t(key);
  return p.reason === "other" ? fallback : p.error || fallback;
}

/**
 * The failure note under a model card: an alert for errors, a polite status for a cancel, plus
 * how much is kept for the next try. `error` is a request error (the install never started).
 */
export function DownloadFailureNote({
  t,
  progress,
  error,
  fallback,
}: {
  t: TFunction;
  progress?: ModelProgress;
  error?: string;
  fallback: string;
}) {
  const failed = progress?.phase === "failed";
  if (!failed && !error) return null;
  const message = failed ? failureMessage(t, progress, fallback) : error || fallback;
  const kept =
    failed && progress && progress.downloaded > 0 && progress.reason !== "integrity"
      ? t("models.fail.kept", { done: formatBytes(progress.downloaded) })
      : null;
  const cancelled = isCancelled(progress);
  return (
    <p className={cancelled ? "note small" : "note note-error"} role={cancelled ? "status" : "alert"}>
      {message}
      {kept && <> {kept}</>}
    </p>
  );
}
