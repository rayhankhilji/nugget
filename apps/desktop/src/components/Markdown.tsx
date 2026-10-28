/**
 * A small, safe Markdown renderer for assistant replies. It builds React elements directly
 * (never HTML strings), so raw HTML in a reply is shown as text. Links open only through the
 * caller's `onOpenLink` (the shell's `open_url`), and only for `https:` URLs; the webview never
 * navigates. It tolerates partial input, so it can render a reply while it streams in (an
 * unterminated code fence renders as an open code block).
 */
import { Fragment, useMemo, type MouseEvent, type ReactNode } from "react";
import { isOpenableUrl } from "../api";
import { useT } from "../i18n";
import { CopyButton } from "./CopyButton";
import "../styles-chat.css";

// ---- blocks ----

export type Align = "left" | "center" | "right" | null;

export type Block =
  | { t: "p"; text: string }
  | { t: "h"; level: number; text: string }
  | { t: "code"; lang: string; code: string; open: boolean }
  | { t: "hr" }
  | { t: "quote"; blocks: Block[] }
  | { t: "list"; ordered: boolean; start: number; tight: boolean; items: Block[][] }
  | { t: "table"; head: string[]; align: Align[]; rows: string[][] };

/** Deeper nesting (e.g. `>>>>>>…`) is shown as text rather than recursing further. */
const MAX_DEPTH = 16;

const FENCE = /^( {0,3})(`{3,}|~{3,})(.*)$/;
const HEADING = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?[ \t]*$/;
const HR = /^ {0,3}(?:(?:-[ \t]*){3,}|(?:\*[ \t]*){3,}|(?:_[ \t]*){3,})$/;
const QUOTE = /^ {0,3}> ?/;
const LIST_ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:( +)(.*))?$/;
const TABLE_SEP = /^ *\|? *:?-+:? *(?:\| *:?-+:? *)*\|? *$/;

/** `lines[i]`, or "" past the end. */
const at = (lines: readonly string[], i: number): string => lines[i] ?? "";
/** Group `k` of a match, or "" when it didn't participate. */
const g = (m: RegExpExecArray, k: number): string => m[k] ?? "";
/** Whether group `k` participated in the match. */
const has = (m: RegExpExecArray, k: number): boolean => m[k] !== undefined;

const isBlank = (l: string) => l.trim() === "";
const indentOf = (l: string) => l.length - l.trimStart().length;

function isTableStart(lines: string[], i: number): boolean {
  return at(lines, i).includes("|") && i + 1 < lines.length && at(lines, i + 1).includes("-") && TABLE_SEP.test(at(lines, i + 1));
}

/** A line that ends a paragraph because it starts another block. */
function startsBlock(lines: string[], i: number): boolean {
  const l = at(lines, i);
  return FENCE.test(l) || HEADING.test(l) || HR.test(l) || QUOTE.test(l) || isListStart(l) || isTableStart(lines, i);
}

function isListStart(l: string): boolean {
  const m = LIST_ITEM.exec(l);
  return !!m && has(m, 4) && g(m, 4).trim() !== "";
}

function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  const cells: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (s[i] === "|") {
      cells.push(cur.trim());
      cur = "";
    } else cur += s[i];
  }
  cells.push(cur.trim());
  return cells;
}

export function parseBlocks(src: string, depth = 0): Block[] {
  if (depth > MAX_DEPTH) return src.trim() ? [{ t: "p", text: src.trim() }] : [];
  const lines = src
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map((l) => l.replace(/^\t+/, (m) => "    ".repeat(m.length)));
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = at(lines, i);
    if (isBlank(line)) {
      i++;
      continue;
    }

    const fence = FENCE.exec(line);
    if (fence && !(g(fence, 2)[0] === "`" && g(fence, 3).includes("`"))) {
      const indent = g(fence, 1).length;
      const mark = g(fence, 2);
      const body: string[] = [];
      let closed = false;
      i++;
      while (i < lines.length) {
        const close = /^ {0,3}(`{3,}|~{3,})[ \t]*$/.exec(at(lines, i));
        if (close && g(close, 1)[0] === mark[0] && g(close, 1).length >= mark.length) {
          closed = true;
          i++;
          break;
        }
        body.push(at(lines, i).slice(Math.min(indent, indentOf(at(lines, i)))));
        i++;
      }
      out.push({ t: "code", lang: g(fence, 3).trim().split(/\s+/)[0] ?? "", code: body.join("\n"), open: !closed });
      continue;
    }

    const heading = HEADING.exec(line);
    if (heading) {
      out.push({ t: "h", level: g(heading, 1).length, text: (g(heading, 2) ?? "").replace(/[ \t]+#+$/, "").replace(/^#+$/, "") });
      i++;
      continue;
    }

    if (HR.test(line)) {
      out.push({ t: "hr" });
      i++;
      continue;
    }

    if (QUOTE.test(line)) {
      const body: string[] = [];
      while (i < lines.length && QUOTE.test(at(lines, i))) {
        body.push(at(lines, i).replace(QUOTE, ""));
        i++;
      }
      out.push({ t: "quote", blocks: parseBlocks(body.join("\n"), depth + 1) });
      continue;
    }

    if (isListStart(line)) {
      i = parseList(lines, i, depth, out);
      continue;
    }

    if (isTableStart(lines, i)) {
      const head = splitRow(line);
      const align: Align[] = splitRow(at(lines, i + 1)).map((c) =>
        c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : null,
      );
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && !isBlank(at(lines, i)) && at(lines, i).includes("|")) {
        const cells = splitRow(at(lines, i)).slice(0, head.length);
        while (cells.length < head.length) cells.push("");
        rows.push(cells);
        i++;
      }
      out.push({ t: "table", head, align: head.map((_, k) => align[k] ?? null), rows });
      continue;
    }

    const para: string[] = [line.trim()];
    i++;
    while (i < lines.length && !isBlank(at(lines, i)) && !startsBlock(lines, i)) {
      para.push(at(lines, i).trim());
      i++;
    }
    out.push({ t: "p", text: para.join("\n") });
  }
  return out;
}

/** Parse a list starting at `at(lines, start)`; returns the index after it. */
function parseList(lines: string[], start: number, depth: number, out: Block[]): number {
  const first = LIST_ITEM.exec(at(lines, start))!;
  const baseIndent = g(first, 1).length;
  const ordered = /\d/.test(g(first, 2));
  const delim = g(first, 2).slice(-1);
  const sameKind = (m: RegExpExecArray) => /\d/.test(g(m, 2)) === ordered && g(m, 2).slice(-1) === delim;

  const items: string[][] = [];
  let tight = true;
  let i = start;
  while (i < lines.length) {
    const m = LIST_ITEM.exec(at(lines, i));
    if (!m || g(m, 1).length > baseIndent + 3 || !sameKind(m) || !has(m, 4)) break;
    const offset = g(m, 1).length + g(m, 2).length + Math.min(g(m, 3).length, 4);
    const body = [g(m, 4)];
    i++;
    let sawBlank = false;
    while (i < lines.length) {
      const l = at(lines, i);
      if (isBlank(l)) {
        sawBlank = true;
        body.push("");
        i++;
        continue;
      }
      const ind = indentOf(l);
      const marker = LIST_ITEM.exec(l);
      if (ind >= offset) {
        body.push(l.slice(offset));
      } else if (marker && has(marker, 4) && ind > baseIndent) {
        // Nested list indented less than the content column (common in model output).
        body.push(l.slice(ind));
      } else if (!sawBlank && !(marker && has(marker, 4)) && !startsBlock(lines, i)) {
        body.push(l.trim()); // lazy continuation of the item's paragraph
      } else {
        break;
      }
      if (sawBlank) tight = false;
      sawBlank = false;
      i++;
    }
    while (body.length && body[body.length - 1] === "") body.pop();
    items.push(body);
    // A blank line before the next item makes the list loose.
    if (sawBlank && i < lines.length) {
      const next = LIST_ITEM.exec(at(lines, i));
      if (next && sameKind(next) && g(next, 1).length <= baseIndent + 3) tight = false;
    }
  }
  out.push({
    t: "list",
    ordered,
    start: ordered ? Math.min(Number.parseInt(g(first, 2), 10) || 1, 1_000_000) : 1,
    tight,
    items: items.map((b) => parseBlocks(b.join("\n"), depth + 1)),
  });
  return i;
}

// ---- inline ----

export interface CitationLinks {
  /** `[n]` markers with an entry here become buttons. */
  valid: ReadonlySet<number>;
  label: (n: number) => string;
  onCite: (n: number) => void;
}

interface Ctx {
  onOpenLink?: (url: string, label: string) => void;
  cite?: CitationLinks;
  /** Inside a link label: no nested links or citation buttons. */
  inLink?: boolean;
}

/** The URL if the link may be offered as clickable: `https:` only, and acceptable to `open_url`. */
export function safeHref(url: string): string | null {
  const u = url.trim();
  if (!/^https:/i.test(u)) return null;
  return isOpenableUrl(u) ? u : null;
}

const ESCAPABLE = /[!-/:-@[-`{-~]/;
const isSpace = (c: string | undefined) => c === undefined || /\s/.test(c);
const isWordChar = (c: string | undefined) => c !== undefined && /[\p{L}\p{N}]/u.test(c);
const MAX_INLINE_DEPTH = 8;

function textOf(nodes: ReactNode[]): string {
  return nodes.map((n) => (typeof n === "string" ? n : "")).join("");
}

/** Index of the `]` closing the `[` at `open`, honouring nesting and escapes; -1 if none. */
function closingBracket(s: string, open: number): number {
  let depth = 0;
  for (let j = open; j < s.length; j++) {
    if (s[j] === "\\") j++;
    else if (s[j] === "[") depth++;
    else if (s[j] === "]" && --depth === 0) return j;
    else if (s[j] === "\n" && s[j + 1] === "\n") return -1;
  }
  return -1;
}

/** Index of the `)` closing the `(` at `open`; -1 if none. */
function closingParen(s: string, open: number): number {
  let depth = 0;
  for (let j = open; j < s.length; j++) {
    if (s[j] === "\\") j++;
    else if (s[j] === "(") depth++;
    else if (s[j] === ")" && --depth === 0) return j;
    else if (s[j] === "\n") return -1;
  }
  return -1;
}

/** Strip trailing punctuation from a bare URL (and a `)` it doesn't open). */
function trimBareUrl(u: string): string {
  let url = u;
  for (;;) {
    const last = url[url.length - 1] ?? "";
    if (/[.,;:!?'"*_~\]]/.test(last)) url = url.slice(0, -1);
    else if (last === ")" && (url.match(/\(/g)?.length ?? 0) < (url.match(/\)/g)?.length ?? 0)) url = url.slice(0, -1);
    else return url;
  }
}

export function parseInline(s: string, ctx: Ctx, depth = 0): ReactNode[] {
  if (depth > MAX_INLINE_DEPTH) return [s];
  const out: ReactNode[] = [];
  let buf = "";
  let key = 0;
  const flush = () => {
    if (buf) out.push(buf);
    buf = "";
  };
  const push = (n: ReactNode) => {
    flush();
    out.push(<Fragment key={key++}>{n}</Fragment>);
  };

  let i = 0;
  while (i < s.length) {
    const c = s[i];

    if (c === "\\" && i + 1 < s.length && ESCAPABLE.test(s[i + 1] ?? "")) {
      buf += s[i + 1];
      i += 2;
      continue;
    }

    if (c === "\n") {
      push(<br />);
      i++;
      continue;
    }

    if (c === "`") {
      let n = 1;
      while (s[i + n] === "`") n++;
      const fence = "`".repeat(n);
      let j = s.indexOf(fence, i + n);
      while (j !== -1 && s[j + n] === "`") {
        let k = j;
        while (s[k] === "`") k++;
        j = s.indexOf(fence, k);
      }
      if (j === -1) {
        buf += fence;
        i += n;
        continue;
      }
      let code = s.slice(i + n, j).replace(/\n/g, " ");
      if (code.length > 2 && code.startsWith(" ") && code.endsWith(" ") && code.trim()) code = code.slice(1, -1);
      push(<code className="md-inline-code">{code}</code>);
      i = j + n;
      continue;
    }

    if (c === "[" && !ctx.inLink) {
      const close = closingBracket(s, i);
      if (close > i && s[close + 1] === "(") {
        const end = closingParen(s, close + 1);
        if (end > close) {
          let dest = s.slice(close + 2, end).trim();
          // Drop an optional title: [x](url "title")
          dest = dest.replace(/\s+(?:"[^"]*"|'[^']*')$/, "");
          if (dest.startsWith("<") && dest.endsWith(">")) dest = dest.slice(1, -1);
          const label = parseInline(s.slice(i + 1, close), { ...ctx, inLink: true }, depth + 1);
          push(<Link url={dest} label={label} ctx={ctx} />);
          i = end + 1;
          continue;
        }
      }
      const cite = /^\[(\d{1,4})\]/.exec(s.slice(i, i + 6));
      if (cite && ctx.cite?.valid.has(Number(cite[1]))) {
        const n = Number(cite[1]);
        const cl = ctx.cite;
        push(
          <button type="button" className="cite" onClick={() => cl.onCite(n)} aria-label={cl.label(n)}>
            {cite[0]}
          </button>,
        );
        i += cite[0].length;
        continue;
      }
    }

    if (c === "<" && !ctx.inLink) {
      const auto = /^<((?:https?|mailto):[^\s<>]+)>/i.exec(s.slice(i, i + 2050));
      if (auto) {
        push(<Link url={g(auto, 1)} label={[g(auto, 1)]} ctx={ctx} />);
        i += auto[0].length;
        continue;
      }
    }

    if ((c === "h" || c === "H") && !ctx.inLink && !isWordChar(s[i - 1])) {
      const bare = /^https?:\/\/[^\s<>"]+/i.exec(s.slice(i, i + 2050));
      if (bare) {
        const url = trimBareUrl(bare[0]);
        if (url.length > 8) {
          push(<Link url={url} label={[url]} ctx={ctx} />);
          i += url.length;
          continue;
        }
      }
    }

    if (c === "*" || c === "_" || c === "~") {
      let run = 1;
      while (s[i + run] === c) run++;
      const r = c === "~" ? 2 : Math.min(run, 3);
      const canOpen =
        (c !== "~" || run === 2) && !isSpace(s[i + run]) && !(c === "_" && isWordChar(s[i - 1]));
      if (canOpen) {
        const delim = c.repeat(r);
        let j = s.indexOf(delim, i + run);
        while (j !== -1) {
          const okClose = !isSpace(s[j - 1]) && !(c === "_" && isWordChar(s[j + r])) && s[j + r] !== c;
          if (okClose) break;
          j = s.indexOf(delim, j + 1);
        }
        if (j !== -1 && j > i + r) {
          // Extra delimiters beyond `r` at the opening stay literal inside.
          const inner = parseInline(s.slice(i + r, j), ctx, depth + 1);
          const node =
            c === "~" ? (
              <del>{inner}</del>
            ) : r === 1 ? (
              <em>{inner}</em>
            ) : r === 2 ? (
              <strong>{inner}</strong>
            ) : (
              <strong>
                <em>{inner}</em>
              </strong>
            );
          push(node);
          i = j + r;
          continue;
        }
      }
      buf += c.repeat(run);
      i += run;
      continue;
    }

    buf += c;
    i++;
  }
  flush();
  return out;
}

function Link({ url, label, ctx }: { url: string; label: ReactNode[]; ctx: Ctx }) {
  const href = safeHref(url);
  const labelText = textOf(label) || url;
  if (href && ctx.onOpenLink) {
    const open = ctx.onOpenLink;
    const onClick = (e: MouseEvent) => {
      e.preventDefault();
      open(href, labelText);
    };
    return (
      <a href={href} className="md-link" title={href} onClick={onClick} onAuxClick={(e) => e.preventDefault()} rel="noreferrer noopener">
        {label}
      </a>
    );
  }
  // Not openable here: show the text, plus the address for http/mailto so it can be copied.
  const showUrl = /^(?:https?|mailto):/i.test(url.trim()) && labelText !== url;
  return (
    <span className="md-link-text">
      {label}
      {showUrl ? ` (${url})` : null}
    </span>
  );
}

// ---- rendering ----

function CodeBlock({ lang, code, open }: { lang: string; code: string; open: boolean }) {
  const t = useT();
  return (
    <div className="md-code" data-open={open || undefined}>
      <div className="md-code-bar">
        <span className="md-code-lang">{lang || t("chat.md.code")}</span>
        <CopyButton text={code} what={t("chat.md.codeWhat")} className="btn btn-small btn-ghost" />
      </div>
      {/* Focusable so keyboard users can scroll long lines. */}
      <pre tabIndex={0}>
        <code>{code}</code>
      </pre>
    </div>
  );
}

function renderBlocks(blocks: Block[], ctx: Ctx, tight = false): ReactNode[] {
  return blocks.map((b, k) => {
    switch (b.t) {
      case "p":
        return tight ? <Fragment key={k}>{parseInline(b.text, ctx)}</Fragment> : <p key={k}>{parseInline(b.text, ctx)}</p>;
      case "h": {
        // Replies sit under the page's own headings, so `#` starts at <h3>.
        const Tag = `h${Math.min(6, b.level + 2)}` as "h3";
        return (
          <Tag key={k} className="md-heading">
            {parseInline(b.text, ctx)}
          </Tag>
        );
      }
      case "code":
        return <CodeBlock key={k} lang={b.lang} code={b.code} open={b.open} />;
      case "hr":
        return <hr key={k} />;
      case "quote":
        return <blockquote key={k}>{renderBlocks(b.blocks, ctx)}</blockquote>;
      case "list": {
        const items = b.items.map((item, n) => <li key={n}>{renderBlocks(item, ctx, b.tight)}</li>);
        return b.ordered ? (
          <ol key={k} start={b.start === 1 ? undefined : b.start}>
            {items}
          </ol>
        ) : (
          <ul key={k}>{items}</ul>
        );
      }
      case "table":
        return (
          <div key={k} className="md-table-wrap" tabIndex={0}>
            <table>
              <thead>
                <tr>
                  {b.head.map((h, n) => (
                    <th key={n} style={b.align[n] ? { textAlign: b.align[n] } : undefined}>
                      {parseInline(h, ctx)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((row, r) => (
                  <tr key={r}>
                    {row.map((cell, n) => (
                      <td key={n} style={b.align[n] ? { textAlign: b.align[n] } : undefined}>
                        {parseInline(cell, ctx)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
    }
  });
}

export function Markdown({
  text,
  onOpenLink,
  citations,
}: {
  text: string;
  /** Called for clickable (`https:`) links; without it links render as text. */
  onOpenLink?: (url: string, label: string) => void;
  citations?: CitationLinks;
}) {
  const blocks = useMemo(() => parseBlocks(text), [text]);
  return <div className="md">{renderBlocks(blocks, { onOpenLink, cite: citations })}</div>;
}
