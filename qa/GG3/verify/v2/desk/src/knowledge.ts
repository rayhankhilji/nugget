/** Display helpers for the knowledge graph: labels, facts, evidence and vault links. */
import { formatNumber, t, type MessageKey } from "./i18n";
import { formatDatesInText } from "./format";
import {
  isOpenableUrl,
  type ClaimDecision,
  type ClaimStatus,
  type ClaimView,
  type EntityDetail,
  type EvidenceView,
  type SourceInfo,
  type SplitRequest,
} from "./api";

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

/**
 * What to show for a fact: its sentence, else `predicate object`, with ISO dates in it written
 * for the reader's locale (the stored claim keeps them as they are).
 */
export function claimText(claim: ClaimView): string {
  const text = claim.text.trim();
  if (text) return formatDatesInText(text);
  return formatDatesInText(`${predicateLabel(claim.predicate)} ${claimObjectText(claim)}`.trim());
}

const STATUS_RANK: Record<ClaimStatus, number> = { user_asserted: 0, active: 0, superseded: 1, retracted: 2, rejected: 2 };

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

// ---- fact decisions (reject, restore, undo a vault assertion) ----

/** Whether a fact is current: shown to apps and written to the vault. */
export function isCurrentStatus(status: string): boolean {
  return status === "active" || status === "user_asserted";
}

/** Rejected by the user or without sources: shown struck through, with a Restore button. */
export function isDroppedStatus(status: string): boolean {
  return status === "rejected" || status === "retracted";
}

export type FactAction = "reject" | "unassert" | "restore";

export interface FactActionPlan {
  action: FactAction;
  /** What to send to `knowledge.claim.status`. */
  decision: ClaimDecision;
  /** What undoes it (`null`: nothing to undo; the fact can simply be rejected again). */
  undo: ClaimDecision | null;
  /** The status the daemon will report afterwards (for an optimistic update). */
  next: ClaimStatus;
}

/**
 * The one action a fact offers: a fact the user added is withdrawn (undo of the assertion), a
 * current fact is rejected (sticky), and anything else is restored.
 */
export function factAction(c: Pick<ClaimView, "status" | "evidence">): FactActionPlan {
  const sourced = c.evidence.length > 0;
  if (c.status === "user_asserted") {
    return { action: "unassert", decision: "unasserted", undo: "user_asserted", next: sourced ? "active" : "retracted" };
  }
  if (c.status === "active") return { action: "reject", decision: "rejected", undo: "active", next: "rejected" };
  return { action: "restore", decision: "active", undo: null, next: sourced ? "active" : "user_asserted" };
}

/** The status an undo decision leads back to. */
export function undoStatus(undo: ClaimDecision, c: Pick<ClaimView, "evidence">): ClaimStatus {
  if (undo === "user_asserted") return "user_asserted";
  if (undo === "active") return c.evidence.length > 0 ? "active" : "user_asserted";
  return undo === "rejected" ? "rejected" : c.evidence.length > 0 ? "active" : "retracted";
}

// ---- entity split ----

export interface SplitDraft {
  name: string;
  claims: readonly number[];
  aliases: readonly string[];
  identifiers: readonly (readonly [string, string])[];
}

export type SplitProblem = "split.needName" | "split.needFacts" | "split.sameName";

/** Why the split can't be sent yet, or `null`. */
export function splitProblem(entity: Pick<EntityDetail, "name">, draft: SplitDraft): SplitProblem | null {
  const name = draft.name.replace(/\s+/g, " ").trim();
  if (!name) return "split.needName";
  if (name.toLocaleLowerCase() === entity.name.trim().toLocaleLowerCase()) return "split.sameName";
  if (draft.claims.length === 0) return "split.needFacts";
  return null;
}

/** The request for a valid draft: trimmed name, unique claims, and only details the entity has. */
export function splitRequest(entity: Pick<EntityDetail, "id" | "aliases" | "identifiers" | "claims">, draft: SplitDraft): SplitRequest {
  const has = new Set(entity.claims.map((c) => c.id));
  const aliases = new Set(entity.aliases);
  const idents = new Set(entity.identifiers.map(([s, v]) => `${s}\u0000${v}`));
  return {
    entity: entity.id,
    claims: [...new Set(draft.claims)].filter((id) => has.has(id)).sort((a, b) => a - b),
    name: draft.name.replace(/\s+/g, " ").trim(),
    aliases: [...new Set(draft.aliases)].filter((a) => aliases.has(a)),
    identifiers: draft.identifiers.filter(([s, v]) => idents.has(`${s}\u0000${v}`)).map(([s, v]) => [s, v] as [string, string]),
  };
}

// ---- graph source filter ----

/** Sources to filter the graph by, by name. */
export function graphSourceOptions(sources: readonly Pick<SourceInfo, "id" | "name">[]): { id: string; name: string }[] {
  return sources
    .map((s) => ({ id: s.id, name: s.name.trim() || s.id }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}
