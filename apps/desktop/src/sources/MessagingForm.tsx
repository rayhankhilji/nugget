/**
 * iMessage, WhatsApp chat exports and Telegram Desktop exports: message history already on this
 * computer, so no sign-in. iMessage needs macOS and Full Disk Access; the other two watch a folder
 * of export files the user saves there.
 */
import { useEffect, useRef, useState, type FormEvent } from "react";
import { api, ApiError, errorMessage, needsFullDiskAccess, type MessagingKind } from "../api";
import { sourceKindLabel } from "../format";
import { Trans, useT, type MessageKey } from "../i18n";
import { detectOs, type Os } from "../onboarding/platform";
import { Actions, Check, Field, FormError, HELP_TAGS, NameField, useAutofocus, useSaveSource, type FormProps } from "./form";
import { EMPTY_MESSAGING, messagingErrors, messagingSettings, type MessagingErrors, type MessagingForm as Form } from "./kinds";

const FDA_STEPS: readonly MessageKey[] = [
  "addSource.imessage.fdaStep1",
  "addSource.imessage.fdaStep2",
  "addSource.imessage.fdaStep3",
  "addSource.imessage.fdaStep4",
  "addSource.imessage.fdaStep5",
];

const WHATSAPP_STEPS: readonly MessageKey[] = [
  "addSource.whatsapp.step1",
  "addSource.whatsapp.step2",
  "addSource.whatsapp.step3",
  "addSource.whatsapp.step4",
];

const TELEGRAM_STEPS: readonly MessageKey[] = [
  "addSource.telegram.step1",
  "addSource.telegram.step2",
  "addSource.telegram.step3",
  "addSource.telegram.step4",
];

function Steps({ steps }: { steps: readonly MessageKey[] }) {
  return (
    <ol className="small">
      {steps.map((k) => (
        <li key={k}>
          <Trans k={k} tags={HELP_TAGS} />
        </li>
      ))}
    </ol>
  );
}

/** Plain-language steps to turn on Full Disk Access for Nugget (macOS). Also shown on the Sources page. */
export function FullDiskAccessSteps() {
  const t = useT();
  return (
    <div className="messaging-fda">
      <p className="small">
        <b>{t("addSource.imessage.fdaTitle")}</b>
      </p>
      <Steps steps={FDA_STEPS} />
    </div>
  );
}

/** A failed save: the Full Disk Access error comes with the steps again, anything else as usual. */
function SaveError({ kind, error }: { kind: MessagingKind; error: unknown }) {
  if (!error) return null;
  const e = ApiError.from(error);
  if (kind === "imessage" && needsFullDiskAccess(e.message)) {
    return (
      <div className="note note-error messaging-error" role="alert">
        <p dir="auto">{e.message}</p>
        <FullDiskAccessSteps />
      </div>
    );
  }
  return <FormError error={error} />;
}

export function MessagingForm({ kind, onBack, onClose, onAdded, os }: FormProps<MessagingKind> & { os?: Os }) {
  const t = useT();
  const label = sourceKindLabel(kind);
  const platform = os ?? detectOs();
  const unsupported = kind === "imessage" && platform !== "macos";
  const [form, setForm] = useState<Form>(EMPTY_MESSAGING);
  const [name, setName] = useState("");
  const [errs, setErrs] = useState<MessagingErrors>({});
  const [pickError, setPickError] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const save = useSaveSource(kind);
  const pathRef = useRef<HTMLInputElement>(null);
  const sinceRef = useRef<HTMLInputElement>(null);
  const browseRef = useAutofocus<HTMLButtonElement>();
  const formRef = useRef<HTMLFormElement>(null);

  // iMessage has no path field: start on the first option (or Back when it can't be added here).
  useEffect(() => {
    if (kind === "imessage") formRef.current?.querySelector<HTMLElement>("input, .btn")?.focus();
  }, [kind]);

  const set = <K extends keyof Form>(key: K) => (v: Form[K]) => setForm((f) => ({ ...f, [key]: v }));

  const browse = async () => {
    try {
      const [picked] = await api.pickFolders(false);
      if (picked) {
        set("path")(picked);
        setErrs((e) => ({ ...e, path: undefined }));
      }
      setPickError(null);
    } catch (e) {
      setPickError(t("addSource.pickerFailed", { error: errorMessage(e) }));
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (unsupported || busy) return;
    const next = messagingErrors(kind, form);
    setErrs(next);
    if (next.path) return pathRef.current?.focus();
    if (next.sinceDays) return sinceRef.current?.focus();
    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || label, messagingSettings(kind, form));
      onAdded(s);
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const err = (k: keyof MessagingErrors) => (errs[k] ? t(errs[k]) : null);
  const pathError = err("path") ?? pickError;

  return (
    <form ref={formRef} onSubmit={submit} noValidate className="messaging-form">
      {kind === "imessage" &&
        (unsupported ? (
          <p className="note" role="note">
            {t("addSource.imessage.macOnly")}
          </p>
        ) : (
          <section className="help">
            <FullDiskAccessSteps />
            <p className="small muted">{t("addSource.imessage.copyNote")}</p>
          </section>
        ))}

      {kind === "whatsapp" && (
        <section className="help">
          <p className="small">
            <b>{t("addSource.whatsapp.howTo")}</b>
          </p>
          <Steps steps={WHATSAPP_STEPS} />
          <p className="small muted">{t("addSource.whatsapp.refresh")}</p>
        </section>
      )}

      {kind === "telegram" && (
        <section className="help">
          <p className="small">
            <b>{t("addSource.telegram.howTo")}</b>
          </p>
          <Steps steps={TELEGRAM_STEPS} />
        </section>
      )}

      {kind !== "imessage" && (
        <Field
          label={kind === "whatsapp" ? t("addSource.whatsapp.folder") : t("addSource.telegram.path")}
          hint={kind === "whatsapp" ? t("addSource.whatsapp.folderHint") : <Trans k="addSource.telegram.pathHint" tags={HELP_TAGS} />}
          error={pathError}
        >
          {(a) => (
            <div className="inline-row">
              <input
                {...a}
                ref={pathRef}
                type="text"
                dir="ltr"
                value={form.path}
                onChange={(e) => set("path")(e.target.value)}
                autoComplete="off"
                spellCheck={false}
              />
              <button ref={browseRef} data-autofocus="" type="button" className="btn" onClick={() => void browse()}>
                {t("addSource.messages.browse")}
              </button>
            </div>
          )}
        </Field>
      )}

      {kind === "whatsapp" && (
        <Field label={t("addSource.whatsapp.me")} optional hint={t("addSource.whatsapp.meHint")}>
          {(a) => <input {...a} type="text" dir="auto" value={form.me} onChange={(e) => set("me")(e.target.value)} autoComplete="off" spellCheck={false} />}
        </Field>
      )}

      {!unsupported && (
        <fieldset className="field messaging-options">
          <legend>{t("addSource.messages.options")}</legend>
          <div className="messaging-checks">
            {kind === "imessage" && (
              <Check checked={form.contacts} onChange={set("contacts")} label={t("addSource.imessage.contacts")} hint={t("addSource.imessage.contactsHint")} />
            )}
            <Check checked={form.includeGroups} onChange={set("includeGroups")} label={t("addSource.messages.groups")} />
            <Check checked={form.private} onChange={set("private")} label={t("addSource.messages.private")} hint={t("addSource.messages.privateHint")} />
          </div>
          <Field label={t("addSource.messages.since")} optional hint={t("addSource.messages.sinceHint")} error={err("sinceDays")}>
            {(a) => (
              <input
                {...a}
                ref={sinceRef}
                type="text"
                inputMode="numeric"
                dir="ltr"
                className="messaging-since"
                value={form.sinceDays}
                onChange={(e) => set("sinceDays")(e.target.value)}
                autoComplete="off"
              />
            )}
          </Field>
        </fieldset>
      )}

      {!unsupported && <NameField value={name} onChange={setName} placeholder={label} />}
      <SaveError kind={kind} error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={t("addSource.messages.submit")} blocked={unsupported ? t("addSource.imessage.macOnlyShort") : null} />
    </form>
  );
}
