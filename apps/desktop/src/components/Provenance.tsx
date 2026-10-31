/**
 * Where something came from, shown the same way on Today, Search and Knowledge: the source's
 * icon and friendly name, plus the item's file name when it adds something. Internal ids
 * (`files-1`) and index keys (`0:mail/mail-036.eml`) stay out of the default view.
 *
 * Also plain-text helpers for snippets and quotes: indexed notes are Markdown, so passages
 * carry `# headings`, `[[wiki links]]` and `**emphasis**` that read as noise out of context.
 */
import { Fragment, type ReactNode } from "react";
import { sourceKindLabel } from "../format";
import { useT } from "../i18n";
import { sourceKindOf } from "../recency";
import { SourceIcon } from "./SourceIcon";

/** Kinds whose item keys are file paths (others key items by opaque ids). */
const FILE_KINDS = new Set(["files", "obsidian", "vault", "gdrive", "onedrive"]);

/** A last path segment that looks like a file name: has a real extension (letters, not `.000200`). */
const FILE_LIKE = /^[^\s/\\]+\.[A-Za-z][A-Za-z0-9]{0,5}$/;

/**
 * The item's file name for display, or `null` when its key is an opaque id (Slack timestamps,
 * Outlook/Calendar ids, uuids): `0:mail/mail-036.eml` → `mail-036.eml`.
 */
export function itemFileName(key: string, kind?: string | null): string | null {
  const k = key.replace(/^\d+:/, "");
  const last = k.split(/[\\/]/).filter(Boolean).at(-1) ?? "";
  if (!last) return null;
  if (FILE_LIKE.test(last)) return last;
  return kind && FILE_KINDS.has(kind) ? last : null;
}

/** Whether the item's file name tells the reader more than its title already does. */
export function nameAddsInfo(name: string | null, title: string | null | undefined): boolean {
  if (!name) return false;
  if (!title) return true;
  const stem = name.replace(/\.[A-Za-z0-9]{1,5}$/, "").toLowerCase();
  const tl = title.toLowerCase();
  return !(tl === name.toLowerCase() || tl === stem || tl.includes(stem));
}

export function SourceBadge({
  sourceId,
  name,
  kind,
  itemKey,
  title,
  size = 14,
}: {
  sourceId: string;
  /** The source's friendly name, if known. */
  name?: string | null;
  kind?: string | null;
  /** The item's index key: its file name is shown when it isn't already the title. */
  itemKey?: string | null;
  /** The title shown next to the badge (to avoid repeating it). */
  title?: string | null;
  size?: number;
}) {
  const t = useT();
  const k = kind || sourceKindOf(sourceId);
  const label = name || sourceKindLabel(k);
  const file = itemKey ? itemFileName(itemKey, k) : null;
  const showFile = nameAddsInfo(file, title);
  return (
    <span className="source-badge" title={t("search.sourceBadge", { kind: sourceKindLabel(k), id: itemKey ? `${sourceId} · ${itemKey}` : sourceId })}>
      <SourceIcon kind={k} size={size} />
      <bdi>{label}</bdi>
      {showFile && (
        <>
          <span aria-hidden="true">·</span>
          <span className="source-badge-file" dir="ltr">
            {file}
          </span>
        </>
      )}
    </span>
  );
}

/** Emphasis may close only before whitespace, closing punctuation or the end (not `__init__.py`). */
const CLOSE = String.raw`(?=$|[\s),;:!?]|\.(?![\w]))`;
const OPEN = String.raw`(^|[\s(])`;

/**
 * Markdown and wiki syntax out of a passage, whitespace collapsed. Run it on the raw text
 * (before collapsing or truncating) so line-start markers are seen per line. Conservative:
 * `PR #45`, `> 50%` mid-sentence, `- 5°C` and `2023. Revenue` on the first line stay as written.
 */
export function plainSnippet(text: string): string {
  const lines = text.split(/\r?\n/).map((line, i) => {
    let l = line;
    // Front matter fences and horizontal rules.
    if (/^\s*(---|\*\*\*|___)\s*$/.test(l)) return "";
    // Headings: `# ` followed by a letter, at a line start.
    l = l.replace(/^\s*#{1,6}[ \t]+(?=\p{L})/u, "");
    // Quote, bullet and numbered markers only where the text really broke the line.
    if (i > 0) l = l.replace(/^\s*(>+[ \t]*|[-*+][ \t]+(?!\d)|\d+[.)][ \t]+(?!\d))/, "");
    return l;
  });
  return (
    lines
      .join("\n")
      // [[Target|Alias]] → Alias, [[Target]] → Target, ![[embed]] → embed.
      .replace(/!?\[\[([^\]|]*)\|([^\]]*)\]\]/g, "$2")
      .replace(/!?\[\[([^\]]*)\]\]/g, "$1")
      // ![alt](url) → alt, [text](url) → text.
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      // #tags (a letter after `#`; `#45` and a lone `#` stay).
      .replace(/(^|\s)#(\p{L}[\p{L}\p{N}_/-]*)/gu, "$1$2")
      // Emphasis, strike-through and inline code, only at word boundaries.
      .replace(new RegExp(`${OPEN}(\\*\\*|__)(?=\\S)([^\\n]*?\\S)\\2${CLOSE}`, "g"), "$1$3")
      .replace(new RegExp(`${OPEN}([*_])(?=[^\\s*_])([^*_\\n]*?[^\\s*_])\\2${CLOSE}`, "g"), "$1$3")
      .replace(/~~(?=\S)([^\n]*?\S)~~/g, "$1")
      .replace(/`([^`\n]*)`/g, "$1")
      .replace(/\s+/g, " ")
      .trim()
  );
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/** `text` with the query's words (2+ characters) marked. */
export function Highlight({ text, query }: { text: string; query: string }): ReactNode {
  const words = [...new Set(query.split(/\s+/).map((w) => w.replace(/^["'“”]+|["'“”]+$/g, "")).filter((w) => w.length >= 2))];
  if (words.length === 0) return text;
  const re = new RegExp(`(${words.sort((a, b) => b.length - a.length).map(escapeRe).join("|")})`, "giu");
  const parts = text.split(re);
  return parts.map((p, i) => (i % 2 === 1 ? <mark key={i}>{p}</mark> : <Fragment key={i}>{p}</Fragment>));
}
