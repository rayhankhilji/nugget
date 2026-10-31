import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import {
  api,
  ApiError,
  errorMessage,
  events,
  sourcesFromChange,
  type ConnectorKind,
  type SourceInfo,
  type SourceKind,
  type SourceSettings,
} from "../api";
import { useEvent } from "../hooks";
import { t as tGlobal, Trans, useT, type MessageKey, type TagRenderer } from "../i18n";
import { baseName, sourceKindLabel, splitList } from "../format";
import { KindPicker } from "../sources/KindPicker";
import { NEW_KIND_TITLE, NewKindForm, isNewKind } from "../sources/forms";
import { Check } from "../sources/form";
import { EMPTY_GOOGLE, GDRIVE_SCOPE, GOOGLE_API, googleSettings, type GoogleKind } from "../sources/kinds";
import { Dialog } from "./Dialog";
import { Spinner } from "./ui";

const KIND_BLURB: Record<SourceKind, MessageKey> = {
  files: "addSource.blurb.files",
  obsidian: "addSource.blurb.obsidian",
  gmail: "addSource.blurb.gmail",
  gcal: "addSource.blurb.gcal",
  gdrive: "addSource.blurb.gdrive",
  imap: "addSource.blurb.imap",
  caldav: "addSource.blurb.caldav",
};

const BOLD: Record<string, TagRenderer> = { b: (c) => <b>{c}</b> };
const CODE: Record<string, TagRenderer> = { code: (c) => <code>{c}</code> };

const GOOGLE_CREDENTIALS_URL = "https://console.cloud.google.com/apis/credentials";

/** Wizard for `sources.add`: pick a kind, fill in its settings, then connect. */
export function AddSourceWizard({
  onClose,
  onAdded,
  initialKind,
}: {
  onClose: () => void;
  onAdded: (s: SourceInfo) => void;
  /** Open straight at this kind's form (Back still leads to the kind picker). */
  initialKind?: ConnectorKind;
}) {
  const t = useT();
  const [kind, setKind] = useState<ConnectorKind>(initialKind ?? "files");
  const [step, setStep] = useState<"kind" | "form">(initialKind ? "form" : "kind");

  const title = step === "kind" ? t("addSource.title") : t(isNewKind(kind) ? NEW_KIND_TITLE[kind] : ADD_TITLE[kind]);

  return (
    <Dialog title={title} onClose={onClose} wide>
      {step === "kind" ? (
        <KindPicker value={kind} onChange={setKind} onNext={() => setStep("form")} onCancel={onClose} blurb={KIND_BLURB} />
      ) : isNewKind(kind) ? (
        <NewKindForm kind={kind} onBack={() => setStep("kind")} onClose={onClose} onAdded={onAdded} />
      ) : (
        <KindForm kind={kind} onBack={() => setStep("kind")} onClose={onClose} onAdded={onAdded} />
      )}
    </Dialog>
  );
}

const ADD_TITLE: Record<SourceKind, MessageKey> = {
  files: "addSource.addTitle.files",
  obsidian: "addSource.addTitle.obsidian",
  gmail: "addSource.addTitle.gmail",
  gcal: "addSource.addTitle.gcal",
  gdrive: "addSource.addTitle.gdrive",
  imap: "addSource.addTitle.imap",
  caldav: "addSource.addTitle.caldav",
};

function useAutofocus<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  useEffect(() => {
    ref.current?.focus();
  }, []);
  return ref;
}

// ---- step 2: settings ----

interface FormProps {
  kind: SourceKind;
  onBack: () => void;
  onClose: () => void;
  onAdded: (s: SourceInfo) => void;
}

function KindForm(props: FormProps) {
  switch (props.kind) {
    case "files":
      return <FolderForm {...props} />;
    case "obsidian":
      return <VaultForm {...props} />;
    case "gmail":
    case "gcal":
    case "gdrive":
      return <GoogleForm {...props} kind={props.kind} />;
    case "imap":
      return <ImapForm {...props} />;
    case "caldav":
      return <CaldavForm {...props} />;
  }
}

/**
 * `sources.add` once; later attempts (after a failed password or auth step) update the source
 * that was already created instead of adding a duplicate.
 */
function useSaveSource(kind: SourceKind) {
  const created = useRef<SourceInfo | null>(null);
  return async (name: string, settings: SourceSettings): Promise<SourceInfo> => {
    const s = created.current
      ? await api.sourcesUpdate({ id: created.current.id, name, settings })
      : await api.sourcesAdd({ id: null, kind, name, settings });
    created.current = s;
    return s;
  };
}

/** Label + control + hint + error, wired with `aria-describedby` / `aria-invalid`. */
function Field({
  label,
  hint,
  error,
  optional,
  children,
}: {
  label: string;
  hint?: ReactNode;
  error?: string | null;
  optional?: boolean;
  children: (a: { id: string; "aria-describedby"?: string; "aria-invalid"?: true }) => ReactNode;
}) {
  const t = useT();
  const id = useId();
  const hintId = `${id}-hint`;
  const errId = `${id}-err`;
  const describedBy = [hint ? hintId : "", error ? errId : ""].filter(Boolean).join(" ") || undefined;
  return (
    <div className="field">
      <label htmlFor={id}>
        {label}
        {optional && <span className="muted small"> {t("addSource.optional")}</span>}
      </label>
      {children({ id, "aria-describedby": describedBy, "aria-invalid": error ? true : undefined })}
      {hint && (
        <p id={hintId} className="muted small field-hint">
          {hint}
        </p>
      )}
      {error && (
        <p id={errId} className="field-error small">
          {error}
        </p>
      )}
    </div>
  );
}

function FormError({ error }: { error: unknown }) {
  const t = useT();
  if (!error) return null;
  const e = ApiError.from(error);
  return (
    <p className="note note-error" role="alert">
      {e.isMethodNotFound ? t("common.notAvailableSentence") : e.isOffline ? t("common.offlineHint") : e.message}
    </p>
  );
}

/**
 * Back and submit. `blocked` says why submitting can't work yet: the submit stays focusable
 * (aria-disabled, so pressing it explains) and the reason is shown next to it.
 */
function Actions({ onBack, busy, submitLabel, blocked }: { onBack: () => void; busy: boolean; submitLabel: string; blocked?: string | null }) {
  const t = useT();
  const hintId = useId();
  return (
    <div className="dialog-actions">
      {blocked && (
        <p id={hintId} className="muted small dialog-actions-hint">
          {blocked}
        </p>
      )}
      <button type="button" className="btn" onClick={onBack} disabled={busy}>
        {t("common.back")}
      </button>
      <button
        type="submit"
        className="btn btn-primary"
        disabled={busy}
        aria-disabled={blocked ? true : undefined}
        aria-describedby={blocked ? hintId : undefined}
      >
        {busy ? (
          <>
            <Spinner label={t("addSource.working")} /> {submitLabel}
          </>
        ) : (
          submitLabel
        )}
      </button>
    </div>
  );
}

/** `fallback` is the name used when the field is left empty (said in the hint, not as a placeholder that looks filled in). */
function NameField({ value, onChange, placeholder }: { value: string; onChange: (v: string) => void; placeholder: string }) {
  const t = useT();
  return (
    <Field label={t("addSource.name")} optional hint={t("addSource.nameHintDefault", { name: placeholder })}>
      {(a) => (
        <input
          {...a}
          type="text"
          dir="auto"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={t("addSource.namePlaceholder")}
          autoComplete="off"
        />
      )}
    </Field>
  );
}

const isAbsolutePath = (p: string) => p.startsWith("/") || p.startsWith("~") || /^[A-Za-z]:[\\/]/.test(p);

/** Native folder picker, with a typed path as the fallback. */
async function pickFolders(multiple: boolean): Promise<{ paths: string[]; error: string | null }> {
  try {
    return { paths: await api.pickFolders(multiple), error: null };
  } catch (e) {
    return { paths: [], error: tGlobal("addSource.pickerFailed", { error: errorMessage(e) }) };
  }
}

// -- folder --

function FolderForm({ kind, onBack, onClose, onAdded }: FormProps) {
  const t = useT();
  const [roots, setRoots] = useState<string[]>([]);
  const [draft, setDraft] = useState("");
  const [include, setInclude] = useState("");
  const [exclude, setExclude] = useState("");
  const [name, setName] = useState("");
  const [rootsError, setRootsError] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const save = useSaveSource(kind);
  const browseRef = useAutofocus<HTMLButtonElement>();

  const addRoots = (paths: string[]) => {
    setRoots((rs) => [...rs, ...paths.filter((p) => !rs.includes(p))]);
    setRootsError(null);
  };

  const browse = async () => {
    const { paths, error } = await pickFolders(true);
    addRoots(paths);
    if (error) setRootsError(error);
  };

  const addDraft = () => {
    const p = draft.trim();
    if (!p) return;
    if (!isAbsolutePath(p)) {
      setRootsError(t("addSource.folder.fullPath"));
      return;
    }
    addRoots([p]);
    setDraft("");
  };

  const onDraftKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      addDraft();
    }
  };

  // What an empty Name saves as: the folders listed plus a typed path not added yet.
  const typed = draft.trim();
  const named = typed && isAbsolutePath(typed) && !roots.includes(typed) ? [...roots, typed] : roots;
  const defaultName = named.length === 1 ? baseName(named[0]!) : named.length > 1 ? t("addSource.folder.defaultMany") : t("addSource.folder.defaultNone");

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    // A typed path that wasn't added with "Add" yet counts too.
    const typed = draft.trim();
    if (typed && !isAbsolutePath(typed)) {
      setRootsError(t("addSource.folder.fullPath"));
      return;
    }
    const all = typed && !roots.includes(typed) ? [...roots, typed] : roots;
    if (all.length === 0) {
      setRootsError(t("addSource.folder.needOne"));
      return;
    }
    if (all !== roots) {
      setRoots(all);
      setDraft("");
    }
    const fallback = all.length === 1 ? baseName(all[0]!) : t("addSource.folder.defaultMany");
    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || fallback, { roots: all, include: splitList(include), exclude: splitList(exclude) });
      onAdded(s);
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate>
      <fieldset className="field" aria-describedby={rootsError ? "roots-err" : undefined}>
        <legend>{t("addSource.folder.legend")}</legend>
        {roots.length > 0 ? (
          <ul className="path-list">
            {roots.map((r) => (
              <li key={r}>
                <code dir="ltr">{r}</code>
                <button type="button" className="btn btn-ghost btn-small" onClick={() => setRoots((rs) => rs.filter((x) => x !== r))} aria-label={t("addSource.folder.removePath", { path: r })}>
                  {t("addSource.folder.remove")}
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted small">{t("addSource.folder.none")}</p>
        )}
        <div className="inline-row">
          <button ref={browseRef} data-autofocus="" type="button" className="btn" onClick={() => void browse()}>
            {t("addSource.folder.choose")}
          </button>
          <label htmlFor="folder-path" className="visually-hidden">
            {t("addSource.folder.pathLabel")}
          </label>
          <input
            id="folder-path"
            type="text"
            dir="ltr"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onDraftKey}
            placeholder={t("addSource.folder.pathPlaceholder")}
            autoComplete="off"
            spellCheck={false}
          />
          <button type="button" className="btn" onClick={addDraft} disabled={!draft.trim()}>
            {t("addSource.folder.add")}
          </button>
        </div>
        {rootsError && (
          <p id="roots-err" className="field-error small" role="alert">
            {rootsError}
          </p>
        )}
      </fieldset>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <details className="advanced" open={Boolean(include || exclude) || undefined}>
        <summary>{t("addSource.advanced")}</summary>
        <Field label={t("addSource.folder.include")} optional hint={<Trans k="addSource.folder.includeHint" tags={CODE} />}>
          {(a) => <textarea {...a} dir="ltr" rows={2} value={include} onChange={(e) => setInclude(e.target.value)} spellCheck={false} />}
        </Field>
        <Field label={t("addSource.folder.exclude")} optional hint={<Trans k="addSource.folder.excludeHint" tags={CODE} />}>
          {(a) => <textarea {...a} dir="ltr" rows={2} value={exclude} onChange={(e) => setExclude(e.target.value)} spellCheck={false} />}
        </Field>
      </details>
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.folder.submit")} blocked={roots.length === 0 && !typed ? t("addSource.folder.needOneHint") : null} />
    </form>
  );
}

// -- obsidian --

function VaultForm({ kind, onBack, onClose, onAdded }: FormProps) {
  const t = useT();
  const [vault, setVault] = useState("");
  const [name, setName] = useState("");
  const [vaultError, setVaultError] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const save = useSaveSource(kind);
  const browseRef = useAutofocus<HTMLButtonElement>();

  const browse = async () => {
    const { paths, error } = await pickFolders(false);
    if (paths[0]) {
      setVault(paths[0]);
      setVaultError(null);
    }
    if (error) setVaultError(error);
  };

  const v = vault.trim();
  const defaultName = v ? baseName(v) : t("addSource.vault.defaultName");

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!v) return setVaultError(t("addSource.vault.choose"));
    if (!isAbsolutePath(v)) return setVaultError(t("addSource.vault.fullPath"));
    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || defaultName, { vault: v });
      onAdded(s);
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate>
      <Field label={t("addSource.vault.label")} hint={t("addSource.vault.hint")} error={vaultError}>
        {(a) => (
          <div className="inline-row">
            <input {...a} type="text" dir="ltr" value={vault} onChange={(e) => setVault(e.target.value)} autoComplete="off" spellCheck={false} />
            <button ref={browseRef} data-autofocus="" type="button" className="btn" onClick={() => void browse()}>
              {t("addSource.vault.browse")}
            </button>
          </div>
        )}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.vault.submit")} />
    </form>
  );
}

// -- Google (Gmail, Calendar, Drive) --

type AuthPhase = "form" | "waiting" | "done";

function GoogleForm({ kind, onBack, onClose, onAdded }: Omit<FormProps, "kind"> & { kind: GoogleKind }) {
  const t = useT();
  const label = sourceKindLabel(kind);
  const apiName = GOOGLE_API[kind];
  const [clientId, setClientId] = useState("");
  const [includeSharedWithMe, setIncludeSharedWithMe] = useState(EMPTY_GOOGLE.includeSharedWithMe);
  const [includeSharedDrives, setIncludeSharedDrives] = useState(EMPTY_GOOGLE.includeSharedDrives);
  const [name, setName] = useState("");
  const [fieldErrors, setFieldErrors] = useState<{ clientId?: string; secret?: string }>({});
  const [secretSaved, setSecretSaved] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [phase, setPhase] = useState<AuthPhase>("form");
  const [source, setSource] = useState<SourceInfo | null>(null);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [authNote, setAuthNote] = useState<string | null>(null);
  // Uncontrolled: the secret never enters React state, and the field is cleared once sent.
  const secretRef = useRef<HTMLInputElement>(null);
  const clientIdRef = useAutofocus<HTMLInputElement>();
  const save = useSaveSource(kind);

  const openAuth = async (url: string) => {
    try {
      await api.openUrl(url);
      setAuthNote(null);
    } catch (e) {
      setAuthNote(t("addSource.google.browserFailed", { error: errorMessage(e) }));
    }
  };

  const connect = async (e: FormEvent) => {
    e.preventDefault();
    const id = clientId.trim();
    const secret = secretRef.current?.value ?? "";
    const errs: typeof fieldErrors = {};
    if (!id) errs.clientId = t("addSource.google.clientIdMissing");
    else if (!/\.apps\.googleusercontent\.com$/.test(id)) errs.clientId = t("addSource.google.clientIdInvalid");
    if (!secret && !secretSaved) errs.secret = t("addSource.google.secretMissing");
    setFieldErrors(errs);
    if (errs.clientId || errs.secret) return;

    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || label, googleSettings(kind, { clientId: id, includeSharedWithMe, includeSharedDrives }));
      setSource(s);
      if (secret) {
        await api.sourcesSecretSet({ id: s.id, secret });
        if (secretRef.current) secretRef.current.value = "";
        setSecretSaved(true);
      }
      const { url } = await api.sourcesAuthBegin({ id: s.id });
      setAuthUrl(url);
      setPhase("waiting");
      await openAuth(url);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  // While waiting: `sources.changed` (plus a slow poll as a safety net) tells us when the
  // daemon has received the redirect.
  const check = async (fromEvent?: unknown) => {
    if (!source) return;
    let latest = sourcesFromChange(fromEvent)?.find((s) => s.id === source.id);
    if (!latest) {
      try {
        latest = (await api.sourcesList()).find((s) => s.id === source.id);
      } catch {
        return;
      }
    }
    if (!latest) return;
    setSource(latest);
    if (latest.state !== "needs_auth" && latest.state !== "error") {
      setPhase("done");
      onAdded(latest);
    }
  };

  const checkRef = useRef(check);
  checkRef.current = check;
  useEvent(events.onSourcesChanged, (p) => {
    if (phase === "waiting") void check(p);
  });
  useEffect(() => {
    if (phase !== "waiting") return;
    const timer = window.setInterval(() => void checkRef.current(), 5000);
    return () => window.clearInterval(timer);
  }, [phase]);

  if (phase === "waiting" && source) {
    return (
      <WaitingForAuth
        label={label}
        source={source}
        url={authUrl}
        note={authNote}
        onReopen={() => authUrl && void openAuth(authUrl)}
        onClose={() => {
          onAdded(source);
          onClose();
        }}
      />
    );
  }
  if (phase === "done" && source) {
    return <Done text={t("addSource.google.connected", { label })} onClose={onClose} />;
  }

  return (
    <form onSubmit={connect} noValidate>
      <details className="help">
        <summary>{t("addSource.google.howTo")}</summary>
        <ol className="small">
          <li>{t("addSource.google.step1")}</li>
          <li>
            <Trans k="addSource.google.step2" args={{ api: apiName }} tags={BOLD} />
            {kind === "gdrive" && (
              <>
                {" "}
                <Trans k="addSource.gdrive.step2Extra" tags={BOLD} />
              </>
            )}
          </li>
          <li>
            <Trans k="addSource.google.step3" tags={BOLD} />
          </li>
          <li>
            <Trans k="addSource.google.step4" tags={BOLD} />
          </li>
          <li>{t("addSource.google.step5")}</li>
        </ol>
        <p className="small muted">{t("addSource.google.privacy")}</p>
        {kind === "gdrive" && (
          <p className="small muted">
            <Trans k="addSource.gdrive.readOnly" args={{ scope: GDRIVE_SCOPE }} tags={BOLD} />
          </p>
        )}
        <button type="button" className="btn btn-small" onClick={() => void api.openUrl(GOOGLE_CREDENTIALS_URL).catch(() => {})}>
          {t("addSource.google.openConsole")}
        </button>
      </details>
      <Field label={t("addSource.google.clientId")} error={fieldErrors.clientId}>
        {(a) => (
          <input
            {...a}
            ref={clientIdRef}
            data-autofocus=""
            type="text"
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            // i18n-ignore: example value, the same in every language
            placeholder="1234-abc.apps.googleusercontent.com"
            autoComplete="off"
            spellCheck={false}
          />
        )}
      </Field>
      <Field
        label={t("addSource.google.clientSecret")}
        error={fieldErrors.secret}
        optional={secretSaved}
        hint={secretSaved ? t("addSource.savedKeepIt") : t("addSource.google.secretHint")}
      >
        {(a) => <input {...a} ref={secretRef} type="password" autoComplete="off" spellCheck={false} />}
      </Field>
      {kind === "gdrive" && (
        <div className="field">
          <Check checked={includeSharedWithMe} onChange={setIncludeSharedWithMe} label={t("addSource.gdrive.sharedWithMe")} hint={t("addSource.gdrive.sharedWithMeHint")} />
          <Check checked={includeSharedDrives} onChange={setIncludeSharedDrives} label={t("addSource.gdrive.sharedDrives")} hint={t("addSource.gdrive.sharedDrivesHint")} />
        </div>
      )}
      <NameField value={name} onChange={setName} placeholder={label} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.google.submit")} />
    </form>
  );
}

function WaitingForAuth({
  label,
  source,
  url,
  note,
  onReopen,
  onClose,
}: {
  label: string;
  source: SourceInfo;
  url: string | null;
  note: string | null;
  onReopen: () => void;
  onClose: () => void;
}) {
  const t = useT();
  const reopenRef = useAutofocus<HTMLButtonElement>();
  return (
    <div className="auth-wait">
      <p role="status">
        <Spinner label={t("addSource.wait.waiting")} /> {t("addSource.wait.text", { label })}
      </p>
      {source.state === "error" && source.last_error && (
        <p className="note note-error" role="alert">
          {source.last_error}
        </p>
      )}
      {note && (
        <p className="note" role="alert">
          {note}
        </p>
      )}
      {url && note && (
        <p className="small">
          <code className="selectable">{url}</code>
        </p>
      )}
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose}>
          {t("addSource.wait.later")}
        </button>
        <button ref={reopenRef} type="button" className="btn btn-primary" onClick={onReopen} disabled={!url}>
          {t("addSource.wait.reopen")}
        </button>
      </div>
    </div>
  );
}

function Done({ text, onClose }: { text: string; onClose: () => void }) {
  const t = useT();
  const ref = useAutofocus<HTMLButtonElement>();
  return (
    <div>
      <p role="status">{text}</p>
      <div className="dialog-actions">
        <button ref={ref} type="button" className="btn btn-primary" onClick={onClose}>
          {t("common.done")}
        </button>
      </div>
    </div>
  );
}

// -- password-based (IMAP, CalDAV) --

/** Shared tail of the IMAP and CalDAV forms: save, send the password, clear it, start a sync. */
async function saveWithPassword(
  save: (name: string, settings: SourceSettings) => Promise<SourceInfo>,
  name: string,
  settings: SourceSettings,
  passwordInput: HTMLInputElement | null,
): Promise<SourceInfo> {
  const s = await save(name, settings);
  const secret = passwordInput?.value ?? "";
  if (secret) {
    await api.sourcesSecretSet({ id: s.id, secret });
    if (passwordInput) passwordInput.value = "";
    // The first pass may have run before the password arrived.
    await api.sourcesSync({ id: s.id }).catch(() => {});
  }
  return s;
}

type TlsMode = "implicit" | "starttls";
const TLS_PORT: Record<TlsMode, number> = { implicit: 993, starttls: 143 };

function ImapForm({ kind, onBack, onClose, onAdded }: FormProps) {
  const t = useT();
  const [host, setHost] = useState("");
  const [port, setPort] = useState("993");
  const [tls, setTls] = useState<TlsMode>("implicit");
  const [username, setUsername] = useState("");
  const [folders, setFolders] = useState("INBOX");
  const [name, setName] = useState("");
  const [errs, setErrs] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [passwordSaved, setPasswordSaved] = useState(false);
  const passwordRef = useRef<HTMLInputElement>(null);
  const hostRef = useAutofocus<HTMLInputElement>();
  const save = useSaveSource(kind);

  const changeTls = (mode: TlsMode) => {
    // Follow the mode's standard port unless the user typed their own.
    if (port === String(TLS_PORT[tls])) setPort(String(TLS_PORT[mode]));
    setTls(mode);
  };

  const defaultName = username.trim() || host.trim() || t("addSource.imap.defaultName");

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const p = Number(port);
    const next: Record<string, string> = {};
    if (!host.trim()) next.host = t("addSource.imap.hostMissing");
    else if (/[\s/:]/.test(host.trim())) next.host = t("addSource.imap.hostInvalid");
    if (!Number.isInteger(p) || p < 1 || p > 65535) next.port = t("addSource.imap.portInvalid");
    if (!username.trim()) next.username = t("addSource.imap.usernameMissing");
    if (!passwordRef.current?.value && !passwordSaved) next.password = t("addSource.passwordMissing");
    if (splitList(folders).length === 0) next.folders = t("addSource.imap.foldersMissing");
    setErrs(next);
    if (Object.keys(next).length > 0) return;

    setBusy(true);
    setError(null);
    try {
      const s = await saveWithPassword(
        save,
        name.trim() || defaultName,
        { host: host.trim(), port: p, tls, username: username.trim(), folders: splitList(folders) },
        passwordRef.current,
      );
      setPasswordSaved(true);
      onAdded(s);
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate>
      <div className="field-grid">
        <Field label={t("addSource.imap.server")} error={errs.host}>
          {(a) => (
            <input
              {...a}
              ref={hostRef}
              data-autofocus=""
              type="text"
              value={host}
              onChange={(e) => setHost(e.target.value)}
              // i18n-ignore: example value, the same in every language
              placeholder="imap.example.com"
              autoComplete="off"
              spellCheck={false}
            />
          )}
        </Field>
        <Field label={t("addSource.imap.port")} error={errs.port}>
          {(a) => <input {...a} type="number" inputMode="numeric" min={1} max={65535} value={port} onChange={(e) => setPort(e.target.value)} />}
        </Field>
        <Field label={t("addSource.imap.security")}>
          {(a) => (
            <select {...a} value={tls} onChange={(e) => changeTls(e.target.value as TlsMode)}>
              <option value="implicit">{t("addSource.imap.tlsImplicit", { port: String(TLS_PORT.implicit) })}</option>
              <option value="starttls">{t("addSource.imap.tlsStarttls", { port: String(TLS_PORT.starttls) })}</option>
            </select>
          )}
        </Field>
      </div>
      <Field label={t("addSource.username")} error={errs.username}>
        {(a) => (
          <input {...a} type="text" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" spellCheck={false} />
        )}
      </Field>
      <Field
        label={t("addSource.password")}
        error={errs.password}
        optional={passwordSaved}
        hint={passwordSaved ? t("addSource.savedKeepIt") : t("addSource.passwordHint")}
      >
        {(a) => <input {...a} ref={passwordRef} type="password" autoComplete="current-password" />}
      </Field>
      <Field label={t("addSource.imap.folders")} error={errs.folders} hint={t("addSource.imap.foldersHint")}>
        {(a) => <input {...a} type="text" value={folders} onChange={(e) => setFolders(e.target.value)} autoComplete="off" spellCheck={false} />}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.imap.submit")} />
    </form>
  );
}

/** HTTPS, or plain HTTP to this machine only. */
export function isCaldavUrl(raw: string): boolean {
  try {
    const u = new URL(raw);
    if (u.protocol === "https:") return u.hostname.length > 0;
    return u.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(u.hostname);
  } catch {
    return false;
  }
}

function CaldavForm({ kind, onBack, onClose, onAdded }: FormProps) {
  const t = useT();
  const [url, setUrl] = useState("");
  const [username, setUsername] = useState("");
  const [name, setName] = useState("");
  const [errs, setErrs] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [passwordSaved, setPasswordSaved] = useState(false);
  const passwordRef = useRef<HTMLInputElement>(null);
  const urlRef = useAutofocus<HTMLInputElement>();
  const save = useSaveSource(kind);

  let defaultName = t("addSource.caldav.defaultName");
  try {
    defaultName = new URL(url.trim()).hostname || defaultName;
  } catch {
    /* keep the default */
  }

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const next: Record<string, string> = {};
    if (!isCaldavUrl(url.trim())) next.url = t("addSource.caldav.urlInvalid");
    if (!username.trim()) next.username = t("addSource.caldav.usernameMissing");
    if (!passwordRef.current?.value && !passwordSaved) next.password = t("addSource.passwordMissing");
    setErrs(next);
    if (Object.keys(next).length > 0) return;

    setBusy(true);
    setError(null);
    try {
      const s = await saveWithPassword(save, name.trim() || defaultName, { url: url.trim(), username: username.trim() }, passwordRef.current);
      setPasswordSaved(true);
      onAdded(s);
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} noValidate>
      <Field label={t("addSource.caldav.url")} error={errs.url} hint={t("addSource.caldav.urlHint")}>
        {(a) => (
          <input
            {...a}
            ref={urlRef}
            data-autofocus=""
            type="url"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            // i18n-ignore: example value, the same in every language
            placeholder="https://"
            autoComplete="url"
            spellCheck={false}
          />
        )}
      </Field>
      <Field label={t("addSource.username")} error={errs.username}>
        {(a) => (
          <input {...a} type="text" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" spellCheck={false} />
        )}
      </Field>
      <Field
        label={t("addSource.password")}
        error={errs.password}
        optional={passwordSaved}
        hint={passwordSaved ? t("addSource.savedKeepIt") : t("addSource.passwordHint")}
      >
        {(a) => <input {...a} ref={passwordRef} type="password" autoComplete="current-password" />}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.caldav.submit")} />
    </form>
  );
}
