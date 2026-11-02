import { useEffect, useState } from "react";
import { ghostApi, type GhostPaint } from "../api";
import { isMac } from "../hooks.shortcuts";
import { ghostStyle, ghostText, splitTail } from "./ghost";

/**
 * The keys hint teaches Tab (and the word key) on the first few suggestions only; after that,
 * the dot alone marks a suggestion. The count is kept in this webview's own storage.
 */
const HINT_SUGGESTIONS = 3;
const HINT_KEY = "heed.ghost.hintsShown";

function hintsShown(): number {
  try {
    return Number(localStorage.getItem(HINT_KEY)) || 0;
  } catch {
    return HINT_SUGGESTIONS;
  }
}

/** Key names on the hint, the same in every language. */
const TAB_KEY = "Tab";
const WORD_KEY_MAC = "⌥→";
const WORD_KEY = "Ctrl+→";

const HIDDEN: GhostPaint = {
  visible: false,
  text: "",
  streaming: false,
  rtl: false,
  words: false,
  font_px: 13,
  line_px: 16,
  indent_px: 0,
  color: null,
};

/**
 * The ghost text window: the suggestion, translucent, over the user's field. The window ignores
 * the mouse and never takes focus; Tab, ⌥→ / Ctrl+→ and Esc are read by the shell's key tap, so
 * nothing here handles input.
 */
export function Ghost({ initial = HIDDEN }: { initial?: GhostPaint }) {
  const [paint, setPaint] = useState<GhostPaint>(initial);
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    ghostApi
      .onPaint((p) => alive && setPaint(p))
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      })
      .catch(() => {});
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);
  const text = ghostText(paint);
  const done = text !== "" && !paint.streaming;
  // Decide once per suggestion whether it shows the keys, and count the ones that did.
  const [hint, setHint] = useState<{ text: string; keys: boolean }>({ text: "", keys: false });
  useEffect(() => {
    if (!done || hint.text === text) return;
    const n = hintsShown();
    const keys = n < HINT_SUGGESTIONS;
    if (keys) {
      try {
        localStorage.setItem(HINT_KEY, String(n + 1));
      } catch {}
    }
    setHint({ text, keys });
  }, [done, text, hint.text]);
  const showKeys = hint.text === text ? hint.keys : hintsShown() < HINT_SUGGESTIONS;
  const { opacity, ...box } = ghostStyle(paint);
  // While it streams, the newest words shimmer in the brand colour; the rest is plain ghost text.
  const [head, tail] = paint.streaming ? splitTail(text) : [text, ""];
  // Read by no one: the window is not in the accessibility tree's focus path, and the user's
  // screen reader stays in their app.
  return (
    <div className="ghost" aria-hidden="true" data-streaming={paint.streaming || undefined} style={box}>
      <span className="ghost-text" style={{ opacity }}>
        {head}
      </span>
      {tail && <span className="ghost-tail">{tail}</span>}
      {text && !paint.streaming && (
        <span className="ghost-chip">
          <i />
          {showKeys && paint.words && <kbd>{isMac() ? WORD_KEY_MAC : WORD_KEY}</kbd>}
          {showKeys && <kbd>{TAB_KEY}</kbd>}
        </span>
      )}
    </div>
  );
}
