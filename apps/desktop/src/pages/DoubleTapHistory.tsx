import { useCallback, useEffect, useState } from "react";
import { draftHistoryApi, errorMessage, type DraftHistoryEntry } from "../api";
import { formatRelative } from "../format";
import { useEvent } from "../hooks";
import { useT } from "../i18n";

/** Unix ms to the relative time used across the app. */
function ago(at: number): string {
  return Number.isFinite(at) ? formatRelative(new Date(at).toISOString()) : "";
}

/**
 * "Recent drafts": the shell's in-memory list of the last drafts (newest first), to copy one
 * again. Nothing here is ever on disk. Hidden in a shell that doesn't keep one.
 */
export function DoubleTapHistory() {
  const t = useT();
  const [entries, setEntries] = useState<DraftHistoryEntry[] | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [note, setNote] = useState<{ text: string; error: boolean } | null>(null);

  const load = useCallback(() => {
    draftHistoryApi
      .list()
      .then(setEntries)
      .catch(() => setUnsupported(true));
  }, []);
  useEffect(load, [load]);
  useEvent(draftHistoryApi.onChange, (next: DraftHistoryEntry[]) => {
    setEntries(next);
    setUnsupported(false);
  });

  if (unsupported || entries === null) return null;

  const fail = (e: unknown) => setNote({ text: t("doubletap.history.failed", { error: errorMessage(e) }), error: true });

  const copy = async (entry: DraftHistoryEntry) => {
    try {
      await navigator.clipboard.writeText(entry.text);
      setNote({ text: t("doubletap.history.copied"), error: false });
    } catch (e) {
      setNote({ text: t("doubletap.history.copyFailed", { error: errorMessage(e) }), error: true });
    }
  };

  const remove = async (entry: DraftHistoryEntry) => {
    setNote(null);
    try {
      await draftHistoryApi.remove(entry.id);
      setEntries((list) => (list ?? []).filter((e) => e.id !== entry.id));
    } catch (e) {
      fail(e);
    }
  };

  const clear = async () => {
    setNote(null);
    try {
      await draftHistoryApi.clear();
      setEntries([]);
      setConfirmClear(false);
    } catch (e) {
      fail(e);
    }
  };

  return (
    <section aria-labelledby="dt-history" className="dt-history">
      <h2 id="dt-history">{t("doubletap.history.title")}</h2>
      <p className="muted small">{t("doubletap.history.about")}</p>
      {entries.length === 0 ? (
        <p className="muted">{t("doubletap.history.empty")}</p>
      ) : (
        <ul className="dt-history-list" aria-labelledby="dt-history">
          {entries.map((entry) => {
            const app = entry.app || t("doubletap.history.unknownApp");
            const metaId = `dt-history-${entry.id}`;
            return (
              <li key={entry.id} className="dt-history-item">
                <p id={metaId} className="muted small dt-history-meta">
                  <bdi>{app}</bdi> · {ago(entry.at)}
                </p>
                <p className="dt-history-text" dir="auto">
                  {entry.text}
                </p>
                <div className="inline-row">
                  <button type="button" className="btn btn-ghost btn-small" aria-describedby={metaId} onClick={() => void copy(entry)}>
                    {t("doubletap.history.copy")}
                  </button>
                  <button type="button" className="btn btn-ghost btn-small" aria-describedby={metaId} onClick={() => void remove(entry)}>
                    {t("doubletap.history.remove")}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
      {entries.length > 0 &&
        (confirmClear ? (
          <div className="inline-row" role="group" aria-label={t("doubletap.history.clearConfirm")}>
            <span className="small">{t("doubletap.history.clearConfirm")}</span>
            <button type="button" className="btn btn-small btn-danger" onClick={() => void clear()}>
              {t("doubletap.history.clearYes")}
            </button>
            <button type="button" className="btn btn-small btn-ghost" onClick={() => setConfirmClear(false)} autoFocus>
              {t("common.cancel")}
            </button>
          </div>
        ) : (
          <button type="button" className="btn btn-ghost btn-small" onClick={() => setConfirmClear(true)}>
            {t("doubletap.history.clear")}
          </button>
        ))}
      <p className={note?.error ? "field-error small" : "muted small"} role="status">
        {note?.text ?? ""}
      </p>
    </section>
  );
}
