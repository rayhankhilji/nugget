/** Display helpers for the knowledge graph: labels, facts, evidence and vault links. */
import { formatNumber, t, type MessageKey } from "./i18n";
import { isOpenableUrl, type ClaimStatus, type ClaimView, type EvidenceView } from "./api";

/** Label for a kind, tolerating kinds a newer daemon may add. */
export function kindLabel(kind: string): string {
  return known(`knowledge.kind.${kind}`, kind);
}

/** Plural label for a kind ("People", "Projects"). */
export function kindPlural(kind: string): string {
  return known(`knowledge.kindPlural.${kind}`, kind);
}

export function claimStatusLabel(status: string): string {
  return known(`knowledge.claimStatus.${status}`, status);
}

/** The catalog entry for `key`, else `fallback` (for values a newer daemon may send). */
function known(key: string, fallback: string): string {
  const out = t(key as MessageKey);
  return out === key ? fallback : out;
}

/** `works_at` → `works at`. */
export function predicateLabel(predicate: string): string {
  const text = predicate.replace(/[_-]+/g, " ").replace(/\s+/g, " ").trim();
  return text || predicate;
}

/** `0.923` → `92%`. Out-of-range values are clamped; non-numbers give `–`. */
export function formatConfidence(c: number): string {
  if (typeof c !== "number" || !Number.isFinite(c)) return "–";
  return formatNumber(Math.round(Math.min(1, Math.max(0, c)) * 100) / 100, { style: "percent" });
}

export function confidenceTone(c: number): "good" | "neutral" | "warn" {
  if (!Number.isFinite(c)) return "neutral";
  if (c >= 0.8) return "good";
  if (c >= 0.5) return "neutral";
  return "warn";
}

/** The object of a fact as text: the linked entity's name or the literal value. */
export function claimObjectText(claim: ClaimView): string {
  return claim.object.entity?.name ?? claim.object.literal ?? "";
}

/** What to show for a fact: its sentence, else `predicate object`. */
export function claimText(claim: ClaimView): string {
  const text = claim.text.trim();
  if (text) return text;
  return `${predicateLabel(claim.predicate)} ${claimObjectText(claim)}`.trim();
}

const STATUS_RANK: Record<ClaimStatus, number> = { user_asserted: 0, active: 0, superseded: 1, retracted: 2 };

export interface ClaimGroup {
  predicate: string;
  label: string;
  claims: ClaimView[];
}

/**
 * Facts grouped by predicate. Groups are ordered by label; within a group current facts come
 * first (by confidence, highest first), then superseded, then rejected ones.
 */
export function groupClaims(claims: readonly ClaimView[]): ClaimGroup[] {
  const groups = new Map<string, ClaimView[]>();
  for (const c of claims) {
    const list = groups.get(c.predicate);
    if (list) list.push(c);
    else groups.set(c.predicate, [c]);
  }
  const rank = (s: string) => STATUS_RANK[s as ClaimStatus] ?? 1;
  return [...groups.entries()]
    .map(([predicate, list]) => ({
      predicate,
      label: predicateLabel(predicate),
      claims: [...list].sort((a, b) => rank(a.status) - rank(b.status) || b.confidence - a.confidence || a.id - b.id),
    }))
    .sort((a, b) => a.label.localeCompare(b.label) || a.predicate.localeCompare(b.predicate));
}

export const MAX_QUOTE_CHARS = 360;

export interface EvidenceDisplay {
  /** Source document title, else its item key. */
  title: string;
  /** Source label (`sourceNames` lookup, else the source id). */
  source: string;
  /** The supporting passage, whitespace-collapsed and cut to `MAX_QUOTE_CHARS`. */
  quote: string | null;
  /** Whether `quote` was cut. */
  truncated: boolean;
  /** A URL `open_url` accepts, else `null` (no Open button). */
  openUrl: string | null;
}

/** Cut `text` to at most `max` characters, at a word boundary when one is near. */
export function truncateText(text: string, max: number): { text: string; truncated: boolean } {
  const chars = Array.from(text);
  if (chars.length <= max) return { text, truncated: false };
  let cut = chars.slice(0, max).join("");
  const space = cut.lastIndexOf(" ");
  if (space > max * 0.6) cut = cut.slice(0, space);
  return { text: `${cut.replace(/[\s.,;:!?-]+$/, "")}…`, truncated: true };
}

export function formatEvidence(ev: EvidenceView, sourceNames?: ReadonlyMap<string, string>): EvidenceDisplay {
  const rawQuote = (ev.quote ?? "").replace(/\s+/g, " ").trim();
  const { text, truncated } = truncateText(rawQuote, MAX_QUOTE_CHARS);
  const title = (ev.title ?? "").replace(/\s+/g, " ").trim();
  return {
    title: title || ev.item.key,
    source: sourceNames?.get(ev.item.source) ?? ev.item.source,
    quote: rawQuote ? text : null,
    truncated,
    openUrl: isOpenableUrl(ev.url) ? ev.url : null,
  };
}

/** "1 source", "3 sources" — distinct items, not passages. */
export function evidenceSummary(evidence: readonly EvidenceView[]): string {
  const items = new Set(evidence.map((e) => `${e.item.source}\u0000${e.item.key}`)).size;
  return t("knowledge.evidenceSummary", { n: items });
}

// ---- vault paths ----

const WIN_DRIVE = /^[A-Za-z]:[\\/]/;

function isWindowsPath(p: string): boolean {
  return WIN_DRIVE.test(p);
}

/**
 * Join the vault root and a page path relative to it. Returns `null` when the relative path is
 * absolute or climbs out of the vault (`..`), so a hostile page name can't point elsewhere.
 */
export function joinVaultPath(root: string, rel: string): string | null {
  if (!root || !rel) return null;
  if (rel.startsWith("/") || rel.startsWith("\\") || WIN_DRIVE.test(rel) || /^[A-Za-z]:/.test(rel)) return null;
  const parts = rel.split(/[\\/]+/).filter((p) => p !== "" && p !== ".");
  if (parts.length === 0 || parts.some((p) => p === "..")) return null;
  if (parts.some((p) => /[\u0000-\u001f]/.test(p))) return null;
  const sep = isWindowsPath(root) ? "\\" : "/";
  const base = root.replace(/[\\/]+$/, "");
  return [base, ...parts].join(sep);
}

/**
 * `file:` URL for an absolute local path (POSIX `/a/b` or Windows `C:\a\b`), or `null` for
 * anything else (relative paths, UNC shares).
 */
export function fileUrl(absPath: string): string | null {
  if (isWindowsPath(absPath)) {
    const [drive, ...rest] = absPath.split(/[\\/]+/);
    return `file:///${drive}/${rest.filter(Boolean).map(encodeURIComponent).join("/")}`;
  }
  if (!absPath.startsWith("/") || absPath.startsWith("//")) return null;
  const segs = absPath.split("/").filter(Boolean).map(encodeURIComponent);
  return `file:///${segs.join("/")}`;
}

/** `obsidian://open?path=…` for an absolute path (opens the page if it is in a known vault). */
export function obsidianUrl(absPath: string): string | null {
  if (!isWindowsPath(absPath) && !(absPath.startsWith("/") && !absPath.startsWith("//"))) return null;
  return `obsidian://open?path=${encodeURIComponent(absPath)}`;
}

/** Links for a vault page, or `null` when the vault location is unknown or the path is unsafe. */
export function vaultLinks(root: string | null | undefined, rel: string | null | undefined): { file: string; obsidian: string | null } | null {
  if (!root || !rel) return null;
  const abs = joinVaultPath(root, rel);
  if (!abs) return null;
  const file = fileUrl(abs);
  if (!file || !isOpenableUrl(file)) return null;
  const obsidian = obsidianUrl(abs);
  return { file, obsidian: obsidian && isOpenableUrl(obsidian) ? obsidian : null };
}

/** `{}` / `null` / non-objects give no rows; values that aren't strings are shown as JSON. */
export function attributeRows(attributes: unknown): [string, string][] {
  if (typeof attributes !== "object" || attributes === null || Array.isArray(attributes)) return [];
  return Object.entries(attributes as Record<string, unknown>)
    .filter(([, v]) => v !== null && v !== undefined && v !== "")
    .map(([k, v]) => [predicateLabel(k), typeof v === "string" ? v : JSON.stringify(v)]);
}
