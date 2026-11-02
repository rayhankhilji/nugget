import { useEffect, useState } from "react";
import { ghostApi, type GhostPaint } from "../api";
import { isMac } from "../hooks.shortcuts";
import { ghostStyle, ghostText, splitTail } from "./ghost";

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
          {paint.words && <kbd>{isMac() ? WORD_KEY_MAC : WORD_KEY}</kbd>}
          <kbd>{TAB_KEY}</kbd>
        </span>
      )}
    </div>
  );
}
