/**
 * GG6-8: the one-time question "When you quit Nugget, also stop background indexing and the local
 * AI?". The shell asks for it (`nugget:quit-ask`) the first time quitting would stop the background
 * service the app started; the answer is saved and Nugget quits. Dismissing it keeps Nugget open.
 * Until it is answered, quitting stops the service (see `src-tauri/src/lifecycle.rs`).
 */
import { useState } from "react";
import { errorMessage, lifecycleApi, type QuitChoice } from "../api";
import { Dialog } from "../components/Dialog";
import { useEvent } from "../hooks";
import { useT } from "../i18n";

export function QuitPrompt() {
  const t = useT();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEvent(lifecycleApi.onQuitAsk, () => {
    setError(null);
    setOpen(true);
  });

  if (!open) return null;

  const answer = async (choice: QuitChoice) => {
    setBusy(true);
    setError(null);
    try {
      await lifecycleApi.quitAnswer(choice, true);
      setOpen(false);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const cancel = () => {
    setOpen(false);
    void lifecycleApi.quitCancel().catch(() => {});
  };

  return (
    <Dialog title={t("quit.ask.title")} description={t("quit.ask.body")} onClose={busy ? () => {} : cancel}>
      <p className="muted small">{t("quit.ask.detail")}</p>
      {error && (
        <div className="note note-error" role="alert">
          {t("quit.ask.failed", { error })}
        </div>
      )}
      <div className="dialog-actions">
        <button type="button" className="btn" disabled={busy} onClick={() => void answer("keep")}>
          {t("quit.ask.keep")}
        </button>
        <button type="button" className="btn btn-primary" data-autofocus disabled={busy} onClick={() => void answer("stop")}>
          {t("quit.ask.stop")}
        </button>
      </div>
    </Dialog>
  );
}
