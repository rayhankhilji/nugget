import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { useT } from "../i18n";

const FOCUSABLE =
  'button:not([disabled]), [href], input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * The Tab stops inside `root`, in order: focusable, rendered, not taken out of the tab order,
 * and one per radio group (its checked radio, else its first), as the browser tabs through them.
 */
export function tabStops(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((el) => {
    if (el.tabIndex < 0 || !rendered(el) || el.closest("[inert]")) return false;
    if (!(el instanceof HTMLInputElement) || el.type !== "radio" || !el.name) return true;
    const group = Array.from(root.querySelectorAll<HTMLInputElement>('input[type="radio"]')).filter((r) => r.name === el.name);
    return el.checked || (!group.some((r) => r.checked) && group[0] === el);
  });
}

/** Laid out and visible (`checkVisibility` where the engine has it, else not inside `[hidden]`). */
function rendered(el: HTMLElement): boolean {
  return typeof el.checkVisibility === "function" ? el.checkVisibility({ visibilityProperty: true }) : !el.closest("[hidden]");
}

/**
 * Keep Tab inside `root`: from the last stop it wraps to the first, and Shift+Tab from the first
 * to the last. A modal `<dialog>` alone lets focus leave the page (to the browser or webview
 * chrome, leaving nothing focused) before it comes back.
 */
export function wrapTab(e: KeyboardEvent<HTMLElement>, root: HTMLElement): void {
  if (e.key !== "Tab" || e.altKey || e.ctrlKey || e.metaKey || e.defaultPrevented) return;
  const stops = tabStops(root);
  const active = document.activeElement;
  const first = stops[0];
  const last = stops[stops.length - 1];
  if (!first || !last) {
    e.preventDefault();
    return;
  }
  const inside = active instanceof HTMLElement && root.contains(active) && active !== root;
  if (e.shiftKey && (!inside || active === first)) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && (!inside || active === last)) {
    e.preventDefault();
    first.focus();
  }
}

/**
 * Modal dialog on the native `<dialog>` element (inert background and Escape come from the
 * platform; Tab wraps at the ends, see `wrapTab`). Mount it to open, unmount to close. On open,
 * focus goes to the element marked `data-autofocus` (else the first focusable one); on close it
 * returns to whatever had focus before. The title bar is fixed and the body scrolls; give the
 * buttons a `.dialog-actions` wrapper and they stay visible at the bottom.
 */
export function Dialog({
  title,
  description,
  onClose,
  children,
  wide = false,
}: {
  title: string;
  description?: ReactNode;
  /** Escape, the close button, or the caller's own Cancel button. */
  onClose: () => void;
  children: ReactNode;
  wide?: boolean;
}) {
  const t = useT();
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const descId = useId();
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  // Taken on first render: by the time effects run, children may already have moved focus
  // (autoFocus, their own effects), and the element to return to would be lost.
  const [opener] = useState(() => (document.activeElement instanceof HTMLElement ? document.activeElement : null));

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (!el.open) el.showModal();
    const target = el.querySelector<HTMLElement>("[data-autofocus]") ?? el.querySelector<HTMLElement>(FOCUSABLE);
    target?.focus();

    // Escape: let React state decide, instead of the element closing itself.
    const onCancel = (e: Event) => {
      e.preventDefault();
      onCloseRef.current();
    };
    el.addEventListener("cancel", onCancel);
    return () => {
      el.removeEventListener("cancel", onCancel);
      if (el.open) el.close();
      if (opener?.isConnected) opener.focus();
    };
  }, []);

  return (
    <dialog
      ref={ref}
      className={wide ? "dialog dialog-wide" : "dialog"}
      aria-labelledby={titleId}
      aria-describedby={description ? descId : undefined}
      onKeyDown={(e) => ref.current && wrapTab(e, ref.current)}
    >
      <div className="dialog-head">
        <h2 id={titleId}>{title}</h2>
        <button type="button" className="btn btn-ghost btn-icon" onClick={onClose} aria-label={t("common.close")}>
          <svg width="14" height="14" viewBox="0 0 14 14" aria-hidden="true" focusable="false">
            <path d="M2 2l10 10M12 2L2 12" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
          </svg>
        </button>
      </div>
      {/* The title bar stays put; this scrolls, and a `.dialog-actions` footer inside it sticks
          to the bottom so the dialog's buttons are always in view. */}
      <div className="dialog-body">
        {description && (
          <p id={descId} className="muted dialog-desc">
            {description}
          </p>
        )}
        {children}
      </div>
    </dialog>
  );
}
