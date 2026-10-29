/** Splitting packed context / answers into text and `[n]` citation markers. */
import { t } from "./i18n";

export type CitationPart = { kind: "text"; text: string } | { kind: "cite"; n: number; raw: string };

const MARKER = /\[(\d{1,4})\]/g;

/**
 * Split `text` at `[n]` markers. Only numbers in `valid` become citations; any other bracketed
 * number stays plain text. Adjacent text parts are merged.
 */
export function splitCitations(text: string, valid: ReadonlySet<number>): CitationPart[] {
  const parts: CitationPart[] = [];
  const pushText = (t: string) => {
    if (!t) return;
    const last = parts[parts.length - 1];
    if (last?.kind === "text") last.text += t;
    else parts.push({ kind: "text", text: t });
  };
  let at = 0;
  for (const m of text.matchAll(MARKER)) {
    const n = Number(m[1]);
    const start = m.index;
    pushText(text.slice(at, start));
    if (valid.has(n)) parts.push({ kind: "cite", n, raw: m[0] });
    else pushText(m[0]);
    at = start + m[0].length;
  }
  pushText(text.slice(at));
  return parts;
}

/**
 * What to tell the user about sensitive items left out of an answer's context, or `null` when
 * none were. They are withheld because the answering model runs in the cloud.
 */
export function withheldNotice(excluded: number | null | undefined, provider: string): string | null {
  if (!excluded || excluded <= 0) return null;
  return t("search.withheld", { n: excluded, provider });
}

/** Code (fenced blocks and inline spans), where `[n]` is an index rather than a citation. */
const CODE = /(```[\s\S]*?(?:```|$)|`[^`\n]*`)/;
/**
 * A run of `[n]` markers (`[1]`, `[2][3]`) with the blanks before it. Not a markdown link
 * (`[1](url)`) or link definition (`[1]: url`), and not an index glued to a name (`arr[1]`).
 */
const MARKER_RUN = /(?:[ \t]+|(?<![\w\])]))(?:\[\d{1,4}\](?![(:]))+/g;
/** Such a run opening a line, with the blanks after it. */
const LINE_START_RUN = /^(?:\[\d{1,4}\](?![(:]))+[ \t]*/gm;

/**
 * `text` without its `[n]` citation markers. For an earlier reply sent back to the model as
 * history: the numbers referred to that reply's own passages and would mislead a later answer.
 * Code, links and other brackets (`[x]`, `[^1]`) are left alone.
 */
export function stripCitationMarkers(text: string): string {
  return text
    .split(CODE)
    .map((part, i) => (i % 2 === 1 ? part : part.replace(LINE_START_RUN, "").replace(MARKER_RUN, "")))
    .join("");
}
