import { useEffect, useRef, useState } from "react";

/**
 * Copy `text` to the clipboard. Falls back to a temporary off-screen textarea when the async
 * clipboard API is unavailable; that element is removed straight away. Never logs `text`.
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    /* fall through */
  }
  try {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.left = "-9999px";
    document.body.appendChild(ta);
    ta.select();
    const ok = document.execCommand("copy");
    ta.value = "";
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}

/** A Copy button that says "Copied" for a moment; the result is announced to screen readers. */
export function CopyButton({
  text,
  label = "Copy",
  what,
  onCopied,
  className = "btn btn-small",
}: {
  text: string;
  label?: string;
  /** What is being copied, for the accessible name ("Copy token"). */
  what: string;
  onCopied?: () => void;
  className?: string;
}) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const copy = async () => {
    const ok = await copyText(text);
    setState(ok ? "copied" : "failed");
    if (ok) onCopied?.();
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setState("idle"), 2000);
  };

  return (
    <>
      <button type="button" className={className} onClick={() => void copy()} aria-label={`${label} ${what}`}>
        {state === "copied" ? "Copied" : state === "failed" ? "Copy failed" : label}
      </button>
      <span className="visually-hidden" aria-live="polite">
        {state === "copied" ? `${what} copied to the clipboard.` : state === "failed" ? `Couldn't copy the ${what}. Select it and copy it yourself.` : ""}
      </span>
    </>
  );
}
