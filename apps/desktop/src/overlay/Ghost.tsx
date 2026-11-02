import { useEffect, useState } from "react";
import { ghostApi, type GhostPaint } from "../api";
import { ghostStyle, ghostText } from "./ghost";

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
  // Read by no one: the window is not in the accessibility tree's focus path, and the user's
  // screen reader stays in their app.
  return (
    <div className="ghost" aria-hidden="true" data-streaming={paint.streaming || undefined} style={ghostStyle(paint)}>
      {text}
    </div>
  );
}
