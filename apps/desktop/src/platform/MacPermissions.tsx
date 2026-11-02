/**
 * macOS privacy permissions Nugget needs, shared by onboarding and the Double Tap page: which
 * are missing, a button per System Settings pane, and a re-check when the user comes back from
 * System Settings (window focus, the page becoming visible, and a slow poll while one is missing).
 */
import { useEffect, useState } from "react";
import { api, doubletapApi, events, type DoubleTapStatus } from "../api";
import { useEvent } from "../hooks";
import { useT, type MessageKey } from "../i18n";

export type PrivacyPane = "accessibility" | "input_monitoring" | "full_disk_access";

/**
 * Source kinds that read folders macOS keeps behind Full Disk Access: the Messages database
 * (`~/Library/Messages`). Email comes over IMAP or an API, which needs no such permission; a
 * source reading Apple Mail's local folders (`~/Library/Mail`) would belong here too.
 */
export const FULL_DISK_ACCESS_KINDS: readonly string[] = ["imessage"];

/** How often to look again while something is missing, in case focus events don't arrive. */
export const RECHECK_MS = 3000;

/**
 * The panes to send the user to, most important first. Accessibility comes with Input Monitoring
 * unless that is known to be on (the recovery steps cover both). Full Disk Access only when a
 * source needs it.
 */
export function missingPanes(status: DoubleTapStatus | undefined, needFullDiskAccess: boolean): PrivacyPane[] {
  if (!status || status.platform !== "macos") return [];
  const out: PrivacyPane[] = [];
  const ax = status.accessibility === "denied";
  if (ax) out.push("accessibility");
  if (status.input_monitoring === "denied" || (ax && status.input_monitoring !== "granted")) out.push("input_monitoring");
  if (needFullDiskAccess && status.full_disk_access === "denied") out.push("full_disk_access");
  return out;
}

/** Whether a source that needs Full Disk Access is set up; follows `sources.changed`. */
export function useNeedsFullDiskAccess(): boolean {
  const [need, setNeed] = useState(false);
  const [tick, setTick] = useState(0);
  useEvent(events.onSourcesChanged, () => setTick((n) => n + 1));
  useEffect(() => {
    let alive = true;
    api
      .sourcesList()
      .then((list) => alive && setNeed(list.some((s) => FULL_DISK_ACCESS_KINDS.includes(s.kind))))
      .catch(() => {
        /* no daemon yet: nothing to ask for */
      });
    return () => {
      alive = false;
    };
  }, [tick]);
  return need;
}

/** Call `recheck` when the user comes back to Nugget, and every few seconds, while `active`. */
export function useRecheckOnReturn(recheck: () => void, active: boolean): void {
  useEffect(() => {
    if (!active) return;
    const onVisible = () => {
      if (document.visibilityState === "visible") recheck();
    };
    window.addEventListener("focus", recheck);
    document.addEventListener("visibilitychange", onVisible);
    const id = window.setInterval(recheck, RECHECK_MS);
    return () => {
      window.removeEventListener("focus", recheck);
      document.removeEventListener("visibilitychange", onVisible);
      window.clearInterval(id);
    };
  }, [recheck, active]);
}

const OPEN_LABEL: Record<PrivacyPane, MessageKey> = {
  accessibility: "doubletap.perm.openAccessibility",
  input_monitoring: "doubletap.perm.openInputMonitoring",
  full_disk_access: "doubletap.perm.openFullDiskAccess",
};

const AX_STEPS: readonly MessageKey[] = ["doubletap.perm.recover1", "doubletap.perm.recover2", "doubletap.perm.recover3"];
const OTHER_STEPS: readonly MessageKey[] = ["doubletap.perm.recoverOther1", "doubletap.perm.recoverOther2", "doubletap.perm.recoverOther3"];

/**
 * What to turn on in System Settings, with a button that opens each pane. macOS shows each
 * prompt only once; after a "Don't Allow" (or for Full Disk Access, which has no prompt) the only
 * way is System Settings.
 */
export function MacPermissionRecovery({
  onCheck,
  panes = ["accessibility", "input_monitoring"],
  firstRun = false,
}: {
  onCheck: () => void;
  panes?: readonly PrivacyPane[];
  /** Setup, before anything was asked: plain steps, not "turn it back on". */
  firstRun?: boolean;
}) {
  const t = useT();
  const [error, setError] = useState<string | null>(null);
  const open = async (pane: PrivacyPane) => {
    setError(null);
    try {
      if (!(await doubletapApi.openPrivacySettings(pane))) setError(t("doubletap.perm.openFailed"));
    } catch {
      setError(t("doubletap.perm.openFailed"));
    }
  };
  const ax = !firstRun && panes.includes("accessibility");
  return (
    <div className="note note-warn dt-recover" role="group" aria-labelledby="dt-recover-title">
      <h3 id="dt-recover-title">{t(ax ? "doubletap.perm.recoverHeading" : "doubletap.perm.recoverHeadingOther")}</h3>
      <ol>
        {(ax ? AX_STEPS : OTHER_STEPS).map((k) => (
          <li key={k}>{t(k)}</li>
        ))}
      </ol>
      {panes.includes("full_disk_access") && <p className="small">{t("doubletap.perm.fullDiskAccessWhy")}</p>}
      <p className="small">{t("doubletap.perm.recoverTerminal")}</p>
      <div className="row">
        {panes.map((pane, i) => (
          <button key={pane} type="button" className={i === 0 ? "btn btn-primary btn-small" : "btn btn-small"} onClick={() => void open(pane)}>
            {t(OPEN_LABEL[pane])}
          </button>
        ))}
        <button type="button" className="btn btn-ghost btn-small" onClick={onCheck}>
          {t("doubletap.perm.checkAgain")}
        </button>
      </div>
      {error && (
        <p className="small" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
