import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import {
  api,
  ApiError,
  errorMessage,
  events,
  SOURCE_KINDS,
  sourcesFromChange,
  type SourceInfo,
  type SourceKind,
  type SourceSettings,
} from "../api";
import { useEvent } from "../hooks";
import { t } from "../i18n";
import { baseName, sourceKindLabel, splitList } from "../format";
import { Dialog } from "./Dialog";
import { SourceIcon } from "./SourceIcon";
import { Spinner } from "./ui";

const KIND_BLURB: Record<SourceKind, string> = {
  files: "Documents, notes and code in folders on this device.",
  obsidian: "Notes, links and tags from an Obsidian vault.",
  gmail: "Your Gmail mail, through your own Google OAuth client.",
  gcal: "Events from Google Calendar, through your own Google OAuth client.",
  imap: "Any IMAP mailbox (Fastmail, iCloud, Outlook, self-hosted…).",
  caldav: "Any CalDAV calendar (iCloud, Fastmail, Nextcloud…).",
};

const GOOGLE_CREDENTIALS_URL = "https://console.cloud.google.com/apis/credentials";

/** Wizard for `sources.add`: pick a kind, fill in its settings, then connect. */
export function AddSourceWizard({ onClose, onAdded }: { onClose: () => void; onAdded: (s: SourceInfo) => void }) {
  const [kind, setKind] = useState<SourceKind>("files");
  const [step, setStep] = useState<"kind" | "form">("kind");

  const title = step === "kind" ? "Add a source" : ADD_TITLE[kind];

  return (
    <Dialog title={title} onClose={onClose} wide>
      {step === "kind" ? (
        <KindPicker value={kind} onChange={setKind} onNext={() => setStep("form")} onCancel={onClose} />
      ) : (
        <KindForm kind={kind} onBack={() => setStep("kind")} onClose={onClose} onAdded={onAdded} />
      )}
    </Dialog>
  );
}

const ADD_TITLE: Record<SourceKind, string> = {
  files: "Add a folder",
  obsidian: "Add an Obsidian vault",
  gmail: "Connect Gmail",
  gcal: "Connect Google Calendar",
  imap: "Add an IMAP mailbox",
  caldav: "Add a CalDAV calendar",
};

function useAutofocus<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  useEffect(() => {
    ref.current?.focus();
  }, []);
  return ref;
}

// ---- step 1: kind ----

function KindPicker({
  value,
  onChange,
  onNext,
  onCancel,
}: {
  value: SourceKind;
  onChange: (k: SourceKind) => void;
  onNext: () => void;
  onCancel: () => void;
}) {
  const first = useAutofocus<HTMLInputElement>();
  const submit = (e: FormEvent) => {
    e.preventDefault();
    onNext();
  };
  return (
    <form onSubmit={submit}>
      <fieldset className="kind-picker">
        <legend className="visually-hidden">What do you want to add?</legend>
        {SOURCE_KINDS.map((k) => (
          <label key={k} className="kind-option">
            <input
              ref={k === value ? first : undefined}
              data-autofocus={k === value ? "" : undefined}
              type="radio"
              name="source-kind"
              value={k}
              checked={k === value}
              onChange={() => onChange(k)}
            />
            <SourceIcon kind={k} size={22} />
            <span>
              <span className="kind-label">{sourceKindLabel(k)}</span>
              <span className="muted small">{KIND_BLURB[k]}</span>
            </span>
          </label>
        ))}
      </fieldset>
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onCancel}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary">
          Next
        </button>
      </div>
    </form>
  );
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
      return <GoogleForm {...props} />;
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
  const id = useId();
  const hintId = `${id}-hint`;
  const errId = `${id}-err`;
  const describedBy = [hint ? hintId : "", error ? errId : ""].filter(Boolean).join(" ") || undefined;
  return (
    <div className="field">
      <label htmlFor={id}>
        {label}
        {optional && <span className="muted small"> (optional)</span>}
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
  if (!error) return null;
  const e = ApiError.from(error);
  return (
    <p className="note note-error" role="alert">
      {e.isMethodNotFound ? t("common.notAvailableSentence") : e.message}
    </p>
  );
}

function Actions({ onBack, busy, submitLabel }: { onBack: () => void; busy: boolean; submitLabel: string }) {
  return (
    <div className="dialog-actions">
      <button type="button" className="btn" onClick={onBack} disabled={busy}>
        Back
      </button>
      <button type="submit" className="btn btn-primary" disabled={busy}>
        {busy ? (
          <>
            <Spinner label="Working" /> {submitLabel}
          </>
        ) : (
          submitLabel
        )}
      </button>
    </div>
  );
}

function NameField({ value, onChange, placeholder }: { value: string; onChange: (v: string) => void; placeholder: string }) {
  return (
    <Field label="Name" optional hint="How this source appears in Nugget.">
      {(a) => (
        <input {...a} type="text" value={value} onChange={(e) => onChange(e.target.value)} placeholder={placeholder} autoComplete="off" />
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
    return { paths: [], error: `Couldn't open the folder picker (${errorMessage(e)}). Type the path instead.` };
  }
}

// -- folder --

function FolderForm({ kind, onBack, onClose, onAdded }: FormProps) {
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
      setRootsError("Use a full path, like /Users/me/Documents or C:\\Users\\me\\Documents.");
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

  const defaultName = roots.length === 1 ? baseName(roots[0]!) : roots.length > 1 ? "Folders" : "Documents";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (roots.length === 0) {
      setRootsError("Add at least one folder.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || defaultName, { roots, include: splitList(include), exclude: splitList(exclude) });
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
        <legend>Folders</legend>
        {roots.length > 0 ? (
          <ul className="path-list">
            {roots.map((r) => (
              <li key={r}>
                <code>{r}</code>
                <button type="button" className="btn btn-ghost btn-small" onClick={() => setRoots((rs) => rs.filter((x) => x !== r))} aria-label={`Remove ${r}`}>
                  Remove
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="muted small">No folders yet.</p>
        )}
        <div className="inline-row">
          <button ref={browseRef} data-autofocus="" type="button" className="btn" onClick={() => void browse()}>
            Choose folders…
          </button>
          <label htmlFor="folder-path" className="visually-hidden">
            Folder path
          </label>
          <input
            id="folder-path"
            type="text"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={onDraftKey}
            placeholder="or type a path"
            autoComplete="off"
            spellCheck={false}
          />
          <button type="button" className="btn" onClick={addDraft} disabled={!draft.trim()}>
            Add
          </button>
        </div>
        {rootsError && (
          <p id="roots-err" className="field-error small" role="alert">
            {rootsError}
          </p>
        )}
      </fieldset>
      <Field label="Include" optional hint="Glob patterns, comma or newline separated, e.g. **/*.md, **/*.pdf. Empty means every supported file.">
        {(a) => <textarea {...a} rows={2} value={include} onChange={(e) => setInclude(e.target.value)} spellCheck={false} />}
      </Field>
      <Field label="Exclude" optional hint="Glob patterns to skip, e.g. **/node_modules/**, **/*.log. .gitignore and .nuggetignore files are honored too.">
        {(a) => <textarea {...a} rows={2} value={exclude} onChange={(e) => setExclude(e.target.value)} spellCheck={false} />}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel="Add folder" />
    </form>
  );
}

// -- obsidian --

function VaultForm({ kind, onBack, onClose, onAdded }: FormProps) {
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
  const defaultName = v ? baseName(v) : "Obsidian";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!v) return setVaultError("Choose the vault folder (the one that contains .obsidian).");
    if (!isAbsolutePath(v)) return setVaultError("Use a full path to the vault folder.");
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
      <Field label="Vault folder" hint="The folder that contains your notes and the .obsidian settings folder." error={vaultError}>
        {(a) => (
          <div className="inline-row">
            <input {...a} type="text" value={vault} onChange={(e) => setVault(e.target.value)} autoComplete="off" spellCheck={false} />
            <button ref={browseRef} data-autofocus="" type="button" className="btn" onClick={() => void browse()}>
              Choose…
            </button>
          </div>
        )}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel="Add vault" />
    </form>
  );
}

// -- Google (Gmail, Calendar) --

type AuthPhase = "form" | "waiting" | "done";

function GoogleForm({ kind, onBack, onClose, onAdded }: FormProps) {
  const label = sourceKindLabel(kind);
  const apiName = kind === "gmail" ? "Gmail API" : "Google Calendar API";
  const [clientId, setClientId] = useState("");
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
      setAuthNote(`Couldn't open your browser (${errorMessage(e)}). Copy the link below into it instead.`);
    }
  };

  const connect = async (e: FormEvent) => {
    e.preventDefault();
    const id = clientId.trim();
    const secret = secretRef.current?.value ?? "";
    const errs: typeof fieldErrors = {};
    if (!id) errs.clientId = "Paste the client ID from the Google Cloud console.";
    else if (!/\.apps\.googleusercontent\.com$/.test(id)) errs.clientId = "Google client IDs end in .apps.googleusercontent.com.";
    if (!secret && !secretSaved) errs.secret = "Paste the client secret too.";
    setFieldErrors(errs);
    if (errs.clientId || errs.secret) return;

    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || label, { client_id: id });
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
    const t = window.setInterval(() => void checkRef.current(), 5000);
    return () => window.clearInterval(t);
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
    return <Done text={`${label} is connected. Nugget is syncing it now.`} onClose={onClose} />;
  }

  return (
    <form onSubmit={connect} noValidate>
      <details className="help">
        <summary>How to create a Google OAuth client</summary>
        <ol className="small">
          <li>Open the Google Cloud console and create a project, or pick an existing one.</li>
          <li>
            Under <b>APIs &amp; Services → Library</b>, enable the <b>{apiName}</b>.
          </li>
          <li>
            Under <b>OAuth consent screen</b>, choose <b>External</b>, fill in an app name and your email, and add your
            own Google account as a <b>test user</b>.
          </li>
          <li>
            Under <b>Credentials → Create credentials → OAuth client ID</b>, choose <b>Desktop app</b>.
          </li>
          <li>Copy the client ID and client secret into the fields below.</li>
        </ol>
        <p className="small muted">
          Nugget signs in through your browser and a redirect to this device only. Tokens and the client secret are kept
          in your OS keychain, never in config files.
        </p>
        <button type="button" className="btn btn-small" onClick={() => void api.openUrl(GOOGLE_CREDENTIALS_URL).catch(() => {})}>
          Open Google Cloud credentials
        </button>
      </details>
      <Field label="Client ID" error={fieldErrors.clientId}>
        {(a) => (
          <input
            {...a}
            ref={clientIdRef}
            data-autofocus=""
            type="text"
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            placeholder="1234-abc.apps.googleusercontent.com"
            autoComplete="off"
            spellCheck={false}
          />
        )}
      </Field>
      <Field
        label="Client secret"
        error={fieldErrors.secret}
        optional={secretSaved}
        hint={secretSaved ? "Saved to your keychain. Leave empty to keep it." : "Stored in your OS keychain."}
      >
        {(a) => <input {...a} ref={secretRef} type="password" autoComplete="off" spellCheck={false} />}
      </Field>
      <NameField value={name} onChange={setName} placeholder={label} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel="Connect" />
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
  const reopenRef = useAutofocus<HTMLButtonElement>();
  return (
    <div className="auth-wait">
      <p role="status">
        <Spinner label="Waiting" /> Finish signing in to {label} in your browser. This window updates as soon as Google
        sends you back.
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
          Finish later
        </button>
        <button ref={reopenRef} type="button" className="btn btn-primary" onClick={onReopen} disabled={!url}>
          Open sign-in page again
        </button>
      </div>
    </div>
  );
}

function Done({ text, onClose }: { text: string; onClose: () => void }) {
  const ref = useAutofocus<HTMLButtonElement>();
  return (
    <div>
      <p role="status">{text}</p>
      <div className="dialog-actions">
        <button ref={ref} type="button" className="btn btn-primary" onClick={onClose}>
          Done
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

  const defaultName = username.trim() || host.trim() || "Mail";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const p = Number(port);
    const next: Record<string, string> = {};
    if (!host.trim()) next.host = "Enter the IMAP server, e.g. imap.fastmail.com.";
    else if (/[\s/:]/.test(host.trim())) next.host = "Enter just the host name, without imap:// or a port.";
    if (!Number.isInteger(p) || p < 1 || p > 65535) next.port = "Enter a port between 1 and 65535.";
    if (!username.trim()) next.username = "Enter your username (usually your email address).";
    if (!passwordRef.current?.value && !passwordSaved) next.password = "Enter the password or app password.";
    if (splitList(folders).length === 0) next.folders = "Name at least one folder, e.g. INBOX.";
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
        <Field label="IMAP server" error={errs.host}>
          {(a) => (
            <input
              {...a}
              ref={hostRef}
              data-autofocus=""
              type="text"
              value={host}
              onChange={(e) => setHost(e.target.value)}
              placeholder="imap.example.com"
              autoComplete="off"
              spellCheck={false}
            />
          )}
        </Field>
        <Field label="Port" error={errs.port}>
          {(a) => <input {...a} type="number" inputMode="numeric" min={1} max={65535} value={port} onChange={(e) => setPort(e.target.value)} />}
        </Field>
        <Field label="Security">
          {(a) => (
            <select {...a} value={tls} onChange={(e) => changeTls(e.target.value as TlsMode)}>
              <option value="implicit">TLS (port 993)</option>
              <option value="starttls">STARTTLS (port 143)</option>
            </select>
          )}
        </Field>
      </div>
      <Field label="Username" error={errs.username}>
        {(a) => (
          <input {...a} type="text" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" spellCheck={false} />
        )}
      </Field>
      <Field
        label="Password"
        error={errs.password}
        optional={passwordSaved}
        hint={passwordSaved ? "Saved to your keychain. Leave empty to keep it." : "Use an app password if your provider offers one. Stored in your OS keychain."}
      >
        {(a) => <input {...a} ref={passwordRef} type="password" autoComplete="current-password" />}
      </Field>
      <Field label="Folders" error={errs.folders} hint="Comma separated, e.g. INBOX, Archive.">
        {(a) => <input {...a} type="text" value={folders} onChange={(e) => setFolders(e.target.value)} autoComplete="off" spellCheck={false} />}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel="Add mailbox" />
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

  let defaultName = "Calendar";
  try {
    defaultName = new URL(url.trim()).hostname || defaultName;
  } catch {
    /* keep the default */
  }

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const next: Record<string, string> = {};
    if (!isCaldavUrl(url.trim())) next.url = "Enter the calendar server's https:// URL.";
    if (!username.trim()) next.username = "Enter your username.";
    if (!passwordRef.current?.value && !passwordSaved) next.password = "Enter the password or app password.";
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
      <Field label="Server URL" error={errs.url} hint="For example https://caldav.fastmail.com/ or https://cloud.example.com/remote.php/dav/.">
        {(a) => (
          <input
            {...a}
            ref={urlRef}
            data-autofocus=""
            type="url"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://"
            autoComplete="url"
            spellCheck={false}
          />
        )}
      </Field>
      <Field label="Username" error={errs.username}>
        {(a) => (
          <input {...a} type="text" value={username} onChange={(e) => setUsername(e.target.value)} autoComplete="username" spellCheck={false} />
        )}
      </Field>
      <Field
        label="Password"
        error={errs.password}
        optional={passwordSaved}
        hint={passwordSaved ? "Saved to your keychain. Leave empty to keep it." : "Use an app password if your provider offers one. Stored in your OS keychain."}
      >
        {(a) => <input {...a} ref={passwordRef} type="password" autoComplete="current-password" />}
      </Field>
      <NameField value={name} onChange={setName} placeholder={defaultName} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel="Add calendar" />
    </form>
  );
}
