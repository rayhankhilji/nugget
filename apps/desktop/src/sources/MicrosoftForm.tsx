/** Outlook mail, Outlook calendar and OneDrive: the user's own Entra app registration, then browser sign-in. */
import { useRef, useState, type FormEvent } from "react";
import { api, type MicrosoftKind } from "../api";
import { sourceKindLabel } from "../format";
import { Trans, useT } from "../i18n";
import { ConnectWait, useAuthWait } from "./auth";
import { Actions, Done, Field, FormError, HELP_TAGS, NameField, useAutofocus, useSaveSource, type FormProps } from "./form";
import { EMPTY_MICROSOFT, MICROSOFT_SCOPE, microsoftErrors, microsoftSettings, type MicrosoftErrors, type MicrosoftForm as Form } from "./kinds";

const ENTRA_APPS_URL = "https://entra.microsoft.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";

/** The setup steps for an Entra app registration, shared by the three Microsoft kinds. */
export function MicrosoftGuide({ kind }: { kind: MicrosoftKind }) {
  const t = useT();
  return (
    <details className="help">
      <summary>{t("addSource.microsoft.howTo")}</summary>
      <ol className="small">
        <li>
          <Trans k="addSource.microsoft.step1" tags={HELP_TAGS} />
        </li>
        <li>
          <Trans k="addSource.microsoft.step2" tags={HELP_TAGS} />
        </li>
        <li>
          <Trans k="addSource.microsoft.step3" tags={HELP_TAGS} />
        </li>
        <li>
          <Trans k="addSource.microsoft.step4" args={{ scope: MICROSOFT_SCOPE[kind] }} tags={HELP_TAGS} />
        </li>
        <li>
          <Trans k="addSource.microsoft.step5" tags={HELP_TAGS} />
        </li>
      </ol>
      <p className="small muted">{t("addSource.microsoft.oneApp")}</p>
      <p className="small muted">{t("addSource.microsoft.privacy")}</p>
      <button type="button" className="btn btn-small" onClick={() => void api.openUrl(ENTRA_APPS_URL).catch(() => {})}>
        {t("addSource.microsoft.openPortal")}
      </button>
    </details>
  );
}

export function MicrosoftForm({ kind, onBack, onClose, onAdded }: FormProps<MicrosoftKind>) {
  const t = useT();
  const label = sourceKindLabel(kind);
  const [form, setForm] = useState<Form>(EMPTY_MICROSOFT);
  const [name, setName] = useState("");
  const [errs, setErrs] = useState<MicrosoftErrors>({});
  const [secretSaved, setSecretSaved] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  // Uncontrolled: the optional client secret never enters React state and is cleared once sent.
  const secretRef = useRef<HTMLInputElement>(null);
  const clientIdRef = useAutofocus<HTMLInputElement>();
  const save = useSaveSource(kind);
  const auth = useAuthWait(onAdded);

  const set = <K extends keyof Form>(key: K) => (v: Form[K]) => setForm((f) => ({ ...f, [key]: v }));

  const connect = async (e: FormEvent) => {
    e.preventDefault();
    const next = microsoftErrors(kind, form);
    setErrs(next);
    if (Object.keys(next).length > 0) return;
    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || label, microsoftSettings(kind, form));
      const secret = secretRef.current?.value ?? "";
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

  if (auth.phase === "waiting" && auth.source) {
    const source = auth.source;
    return (
      <ConnectWait
        label={label}
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
  if (auth.phase === "done") return <Done text={t("addSource.google.connected", { label })} onClose={onClose} />;

  const err = (k: keyof MicrosoftErrors) => (errs[k] ? t(errs[k]) : null);

  return (
    <form onSubmit={connect} noValidate>
      <MicrosoftGuide kind={kind} />
      <Field label={t("addSource.microsoft.clientId")} error={err("clientId")} hint={t("addSource.microsoft.clientIdHint")}>
        {(a) => (
          <input
            {...a}
            ref={clientIdRef}
            data-autofocus=""
            type="text"
            value={form.clientId}
            onChange={(e) => set("clientId")(e.target.value)}
            // i18n-ignore: example value, the same in every language
            placeholder="00000000-0000-0000-0000-000000000000"
            autoComplete="off"
            spellCheck={false}
          />
        )}
      </Field>
      <Field label={t("addSource.microsoft.tenant")} optional error={err("tenant")} hint={t("addSource.microsoft.tenantHint")}>
        {(a) => (
          <input
            {...a}
            type="text"
            value={form.tenant}
            onChange={(e) => set("tenant")(e.target.value)}
            // i18n-ignore: the daemon's default tenant id, not prose
            placeholder="common"
            autoComplete="off"
            spellCheck={false}
          />
        )}
      </Field>
      <Field label={t("addSource.microsoft.account")} optional hint={t("addSource.microsoft.accountHint")}>
        {(a) => (
          <input {...a} type="email" value={form.accountHint} onChange={(e) => set("accountHint")(e.target.value)} autoComplete="email" spellCheck={false} />
        )}
      </Field>
      {kind === "outlook" && (
        <Field label={t("addSource.microsoft.folders")} optional error={err("folders")} hint={t("addSource.microsoft.foldersHint")}>
          {(a) => (
            <input {...a} type="text" value={form.folders} onChange={(e) => set("folders")(e.target.value)} autoComplete="off" spellCheck={false} />
          )}
        </Field>
      )}
      {kind === "onedrive" && (
        <>
          <Field label={t("addSource.microsoft.include")} optional error={err("includeFolders")} hint={t("addSource.microsoft.includeHint")}>
            {(a) => (
              <textarea {...a} rows={2} value={form.includeFolders} onChange={(e) => set("includeFolders")(e.target.value)} spellCheck={false} />
            )}
          </Field>
          <Field label={t("addSource.microsoft.exclude")} optional error={err("excludeFolders")} hint={t("addSource.microsoft.excludeHint")}>
            {(a) => (
              <textarea {...a} rows={2} value={form.excludeFolders} onChange={(e) => set("excludeFolders")(e.target.value)} spellCheck={false} />
            )}
          </Field>
        </>
      )}
      <details className="help">
        <summary>{t("addSource.advanced")}</summary>
        <Field
          label={t("addSource.google.clientSecret")}
          optional
          hint={secretSaved ? t("addSource.savedKeepIt") : t("addSource.microsoft.secretHint")}
        >
          {(a) => <input {...a} ref={secretRef} type="password" autoComplete="off" spellCheck={false} />}
        </Field>
      </details>
      <NameField value={name} onChange={setName} placeholder={label} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.microsoft.submit")} />
    </form>
  );
}
