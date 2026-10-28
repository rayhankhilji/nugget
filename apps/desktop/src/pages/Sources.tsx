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
import { formatRelative, formatTime, SOURCE_KIND_LABEL, SOURCE_STATE_LABEL, SOURCE_STATE_TONE } from "../format";
import { AddSourceWizard } from "../components/AddSourceWizard";
import { Dialog } from "../components/Dialog";
import { SourceIcon } from "../components/SourceIcon";
import { Badge, Empty, ErrorNote, NotAvailable, PageHeader, ProgressBar, Spinner } from "../components/ui";

const OAUTH_KINDS = new Set(["gmail", "gcal"]);
const PASSWORD_KINDS = new Set(["imap", "caldav"]);

export function SourcesPage() {
  const loaded = useAsync(api.sourcesList);
  const [sources, setSources] = useState<SourceInfo[] | null>(null);
  const [progress, setProgress] = useState<Record<string, IngestProgress>>({});
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<SourceInfo | null>(null);
  const [passwordFor, setPasswordFor] = useState<SourceInfo | null>(null);
  const [announce, setAnnounce] = useState("");
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
      setAnnounce("Syncing every enabled source.");
    } catch (e) {
      setSyncAllError(errorMessage(e));
    }
  };

  const unavailable = isMethodNotFound(loaded.error);
  const list = sources ?? [];

  return (
    <div className="page">
      <PageHeader
        title="Sources"
        description="Folders, notes, mail and calendars Nugget keeps indexed on this device. Changes show up in search within a minute."
        actions={
          !unavailable && (
            <>
              <button type="button" className="btn btn-ghost" onClick={() => void syncAll()} disabled={list.length === 0}>
                Sync all
              </button>
              <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
                Add source
              </button>
            </>
          )
        }
      />
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
      {syncAllError && (
        <p className="note note-error" role="alert">
          {syncAllError}
        </p>
      )}

      {unavailable ? (
        <NotAvailable what="Sources" />
      ) : (
        <>
          <ErrorNote error={loaded.error} onRetry={loaded.reload} />
          {loaded.loading && !sources && <Spinner />}
          {sources && list.length === 0 && (
            <Empty title="No sources yet">Add a folder, an Obsidian vault, or connect mail and calendars to make them searchable.</Empty>
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
            setAnnounce(`Added ${s.name}.`);
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
            setAnnounce(`Removed ${removing.name}.`);
            setRemoving(null);
          }}
        />
      )}
      {passwordFor && (
        <PasswordDialog
          source={passwordFor}
          onClose={() => setPasswordFor(null)}
          onSaved={() => {
            setAnnounce(`Password saved for ${passwordFor.name}.`);
            setPasswordFor(null);
            reloadSoon();
          }}
        />
      )}
    </div>
  );
}

function progressText(p: IngestProgress): string {
  const verb = p.phase === "scanning" ? "Scanning" : "Indexing";
  if (p.total) return `${verb} ${p.done.toLocaleString()} of ${p.total.toLocaleString()}`;
  return p.done > 0 ? `${verb}… ${p.done.toLocaleString()} so far` : `${verb}…`;
}

function SourceRow({
  source: s,
  progress,
  now,
  onChanged,
  onRemove,
  onSetPassword,
  onAnnounce,
}: {
  source: SourceInfo;
  progress: IngestProgress | undefined;
  now: number;
  onChanged: (s: SourceInfo) => void;
  onRemove: () => void;
  onSetPassword: () => void;
  onAnnounce: (text: string) => void;
}) {
  const [busy, setBusy] = useState<null | "toggle" | "sync" | "auth">(null);
  const [error, setError] = useState<unknown>(null);
  const [note, setNote] = useState<string | null>(null);
  const kindLabel = SOURCE_KIND_LABEL[s.kind] ?? s.kind;
  const tone = SOURCE_STATE_TONE[s.state] ?? "neutral";
  const headingId = `source-${s.id}`;

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
      onAnnounce(`${s.name} ${next.enabled ? "enabled" : "disabled"}.`);
    });

  const sync = () =>
    run("sync", async () => {
      await api.sourcesSync({ id: s.id });
      onAnnounce(`Syncing ${s.name}.`);
    });

  const reauth = () =>
    run("auth", async () => {
      const { url } = await api.sourcesAuthBegin({ id: s.id });
      await api.openUrl(url);
      setNote("Finish signing in in your browser. This updates when you're done.");
    });

  return (
    <li className="source">
      <div className="source-icon-wrap">
        <SourceIcon kind={s.kind} />
      </div>
      <div className="source-body">
        <div className="source-head">
          <h2 id={headingId} className="source-name">
            {s.name}
          </h2>
          <Badge tone={tone}>{SOURCE_STATE_LABEL[s.state] ?? s.state}</Badge>
        </div>
        <p className="muted small source-meta">
          {kindLabel} · <code>{s.id}</code>
        </p>
        <p className="small source-meta">
          <span>{s.items.toLocaleString()} items</span>
          <span>{s.chunks.toLocaleString()} chunks</span>
          <span title={s.last_sync ? formatTime(s.last_sync) : undefined}>
            {s.last_sync ? `Synced ${formatRelative(s.last_sync, now)}` : "Never synced"}
          </span>
        </p>
        {progress && (
          <div className="progress source-progress">
            <ProgressBar done={progress.done} total={progress.total} label={`${s.name} sync progress`} />
            <span className="muted small">{progressText(progress)}</span>
          </div>
        )}
        {s.last_error && (
          <p className="note note-error small" role="status">
            {s.last_error}
          </p>
        )}
        {s.state === "needs_auth" && !s.last_error && (
          <p className="note small">
            {OAUTH_KINDS.has(s.kind)
              ? "Nugget needs you to sign in again to keep this source up to date."
              : "Nugget needs the password for this source."}
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
            <span>Enabled</span>
          </label>
          <button type="button" className="btn btn-small" onClick={() => void sync()} disabled={!s.enabled || busy !== null}>
            {busy === "sync" ? <Spinner label="Starting sync" /> : "Sync now"}
          </button>
          {OAUTH_KINDS.has(s.kind) && (
            <button type="button" className="btn btn-small" onClick={() => void reauth()} disabled={busy !== null}>
              {busy === "auth" ? <Spinner label="Opening sign-in" /> : s.state === "needs_auth" ? "Sign in" : "Sign in again"}
            </button>
          )}
          {PASSWORD_KINDS.has(s.kind) && (
            <button type="button" className="btn btn-small" onClick={onSetPassword} disabled={busy !== null}>
              {s.state === "needs_auth" ? "Set password" : "Change password"}
            </button>
          )}
          <button type="button" className="btn btn-small btn-danger-ghost" onClick={onRemove} disabled={busy !== null}>
            Remove
          </button>
        </div>
      </div>
    </li>
  );
}

function RemoveDialog({ source, onClose, onRemoved }: { source: SourceInfo; onClose: () => void; onRemoved: () => void }) {
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
    <Dialog title={`Remove ${source.name}?`} description="Nugget stops syncing this source. Your original files, mail and events are not touched." onClose={onClose}>
      <form onSubmit={submit}>
        <label className="check">
          <input type="checkbox" checked={purge} onChange={(e) => setPurge(e.target.checked)} />
          <span>
            Also delete indexed data
            <span className="muted small block">
              Removes {source.items.toLocaleString()} items and their search index from this device.
            </span>
          </span>
        </label>
        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy} data-autofocus="">
            Cancel
          </button>
          <button type="submit" className="btn btn-danger" disabled={busy}>
            {busy ? <Spinner label="Removing" /> : "Remove"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

/** Set or replace an IMAP/CalDAV password. The value lives only in the input until sent. */
function PasswordDialog({ source, onClose, onSaved }: { source: SourceInfo; onClose: () => void; onSaved: () => void }) {
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

  return (
    <Dialog title={`Password for ${source.name}`} description="Saved to your OS keychain. Nugget never shows it again." onClose={onClose}>
      <form onSubmit={submit} noValidate>
        {/* Lets password managers match the account. */}
        {username && <input type="text" name="username" autoComplete="username" value={username} readOnly hidden />}
        <div className="field">
          <label htmlFor="source-password">Password or app password</label>
          <input
            id="source-password"
            ref={ref}
            type="password"
            autoComplete="current-password"
            data-autofocus=""
            aria-invalid={empty || undefined}
            aria-describedby={empty ? "source-password-err" : undefined}
            onChange={() => setEmpty(false)}
          />
          {empty && (
            <p id="source-password-err" className="field-error small">
              Enter the password.
            </p>
          )}
        </div>
        <ErrorNote error={error} />
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {busy ? <Spinner label="Saving" /> : "Save"}
          </button>
        </div>
      </form>
    </Dialog>
  );
}
