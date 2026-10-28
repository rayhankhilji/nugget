import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { doubletapApi, errorMessage, type OverlayView } from "../api";
import { keyOrText } from "../doubletap";
import { localityLabel } from "../format";
import { useT, type MessageKey } from "../i18n";
import { detectOs } from "../onboarding/platform";
import {
  accept,
  applyView,
  canAccept,
  cancel,
  handleKey,
  initialState,
  isFinished,
  setDraft,
  startEdit,
  type Effect,
  type OverlayState,
  type Step,
} from "./state";

const LOCALITY_TONE = { on_device: "good", local_network: "accent", cloud: "warn" } as const;

export function Overlay() {
  const t = useT();
  const [state, setState] = useState<OverlayState>(() => initialState());
  const ref = useRef(state);
  ref.current = state;
  const [failure, setFailure] = useState<string | null>(null);
  const editor = useRef<HTMLTextAreaElement>(null);
  const mac = detectOs() === "macos";

  const update = useCallback((next: OverlayState) => {
    ref.current = next;
    setState(next);
  }, []);

  const run = useCallback((effect: Effect) => {
    setFailure(null);
    const p = effect.kind === "accept" ? doubletapApi.overlayAccept(effect.text) : doubletapApi.overlayCancel();
    p.catch((e: unknown) => {
      setFailure(errorMessage(e));
      // Let the user try again.
      if (effect.kind === "accept") update({ ...ref.current, accepting: false });
    });
  }, [update]);

  const apply = useCallback(
    (step: Step) => {
      update(step.state);
      if (step.effect) run(step.effect);
    },
    [run, update],
  );

  // Subscribe first, then ask for the current state, so no change is missed in between.
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void doubletapApi
      .onOverlay((view) => alive && update(applyView(ref.current, view)))
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      })
      .catch(() => {});
    doubletapApi
      .overlayState()
      .then((view: OverlayView) => alive && update(applyView(ref.current, view)))
      .catch((e: unknown) => alive && setFailure(errorMessage(e)));
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [update]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // A focused button handles its own Enter/Space; Tab moves between buttons. Esc always cancels.
      if (e.key !== "Escape" && e.target instanceof HTMLButtonElement) return;
      const step = handleKey(ref.current, e);
      if (step.preventDefault) e.preventDefault();
      apply(step);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [apply]);

  const editing = state.mode === "edit";
  useLayoutEffect(() => {
    if (!editing || !editor.current) return;
    const el = editor.current;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, [editing]);

  const { view } = state;
  const phaseKey = `doubletap.overlay.phase.${view.phase}` as MessageKey;
  const acceptable = canAccept(state);

  return (
    <main className={`overlay overlay-${view.phase}`} aria-label={t("doubletap.title")}>
      <div className="overlay-head">
        <span className="overlay-phase" role="status">
          {t(phaseKey)}
        </span>
        <span className="overlay-badges">
          {view.provider && <span className="badge badge-neutral">{view.provider}</span>}
          {view.locality && (
            <span className={`badge badge-${LOCALITY_TONE[view.locality] ?? "neutral"}`}>{localityLabel(view.locality)}</span>
          )}
        </span>
      </div>

      {editing ? (
        <textarea
          ref={editor}
          className="overlay-editor"
          aria-label={t("doubletap.overlay.editLabel")}
          value={state.draft}
          onChange={(e) => update(setDraft(ref.current, e.target.value))}
          rows={5}
        />
      ) : (
        view.text && (
          <div className="overlay-text" aria-live="off">
            {view.text}
            {view.phase === "streaming" && <span className="caret" aria-hidden="true" />}
          </div>
        )
      )}

      {view.message && (view.phase === "refused" || view.phase === "error") && (
        <p className={view.phase === "error" ? "overlay-msg overlay-msg-error" : "overlay-msg"} role="alert">
          {keyOrText(view.message)}
        </p>
      )}

      {view.phase === "inserted" && (
        <p className="overlay-msg overlay-msg-good" role="status">
          {view.method === "clipboard"
            ? t("doubletap.overlay.insertedClipboard")
            : view.verified
              ? t("doubletap.overlay.insertedVerified")
              : t("doubletap.overlay.insertedUnverified")}
        </p>
      )}

      {(view.citations > 0 || view.excluded_sensitive > 0) && (
        <p className="overlay-meta muted small">
          {view.citations > 0 && <span>{t("doubletap.overlay.citations", { n: view.citations })}</span>}
          {view.excluded_sensitive > 0 && <span>{t("doubletap.overlay.withheld", { n: view.excluded_sensitive })}</span>}
        </p>
      )}

      {failure && (
        <p className="overlay-msg overlay-msg-error" role="alert">
          {failure}
        </p>
      )}

      <div className="overlay-foot">
        {acceptable || editing ? (
          <>
            <button type="button" className="btn btn-primary btn-small" disabled={!acceptable} onClick={() => apply(accept(ref.current))}>
              {t("doubletap.overlay.accept")}
              <kbd>{editing ? (mac ? "⌘↩" : "Ctrl+↩") : "↩"}</kbd>
            </button>
            {!editing && (
              <button type="button" className="btn btn-ghost btn-small" onClick={() => update(startEdit(ref.current))}>
                {t("doubletap.overlay.edit")}
                <kbd>⇥</kbd>
              </button>
            )}
          </>
        ) : null}
        <button type="button" className="btn btn-ghost btn-small" onClick={() => apply(cancel(ref.current))}>
          {isFinished(view.phase) ? t("doubletap.overlay.dismiss") : t("common.cancel")}
          <kbd>{t("doubletap.overlay.escKey")}</kbd>
        </button>
      </div>
    </main>
  );
}
