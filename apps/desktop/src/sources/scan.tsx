import type { SourceInfo } from "../api";
import { formatBytes } from "../format";
import { useT } from "../i18n";

// KK2 (GG6-2, GG6-26): what a source's folder scan found, for the setup step and the Sources page.

/**
 * Where a source is: not started, indexing, searchable, failed, holding nothing Nugget can index
 * (`empty`), or a folder Nugget can't open (`denied`) or can't find (`missing`). The last three
 * settle within seconds of the folder scan instead of spinning (GG6-2).
 */
export type IndexPhase = "waiting" | "indexing" | "ready" | "failed" | "empty" | "denied" | "missing";

export function indexPhase(s: Pick<SourceInfo, "state" | "items" | "last_error"> & Partial<Pick<SourceInfo, "scan" | "health">>): IndexPhase {
  if (s.state === "error" || (s.last_error && s.items === 0)) return "failed";
  const scan = s.scan ?? null;
  if (s.items === 0) {
    if ((scan?.roots_denied ?? 0) > 0 || s.health?.code === "unreadable") return "denied";
    if (scan?.finished && scan.found === 0) {
      return scan.roots_missing > 0 && scan.unsupported + scan.too_large + scan.unreadable === 0 ? "missing" : "empty";
    }
    if (s.health?.code === "nothing_indexable") return "empty";
  }
  if (s.items > 0) return s.state === "syncing" ? "indexing" : "ready";
  return (scan?.found ?? 0) > 0 ? "indexing" : "waiting";
}

/** Phases after which the step stops polling: nothing more will happen without the user. */
export function isSettled(p: IndexPhase): boolean {
  return p !== "waiting" && p !== "indexing";
}

/** What the scan skipped and why, with a few file names; null when nothing was skipped. */
export function SkippedSummary({ scan }: { scan: SourceInfo["scan"] }) {
  const t = useT();
  if (!scan?.finished) return null;
  const lines: string[] = [];
  if (scan.unsupported > 0) lines.push(t("onboarding.source.skipped.unsupported", { count: scan.unsupported }));
  if (scan.too_large > 0) lines.push(t("onboarding.source.skipped.tooLarge", { count: scan.too_large, limit: formatBytes(scan.max_file_bytes) }));
  if (scan.unreadable > 0) lines.push(t("onboarding.source.skipped.unreadable", { count: scan.unreadable }));
  if (lines.length === 0) return null;
  const examples = (scan.skipped ?? []).slice(0, 5);
  return (
    <div className="ob-skipped">
      <p className="small">{t("onboarding.source.skipped.heading")}</p>
      <ul className="small">
        {lines.map((l) => (
          <li key={l}>{l}</li>
        ))}
      </ul>
      {examples.length > 0 && (
        <p className="small muted">
          {t("onboarding.source.skipped.examples")}{" "}
          {examples.map((e, i) => (
            <span key={e.path}>
              {i > 0 && ", "}
              <bdi dir="auto">{e.path}</bdi>
            </span>
          ))}
        </p>
      )}
    </div>
  );
}

