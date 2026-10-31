/**
 * Backup and export settings (Status page) and the app-wide recovery banner, backed by the
 * daemon's `backup.*` and `export.run` methods (see the FF1 block at the end of `api.ts`).
 *
 * Backups are encrypted; the passphrase only seals the key, so scheduled backups never need it.
 * A restore outlives this component: once the backup is staged the daemon exits and the app shows
 * its offline screen until Nugget is back, so the restore's progress lives in a module store.
 */
import { useCallback, useEffect, useId, useState, useSyncExternalStore, type FormEvent, type ReactNode } from "react";
import {
  api,
  BACKUP_INTERVALS,
  BACKUP_NOTIFICATION,
  backupApi,
  errorMessage,
  events,
  isBackupStatus,
  isExportProgress,
  isMethodNotFound,
  MIN_BACKUP_PASSPHRASE,
  restoreAndRestart,
  scheduleOf,
  type BackupInfo,
  type BackupRecovery,
  type BackupSchedule,
  type BackupStatus,
  type ExportProgress,
  type ExportResult,
  type RawNotification,
  type RestorePhase,
} from "../api";
import { useAsync, useEvent, useNow } from "../hooks";
import { formatBytes, formatRelative } from "../format";
import { formatNumber, Trans, useT, type MessageKey } from "../i18n";
import { Dialog } from "./Dialog";
import { Badge, EmptyState, ErrorNote, Loading, NotAvailable, Spinner } from "./ui";

const SCHEDULES: { id: BackupSchedule; label: MessageKey }[] = [
  { id: "manual", label: "backup.schedule.manual" },
  { id: "daily", label: "backup.schedule.daily" },
  { id: "weekly", label: "backup.schedule.weekly" },
];

const KEEP_MIN = 1;
const KEEP_MAX = 365;

const OS_LABEL: Record<string, MessageKey> = {
  linux: "onboarding.perm.os.linux",
  macos: "onboarding.perm.os.macos",
  windows: "onboarding.perm.os.windows",
};

const EXPORT_PHASE: Record<string, MessageKey> = {
  items: "backup.export.progress.items",
  graph: "backup.export.progress.graph",
  audit: "backup.export.progress.audit",
  egress: "backup.export.progress.egress",
  vault: "backup.export.progress.vault",
};

const when = (rfc3339: string) => new Date(rfc3339);

// ---- the restore in progress (outlives the page while Nugget restarts) ----

export interface RestoreJob {
  phase: RestorePhase | "failed";
  /** The restored backup's `created_at`. */
  createdAt: string;
  /** Whether a passphrase was given (a failure without one may just need it). */
  withPassphrase: boolean;
  error?: unknown;
}

let restoreJob: RestoreJob | null = null;
const restoreListeners = new Set<() => void>();

function setRestoreJob(job: RestoreJob | null) {
  restoreJob = job;
  restoreListeners.forEach((l) => l());
}

function subscribeRestore(l: () => void) {
  restoreListeners.add(l);
  return () => restoreListeners.delete(l);
}

export function useRestoreJob(): RestoreJob | null {
  return useSyncExternalStore(subscribeRestore, () => restoreJob);
}

/** Forget a finished restore (tests, and the "dismiss" of its result). */
export function resetRestoreJob(): void {
  setRestoreJob(null);
}

function restoreBusy(job: RestoreJob | null): boolean {
  return job?.phase === "staging" || job?.phase === "restarting";
}

/** Stage `backup`, let the daemon exit, start it again and wait until it answers. Never throws. */
export async function startRestore(backup: BackupInfo, passphrase: string): Promise<void> {
  if (restoreBusy(restoreJob)) return;
  const base = { createdAt: backup.created_at, withPassphrase: passphrase !== "" };
  setRestoreJob({ ...base, phase: "staging" });
  try {
    await restoreAndRestart({ path: backup.path, ...(passphrase ? { passphrase } : {}) }, (phase) => setRestoreJob({ ...base, phase }));
  } catch (error) {
    setRestoreJob({ ...base, phase: "failed", error });
  }
}

/** The restore's progress or outcome, for a polite live region. */
function RestoreProgress({ job }: { job: RestoreJob | null }) {
  const t = useT();
  if (!job) return null;
  if (job.phase === "staging" || job.phase === "restarting") {
    return (
      <p className="note small">
        <span className="spinner" aria-hidden="true" /> {t(job.phase === "staging" ? "backup.restore.staging" : "backup.restore.restarting")}
      </p>
    );
  }
  if (job.phase === "done") {
    return (
      <p className="note small">
        {t("backup.restore.done", { when: when(job.createdAt) })}{" "}
        <button type="button" className="btn btn-ghost btn-small" onClick={resetRestoreJob}>
          {t("common.close")}
        </button>
      </p>
    );
  }
  return (
    <div className="note note-error small" role="alert">
      <span>{t("backup.restore.failed", { error: errorMessage(job.error) })}</span>
      {!job.withPassphrase && <span> {t("backup.restore.needPassphrase")}</span>}
    </div>
  );
}

// ---- restore dialog ----

export function RestoreDialog({ backup, onClose }: { backup: BackupInfo; onClose: () => void }) {
  const t = useT();
  const job = useRestoreJob();
  const id = useId();
  const [passphrase, setPassphrase] = useState("");
  const [submitted, setSubmitted] = useState(false);
  const busy = restoreBusy(job);

  // Once the backup is staged the daemon goes away: close, the page shows the rest.
  useEffect(() => {
    if (submitted && (job?.phase === "restarting" || job?.phase === "done")) onClose();
  }, [submitted, job?.phase, onClose]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setSubmitted(true);
    const p = passphrase;
    setPassphrase("");
    void startRestore(backup, p);
  };

  const failed = submitted && job?.phase === "failed";
  return (
    <Dialog title={t("backup.restore.title")} description={t("backup.restore.body", { when: when(backup.created_at) })} onClose={busy ? () => {} : onClose}>
      <form onSubmit={submit}>
        <p className="small">{t("backup.restore.setAside")}</p>
        <div className="field">
          <label htmlFor={`${id}-pass`}>{t("backup.restore.passphrase")}</label>
          <input
            id={`${id}-pass`}
            type="password"
            autoComplete="off"
            value={passphrase}
            onChange={(e) => setPassphrase(e.target.value)}
            disabled={busy}
            aria-describedby={`${id}-hint`}
            data-autofocus
          />
          <p id={`${id}-hint`} className="muted small field-hint">
            {t("backup.restore.passphraseHint", { os: osLabel(t, backup.os) })}
          </p>
        </div>
        <p className="muted small">{t("backup.restore.restartNote")}</p>
        <div aria-live="polite">{submitted && <RestoreProgress job={job} />}</div>
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy}>
            {busy ? <Spinner label={t("backup.restore.staging")} /> : failed ? t("backup.restore.retry") : t("backup.restore.confirm")}
          </button>
        </div>
      </form>
    </Dialog>
  );
}

function osLabel(t: ReturnType<typeof useT>, os: string): string {
  const key = OS_LABEL[os];
  return key ? t(key) : os;
}

// ---- recovery ----

/** What the user sees when the daemon refused a damaged database. */
export function RecoveryNotice({ recovery, onRestore }: { recovery: BackupRecovery; onRestore: (b: BackupInfo) => void }) {
  const t = useT();
  const job = useRestoreJob();
  const latest = recovery.latest_backup;
  return (
    <div className="note note-error" role="alert" style={{ display: "block" }}>
      <p>
        <strong>{t("backup.recovery.title")}</strong>
      </p>
      <p>
        <Trans k="backup.recovery.body" args={{ path: recovery.moved_to ?? recovery.db_path }} tags={{ code: (c) => <code>{c}</code> }} />
      </p>
      {recovery.reason && <p className="small">{t("backup.recovery.reason", { reason: recovery.reason })}</p>}
      {latest ? (
        <div className="row" style={{ justifyContent: "flex-start", alignItems: "center" }}>
          <button type="button" className="btn btn-primary btn-small" onClick={() => onRestore(latest)} disabled={restoreBusy(job)}>
            {t("backup.recovery.restoreLatest")}
          </button>
          <span className="small">{t("backup.recovery.latest", { when: when(latest.created_at), items: latest.items })}</span>
        </div>
      ) : (
        <p className="small">{t("backup.recovery.noBackup")}</p>
      )}
    </div>
  );
}

/**
 * App-wide: shown above every page while the daemon reports `recovery`, so a damaged database
 * is explained wherever the user is. Renders nothing otherwise (or on daemons without backups).
 */
export function RecoveryBanner() {
  const status = useAsync(backupApi.status);
  const job = useRestoreJob();
  const [restoring, setRestoring] = useState<BackupInfo | null>(null);
  const { reload } = status;
  useEvent(events.onNotification, (n: RawNotification) => {
    if (n.method === BACKUP_NOTIFICATION.changed) reload();
  });
  useEffect(() => {
    if (job?.phase === "done") reload();
  }, [job?.phase, reload]);
  const recovery = status.data?.recovery;
  if (!recovery) return null;
  return (
    <div style={{ padding: "12px 16px 0" }}>
      <RecoveryNotice recovery={recovery} onRestore={setRestoring} />
      <div aria-live="polite">
        <RestoreProgress job={job} />
      </div>
      {restoring && <RestoreDialog backup={restoring} onClose={() => setRestoring(null)} />}
    </div>
  );
}

// ---- the settings section ----

export function BackupSettings() {
  const t = useT();
  const status = useAsync(backupApi.status);
  const job = useRestoreJob();
  const [restoring, setRestoring] = useState<BackupInfo | null>(null);
  const [live, setLive] = useState<BackupStatus | null>(null);
  const s = live ?? status.data;
  const backups = useAsync(() => (s?.folder ? backupApi.list(s.folder) : Promise.resolve({ backups: [] })), [s?.folder]);

  const { reload } = status;
  const reloadList = backups.reload;
  const refresh = useCallback(() => {
    setLive(null);
    reload();
    reloadList();
  }, [reload, reloadList]);

  useEvent(events.onNotification, (n: RawNotification) => {
    if (n.method !== BACKUP_NOTIFICATION.changed) return;
    if (isBackupStatus(n.params)) setLive(n.params);
    else reload();
    reloadList();
  });
  useEffect(() => {
    if (job?.phase === "done") refresh();
  }, [job?.phase, refresh]);

  return (
    <section className="status-card" aria-labelledby="backup-title">
      <h2 id="backup-title">{t("backup.title")}</h2>
      <p className="muted small">{t("backup.intro")}</p>
      {isMethodNotFound(status.error) ? (
        <NotAvailable />
      ) : (
        <>
          <ErrorNote error={status.error} onRetry={refresh} />
          {status.loading && !s && <Loading label={t("backup.loading")} lines={3} />}
          {s && (
            <>
              {/* A `recovery` state is shown app-wide by `RecoveryBanner`, above this page too. */}
              <ScheduleForm status={s} onSaved={(next) => setLive(next)} />
              <PassphraseForm passphraseSet={s.passphrase_set} onSet={refresh} />
              <BackupNow status={s} onDone={refresh} />
              <h3 className="ask-sub">{t("backup.list.title")}</h3>
              <div aria-live="polite">
                <RestoreProgress job={job} />
              </div>
              <BackupList folder={s.folder} state={backups} onRestore={setRestoring} />
            </>
          )}
          <ExportForm />
        </>
      )}
      {restoring && <RestoreDialog backup={restoring} onClose={() => setRestoring(null)} />}
    </section>
  );
}

function Field({ id, label, hint, error, children }: { id: string; label: string; hint?: ReactNode; error?: string | null; children: ReactNode }) {
  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      {children}
      {hint && (
        <p id={`${id}-hint`} className="muted small field-hint">
          {hint}
        </p>
      )}
      {error && (
        <p id={`${id}-err`} className="small field-error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** `aria-describedby` for a `Field`'s control. */
function describedBy(id: string, hint: boolean, error: unknown): string | undefined {
  return [hint && `${id}-hint`, error && `${id}-err`].filter(Boolean).join(" ") || undefined;
}

function ScheduleForm({ status, onSaved }: { status: BackupStatus; onSaved: (s: BackupStatus) => void }) {
  const t = useT();
  const id = useId();
  const [folder, setFolder] = useState(status.folder ?? "");
  const [schedule, setSchedule] = useState<BackupSchedule>(scheduleOf(status.interval_hours));
  const [keep, setKeep] = useState(String(status.keep));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [invalid, setInvalid] = useState<{ folder?: string; keep?: string }>({});
  const [saved, setSaved] = useState(false);

  const choose = async () => {
    try {
      const [picked] = await api.pickFolders(false);
      if (picked) setFolder(picked);
    } catch (e) {
      setError(e);
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setSaved(false);
    const n = Number(keep);
    const problems: { folder?: string; keep?: string } = {};
    if (!folder.trim()) problems.folder = t("backup.error.folder");
    if (!Number.isInteger(n) || n < KEEP_MIN || n > KEEP_MAX) problems.keep = t("backup.error.keep", { min: KEEP_MIN, max: KEEP_MAX });
    setInvalid(problems);
    if (problems.folder || problems.keep) return;
    setBusy(true);
    setError(null);
    try {
      const next = await backupApi.configure({ folder: folder.trim(), interval_hours: BACKUP_INTERVALS[schedule], keep: n });
      onSaved(next);
      setSaved(true);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate aria-busy={busy}>
      <Field id={`${id}-folder`} label={t("backup.folder")} hint={t("backup.folderHint")} error={invalid.folder}>
        <div className="inline-row">
          <input
            id={`${id}-folder`}
            type="text"
            value={folder}
            onChange={(e) => setFolder(e.target.value)}
            spellCheck={false}
            autoComplete="off"
            aria-describedby={describedBy(`${id}-folder`, true, invalid.folder)}
            aria-invalid={invalid.folder ? true : undefined}
          />
          <button type="button" className="btn btn-small" onClick={() => void choose()}>
            {t("backup.choose")}
          </button>
        </div>
      </Field>
      <div className="row" style={{ justifyContent: "flex-start", alignItems: "flex-end" }}>
        <Field id={`${id}-schedule`} label={t("backup.schedule")}>
          <select id={`${id}-schedule`} value={schedule} onChange={(e) => setSchedule(e.target.value as BackupSchedule)}>
            {SCHEDULES.map((c) => (
              <option key={c.id} value={c.id}>
                {t(c.label)}
              </option>
            ))}
          </select>
        </Field>
        <Field id={`${id}-keep`} label={t("backup.keep")} hint={t("backup.keepHint")} error={invalid.keep}>
          <input
            id={`${id}-keep`}
            type="number"
            inputMode="numeric"
            min={KEEP_MIN}
            max={KEEP_MAX}
            step={1}
            value={keep}
            onChange={(e) => setKeep(e.target.value)}
            aria-describedby={describedBy(`${id}-keep`, true, invalid.keep)}
            aria-invalid={invalid.keep ? true : undefined}
          />
        </Field>
      </div>
      <div className="row" style={{ justifyContent: "flex-start", alignItems: "center" }}>
        <button type="submit" className="btn btn-small" disabled={busy}>
          {busy ? <Spinner label={t("backup.saving")} /> : t("backup.save")}
        </button>
        <span aria-live="polite" className="muted small">
          {saved && t("backup.saved")}
        </span>
      </div>
      <ErrorNote error={error} />
    </form>
  );
}

function PassphraseForm({ passphraseSet, onSet }: { passphraseSet: boolean; onSet: () => void }) {
  const t = useT();
  const id = useId();
  const [pass, setPass] = useState("");
  const [confirm, setConfirm] = useState("");
  const [invalid, setInvalid] = useState<{ pass?: string; confirm?: string }>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [saved, setSaved] = useState(false);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setSaved(false);
    const problems: { pass?: string; confirm?: string } = {};
    if ([...pass].length < MIN_BACKUP_PASSPHRASE) problems.pass = t("backup.passphrase.tooShort", { min: MIN_BACKUP_PASSPHRASE });
    else if (pass !== confirm) problems.confirm = t("backup.passphrase.mismatch");
    setInvalid(problems);
    if (problems.pass || problems.confirm) return;
    setBusy(true);
    setError(null);
    try {
      await backupApi.setPassphrase(pass);
      setPass("");
      setConfirm("");
      setSaved(true);
      onSet();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <h3 className="ask-sub">
        {t("backup.passphrase.title")}{" "}
        <Badge tone={passphraseSet ? "good" : "warn"}>{t(passphraseSet ? "backup.passphrase.isSet" : "backup.passphrase.notSet")}</Badge>
      </h3>
      <p className="muted small">{t("backup.passphrase.intro")}</p>
      <form onSubmit={submit} noValidate aria-busy={busy}>
        <Field id={`${id}-pass`} label={t("backup.passphrase.new")} hint={t("backup.passphrase.hint", { min: MIN_BACKUP_PASSPHRASE })} error={invalid.pass}>
          <input
            id={`${id}-pass`}
            type="password"
            autoComplete="new-password"
            value={pass}
            onChange={(e) => setPass(e.target.value)}
            aria-describedby={describedBy(`${id}-pass`, true, invalid.pass)}
            aria-invalid={invalid.pass ? true : undefined}
          />
        </Field>
        <Field id={`${id}-confirm`} label={t("backup.passphrase.confirm")} error={invalid.confirm}>
          <input
            id={`${id}-confirm`}
            type="password"
            autoComplete="new-password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            aria-describedby={describedBy(`${id}-confirm`, false, invalid.confirm)}
            aria-invalid={invalid.confirm ? true : undefined}
          />
        </Field>
        <p className="note note-warn small">{t("backup.passphrase.warning")}</p>
        <div className="row" style={{ justifyContent: "flex-start", alignItems: "center" }}>
          <button type="submit" className="btn btn-small" disabled={busy}>
            {busy ? <Spinner label={t("backup.passphrase.saving")} /> : t(passphraseSet ? "backup.passphrase.change" : "backup.passphrase.submit")}
          </button>
          <span aria-live="polite" className="muted small">
            {saved && t("backup.passphrase.saved")}
          </span>
        </div>
        <ErrorNote error={error} />
      </form>
    </>
  );
}

function BackupNow({ status, onDone }: { status: BackupStatus; onDone: () => void }) {
  const t = useT();
  const now = useNow();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [done, setDone] = useState<BackupInfo | null>(null);
  const running = busy || status.running;
  const ready = !!status.folder && status.passphrase_set;

  const run = async () => {
    setBusy(true);
    setError(null);
    setDone(null);
    try {
      setDone(await backupApi.run());
      onDone();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const last = status.last_backup;
  return (
    <>
      <h3 className="ask-sub">{t("backup.now.title")}</h3>
      <dl className="stats">
        <div>
          <dt>{t("backup.last")}</dt>
          <dd className="stat-text">
            {last ? (
              <>
                {formatRelative(last.created_at, now)}
                <div className="muted small">{t("backup.itemsSize", { items: last.items, size: formatBytes(last.bytes) })}</div>
              </>
            ) : (
              <span className="muted">{t("backup.none")}</span>
            )}
          </dd>
        </div>
        <div>
          <dt>{t("backup.nextDue")}</dt>
          <dd className="stat-text">
            {status.interval_hours <= 0 ? (
              <span className="muted">{t("backup.nextManual")}</span>
            ) : status.next_due ? (
              formatRelative(status.next_due, now)
            ) : (
              <span className="muted">{t("backup.nextNotSet")}</span>
            )}
          </dd>
        </div>
      </dl>
      {status.last_error && (
        <p className="note note-warn small">{t("backup.lastError", { error: status.last_error })}</p>
      )}
      <div className="row" style={{ justifyContent: "flex-start", alignItems: "center" }}>
        <button type="button" className="btn btn-primary btn-small" onClick={() => void run()} disabled={running || !ready} aria-describedby={ready ? undefined : "backup-needs-setup"}>
          {t("backup.now")}
        </button>
        {!ready && (
          <span id="backup-needs-setup" className="muted small">
            {t("backup.needsSetup")}
          </span>
        )}
      </div>
      <div aria-live="polite">
        {running && (
          <p className="note small">
            <span className="spinner" aria-hidden="true" /> {t("backup.running")}
          </p>
        )}
        {!running && done && <p className="note small">{t("backup.done", { items: done.items, size: formatBytes(done.bytes) })}</p>}
      </div>
      <ErrorNote error={error} />
    </>
  );
}

function BackupList({
  folder,
  state,
  onRestore,
}: {
  folder: string | null;
  state: { data: { backups: BackupInfo[] } | undefined; error: unknown; loading: boolean; reload: () => void };
  onRestore: (b: BackupInfo) => void;
}) {
  const t = useT();
  const job = useRestoreJob();
  if (!folder) return <p className="muted small">{t("backup.list.noFolder")}</p>;
  if (state.error) return <ErrorNote error={state.error} onRetry={state.reload} />;
  if (state.loading && !state.data) return <Loading lines={2} />;
  const list = state.data?.backups ?? [];
  if (list.length === 0) return <EmptyState title={t("backup.list.emptyTitle")}>{t("backup.list.empty")}</EmptyState>;
  return (
    <ul className="vault-pages" aria-label={t("backup.list.title")}>
      {list.map((b) => (
        <li key={b.path}>
          <span>
            <b>{t("backup.list.when", { when: when(b.created_at) })}</b>{" "}
            <span className="muted small">
              {t("backup.list.meta", { items: b.items, size: formatBytes(b.bytes), version: b.app_version, os: osLabel(t, b.os) })}
            </span>
          </span>{" "}
          <button
            type="button"
            className="btn btn-ghost btn-small"
            onClick={() => onRestore(b)}
            disabled={restoreBusy(job)}
            aria-label={t("backup.restore.buttonLabel", { when: when(b.created_at) })}
          >
            {t("backup.restore.button")}
          </button>
        </li>
      ))}
    </ul>
  );
}

function ExportForm() {
  const t = useT();
  const id = useId();
  const [folder, setFolder] = useState("");
  const [invalid, setInvalid] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [progress, setProgress] = useState<ExportProgress | null>(null);
  const [result, setResult] = useState<ExportResult | null>(null);

  useEvent(events.onNotification, (n: RawNotification) => {
    if (busy && n.method === BACKUP_NOTIFICATION.exportProgress && isExportProgress(n.params)) setProgress(n.params);
  });

  const choose = async () => {
    try {
      const [picked] = await api.pickFolders(false);
      if (picked) setFolder(picked);
    } catch (e) {
      setError(e);
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!folder.trim()) {
      setInvalid(t("backup.export.error.folder"));
      return;
    }
    setInvalid(null);
    setBusy(true);
    setError(null);
    setResult(null);
    setProgress(null);
    try {
      setResult(await backupApi.exportRun(folder.trim()));
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const phaseKey = progress ? EXPORT_PHASE[progress.phase] : undefined;
  return (
    <>
      <h3 className="ask-sub">{t("backup.export.title")}</h3>
      <p className="muted small">{t("backup.export.intro")}</p>
      <p className="note note-warn small">{t("backup.export.warning")}</p>
      <form onSubmit={submit} noValidate aria-busy={busy}>
        <Field id={`${id}-folder`} label={t("backup.export.folder")} error={invalid}>
          <div className="inline-row">
            <input
              id={`${id}-folder`}
              type="text"
              value={folder}
              onChange={(e) => setFolder(e.target.value)}
              spellCheck={false}
              autoComplete="off"
              aria-describedby={describedBy(`${id}-folder`, false, invalid)}
              aria-invalid={invalid ? true : undefined}
            />
            <button type="button" className="btn btn-small" onClick={() => void choose()}>
              {t("backup.choose")}
            </button>
          </div>
        </Field>
        <button type="submit" className="btn btn-small" disabled={busy}>
          {t("backup.export.submit")}
        </button>
      </form>
      <div aria-live="polite">
        {busy && (
          <p className="note small">
            <span className="spinner" aria-hidden="true" /> {phaseKey && progress ? t(phaseKey, { n: progress.done }) : t("backup.export.running")}
          </p>
        )}
        {result && (
          <div className="note small" style={{ display: "block" }}>
            <p>
              <Trans k="backup.export.done" args={{ path: result.path }} tags={{ code: (c) => <code>{c}</code> }} />
            </p>
          </div>
        )}
      </div>
      {result && (
        <dl className="stats">
          <div>
            <dt>{t("backup.export.count.items")}</dt>
            <dd>{formatNumber(result.items)}</dd>
          </div>
          <div>
            <dt>{t("backup.export.count.entities")}</dt>
            <dd>{formatNumber(result.entities)}</dd>
          </div>
          <div>
            <dt>{t("backup.export.count.claims")}</dt>
            <dd>{formatNumber(result.claims)}</dd>
          </div>
          <div>
            <dt>{t("backup.export.count.audit")}</dt>
            <dd>{formatNumber(result.audit_entries)}</dd>
          </div>
          <div>
            <dt>{t("backup.export.count.egress")}</dt>
            <dd>{formatNumber(result.egress_rows)}</dd>
          </div>
          <div>
            <dt>{t("backup.export.count.vault")}</dt>
            <dd>{formatNumber(result.vault_files)}</dd>
          </div>
        </dl>
      )}
      <ErrorNote error={error} />
    </>
  );
}
