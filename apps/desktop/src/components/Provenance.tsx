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

/** `0:mail/mail-036.eml` → `mail-036.eml`; keys without a path come back trimmed. */
export function itemFileName(key: string): string {
  const k = key.replace(/^\d+:/, "");
  const parts = k.split(/[\\/]/).filter(Boolean);
  return parts.at(-1) ?? k;
}

/** Whether the item's file name tells the reader more than its title already does. */
function nameAddsInfo(name: string, title: string | null | undefined): boolean {
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
  const file = itemKey ? itemFileName(itemKey) : "";
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

/** Markdown and wiki syntax out of a passage, whitespace collapsed. */
export function plainSnippet(text: string): string {
  return (
    text
      // Front matter fences and horizontal rules.
      .replace(/(^|\n)\s*(---|\*\*\*|___)\s*(?=\n|$)/g, "$1")
      // [[Target|Alias]] → Alias, [[Target]] → Target, ![[embed]] → embed.
      .replace(/!?\[\[([^\]|]*)\|([^\]]*)\]\]/g, "$2")
      .replace(/!?\[\[([^\]]*)\]\]/g, "$1")
      // ![alt](url) → alt, [text](url) → text.
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
      // Headings, quotes and list markers at line starts.
      .replace(/(^|\n)[ \t]*(#{1,6}|>+|[-*+]|\d+[.)])[ \t]+/g, "$1")
      // Headings that ended up mid-text once lines were joined.
      .replace(/(^|\s)#{1,6}\s+/g, "$1")
      // #tags → tags.
      .replace(/(^|\s)#([\p{L}\p{N}_][\p{L}\p{N}_/-]*)/gu, "$1$2")
      // Emphasis, strike-through and inline code.
      .replace(/(\*\*|__)(?=\S)([\s\S]*?\S)\1/g, "$2")
      .replace(/(^|[\s(])([*_])(?=\S)([^*_\n]*?\S)\2(?=[\s).,;:!?]|$)/g, "$1$3")
      .replace(/~~(?=\S)([\s\S]*?\S)~~/g, "$1")
      .replace(/`([^`]*)`/g, "$1")
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
