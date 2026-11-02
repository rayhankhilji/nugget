import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { api, errorMessage, type ConnectorKind, type SourceInfo, type SourceKind, type SourceSettings } from "../api";
import { t as tGlobal, Trans, useT, type MessageKey, type TagRenderer } from "../i18n";
import { baseName, sourceKindLabel, splitList } from "../format";
import { KindPicker } from "../sources/KindPicker";
import { NEW_KIND_TITLE, NewKindForm, isNewKind } from "../sources/forms";
import { Actions, Check, Done, Field, FormError, NameField, OwnClient, useAutofocus, useSaveSource, type FormProps as BaseFormProps } from "../sources/form";
import { ConnectWait, useAuthWait, useOAuthBuiltin } from "../sources/auth";
import { EMPTY_GOOGLE, GDRIVE_SCOPE, GOOGLE_API, googleSettings, hasBuiltin, OAUTH_PROVIDER_NAME, oauthProvider, type GoogleKind } from "../sources/kinds";
import { Dialog } from "./Dialog";
import { Spinner } from "./ui";

type FormProps = BaseFormProps<SourceKind>;

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
  const builtin = useOAuthBuiltin();

  const title = step === "kind" ? t("addSource.title") : t(isNewKind(kind) ? NEW_KIND_TITLE[kind] : ADD_TITLE[kind]);

  return (
    <Dialog title={title} onClose={onClose} wide>
      {step === "kind" ? (
        <KindPicker value={kind} onChange={setKind} onNext={() => setStep("form")} onCancel={onClose} blurb={KIND_BLURB} />
      ) : builtin === null && oauthProvider(kind) ? (
        // Which sign-in this build offers decides the form; it is known almost at once.
        <Spinner label={t("common.loading")} />
      ) : isNewKind(kind) ? (
        <NewKindForm kind={kind} onBack={() => setStep("kind")} onClose={onClose} onAdded={onAdded} builtin={builtin ?? []} />
      ) : (
        <KindForm kind={kind} onBack={() => setStep("kind")} onClose={onClose} onAdded={onAdded} builtin={builtin ?? []} />
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

// ---- step 2: settings ----

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

// `~` and `~/…` are the home folder (the daemon expands them); `~name` is not supported.
const isAbsolutePath = (p: string) =>
  p.startsWith("/") || p === "~" || p.startsWith("~/") || p.startsWith("~\\") || /^[A-Za-z]:[\\/]/.test(p) || p.startsWith("\\\\");

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
  // A removed folder takes its Remove button with it: focus the next folder's Remove (or
  // the one before it), else Choose folder.
  const listRef = useRef<HTMLUListElement>(null);
  const focusAfterRemove = useRef<number | null>(null);
  useEffect(() => {
    const i = focusAfterRemove.current;
    if (i === null) return;
    focusAfterRemove.current = null;
    const buttons = Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>("button") ?? []);
    (buttons[Math.min(i, buttons.length - 1)] ?? browseRef.current)?.focus();
  }, [roots, browseRef]);
  const removeRoot = (i: number) => {
    focusAfterRemove.current = i;
    setRoots((rs) => rs.filter((_, j) => j !== i));
  };

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
          <ul className="path-list" ref={listRef}>
            {roots.map((r, i) => (
              <li key={r}>
                <code dir="ltr">{r}</code>
                <button type="button" className="btn btn-ghost btn-small" onClick={() => removeRoot(i)} aria-label={t("addSource.folder.removePath", { path: r })}>
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

function GoogleForm({ kind, onBack, onClose, onAdded, builtin: providers }: Omit<FormProps, "kind"> & { kind: GoogleKind }) {
  const t = useT();
  const label = sourceKindLabel(kind);
  const apiName = GOOGLE_API[kind];
  // With Nugget's own client the user's client is optional (and wins when filled in).
  const builtin = hasBuiltin(kind, providers);
  const [clientId, setClientId] = useState("");
  const [includeSharedWithMe, setIncludeSharedWithMe] = useState(EMPTY_GOOGLE.includeSharedWithMe);
  const [includeSharedDrives, setIncludeSharedDrives] = useState(EMPTY_GOOGLE.includeSharedDrives);
  const [name, setName] = useState("");
  const [fieldErrors, setFieldErrors] = useState<{ clientId?: string; secret?: string }>({});
  const [secretSaved, setSecretSaved] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const auth = useAuthWait(onAdded);
  // Uncontrolled: the secret never enters React state, and the field is cleared once sent.
  const secretRef = useRef<HTMLInputElement>(null);
  const clientIdRef = useAutofocus<HTMLInputElement>();
  const save = useSaveSource(kind);

  const connect = async (e: FormEvent) => {
    e.preventDefault();
    const id = clientId.trim();
    const secret = secretRef.current?.value ?? "";
    const errs: typeof fieldErrors = {};
    if (!id) {
      if (!builtin) errs.clientId = t("addSource.google.clientIdMissing");
    } else if (!/\.apps\.googleusercontent\.com$/.test(id)) errs.clientId = t("addSource.google.clientIdInvalid");
    if (id && !secret && !secretSaved) errs.secret = t("addSource.google.secretMissing");
    setFieldErrors(errs);
    if (errs.clientId || errs.secret) return;

    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || label, googleSettings(kind, { clientId: id, includeSharedWithMe, includeSharedDrives }));
      if (secret) {
        await api.sourcesSecretSet({ id: s.id, secret });
        if (secretRef.current) secretRef.current.value = "";
        setSecretSaved(true);
      }
      await auth.begin(s);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const driveOptions = (
    <div className="field">
      <Check checked={includeSharedWithMe} onChange={setIncludeSharedWithMe} label={t("addSource.gdrive.sharedWithMe")} hint={t("addSource.gdrive.sharedWithMeHint")} />
      <Check checked={includeSharedDrives} onChange={setIncludeSharedDrives} label={t("addSource.gdrive.sharedDrives")} hint={t("addSource.gdrive.sharedDrivesHint")} />
    </div>
  );

  const ownClient = (
    <OwnClient builtin={builtin} provider={OAUTH_PROVIDER_NAME.google}>
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
      <Field label={t("addSource.google.clientId")} error={fieldErrors.clientId} optional={builtin}>
        {(a) => (
          <input
            {...a}
            ref={builtin ? undefined : clientIdRef}
            data-autofocus={builtin ? undefined : ""}
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
        optional={builtin || secretSaved}
        hint={secretSaved ? t("addSource.savedKeepIt") : t("addSource.google.secretHint")}
      >
        {(a) => <input {...a} ref={secretRef} type="password" autoComplete="off" spellCheck={false} />}
      </Field>
    </OwnClient>
  );

  const { phase, source } = auth;
  if (phase === "waiting" && source) {
    return (
      <ConnectWait
        label={label}
        text={t("addSource.wait.text", { label })}
        source={source}
        flow={auth.flow}
        note={auth.note}
        onOpen={(url) => void auth.open(url)}
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
      {builtin && <p className="muted small">{t("addSource.oauth.builtinHint", { provider: OAUTH_PROVIDER_NAME.google })}</p>}
      {!builtin && ownClient}
      {kind === "gdrive" && driveOptions}
      <NameField value={name} onChange={setName} placeholder={label} />
      {builtin && ownClient}
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={builtin ? t("addSource.oauth.connect", { provider: OAUTH_PROVIDER_NAME.google }) : t("addSource.google.submit")} />
    </form>
  );
}

// -- password-based (IMAP, CalDAV) --

/**
 * Shared state of the IMAP and CalDAV forms. `submit` validates (`check` adds field errors),
 * saves, sends the password (then clears the field) and starts a sync.
 */
function usePasswordForm({ kind, onClose, onAdded }: FormProps) {
  const t = useT();
  const [username, setUsername] = useState("");
  const [name, setName] = useState("");
  const [errs, setErrs] = useState<Record<string, string>>({});
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [passwordSaved, setPasswordSaved] = useState(false);
  const passwordRef = useRef<HTMLInputElement>(null);
  const save = useSaveSource(kind);

  const submit = async (e: FormEvent, check: (next: Record<string, string>) => void, defaultName: string, settings: () => SourceSettings) => {
    e.preventDefault();
    const next: Record<string, string> = {};
    check(next);
    if (!passwordRef.current?.value && !passwordSaved) next.password = t("addSource.passwordMissing");
    setErrs(next);
    if (Object.keys(next).length > 0) return;

    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || defaultName, settings());
      const input = passwordRef.current;
      const secret = input?.value ?? "";
      if (secret) {
        await api.sourcesSecretSet({ id: s.id, secret });
        if (input) input.value = "";
        // The first pass may have run before the password arrived.
        await api.sourcesSync({ id: s.id }).catch(() => {});
      }
      setPasswordSaved(true);
      onAdded(s);
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const credentials = (
    <>
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
    </>
  );

  return { username, name, setName, errs, error, busy, submit, credentials };
}

type TlsMode = "implicit" | "starttls";
const TLS_PORT: Record<TlsMode, number> = { implicit: 993, starttls: 143 };

function ImapForm(props: FormProps) {
  const t = useT();
  const [host, setHost] = useState("");
  const [port, setPort] = useState("993");
  const [tls, setTls] = useState<TlsMode>("implicit");
  const [folders, setFolders] = useState("INBOX");
  const f = usePasswordForm(props);
  const hostRef = useAutofocus<HTMLInputElement>();

  const changeTls = (mode: TlsMode) => {
    // Follow the mode's standard port unless the user typed their own.
    if (port === String(TLS_PORT[tls])) setPort(String(TLS_PORT[mode]));
    setTls(mode);
  };

  const defaultName = f.username.trim() || host.trim() || t("addSource.imap.defaultName");

  const submit = (e: FormEvent) =>
    f.submit(
      e,
      (next) => {
        if (!host.trim()) next.host = t("addSource.imap.hostMissing");
        else if (/[\s/:]/.test(host.trim())) next.host = t("addSource.imap.hostInvalid");
        const p = Number(port);
        if (!Number.isInteger(p) || p < 1 || p > 65535) next.port = t("addSource.imap.portInvalid");
        if (!f.username.trim()) next.username = t("addSource.imap.usernameMissing");
        if (splitList(folders).length === 0) next.folders = t("addSource.imap.foldersMissing");
      },
      defaultName,
      () => ({ host: host.trim(), port: Number(port), tls, username: f.username.trim(), folders: splitList(folders) }),
    );

  return (
    <form onSubmit={submit} noValidate>
      <div className="field-grid">
        <Field label={t("addSource.imap.server")} error={f.errs.host}>
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
        <Field label={t("addSource.imap.port")} error={f.errs.port}>
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
      {f.credentials}
      <Field label={t("addSource.imap.folders")} error={f.errs.folders} hint={t("addSource.imap.foldersHint")}>
        {(a) => <input {...a} type="text" value={folders} onChange={(e) => setFolders(e.target.value)} autoComplete="off" spellCheck={false} />}
      </Field>
      <NameField value={f.name} onChange={f.setName} placeholder={defaultName} />
      <FormError error={f.error} />
      <Actions onBack={props.onBack} busy={f.busy} submitLabel={t("addSource.imap.submit")} />
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

function CaldavForm(props: FormProps) {
  const t = useT();
  const [url, setUrl] = useState("");
  const f = usePasswordForm(props);
  const urlRef = useAutofocus<HTMLInputElement>();

  let defaultName = t("addSource.caldav.defaultName");
  try {
    defaultName = new URL(url.trim()).hostname || defaultName;
  } catch {
    /* keep the default */
  }

  const submit = (e: FormEvent) =>
    f.submit(
      e,
      (next) => {
        if (!isCaldavUrl(url.trim())) next.url = t("addSource.caldav.urlInvalid");
        if (!f.username.trim()) next.username = t("addSource.caldav.usernameMissing");
      },
      defaultName,
      () => ({ url: url.trim(), username: f.username.trim() }),
    );

  return (
    <form onSubmit={submit} noValidate>
      <Field label={t("addSource.caldav.url")} error={f.errs.url} hint={t("addSource.caldav.urlHint")}>
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
      {f.credentials}
      <NameField value={f.name} onChange={f.setName} placeholder={defaultName} />
      <FormError error={f.error} />
      <Actions onBack={props.onBack} busy={f.busy} submitLabel={t("addSource.caldav.submit")} />
    </form>
  );
}
