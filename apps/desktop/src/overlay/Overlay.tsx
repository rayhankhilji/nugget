import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type RefObject } from "react";
import {
  doubletapApi,
  draftHistoryApi,
  errorMessage,
  overlayApi,
  OVERLAY_MAX_HEIGHT,
  OVERLAY_MIN_HEIGHT,
  REFINEMENTS,
  type DraftHistoryEntry,
  type OverlayView,
  type Refinement,
} from "../api";
import { keyOrText } from "../doubletap";
import { formatRelative, localityLabel } from "../format";
import { t as translate, useT, type MessageKey } from "../i18n";
import { textDir, type TextDir } from "../bidi";
import { detectOs } from "../onboarding/platform";
import {
  accept,
  acceptAsIs,
  applyView,
  canAccept,
  canInsertPrevious,
  canRefine,
  canRetry,
  cancel,
  edited,
  handleKey,
  initialState,
  insertPrevious,
  isFinished,
  refine,
  retry,
  setDraft,
  setHistoryOpen,
  settle,
  SHORTCUTS,
  startEdit,
  wordCount,
  type Effect,
  type OverlayState,
  type Step,
} from "./state";

const LOCALITY_TONE: Record<string, "good" | "accent" | "warn"> = { on_device: "good", local_network: "accent", cloud: "warn" };

/**
 * One pill for where the draft is written: the translated locality ("On device"), plus the
 * provider's id when it says more than that ("Cloud · anthropic"). An unknown locality (a newer
 * shell) is left out rather than shown raw; a generic provider id ("local") is left out too.
 */
function sourceBadge(provider: string | null, locality: string | null): { tone: string; locality: string | null; provider: string | null } | null {
  const known = locality != null && locality in LOCALITY_TONE;
  const loc = known ? localityLabel(locality) : null;
  const norm = (v: string) => v.trim().toLowerCase().replace(/[\s_-]+/g, " ");
  const generic = (p: string) => norm(p) === "local" || (locality != null && norm(p) === norm(locality)) || (loc != null && norm(p) === norm(loc));
  const prov = provider && provider.trim() && !generic(provider) ? provider.trim() : null;
  if (!loc && !prov) return null;
  return { tone: (known && LOCALITY_TONE[locality]) || "neutral", locality: loc, provider: prov };
}

const REFINE_LABEL: Record<Refinement, MessageKey> = {
  shorter: "doubletap.overlay.refine.shorter",
  longer: "doubletap.overlay.refine.longer",
  formal: "doubletap.overlay.refine.formal",
  casual: "doubletap.overlay.refine.casual",
};

const REFINING: Record<Refinement, MessageKey> = {
  shorter: "doubletap.overlay.refining.shorter",
  longer: "doubletap.overlay.refining.longer",
  formal: "doubletap.overlay.refining.formal",
  casual: "doubletap.overlay.refining.casual",
};

/** The phase line: "Making it shorter…" while a refinement is on its way, else the phase. */
function phaseText(view: OverlayView): string {
  if (view.refinement && (view.phase === "reading" || view.phase === "streaming")) return translate(REFINING[view.refinement]);
  return translate(`doubletap.overlay.phase.${view.phase}` as MessageKey);
}

function insertedText(view: OverlayView): string {
  if (view.method === "clipboard") return translate("doubletap.overlay.insertedClipboard");
  return view.verified ? translate("doubletap.overlay.insertedVerified") : translate("doubletap.overlay.insertedUnverified");
}

/**
 * What the live region says for a view: one line per state change, never the streamed text. The
 * same string while nothing changes, so it is read once (streaming repeats "Drafting…").
 */
export function announcement(view: OverlayView): string {
  switch (view.phase) {
    case "done": {
      const ready = translate("doubletap.overlay.announceReady", { n: wordCount(view.text) });
      // A refinement that missed after its second try says so with the draft.
      return view.message ? `${ready} ${keyOrText(view.message)}` : ready;
    }
    case "inserted":
      return insertedText(view);
    case "refused":
    case "error":
      return view.message ? keyOrText(view.message) : phaseText(view);
    default:
      return phaseText(view);
  }
}

/** Unix ms to the relative time the rest of the app shows. */
function ago(at: number): string {
  return Number.isFinite(at) ? formatRelative(new Date(at).toISOString()) : "";
}

/**
 * Ask the shell to fit the window to the content: the card's natural height plus whatever the
 * draft (or editor) would still need to show without scrolling. Coalesced to one call per frame,
 * only when it changes by 4 px or more; failures (an older shell, a test) are ignored.
 */
function useFitHeight(main: RefObject<HTMLElement | null>, scroller: RefObject<HTMLElement | null>) {
  const frame = useRef<number | null>(null);
  const last = useRef<number | null>(null);
  const schedule = useCallback(() => {
    if (frame.current !== null) return;
    const raf = typeof window.requestAnimationFrame === "function" ? window.requestAnimationFrame.bind(window) : (cb: FrameRequestCallback) => window.setTimeout(() => cb(0), 16);
    frame.current = raf(() => {
      frame.current = null;
      const box = main.current;
      if (!box) return;
      const inner = scroller.current;
      const extra = inner ? Math.max(0, inner.scrollHeight - inner.clientHeight) : 0;
      const want = Math.round(Math.min(OVERLAY_MAX_HEIGHT, Math.max(OVERLAY_MIN_HEIGHT, box.getBoundingClientRect().height + extra)));
      if (last.current !== null && Math.abs(want - last.current) < 4) return;
      last.current = want;
      overlayApi.resize(want).catch(() => {});
    });
  }, [main, scroller]);
  // Every render may change the content (a streamed delta, a new button row).
  useLayoutEffect(schedule);
  // Size changes that don't come from a render (fonts loading, the editor resized).
  useEffect(() => {
    if (typeof ResizeObserver === "undefined" || !main.current) return;
    const ro = new ResizeObserver(() => schedule());
    ro.observe(main.current);
    return () => ro.disconnect();
  }, [main, schedule]);
}

export function Overlay() {
  const t = useT();
  const [state, setState] = useState<OverlayState>(() => initialState());
  const ref = useRef(state);
  ref.current = state;
  const [failure, setFailure] = useState<string | null>(null);
  const [history, setHistory] = useState<DraftHistoryEntry[]>([]);
  const editor = useRef<HTMLTextAreaElement>(null);
  const mainRef = useRef<HTMLElement>(null);
  const textRef = useRef<HTMLDivElement>(null);
  const historyToggle = useRef<HTMLButtonElement>(null);
  const historyList = useRef<HTMLUListElement>(null);
  /** The list was opened from the keyboard: move focus into it. */
  const focusList = useRef(false);
  const mac = detectOs() === "macos";

  const update = useCallback((next: OverlayState) => {
    ref.current = next;
    setState(next);
  }, []);

  const run = useCallback(
    (effect: Effect) => {
      setFailure(null);
      let p: Promise<unknown>;
      switch (effect.kind) {
        case "accept":
          p = doubletapApi.overlayAccept(effect.text);
          break;
        case "cancel":
          p = doubletapApi.overlayCancel();
          break;
        case "retry":
          p = overlayApi.retry();
          break;
        case "refine":
          p = overlayApi.refine(effect.refinement, effect.base);
          break;
        case "insertPrevious":
          p = overlayApi.insertPrevious(effect.id);
          break;
      }
      if (effect.kind === "retry" || effect.kind === "refine") {
        // The button (or the editor) goes away with the new draft: keep focus on the card, where
        // the keys still work, rather than on nothing.
        mainRef.current?.focus();
      }
      p.catch((e: unknown) => {
        setFailure(errorMessage(e));
        // Let the user try again.
        if (effect.kind !== "cancel") update(settle(ref.current));
      });
    },
    [update],
  );

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
    const unlisteners: (() => void)[] = [];
    const listen = (p: Promise<() => void>) =>
      void p
        .then((u) => {
          if (alive) unlisteners.push(u);
          else u();
        })
        .catch(() => {});
    listen(doubletapApi.onOverlay((view) => alive && update(applyView(ref.current, view))));
    listen(draftHistoryApi.onChange((entries) => alive && setHistory(entries)));
    doubletapApi
      .overlayState()
      .then((view: OverlayView) => alive && update(applyView(ref.current, view)))
      .catch((e: unknown) => alive && setFailure(errorMessage(e)));
    // An older shell has no history: the button just stays hidden.
    draftHistoryApi
      .list()
      .then((entries) => alive && setHistory(entries))
      .catch(() => {});
    return () => {
      alive = false;
      unlisteners.forEach((u) => u());
    };
  }, [update]);

  const practice = state.view.practice === true;
  const hasHistory = history.length > 0 && !practice;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // A focused button handles its own Enter/Space and Tab moves between buttons; Esc and the
      // letter shortcuts still work there.
      if (e.target instanceof HTMLButtonElement && e.key !== "Escape" && !/^[a-z]$/i.test(e.key) && !/^Key[A-Z]$/.test(e.code)) return;
      const step = handleKey(ref.current, e, { history: hasHistory });
      if (step.preventDefault) e.preventDefault();
      if (step.state.historyOpen && !ref.current.historyOpen) focusList.current = true;
      apply(step);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [apply, hasHistory]);

  const editing = state.mode === "edit";
  // The editor keeps the direction the draft had when editing began, so typing never flips it.
  const editDir = useRef<TextDir>("auto");
  const wasEditing = useRef(false);
  if (editing && !wasEditing.current) editDir.current = textDir(state.draft);
  wasEditing.current = editing;
  useLayoutEffect(() => {
    if (!editing || !editor.current) return;
    const el = editor.current;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, [editing]);

  // Into the list when opened from the keyboard; back to its button when it closes under focus.
  const historyOpen = state.historyOpen && hasHistory;
  const wasOpen = useRef(false);
  useLayoutEffect(() => {
    if (historyOpen && focusList.current) historyList.current?.querySelector<HTMLButtonElement>("button")?.focus();
    focusList.current = false;
    if (!historyOpen && wasOpen.current && (document.activeElement === document.body || document.activeElement === null)) historyToggle.current?.focus();
    wasOpen.current = historyOpen;
  }, [historyOpen]);

  useFitHeight(mainRef, editing ? editor : textRef);

  const { view } = state;
  const acceptable = canAccept(state);
  const streaming = view.phase === "streaming";
  const badge = sourceBadge(view.provider, view.locality);
  const changed = editing && edited(state);
  const retryable = canRetry(state);
  const refinable = canRefine(state);
  const pickable = hasHistory && canInsertPrevious(state);
  const saying = announcement(view);

  const onListKey = (e: ReactKeyboardEvent<HTMLUListElement>) => {
    const items = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button"));
    const at = items.indexOf(document.activeElement as HTMLButtonElement);
    let next = -1;
    if (e.key === "ArrowDown") next = at < 0 ? 0 : Math.min(items.length - 1, at + 1);
    else if (e.key === "ArrowUp") next = at <= 0 ? 0 : at - 1;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = items.length - 1;
    if (next < 0) return;
    e.preventDefault();
    items[next]?.focus();
  };

  return (
    <main ref={mainRef} tabIndex={-1} className={`overlay overlay-${view.phase}`} aria-label={t("doubletap.title")}>
      {/* The window's one heading, for screen readers' heading navigation. */}
      <h1 className="visually-hidden">{t("doubletap.title")}</h1>
      <div className="visually-hidden" aria-live="polite" aria-atomic="true">
        {saying}
      </div>
      <div className="overlay-head">
        <span className="overlay-phase">
          {(view.phase === "reading" || view.phase === "inserting") && <span className="spinner" aria-hidden="true" />}
          {(view.phase === "error" || view.phase === "refused") && <span className="overlay-icon" aria-hidden="true" />}
          {phaseText(view)}
        </span>
        {(badge || practice) && (
          <span className="overlay-badges">
            {practice && <span className="badge badge-accent">{t("doubletap.overlay.practice")}</span>}
            {badge && (
              <span className={`badge badge-${badge.tone}`}>
                {badge.locality}
                {badge.locality && badge.provider && " · "}
                {badge.provider && <bdi>{badge.provider}</bdi>}
              </span>
            )}
          </span>
        )}
      </div>

      {editing ? (
        <textarea
          ref={editor}
          className="overlay-editor"
          dir={editDir.current}
          aria-label={t("doubletap.overlay.editLabel")}
          value={state.draft}
          onChange={(e) => update(setDraft(ref.current, e.target.value))}
          rows={5}
        />
      ) : (
        view.text && (
          // Focusable so the draft can be scrolled and read; Tab from here still starts editing.
          <div ref={textRef} className="overlay-text" dir={textDir(view.text)} role="region" tabIndex={0} aria-label={t("doubletap.overlay.draftLabel")}>
            {view.text}
            {view.phase === "streaming" && <span className="caret" aria-hidden="true" />}
          </div>
        )
      )}

      {view.message && view.phase === "done" && (
        <p className="overlay-hint muted small" dir="auto">
          {keyOrText(view.message)}
        </p>
      )}

      {view.message && (view.phase === "refused" || view.phase === "error") && (
        <p className={view.phase === "error" ? "overlay-msg overlay-msg-error" : "overlay-msg"} dir="auto">
          {keyOrText(view.message)}
        </p>
      )}

      {view.phase === "error" && !retryable && <p className="overlay-hint muted small">{t("doubletap.overlay.retryHint")}</p>}

      {view.phase === "inserted" && <p className="overlay-msg overlay-msg-good">{insertedText(view)}</p>}

      {(view.citations > 0 || view.excluded_sensitive > 0) && (
        <p className="overlay-meta muted small">
          {view.citations > 0 && <span>{t("doubletap.overlay.citations", { n: view.citations })}</span>}
          {view.excluded_sensitive > 0 && <span>{t("doubletap.overlay.withheld", { n: view.excluded_sensitive })}</span>}
        </p>
      )}

      {failure && (
        <p className="overlay-msg overlay-msg-error" dir="auto" role="alert">
          {failure}
        </p>
      )}

      {(retryable || refinable || pickable) && (
        <div className="overlay-tools">
          {retryable && (
            <button type="button" className="btn btn-ghost btn-small" aria-keyshortcuts={editing ? undefined : SHORTCUTS.retry} onClick={() => apply(retry(ref.current))}>
              {t("doubletap.overlay.retry")}
              {!editing && <kbd aria-hidden="true">{SHORTCUTS.retry}</kbd>}
            </button>
          )}
          {refinable && (
            <div className="overlay-refine" role="group" aria-label={t("doubletap.overlay.refineGroup")}>
              {REFINEMENTS.map((r) => (
                <button
                  key={r}
                  type="button"
                  className="btn btn-ghost btn-small"
                  aria-keyshortcuts={editing ? undefined : SHORTCUTS[r]}
                  onClick={() => apply(refine(ref.current, r))}
                >
                  {t(REFINE_LABEL[r])}
                  {!editing && <kbd aria-hidden="true">{SHORTCUTS[r]}</kbd>}
                </button>
              ))}
            </div>
          )}
          {pickable && (
            <button
              ref={historyToggle}
              type="button"
              className="btn btn-ghost btn-small"
              aria-expanded={historyOpen}
              aria-controls="overlay-history"
              aria-keyshortcuts={editing ? undefined : SHORTCUTS.history}
              onClick={() => update(setHistoryOpen(ref.current, !ref.current.historyOpen))}
            >
              {t("doubletap.overlay.history")}
              {!editing && <kbd aria-hidden="true">{SHORTCUTS.history}</kbd>}
            </button>
          )}
        </div>
      )}

      {historyOpen && pickable && (
        <ul id="overlay-history" ref={historyList} className="overlay-history" aria-label={t("doubletap.overlay.history")} onKeyDown={onListKey}>
          {history.map((h) => (
            <li key={h.id}>
              <button type="button" className="overlay-history-item" onClick={() => apply(insertPrevious(ref.current, h.id))}>
                <span className="overlay-history-text" dir="auto">
                  {h.text}
                </span>
                <span className="overlay-history-meta muted small">
                  <bdi>{h.app || t("doubletap.history.unknownApp")}</bdi>
                  {" · "}
                  {ago(h.at)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className="overlay-foot">
        {acceptable || editing ? (
          changed ? (
            <>
              <button type="button" className="btn btn-small btn-primary" disabled={!acceptable} onClick={() => apply(accept(ref.current))}>
                {t("doubletap.overlay.insert")}
                <kbd>{mac ? "⌘↩" : "Ctrl+↩"}</kbd>
              </button>
              <button
                type="button"
                className="btn btn-ghost btn-small"
                disabled={!acceptable}
                title={t("doubletap.overlay.insertAsIsHint")}
                aria-keyshortcuts={mac ? "Meta+Shift+Enter" : "Control+Shift+Enter"}
                onClick={() => apply(acceptAsIs(ref.current))}
              >
                {t("doubletap.overlay.insertAsIs")}
                <kbd>{mac ? "⌘⇧↩" : "Ctrl+Shift+↩"}</kbd>
              </button>
            </>
          ) : (
            <>
              <button
                type="button"
                className={`btn btn-small${streaming && !editing ? "" : " btn-primary"}`}
                disabled={!acceptable}
                title={streaming && !editing ? t("doubletap.overlay.acceptSoFarHint") : undefined}
                onClick={() => apply(accept(ref.current))}
              >
                {streaming && !editing ? t("doubletap.overlay.acceptSoFar") : t("doubletap.overlay.accept")}
                <kbd>{editing ? (mac ? "⌘↩" : "Ctrl+↩") : "↩"}</kbd>
              </button>
              {!editing && (
                <button type="button" className="btn btn-ghost btn-small" onClick={() => update(startEdit(ref.current))}>
                  {t("doubletap.overlay.edit")}
                  <kbd>⇥</kbd>
                </button>
              )}
            </>
          )
        ) : null}
        <button type="button" className="btn btn-ghost btn-small" onClick={() => apply(cancel(ref.current))}>
          {isFinished(view.phase) ? t("doubletap.overlay.dismiss") : t("common.cancel")}
          <kbd>{t("doubletap.overlay.escKey")}</kbd>
        </button>
      </div>
    </main>
  );
}
