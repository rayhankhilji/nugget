/**
 * Updates and start-at-login, backed by `src-tauri/src/updater.rs`.
 *
 * Nothing installs without the user: the default is notify-only, the release notes are shown
 * before the Install button, and Install names the exact version shown. Auto-install is opt-in.
 */
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { errorMessage, inTauri } from "../api";
import { Badge, Spinner } from "./ui";

type Channel = "stable" | "beta" | "off";

interface UpdateStatus {
  current_version: string;
  configured: boolean;
  settings: { channel: Channel; auto_install: boolean; start_at_login: boolean | null };
  available: { version: string; notes: string | null; date: number | null } | null;
  staged: boolean;
  busy: boolean;
  last_checked: number | null;
  last_error: string | null;
}

interface Progress {
  downloaded: number;
  total: number | null;
}

const CHANNELS: { id: Channel; label: string }[] = [
  { id: "stable", label: "Stable" },
  { id: "beta", label: "Beta (prereleases)" },
  { id: "off", label: "Off (never check)" },
];

function when(secs: number | null): string {
  return secs ? new Date(secs * 1000).toLocaleString() : "never";
}

export function UpdateSettings() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [progress, setProgress] = useState<Progress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  const run = useCallback(async (command: string, args?: Record<string, unknown>) => {
    setError(null);
    setWorking(true);
    try {
      const next = await invoke<UpdateStatus | null>(command, args);
      if (next) setStatus(next);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setWorking(false);
    }
  }, []);

  useEffect(() => {
    if (!inTauri()) return;
    void run("updates_status");
    const subs = [
      listen<UpdateStatus>("nugget:update-status", (e) => setStatus(e.payload)),
      listen<Progress>("nugget:update-progress", (e) => setProgress(e.payload)),
    ];
    return () => subs.forEach((p) => void p.then((un) => un()).catch(() => {}));
  }, [run]);

  if (!inTauri()) return null;
  if (!status) {
    return (
      <section className="status-card" aria-labelledby="updates-title">
        <h2 id="updates-title">Updates</h2>
        {error ? <div className="note note-error">{error}</div> : <Spinner />}
      </section>
    );
  }

  const { settings, available } = status;
  const busy = working || status.busy;
  const setSettings = (channel: Channel, autoInstall: boolean) =>
    void run("updates_set_settings", { channel, autoInstall });

  return (
    <section className="status-card" aria-labelledby="updates-title">
      <h2 id="updates-title">Updates</h2>
      <p className="muted small">
        Nugget {status.current_version}. Checks send no identifiers and never install anything without you.
      </p>

      {!status.configured ? (
        <div className="note">This build has no update signing key (a development or self-built copy), so it can't update itself.</div>
      ) : (
        <>
          <div className="row" style={{ justifyContent: "flex-start", alignItems: "center" }}>
            <label className="field-inline">
              Channel
              <select value={settings.channel} disabled={busy} onChange={(e) => setSettings(e.target.value as Channel, settings.auto_install)}>
                {CHANNELS.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.label}
                  </option>
                ))}
              </select>
            </label>
            <label className="field-inline">
              <input
                type="checkbox"
                checked={settings.auto_install}
                disabled={busy || settings.channel === "off"}
                onChange={(e) => setSettings(settings.channel, e.target.checked)}
              />
              Download updates automatically and install them when I quit Nugget
            </label>
            <button type="button" className="btn btn-ghost btn-small" disabled={busy || settings.channel === "off"} onClick={() => void run("updates_check")}>
              Check now
            </button>
            {busy && <Spinner label="Working" />}
          </div>
          <p className="muted small">
            Last checked: {when(status.last_checked)}
            {progress && busy && progress.total ? ` · downloading ${Math.round((100 * progress.downloaded) / progress.total)}%` : ""}
          </p>
          {status.last_error && <div className="note note-warn">Update problem: {status.last_error}</div>}

          {available ? (
            <div className="note" role="status" style={{ display: "block" }}>
              <p>
                <strong>Nugget {available.version}</strong> is available <Badge tone="accent">{settings.channel}</Badge>
                {available.date ? <span className="muted small"> · released {when(available.date)}</span> : null}
              </p>
              <h3 className="small">What's new</h3>
              <div className="small" style={{ whiteSpace: "pre-wrap", maxHeight: 240, overflow: "auto" }}>
                {available.notes?.trim() || "No release notes were published for this version."}
              </div>
              <div className="row" style={{ justifyContent: "flex-start", marginTop: 8 }}>
                <button
                  type="button"
                  className="btn btn-primary btn-small"
                  disabled={busy}
                  onClick={() => void run("updates_install", { version: available.version })}
                >
                  Install {available.version} and restart
                </button>
                {status.staged && <span className="muted small">Downloaded and verified; installs when you quit Nugget.</span>}
              </div>
            </div>
          ) : (
            settings.channel !== "off" && status.last_checked && <p className="muted small">You're up to date.</p>
          )}
        </>
      )}

      <h2>Start at login</h2>
      {settings.start_at_login === null ? (
        <div className="note" style={{ display: "block" }}>
          <p>Start the Nugget background service when you log in, so your sources stay indexed without the app open?</p>
          <div className="row" style={{ justifyContent: "flex-start" }}>
            <button type="button" className="btn btn-primary btn-small" disabled={busy} onClick={() => void run("login_set", { enabled: true })}>
              Yes, start at login
            </button>
            <button type="button" className="btn btn-ghost btn-small" disabled={busy} onClick={() => void run("login_set", { enabled: false })}>
              Not now
            </button>
          </div>
        </div>
      ) : (
        <label className="field-inline">
          <input
            type="checkbox"
            checked={settings.start_at_login}
            disabled={busy}
            onChange={(e) => void run("login_set", { enabled: e.target.checked })}
          />
          Start the Nugget background service at login
        </label>
      )}
      {error && (
        <div className="note note-error" role="alert">
          {error}
        </div>
      )}
    </section>
  );
}
