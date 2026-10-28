import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { api, MCP_TOOLS, type McpClientCreated, type McpClientInfo, type SourceInfo } from "../api";
import { useAsync } from "../hooks";
import { DEFAULT_SCOPE_FORM, MAX_CLIENT_NAME, scopesFromForm, setupAppLabel, sortSnippets, validateScopeForm, type ScopeForm, type ScopeFormErrors } from "../mcp";
import { sourceKindLabel } from "../format";
import { CopyButton } from "./CopyButton";
import { Dialog } from "./Dialog";
import { ErrorNote, Spinner } from "./ui";
import { Trans, useT } from "../i18n";

/**
 * "Connect an app": name and scopes, then the client's token (shown once) and setup snippets.
 *
 * The token lives only in this component's state. The parent gets the client's info without
 * it, and closing the dialog unmounts this component, which drops the token.
 */
export function ConnectAppWizard({ onClose, onCreated }: { onClose: () => void; onCreated: (client: McpClientInfo) => void }) {
  const t = useT();
  const [created, setCreated] = useState<McpClientCreated | null>(null);
  const [copied, setCopied] = useState(false);
  const [confirmClose, setConfirmClose] = useState(false);
  // Closing (Escape, ×, Done) before anything was copied asks once, since the token is gone after.
  const close = () => {
    if (created && !copied && !confirmClose) setConfirmClose(true);
    else onClose();
  };

  return (
    <Dialog
      title={created ? t("connectApp.titleFor", { name: created.client.name }) : t("connectApp.title")}
      description={created ? undefined : t("connectApp.description")}
      onClose={close}
      wide
    >
      {created ? (
        <TokenStep
          created={created}
          onCopied={() => setCopied(true)}
          confirmClose={confirmClose && !copied}
          onCancelClose={() => setConfirmClose(false)}
          onDone={close}
        />
      ) : (
        <ScopeStep
          onCancel={onClose}
          onCreated={(c) => {
            setCreated(c);
            onCreated(c.client);
          }}
        />
      )}
    </Dialog>
  );
}

function ScopeStep({ onCancel, onCreated }: { onCancel: () => void; onCreated: (c: McpClientCreated) => void }) {
  const t = useT();
  const [form, setForm] = useState<ScopeForm>(DEFAULT_SCOPE_FORM);
  const [errors, setErrors] = useState<ScopeFormErrors>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const sources = useAsync(() => api.sourcesList());
  const ids = {
    name: useId(),
    nameErr: useId(),
    srcErr: useId(),
    toolErr: useId(),
    sensitiveWarn: useId(),
  };
  const formRef = useRef<HTMLFormElement>(null);

  const set = <K extends keyof ScopeForm>(k: K, v: ScopeForm[K]) => {
    setForm((f) => ({ ...f, [k]: v }));
    setErrors((e) => ({ ...e, [k === "allSources" ? "sources" : k === "allTools" ? "tools" : k]: undefined }));
  };
  const toggle = (k: "sources" | "tools", id: string, on: boolean) =>
    set(k, on ? [...form[k], id] : form[k].filter((x) => x !== id));

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const errs = validateScopeForm(form);
    setErrors(errs);
    const first = (["name", "sources", "tools"] as const).find((k) => errs[k]);
    if (first) {
      formRef.current?.querySelector<HTMLElement>(`[data-field="${first}"]`)?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      onCreated(await api.mcpClientsCreate({ name: form.name.trim(), scopes: scopesFromForm(form) }));
    } catch (err) {
      setError(err);
      setBusy(false);
    }
  };

  const sourceList: SourceInfo[] = sources.data ?? [];
  const sourcesUnavailable = sources.error != null;

  return (
    <form ref={formRef} onSubmit={submit} noValidate>
      <div className="field">
        <label htmlFor={ids.name}>{t("connectApp.name")}</label>
        <input
          id={ids.name}
          data-field="name"
          data-autofocus=""
          type="text"
          value={form.name}
          maxLength={MAX_CLIENT_NAME}
          placeholder={t("connectApp.namePlaceholder")}
          autoComplete="off"
          onChange={(e) => set("name", e.target.value)}
          aria-invalid={errors.name ? true : undefined}
          aria-describedby={errors.name ? ids.nameErr : undefined}
        />
        {errors.name && (
          <p id={ids.nameErr} className="field-error small">
            {errors.name}
          </p>
        )}
      </div>

      <fieldset className="field" aria-describedby={errors.sources ? ids.srcErr : undefined}>
        <legend>{t("connectApp.canRead")}</legend>
        <label className="check">
          <input type="radio" name="mcp-sources" checked={form.allSources} onChange={() => set("allSources", true)} data-field={form.allSources ? "sources" : undefined} />
          <span>
            {t("connectApp.allSources")} <span className="muted small block">{t("connectApp.allSourcesHint")}</span>
          </span>
        </label>
        <label className="check">
          <input
            type="radio"
            name="mcp-sources"
            checked={!form.allSources}
            onChange={() => set("allSources", false)}
            disabled={sourcesUnavailable || sourceList.length === 0}
          />
          <span>{t("connectApp.onlySources")}</span>
        </label>
        {sources.loading && !sources.data && <Spinner label={t("connectApp.loadingSources")} />}
        {sourcesUnavailable && <p className="muted small">{t("connectApp.sourcesUnavailable")}</p>}
        {!form.allSources && (
          <div className="scope-options">
            {sourceList.map((s, i) => (
              <label key={s.id} className="check">
                <input
                  type="checkbox"
                  checked={form.sources.includes(s.id)}
                  onChange={(e) => toggle("sources", s.id, e.target.checked)}
                  data-field={i === 0 ? "sources" : undefined}
                />
                <span>
                  {s.name} <span className="muted small">{sourceKindLabel(s.kind)}</span>
                </span>
              </label>
            ))}
          </div>
        )}
        {errors.sources && (
          <p id={ids.srcErr} className="field-error small">
            {errors.sources}
          </p>
        )}
      </fieldset>

      <fieldset className="field" aria-describedby={errors.tools ? ids.toolErr : undefined}>
        <legend>{t("connectApp.canDo")}</legend>
        <label className="check">
          <input type="radio" name="mcp-tools" checked={form.allTools} onChange={() => set("allTools", true)} data-field={form.allTools ? "tools" : undefined} />
          <span>
            {t("connectApp.allTools")} <span className="muted small block">{t("connectApp.allToolsHint")}</span>
          </span>
        </label>
        <label className="check">
          <input type="radio" name="mcp-tools" checked={!form.allTools} onChange={() => set("allTools", false)} />
          <span>{t("connectApp.onlyTools")}</span>
        </label>
        {!form.allTools && (
          <div className="scope-options">
            {MCP_TOOLS.map((tool, i) => (
              <label key={tool.name} className="check">
                <input
                  type="checkbox"
                  checked={form.tools.includes(tool.name)}
                  onChange={(e) => toggle("tools", tool.name, e.target.checked)}
                  data-field={i === 0 ? "tools" : undefined}
                />
                <span>
                  {t(tool.label)} <code className="muted small">{tool.name}</code>
                  <span className="muted small block">{t(tool.description)}</span>
                </span>
              </label>
            ))}
          </div>
        )}
        {errors.tools && (
          <p id={ids.toolErr} className="field-error small">
            {errors.tools}
          </p>
        )}
      </fieldset>

      <div className="field">
        <label className="switch">
          <input
            type="checkbox"
            role="switch"
            checked={form.includeSensitive}
            onChange={(e) => set("includeSensitive", e.target.checked)}
            aria-describedby={ids.sensitiveWarn}
          />
          <span className="switch-track" aria-hidden="true" />
          <span>{t("connectApp.allowSensitive")}</span>
        </label>
        <p id={ids.sensitiveWarn} className={form.includeSensitive ? "note note-warn small" : "muted small field-hint"}>
          {form.includeSensitive ? t("connectApp.sensitiveOn") : t("connectApp.sensitiveOff")}
        </p>
      </div>

      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onCancel} disabled={busy}>
          {t("common.cancel")}
        </button>
        <button type="submit" className="btn btn-primary" disabled={busy}>
          {busy ? <Spinner label={t("connectApp.creating")} /> : t("connectApp.createToken")}
        </button>
      </div>
    </form>
  );
}

function TokenStep({
  created,
  onCopied,
  confirmClose,
  onCancelClose,
  onDone,
}: {
  created: McpClientCreated;
  onCopied: () => void;
  /** Asking "close without copying?". */
  confirmClose: boolean;
  onCancelClose: () => void;
  onDone: () => void;
}) {
  const t = useT();
  const snippets = sortSnippets(created.setup);
  const [tab, setTab] = useState(0);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const ids = { warn: useId(), token: useId(), tabs: useId() };

  // The step replaces the form: move focus to its heading so screen readers start here.
  useEffect(() => headingRef.current?.focus(), []);
  useEffect(() => {
    if (confirmClose) confirmRef.current?.focus();
  }, [confirmClose]);

  const onTabKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    const next = (tab + (e.key === "ArrowRight" ? 1 : snippets.length - 1)) % snippets.length;
    setTab(next);
    tabRefs.current[next]?.focus();
  };

  const snippet = snippets[tab];

  return (
    <div>
      <h3 ref={headingRef} tabIndex={-1} className="token-heading">
        {t("connectApp.copyNow")}
      </h3>
      <p id={ids.warn} className="note note-warn">
        <Trans k="connectApp.shownOnce" tags={{ b: (c) => <b>{c}</b> }} />
      </p>
      <div className="field">
        <label htmlFor={ids.token}>{t("connectApp.tokenFor", { name: created.client.name })}</label>
        <div className="inline-row">
          <input
            id={ids.token}
            className="token-input"
            type="text"
            readOnly
            value={created.token}
            spellCheck={false}
            autoComplete="off"
            aria-describedby={ids.warn}
            onFocus={(e) => e.currentTarget.select()}
          />
          <CopyButton text={created.token} what={t("connectApp.tokenWhat")} className="btn btn-primary" onCopied={onCopied} />
        </div>
      </div>

      <h3 className="ask-sub">{t("connectApp.setUp")}</h3>
      {snippets.length === 0 ? (
        <p className="muted small">
          <Trans k="connectApp.noSnippets" tags={{ code: (c) => <code>{c}</code> }} />
        </p>
      ) : (
        <>
          <div className="tabs tabs-small" role="tablist" aria-label={t("connectApp.tabsLabel")} onKeyDown={onTabKey}>
            {snippets.map((s, i) => (
              <button
                key={`${s.app}-${i}`}
                ref={(el) => {
                  tabRefs.current[i] = el;
                }}
                type="button"
                role="tab"
                id={`${ids.tabs}-tab-${i}`}
                aria-selected={i === tab}
                aria-controls={`${ids.tabs}-panel`}
                tabIndex={i === tab ? 0 : -1}
                className="tab"
                onClick={() => setTab(i)}
              >
                {setupAppLabel(s.app)}
              </button>
            ))}
          </div>
          {snippet && (
            <div id={`${ids.tabs}-panel`} role="tabpanel" aria-labelledby={`${ids.tabs}-tab-${tab}`} className="snippet">
              {snippet.config_path ? (
                <p className="small">
                  <Trans
                    k="connectApp.addToConfig"
                    args={{ path: snippet.config_path, app: setupAppLabel(snippet.app) }}
                    tags={{ code: (c) => <code>{c}</code> }}
                  />
                </p>
              ) : (
                <p className="small">{t("connectApp.runIn", { app: setupAppLabel(snippet.app) })}</p>
              )}
              <pre className="snippet-text" tabIndex={0} aria-label={t("connectApp.setupWhat", { app: setupAppLabel(snippet.app) })}>
                <code>{snippet.text}</code>
              </pre>
              <CopyButton text={snippet.text} what={t("connectApp.setupWhat", { app: setupAppLabel(snippet.app) })} onCopied={onCopied} />
            </div>
          )}
        </>
      )}

      {confirmClose && (
        <p className="note note-warn small" role="alert">
          {t("connectApp.notCopied")}
        </p>
      )}
      <div className="dialog-actions">
        {confirmClose && (
          <button type="button" className="btn" onClick={onCancelClose}>
            {t("connectApp.goBack")}
          </button>
        )}
        <button ref={confirmRef} type="button" className={confirmClose ? "btn btn-danger" : "btn btn-primary"} onClick={onDone}>
          {confirmClose ? t("connectApp.closeAnyway") : t("common.done")}
        </button>
      </div>
    </div>
  );
}
