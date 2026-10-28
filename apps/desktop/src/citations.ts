/** Splitting packed context / answers into text and `[n]` citation markers. */

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
  const items = excluded === 1 ? "1 sensitive item was" : `${excluded} sensitive items were`;
  return `${items} left out because ${provider} is a cloud model. Sensitive items are only sent to models on this device or your network.`;
}
