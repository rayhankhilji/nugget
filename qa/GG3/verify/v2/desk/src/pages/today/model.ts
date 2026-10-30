/** Display helpers for the Today page: factor names, kinds, snooze times, risk tones. */
import type { CardEvidence, CardFactor, CardLearned, LearnedView } from "../../api";
import { isSafeEvidenceUrl } from "../../api";
import { formatDate, t, type MessageKey } from "../../i18n";
import { truncateText } from "../../knowledge";

/** The catalog entry for `key`, else `fallback` (for values a newer daemon may send). */
function known(key: string, fallback: string): string {
  const out = t(key as MessageKey);
  return out === key ? fallback : out;
}

/** `snake_case` → `snake case`, for names the catalog doesn't know. */
function humanize(name: string): string {
  const text = name.replace(/[_:-]+/g, " ").replace(/\s+/g, " ").trim();
  return text || name;
}

export function cardKindLabel(kind: string): string {
  return known(`today.kind.${kind}`, humanize(kind));
}

export function riskLabel(risk: string): string {
  return known(`today.risk.${risk}`, humanize(risk));
}

export function targetLabel(target: string): string {
  return known(`today.target.${target}`, target);
}

/** `urgency` → "Due soon"; `kind_renewal` / `kind:renewal` → "Type: Renewal"; others humanized. */
export function factorLabel(name: string): string {
  const kind = /^kind[:_](.+)$/.exec(name);
  if (kind) return t("today.factor.kind", { kind: cardKindLabel(kind[1]!) });
  return known(`today.factor.${name}`, humanize(name));
}

/** Score terms that explain nothing to a person (the model's intercept). */
const HIDDEN_FACTORS = new Set(["bias"]);

/**
 * The factors that pushed a card up, strongest first: those with a positive contribution, else
 * (when none is positive) the largest by magnitude. At most `n`.
 */
export function topFactors(factors: readonly CardFactor[], n = 3): CardFactor[] {
  const finite = factors.filter((f) => Number.isFinite(f.contribution) && !HIDDEN_FACTORS.has(f.name));
  const positive = finite.filter((f) => f.contribution > 0).sort((a, b) => b.contribution - a.contribution);
  if (positive.length > 0) return positive.slice(0, n);
  return [...finite].sort((a, b) => Math.abs(b.contribution) - Math.abs(a.contribution)).slice(0, n);
}

export type Tone = "neutral" | "accent" | "good" | "warn" | "bad";

export function riskTone(risk: string): Tone {
  switch (risk) {
    case "observe":
      return "neutral";
    case "edit":
      return "warn";
    default:
      return "bad";
  }
}

export type SnoozeChoice = "later_today" | "tomorrow" | "next_week";
export const SNOOZE_CHOICES: readonly SnoozeChoice[] = ["later_today", "tomorrow", "next_week"];

/** Morning hour cards come back at when snoozed to another day (local time). */
export const SNOOZE_MORNING_HOUR = 8;

/**
 * When a snooze ends, as RFC 3339 (UTC): later today = in three hours; tomorrow = tomorrow at
 * 08:00 local; next week = next Monday at 08:00 local (a week from Monday when today is Monday).
 */
export function snoozeUntil(choice: SnoozeChoice, now: Date = new Date()): string {
  if (choice === "later_today") return new Date(now.getTime() + 3 * 3600_000).toISOString();
  const d = new Date(now.getFullYear(), now.getMonth(), now.getDate(), SNOOZE_MORNING_HOUR, 0, 0, 0);
  if (choice === "tomorrow") {
    d.setDate(d.getDate() + 1);
  } else {
    const untilMonday = ((8 - d.getDay()) % 7) || 7;
    d.setDate(d.getDate() + untilMonday);
  }
  return d.toISOString();
}

/** `YYYY-MM-DD` as a local date ("Wednesday, October 28"); anything else is returned as is. */
export function formatBriefDate(date: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date);
  if (!m) return date;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  return formatDate(d, { weekday: "long", month: "long", day: "numeric" });
}

export function formatClock(rfc3339: string): string {
  const d = new Date(rfc3339);
  return Number.isNaN(d.getTime()) ? rfc3339 : formatDate(d, { timeStyle: "short" });
}

export const MAX_CARD_QUOTE_CHARS = 280;

export interface EvidenceLine {
  quote: string | null;
  truncated: boolean;
  /** `source · key`, shown as the citation. */
  source: string;
  /** An `https:` link, else `null` (no Open button). */
  openUrl: string | null;
}

/** A card citation for display: plain text only, quote cut short, link only when safe. */
export function evidenceLine(ev: CardEvidence): EvidenceLine {
  const raw = (ev.quote ?? "").replace(/\s+/g, " ").trim();
  const { text, truncated } = truncateText(raw, MAX_CARD_QUOTE_CHARS);
  return {
    quote: raw ? text : null,
    truncated,
    source: `${ev.item.source} · ${ev.item.key}`,
    openUrl: isSafeEvidenceUrl(ev.url) ? ev.url : null,
  };
}

/** Preview diff marker for a line op. */
export function diffMarker(op: string): string {
  if (op === "add") return "+";
  if (op === "remove") return "-";
  return " ";
}

// ---- BB2: what feedback taught the ranking ----

const LEARNING_SCOPES = new Set(["sender", "domain", "kind", "thread"]);
const LEARNED_REASONS = new Set(["not_relevant", "already_handled", "wrong_info", "not_important"]);

/** `sender` → "Sender"; an unknown (newer) scope humanized. */
export function learningScopeLabel(scope: string): string {
  return known(`today.learning.scope.${scope}`, humanize(scope));
}

/** `less` → "Shown less often"; an unknown (newer) direction humanized. */
export function learnedDirectionLabel(direction: string): string {
  return known(`today.learning.direction.${direction}`, humanize(direction));
}

/** What an entry is about, as plain text: the sender/domain, the kind's label, a thread's title. */
export function learnedName(scope: string, key: string, label?: string | null): string {
  if (scope === "kind") return cardKindLabel(key);
  const l = label?.replace(/\s+/g, " ").trim();
  if (l) return truncateText(l, 120).text;
  if (scope === "thread") return t("today.learning.unnamedThread");
  return key;
}

/** Direction of a `proactive.learning` entry. */
export function learnedViewDirection(e: LearnedView): "less" | "more" | "hidden" {
  if (e.suppresses) return "hidden";
  return e.adjustment < 0 ? "less" : "more";
}

/**
 * One line saying how feedback moved a card ("Shown less often: you dismissed 3 similar cards
 * from this sender"), or `null` for a direction this app does not know.
 */
export function learnedLine(l: CardLearned): string | null {
  if (l.direction !== "less" && l.direction !== "more" && l.direction !== "hidden") return null;
  const effect = learnedDirectionLabel(l.direction);
  return t("today.learned.line", { effect, cause: learnedCause(l) });
}

function learnedCause(l: CardLearned): string {
  if (l.scope === "thread" && l.direction === "hidden" && l.reason === "already_handled") {
    return t("today.learned.cause.threadHandled");
  }
  if (!LEARNING_SCOPES.has(l.scope)) return t("today.learned.cause.generic");
  const more = l.direction === "more";
  const n = Math.round(Math.max(0, more ? l.accepted : l.dismissed));
  if (!Number.isFinite(n) || n === 0) return t("today.learned.cause.generic");
  const kind = l.scope === "kind" ? cardKindLabel(l.key) : undefined;
  if (more) return t(`today.learned.cause.accepted.${l.scope}` as MessageKey, { n, kind });
  if (l.reason && LEARNED_REASONS.has(l.reason)) {
    const reason = t(`today.learned.reason.${l.reason}` as MessageKey);
    return t(`today.learned.cause.marked.${l.scope}` as MessageKey, { n, kind, reason });
  }
  return t(`today.learned.cause.dismissed.${l.scope}` as MessageKey, { n, kind });
}
