/**
 * Slack, Notion, GitHub and Linear: a pasted token (stored with `sources.secret.set`), or for
 * GitHub (device flow) and Linear (PKCE) a sign-in through the user's own OAuth app.
 */
import { useRef, useState, type FormEvent, type ReactNode } from "react";
import { api, type SaasKind } from "../api";
import { sourceKindLabel } from "../format";
import { Trans, useT, type MessageKey } from "../i18n";
import { ConnectWait, useAuthWait } from "./auth";
import { Actions, Check, Done, Field, FormError, HELP_TAGS, NameField, useAutofocus, useSaveSource, type FormProps } from "./form";
import {
  EMPTY_SAAS,
  LINEAR_INCLUDE,
  saasErrors,
  saasSettings,
  supportsOAuth,
  type LinearInclude,
  type SaasErrors,
  type SaasForm as Form,
  type SaasMethod,
} from "./kinds";

interface Guide {
  steps: MessageKey[];
  /** Where the user creates the token or app. */
  url: string;
}

const TOKEN_GUIDE: Record<SaasKind, Guide> = {
  slack: { steps: ["addSource.slack.step1", "addSource.slack.step2", "addSource.slack.step3"], url: "https://api.slack.com/apps" },
  notion: {
    steps: ["addSource.notion.step1", "addSource.notion.step2", "addSource.notion.step3"],
    url: "https://www.notion.so/profile/integrations",
  },
  github: {
    steps: ["addSource.github.tokenStep1", "addSource.github.tokenStep2", "addSource.github.tokenStep3"],
    url: "https://github.com/settings/personal-access-tokens",
  },
  linear: { steps: ["addSource.linear.tokenStep1", "addSource.linear.tokenStep2"], url: "https://linear.app/settings/account/security" },
};

const OAUTH_GUIDE: Partial<Record<SaasKind, Guide>> = {
  github: {
    steps: ["addSource.github.appStep1", "addSource.github.appStep2", "addSource.github.appStep3"],
    url: "https://github.com/settings/developers",
  },
  linear: { steps: ["addSource.linear.appStep1", "addSource.linear.appStep2", "addSource.linear.appStep3"], url: "https://linear.app/settings/api" },
};

/** What the pasted secret is called, per kind (also used when replacing it later). */
export const TOKEN_LABEL: Record<SaasKind, MessageKey> = {
  slack: "addSource.slack.tokenLabel",
  notion: "addSource.notion.tokenLabel",
  github: "addSource.github.tokenLabel",
  linear: "addSource.linear.tokenLabel",
};

// i18n-ignore-block: token prefixes, the same in every language
const TOKEN_PLACEHOLDER: Record<SaasKind, string> = {
  slack: "xoxp-…",
  notion: "ntn_…",
  github: "github_pat_…",
  linear: "lin_api_…",
};
// i18n-ignore-end

const LINEAR_INCLUDE_LABEL: Record<LinearInclude, MessageKey> = {
  assigned: "addSource.linear.assigned",
  created: "addSource.linear.created",
  subscribed: "addSource.linear.subscribed",
};

export function SaasGuide({ kind, method }: { kind: SaasKind; method: SaasMethod }) {
  const t = useT();
  const guide = (method === "oauth" && OAUTH_GUIDE[kind]) || TOKEN_GUIDE[kind];
  const site = new URL(guide.url).host;
  return (
    <details className="help">
      <summary>{method === "oauth" ? t("addSource.saas.howToApp", { label: sourceKindLabel(kind) }) : t(TOKEN_HOWTO[kind])}</summary>
      <ol className="small">
        {guide.steps.map((k) => (
          <li key={k}>
            <Trans k={k} tags={HELP_TAGS} />
          </li>
        ))}
      </ol>
      <p className="small muted">{t("addSource.saas.privacy")}</p>
      <button type="button" className="btn btn-small" onClick={() => void api.openUrl(guide.url).catch(() => {})}>
        {t("addSource.saas.open", { site })}
      </button>
    </details>
  );
}

const TOKEN_HOWTO: Record<SaasKind, MessageKey> = {
  slack: "addSource.slack.howTo",
  notion: "addSource.notion.howTo",
  github: "addSource.github.howTo",
  linear: "addSource.linear.howTo",
};

export function SaasForm({ kind, onBack, onClose, onAdded }: FormProps<SaasKind>) {
  const t = useT();
  const label = sourceKindLabel(kind);
  const [form, setForm] = useState<Form>(EMPTY_SAAS);
  const [name, setName] = useState("");
  const [errs, setErrs] = useState<SaasErrors>({});
  const [tokenSaved, setTokenSaved] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  // Uncontrolled: the token never enters React state and is cleared once sent.
  const tokenRef = useRef<HTMLInputElement>(null);
  const firstRef = useAutofocus<HTMLInputElement>();
  const save = useSaveSource(kind);
  const auth = useAuthWait(onAdded);

  const set = <K extends keyof Form>(key: K) => (v: Form[K]) => setForm((f) => ({ ...f, [key]: v }));
  const oauth = supportsOAuth(kind) && form.method === "oauth";

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const token = oauth ? "" : (tokenRef.current?.value ?? "");
    const next = saasErrors(kind, form, token, tokenSaved);
    setErrs(next);
    if (Object.keys(next).length > 0) return;
    setBusy(true);
    setError(null);
    try {
      const s = await save(name.trim() || label, saasSettings(kind, form));
      if (oauth) {
        await auth.begin(s);
        return;
      }
      if (token.trim()) {
        await api.sourcesSecretSet({ id: s.id, secret: token.trim() });
        if (tokenRef.current) tokenRef.current.value = "";
        setTokenSaved(true);
        // The first pass may have run before the token arrived.
        await api.sourcesSync({ id: s.id }).catch(() => {});
      }
      onAdded(s);
      onClose();
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

  const err = (k: keyof SaasErrors) => (errs[k] ? t(errs[k]) : null);

  return (
    <form onSubmit={submit} noValidate>
      {isOAuthKind(kind) && (
        <fieldset className="field">
          <legend>{t("addSource.saas.methodLegend")}</legend>
          {(["token", "oauth"] as const).map((m, i) => (
            <label key={m} className="check">
              <input
                ref={i === 0 ? firstRef : undefined}
                data-autofocus={i === 0 ? "" : undefined}
                type="radio"
                name="saas-method"
                checked={form.method === m}
                onChange={() => set("method")(m)}
              />
              <span>
                {m === "token" ? t(TOKEN_METHOD[kind]) : t("addSource.saas.methodOAuth", { label })}
                <span className="muted small block">{m === "token" ? t("addSource.saas.methodTokenHint") : t(OAUTH_METHOD_HINT[kind])}</span>
              </span>
            </label>
          ))}
        </fieldset>
      )}
      <SaasGuide kind={kind} method={oauth ? "oauth" : "token"} />
      {oauth ? (
        <Field label={t("addSource.saas.clientId")} error={err("clientId")} hint={isOAuthKind(kind) ? t(OAUTH_METHOD_HINT[kind]) : undefined}>
          {(a) => (
            <input {...a} type="text" value={form.clientId} onChange={(e) => set("clientId")(e.target.value)} autoComplete="off" spellCheck={false} />
          )}
        </Field>
      ) : (
        <Field
          label={t(TOKEN_LABEL[kind])}
          error={err("token")}
          optional={tokenSaved}
          hint={tokenSaved ? t("addSource.savedKeepIt") : t("addSource.saas.tokenHint")}
        >
          {(a) => (
            <input
              {...a}
              ref={supportsOAuth(kind) ? tokenRef : mergeRefs(tokenRef, firstRef)}
              data-autofocus={supportsOAuth(kind) ? undefined : ""}
              type="password"
              autoComplete="off"
              spellCheck={false}
              placeholder={TOKEN_PLACEHOLDER[kind]}
            />
          )}
        </Field>
      )}
      <KindOptions kind={kind} form={form} set={set} err={err} />
      <NameField value={name} onChange={setName} placeholder={label} />
      <FormError error={error} />
      <Actions onBack={onBack} busy={busy} submitLabel={oauth ? t("addSource.saas.signIn", { label }) : t("addSource.saas.submit")} />
    </form>
  );
}

/** Only for the kinds that `supportsOAuth`. */
type OAuthKind = "github" | "linear";

const TOKEN_METHOD: Record<OAuthKind, MessageKey> = {
  github: "addSource.github.methodToken",
  linear: "addSource.linear.methodToken",
};

const OAUTH_METHOD_HINT: Record<OAuthKind, MessageKey> = {
  github: "addSource.github.clientIdHint",
  linear: "addSource.linear.clientIdHint",
};

const isOAuthKind = (k: SaasKind): k is OAuthKind => supportsOAuth(k);

function mergeRefs<T>(...refs: { current: T | null }[]) {
  return (el: T | null) => {
    for (const r of refs) r.current = el;
  };
}

function KindOptions({
  kind,
  form,
  set,
  err,
}: {
  kind: SaasKind;
  form: Form;
  set: <K extends keyof Form>(key: K) => (v: Form[K]) => void;
  err: (k: keyof SaasErrors) => string | null;
}): ReactNode {
  const t = useT();
  switch (kind) {
    case "slack": {
      const listed = form.conversations.trim() !== "";
      return (
        <>
          <Field label={t("addSource.slack.conversations")} optional error={err("conversations")} hint={t("addSource.slack.conversationsHint")}>
            {(a) => (
              <input {...a} type="text" value={form.conversations} onChange={(e) => set("conversations")(e.target.value)} autoComplete="off" spellCheck={false} />
            )}
          </Field>
          <div className="field">
            <Check checked={form.includeDms} onChange={set("includeDms")} label={t("addSource.slack.dms")} disabled={listed} />
            <Check checked={form.includeChannels} onChange={set("includeChannels")} label={t("addSource.slack.channels")} disabled={listed} />
          </div>
        </>
      );
    }
    case "notion":
      return (
        <div className="field">
          <Check
            checked={form.includeDatabases}
            onChange={set("includeDatabases")}
            label={t("addSource.notion.databases")}
            hint={t("addSource.notion.databasesHint")}
          />
        </div>
      );
    case "github": {
      const listed = form.repos.trim() !== "";
      return (
        <>
          <Field label={t("addSource.github.repos")} optional error={err("repos")} hint={t("addSource.github.reposHint")}>
            {(a) => <input {...a} type="text" value={form.repos} onChange={(e) => set("repos")(e.target.value)} autoComplete="off" spellCheck={false} />}
          </Field>
          <div className="field">
            <Check
              checked={form.allActivity && listed}
              onChange={set("allActivity")}
              label={t("addSource.github.allActivity")}
              hint={t("addSource.github.allActivityHint")}
              disabled={!listed}
            />
            <Check checked={form.includeDiscussions} onChange={set("includeDiscussions")} label={t("addSource.github.discussions")} />
          </div>
        </>
      );
    }
    case "linear":
      return (
        <>
          <fieldset className="field" aria-describedby={err("include") ? "linear-include-err" : undefined}>
            <legend>{t("addSource.linear.include")}</legend>
            {LINEAR_INCLUDE.map((i) => (
              <Check
                key={i}
                checked={form.include.includes(i)}
                onChange={(on) => set("include")(on ? [...form.include, i] : form.include.filter((x) => x !== i))}
                label={t(LINEAR_INCLUDE_LABEL[i])}
              />
            ))}
            {err("include") && (
              <p id="linear-include-err" className="field-error small">
                {err("include")}
              </p>
            )}
          </fieldset>
          <Field label={t("addSource.linear.teams")} optional error={err("teams")} hint={t("addSource.linear.teamsHint")}>
            {(a) => <input {...a} type="text" value={form.teams} onChange={(e) => set("teams")(e.target.value)} autoComplete="off" spellCheck={false} />}
          </Field>
        </>
      );
  }
}
