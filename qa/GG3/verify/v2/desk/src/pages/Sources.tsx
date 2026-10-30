import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import {
  api,
  errorMessage,
  events,
  isMethodNotFound,
  mergeSources,
  sourcesFromChange,
  type IngestProgress,
  type SourceInfo,
} from "../api";
import { useAsync, useEvent, useNow } from "../hooks";
import { formatRelative, formatTime, sourceKindLabel, sourceStateLabel, SOURCE_STATE_TONE } from "../format";
import { useT, type TFunction } from "../i18n";
import { AddSourceWizard } from "../components/AddSourceWizard";
import { Announce, useAnnouncer, useIngestAnnouncements } from "../components/Announce";
import { Dialog } from "../components/Dialog";
import { Badge, EmptyState, ErrorNote, Loading, NotAvailable, PageHeader, ProgressBar, Spinner } from "../components/ui";
import { DeviceSignInDialog } from "../sources/auth";
import { connectMode, isGoogleKind, isSaasKind } from "../sources/kinds";
import { KindIcon } from "../sources/KindIcon";
import { TOKEN_LABEL } from "../sources/SaasForm";

export function SourcesPage() {
  const t = useT();
  const loaded = useAsync(api.sourcesList);
  const [sources, setSources] = useState<SourceInfo[] | null>(null);
  const [progress, setProgress] = useState<Record<string, IngestProgress>>({});
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<SourceInfo | null>(null);
  const [passwordFor, setPasswordFor] = useState<SourceInfo | null>(null);
  const [deviceFor, setDeviceFor] = useState<SourceInfo | null>(null);
  const [announcement, setAnnounce] = useAnnouncer();
  const [syncAllError, setSyncAllError] = useState<string | null>(null);
  const now = useNow();

  useEffect(() => {
    if (loaded.data) setSources(loaded.data);
  }, [loaded.data]);

  // Coalesce bursts of events into one `sources.list`.
  const { reload } = loaded;
  const reloadTimer = useRef<number | undefined>(undefined);
  const reloadSoon = useCallback(() => {
    window.clearTimeout(reloadTimer.current);
    reloadTimer.current = window.setTimeout(reload, 400);
  }, [reload]);
  useEffect(() => () => window.clearTimeout(reloadTimer.current), []);

  // Progress bars tick silently; a pass is announced when it starts and when it ends.
  useIngestAnnouncements(setAnnounce, (id) => sources?.find((s) => s.id === id));

  useEvent(events.onIngestProgress, (p) => {
    if (p.phase === "idle" || p.phase === "error") {
      setProgress(({ [p.source]: _, ...rest }) => rest);
      reloadSoon();
    } else {
      setProgress((all) => ({ ...all, [p.source]: p }));
    }
  });

  useEvent(events.onSourcesChanged, (payload) => {
    const changed = sourcesFromChange(payload);
    if (changed) setSources((list) => mergeSources(list ?? [], changed));
    else reloadSoon();
  });

  const replace = (s: SourceInfo) => setSources((list) => mergeSources(list ?? [], [s]));

  const syncAll = async () => {
    setSyncAllError(null);
    try {
      await api.sourcesSync({ id: null });
      setAnnounce(t("sources.announce.syncingAll"));
    } catch (e) {
      setSyncAllError(errorMessage(e));
    }
  };

  const unavailable = isMethodNotFound(loaded.error);
  const list = sources ?? [];
  const failing = list.filter(hasProblem).length;

  return (
    <div className="page">
      <PageHeader
        title={t("sources.title")}
        description={t("sources.description")}
        actions={
          !unavailable && (
            <>
              <button type="button" className="btn btn-ghost" onClick={() => void syncAll()} disabled={list.length === 0}>
                {t("sources.syncAll")}
              </button>
              <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
                {t("sources.add")}
              </button>
            </>
          )
        }
      />
      <Announce message={announcement} />
      {syncAllError && (
        <p className="note note-error" role="alert">
          {syncAllError}
        </p>
      )}

      {unavailable ? (
        <NotAvailable what={t("sources.title")} />
      ) : (
        <>
          <ErrorNote error={loaded.error} onRetry={loaded.reload} />
          {loaded.loading && !sources && <Loading label={t("sources.loading")} />}
          {sources && list.length === 0 && (
            <EmptyState
              title={t("sources.empty.title")}
              action={
                <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
                  {t("sources.add")}
                </button>
              }
            >
              {t("sources.empty.body")}
            </EmptyState>
          )}
          {failing > 0 && (
            <p className="note" role="status">
              {t("sources.problemSummary", { n: failing })}
            </p>
          )}
          {list.length > 0 && (
            <ul className="source-list">
              {list.map((s) => (
                <SourceRow
                  key={s.id}
                  source={s}
                  progress={progress[s.id]}
                  now={now}
                  onChanged={replace}
                  onRemove={() => setRemoving(s)}
                  onSetPassword={() => setPasswordFor(s)}
                  onDeviceSignIn={() => setDeviceFor(s)}
                  onAnnounce={setAnnounce}
                />
              ))}
            </ul>
          )}
        </>
      )}

      {adding && (
        <AddSourceWizard
          onClose={() => {
            setAdding(false);
            // A source may have been created even if the wizard was closed part-way.
            reloadSoon();
          }}
          onAdded={(s) => {
            replace(s);
            setAnnounce(t("sources.announce.added", { name: s.name }));
            reloadSoon();
          }}
        />
      )}
      {removing && (
        <RemoveDialog
          source={removing}
          onClose={() => setRemoving(null)}
          onRemoved={() => {
            setSources((l) => (l ?? []).filter((x) => x.id !== removing.id));
            setAnnounce(t("sources.announce.removed", { name: removing.name }));
            setRemoving(null);
          }}
        />
      )}
      {passwordFor && (
        <PasswordDialog
          source={passwordFor}
          onClose={() => setPasswordFor(null)}
          onSaved={() => {
            setAnnounce(t("sources.announce.passwordSaved", { name: passwordFor.name }));
            setPasswordFor(null);
            reloadSoon();
          }}
        />
      )}
      {deviceFor && (
        <DeviceSignInDialog
          source={deviceFor}
          onClose={() => {
            setDeviceFor(null);
            reloadSoon();
          }}
          onConnected={(s) => {
            replace(s);
            setAnnounce(t("sources.announce.connected", { name: s.name }));
          }}
        />
      )}
    </div>
  );
}

/** The last sync failed (an error state or a recorded error). Sign-in needs are shown separately. */
function hasProblem(s: SourceInfo): boolean {
  return s.state === "error" || Boolean(s.last_error);
}

function progressText(p: IngestProgress, t: TFunction): string {
  const args = { phase: p.phase, done: p.done, total: p.total };
  if (p.total) return t("sources.progress.total", args);
  return p.done > 0 ? t("sources.progress.soFar", args) : t("sources.progress.started", args);
}

function SourceRow({
  source: s,
  progress,
  now,
  onChanged,
  onRemove,
  onSetPassword,
  onDeviceSignIn,
  onAnnounce,
}: {
  source: SourceInfo;
  progress: IngestProgress | undefined;
  now: number;
  onChanged: (s: SourceInfo) => void;
  onRemove: () => void;
  onSetPassword: () => void;
  onDeviceSignIn: () => void;
  onAnnounce: (text: string) => void;
}) {
  const t = useT();
  const [busy, setBusy] = useState<null | "toggle" | "sync" | "auth">(null);
  const [error, setError] = useState<unknown>(null);
  const [note, setNote] = useState<string | null>(null);
  const kindLabel = sourceKindLabel(s.kind);
  const tone = SOURCE_STATE_TONE[s.state] ?? "neutral";
  const headingId = `source-${s.id}`;
  const mode = connectMode(s.kind, s.settings);
  const signIn = mode === "browser" || mode === "device";

  const run = async (what: "toggle" | "sync" | "auth", f: () => Promise<void>) => {
    setBusy(what);
    setError(null);
    setNote(null);
    try {
      await f();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };

  const toggle = () =>
    run("toggle", async () => {
      const next = await api.sourcesUpdate({ id: s.id, enabled: !s.enabled });
      onChanged(next);
      onAnnounce(t(next.enabled ? "sources.announce.enabled" : "sources.announce.disabled", { name: s.name }));
    });

  const sync = () =>
    run("sync", async () => {
      await api.sourcesSync({ id: s.id });
      onAnnounce(t("sources.announce.syncing", { name: s.name }));
    });

  const reauth = () => {
    // A device code has to stay on screen until it is entered: that flow runs in a dialog.
    if (mode === "device") return onDeviceSignIn();
    return run("auth", async () => {
      const { url } = await api.sourcesAuthBegin({ id: s.id });
      await api.openUrl(url);
      setNote(t("sources.finishSignIn"));
    });
  };

  return (
    <li className="source">
      <div className="source-icon-wrap">
        <KindIcon kind={s.kind} />
      </div>
      <div className="source-body">
        <div className="source-head">
          <h2 id={headingId} className="source-name" dir="auto">
            {s.name}
          </h2>
          <Badge tone={tone}>{sourceStateLabel(s.state)}</Badge>
        </div>
        <p className="muted small source-meta">
          {kindLabel} · <code>{s.id}</code>
        </p>
        <p className="small source-meta">
          <span>{t("sources.items", { n: s.items })}</span>
          <span>{t("sources.chunks", { n: s.chunks })}</span>
          <span title={s.last_sync ? formatTime(s.last_sync) : undefined}>
            {s.last_sync ? t("sources.synced", { when: formatRelative(s.last_sync, now) }) : t("sources.neverSynced")}
          </span>
        </p>
        {progress && (
          <div className="progress source-progress">
            <ProgressBar done={progress.done} total={progress.total} label={t("sources.syncProgressLabel", { name: s.name })} />
            <span className="muted small">{progressText(progress, t)}</span>
          </div>
        )}
        {hasProblem(s) && (
          <div className="note note-error small" role="status">
            <span>{s.last_error ? t("sources.lastError", { error: s.last_error }) : t("sources.errorNoDetail")}</span>
            <span className="block">{signIn || mode === "password" || mode === "token" ? t("sources.errorHintAuth") : t("sources.errorHint")}</span>
            {isGoogleKind(s.kind) && /^rate limited/i.test(s.last_error ?? "") && <span className="block">{t("sources.googleQuotaHint")}</span>}
          </div>
        )}
        {s.state === "needs_auth" && !s.last_error && mode !== "none" && mode !== "env" && (
          <p className="note small">
            {signIn ? t("sources.needsSignIn") : mode === "token" ? t("sources.needsToken") : t("sources.needsPassword")}
          </p>
        )}
        {note && (
          <p className="note small" role="status">
            {note}
          </p>
        )}
        <ErrorNote error={error} />
        <div className="source-actions">
          <label className="switch">
            <input
              type="checkbox"
              role="switch"
              checked={s.enabled}
              onChange={() => void toggle()}
              disabled={busy !== null}
              aria-describedby={headingId}
            />
            <span className="switch-track" aria-hidden="true" />
            <span>{t("sources.enabled")}</span>
          </label>
          <button type="button" className="btn btn-small" onClick={() => void sync()} disabled={!s.enabled || busy !== null}>
            {busy === "sync" ? <Spinner label={t("sources.startingSync")} /> : t("sources.syncNow")}
          </button>
          {signIn && (
            <button type="button" className="btn btn-small" onClick={() => void reauth()} disabled={busy !== null}>
              {busy === "auth" ? (
                <Spinner label={t("sources.openingSignIn")} />
              ) : s.state === "needs_auth" ? (
                t("sources.signIn")
              ) : (
                t("sources.signInAgain")
              )}
            </button>
          )}
          {mode === "password" && (
            <button type="button" className="btn btn-small" onClick={onSetPassword} disabled={busy !== null}>
              {s.state === "needs_auth" ? t("sources.setPassword") : t("sources.changePassword")}
            </button>
          )}
          {mode === "token" && (
            <button type="button" className="btn btn-small" onClick={onSetPassword} disabled={busy !== null}>
              {s.state === "needs_auth" ? t("sources.setToken") : t("sources.changeToken")}
            </button>
          )}
          <button type="button" className="btn btn-small btn-danger-ghost" onClick={onRemove} disabled={busy !== null}>
            {t("sources.remove")}
          </button>
        </div>
      </div>
    </li>
  );
}

function RemoveDialog({ source, onClose, onRemoved }: { source: SourceInfo; onClose: () => void; onRemoved: () => void }) {
  const t = useT();
  const [purge, setPurge] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.sourcesRemove({ id: source.id, purge });
      onRemoved();
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={t("sources.removeDialog.title", { name: source.name })}
      description={t("sources.removeDialog.description")}
      onClose={onClose}
    >
      <form onSubmit={submit}>
        <label className="check">
          <input type="checkbox" checked={purge} onChange={(e) => setPurge(e.target.checked)} />
          <span>
            {t("sources.removeDialog.purge")}
            <span className="muted small block">{t("sources.removeDialog.purgeHint", { n: source.items })}</span>
          </span>
        </label>
        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy} data-autofocus="">
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-danger" disabled={busy}>
            {busy ? <Spinner label={t("sources.removeDialog.removing")} /> : t("sources.remove")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

/**
 * Set or replace an IMAP/CalDAV password, or a pasted API token (Slack, Notion, GitHub, Linear).
 * The value lives only in the input until sent.
 */
function PasswordDialog({ source, onClose, onSaved }: { source: SourceInfo; onClose: () => void; onSaved: () => void }) {
  const t = useT();
  const ref = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [empty, setEmpty] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const secret = ref.current?.value ?? "";
    if (!secret) {
      setEmpty(true);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.sourcesSecretSet({ id: source.id, secret });
      if (ref.current) ref.current.value = "";
      await api.sourcesSync({ id: source.id }).catch(() => {});
      onSaved();
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  const username = typeof source.settings?.username === "string" ? source.settings.username : "";
  const token = isSaasKind(source.kind);

  return (
    <Dialog
      title={token ? t("sources.tokenDialog.title", { name: source.name }) : t("sources.passwordDialog.title", { name: source.name })}
      description={t("sources.passwordDialog.description")}
      onClose={onClose}
    >
      <form onSubmit={submit} noValidate>
        {/* Lets password managers match the account. */}
        {username && <input type="text" name="username" autoComplete="username" value={username} readOnly hidden />}
        <div className="field">
          <label htmlFor="source-password">{token ? t(TOKEN_LABEL[source.kind as keyof typeof TOKEN_LABEL]) : t("sources.passwordDialog.label")}</label>
          <input
            id="source-password"
            ref={ref}
            type="password"
            autoComplete={token ? "off" : "current-password"}
            spellCheck={false}
            data-autofocus=""
            aria-invalid={empty || undefined}
            aria-describedby={empty ? "source-password-err" : undefined}
            onChange={() => setEmpty(false)}
          />
          {empty && (
            <p id="source-password-err" className="field-error small">
              {token ? t("addSource.saas.tokenMissing") : t("sources.passwordDialog.empty")}
            </p>
          )}
        </div>
        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {busy ? <Spinner label={t("sources.passwordDialog.saving")} /> : t("sources.passwordDialog.save")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
