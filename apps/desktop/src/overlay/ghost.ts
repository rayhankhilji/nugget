import type { CSSProperties } from "react";
import type { GhostPaint } from "../api";

/** How translucent ghost text is: readable, but clearly not typed yet. */
export const GHOST_OPACITY = 0.45;
/** Used when the app reports no text colour: mid grey reads on light and dark fields alike. */
export const FALLBACK_COLOR = "#808080";

const HEX = /^#(?:[0-9a-f]{6}|[0-9a-f]{8})$/i;

/** Only a plain hex colour from the shell is used (never arbitrary CSS). */
export function safeColor(c: string | null | undefined): string {
  return c && HEX.test(c) ? c : FALLBACK_COLOR;
}

function px(v: number, min: number, max: number, fallback: number): number {
  return Number.isFinite(v) && v > 0 ? Math.min(max, Math.max(min, v)) : fallback;
}

/** The text's style: the field's size and colour at ghost opacity, the first line indented up to the caret. */
export function ghostStyle(p: GhostPaint): CSSProperties {
  const font = px(p.font_px, 6, 96, 13);
  const line = Math.max(px(p.line_px, 6, 200, font * 1.25), font);
  return {
    fontSize: `${font}px`,
    lineHeight: `${line}px`,
    textIndent: `${px(p.indent_px, 0, 4000, 0)}px`,
    color: safeColor(p.color),
    opacity: GHOST_OPACITY,
    direction: p.rtl ? "rtl" : "ltr",
  };
}

/**
 * The text as drawn: a leading newline is kept as a line break (CSS `pre-wrap`), and a run of
 * trailing whitespace is dropped (Tab inserts the text, not the trailing space).
 */
export function ghostText(p: GhostPaint): string {
  return p.visible ? p.text.replace(/[ \t]+$/u, "") : "";
}

/** The last few words (about 24 characters, cut at a space), which shimmer while text streams in. */
export function splitTail(text: string, chars = 24): [string, string] {
  if (text.length <= chars) return ["", text];
  const cut = text.lastIndexOf(" ", text.length - chars);
  const at = cut > 0 ? cut : text.length - chars;
  return [text.slice(0, at), text.slice(at)];
}
