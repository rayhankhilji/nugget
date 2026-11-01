/**
 * The strings the Rust shell draws itself (tray menu, OS notifications), sent in the current
 * language whenever it changes (`shell_labels_set`; see `src-tauri/src/platform.rs`, whose
 * `LABELS` lists the same keys with English defaults).
 */
import { useEffect } from "react";
import { platformApi } from "../api";
import { t, useLocale, type MessageKey } from "../i18n";

export const SHELL_KEYS = [
  "shell.tray.running",
  "shell.tray.notRunning",
  "shell.tray.checking",
  "shell.tray.indexing",
  "shell.tray.indexed",
  "shell.tray.indexPaused",
  "shell.tray.modelReady",
  "shell.tray.modelMissing",
  "shell.tray.agentRunning",
  "shell.tray.agentWaiting",
  "shell.tray.agentIdle",
  "shell.tray.ask",
  "shell.tray.doubleTap",
  "shell.tray.pause",
  "shell.tray.resume",
  "shell.tray.stopAgents",
  "shell.tray.resumeAgents",
  "shell.tray.open",
  "shell.tray.status",
  "shell.tray.updates",
  "shell.tray.quit",
  "shell.notify.agentTitle",
  "shell.notify.agentBody",
  "shell.notify.open",
] as const satisfies readonly MessageKey[];

export function shellLabels(): Record<string, string> {
  return Object.fromEntries(SHELL_KEYS.map((k) => [k, t(k)]));
}

/** Keep the shell's strings in the app's language. */
export function useShellLabels(): void {
  const { code } = useLocale();
  useEffect(() => {
    platformApi.setShellLabels(shellLabels()).catch(() => {
      /* outside the app, or an older shell: it keeps its English defaults */
    });
  }, [code]);
}
