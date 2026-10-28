import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { api, MCP_TOOLS, type McpClientCreated, type McpClientInfo, type SourceInfo } from "../api";
import { useAsync } from "../hooks";
import { DEFAULT_SCOPE_FORM, MAX_CLIENT_NAME, scopesFromForm, setupAppLabel, sortSnippets, validateScopeForm, type ScopeForm, type ScopeFormErrors } from "../mcp";
import { SOURCE_KIND_LABEL } from "../format";
import { CopyButton } from "./CopyButton";
import { Dialog } from "./Dialog";
import { ErrorNote, Spinner } from "./ui";

/**
 * "Connect an app": name and scopes, then the client's token (shown once) and setup snippets.
 *
 * The token lives only in this component's state. The parent gets the client's info without
 * it, and closing the dialog unmounts this component, which drops the token.
 */
export function ConnectAppWizard({ onClose, onCreated }: { onClose: () => void; onCreated: (client: McpClientInfo) => void }) {
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
      title={created ? `Connect ${created.client.name}` : "Connect an app"}
      description={
        created
          ? undefined
          : "Give an AI app on this device read access to Nugget over MCP. You choose what it can see, and you can revoke it at any time."
      }
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
        <label htmlFor={ids.name}>App name</label>
        <input
          id={ids.name}
          data-field="name"
          data-autofocus=""
          type="text"
          value={form.name}
          maxLength={MAX_CLIENT_NAME}
          placeholder="Claude Desktop"
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
        <legend>What it can read</legend>
        <label className="check">
          <input type="radio" name="mcp-sources" checked={form.allSources} onChange={() => set("allSources", true)} data-field={form.allSources ? "sources" : undefined} />
          <span>
            All sources <span className="muted small block">Including sources you add later.</span>
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
          <span>Only these sources</span>
        </label>
        {sources.loading && !sources.data && <Spinner label="Loading sources" />}
        {sourcesUnavailable && <p className="muted small">Couldn't list your sources, so only “All sources” is available.</p>}
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
                  {s.name} <span className="muted small">{SOURCE_KIND_LABEL[s.kind] ?? s.kind}</span>
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
        <legend>What it can do</legend>
        <label className="check">
          <input type="radio" name="mcp-tools" checked={form.allTools} onChange={() => set("allTools", true)} data-field={form.allTools ? "tools" : undefined} />
          <span>
            All read-only tools <span className="muted small block">Nugget's MCP tools only read; none change your data.</span>
          </span>
        </label>
        <label className="check">
          <input type="radio" name="mcp-tools" checked={!form.allTools} onChange={() => set("allTools", false)} />
          <span>Only these tools</span>
        </label>
        {!form.allTools && (
          <div className="scope-options">
            {MCP_TOOLS.map((t, i) => (
              <label key={t.name} className="check">
                <input
                  type="checkbox"
                  checked={form.tools.includes(t.name)}
                  onChange={(e) => toggle("tools", t.name, e.target.checked)}
                  data-field={i === 0 ? "tools" : undefined}
                />
                <span>
                  {t.label} <code className="muted small">{t.name}</code>
                  <span className="muted small block">{t.description}</span>
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
          <span>Allow sensitive items</span>
        </label>
        <p id={ids.sensitiveWarn} className={form.includeSensitive ? "note note-warn small" : "muted small field-hint"}>
          {form.includeSensitive
            ? "This app will be able to read items Nugget marked sensitive, such as messages with financial, health or identity details. Many apps send what they read to a cloud AI model. Only allow this for apps you trust with that."
            : "Off: items Nugget marked sensitive (financial, health, identity details) are never returned to this app."}
        </p>
      </div>

      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onCancel} disabled={busy}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary" disabled={busy}>
          {busy ? <Spinner label="Creating" /> : "Create token"}
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
        Copy the token now
      </h3>
      <p id={ids.warn} className="note note-warn">
        <b>This token is shown only once.</b> Nugget doesn't keep it in readable form, so it can't show it again. If you lose it, revoke this app and connect it
        again.
      </p>
      <div className="field">
        <label htmlFor={ids.token}>Access token for {created.client.name}</label>
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
          <CopyButton text={created.token} what="token" className="btn btn-primary" onCopied={onCopied} />
        </div>
      </div>

      <h3 className="ask-sub">Set up your app</h3>
      {snippets.length === 0 ? (
        <p className="muted small">
          Point your app's MCP settings at Nugget and use the token above as its bearer token. Run <code>nuggetd mcp --help</code> for the options.
        </p>
      ) : (
        <>
          <div className="tabs tabs-small" role="tablist" aria-label="Apps" onKeyDown={onTabKey}>
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
                  Add this to <code>{snippet.config_path}</code>, then restart {setupAppLabel(snippet.app)}.
                </p>
              ) : (
                <p className="small">Run or paste this in {setupAppLabel(snippet.app)}:</p>
              )}
              <pre className="snippet-text" tabIndex={0} aria-label={`${setupAppLabel(snippet.app)} setup`}>
                <code>{snippet.text}</code>
              </pre>
              <CopyButton text={snippet.text} what={`${setupAppLabel(snippet.app)} setup`} onCopied={onCopied} />
            </div>
          )}
        </>
      )}

      {confirmClose && (
        <p className="note note-warn small" role="alert">
          You haven't copied the token or a setup snippet. Once you close this, the token can't be shown again.
        </p>
      )}
      <div className="dialog-actions">
        {confirmClose && (
          <button type="button" className="btn" onClick={onCancelClose}>
            Go back
          </button>
        )}
        <button ref={confirmRef} type="button" className={confirmClose ? "btn btn-danger" : "btn btn-primary"} onClick={onDone}>
          {confirmClose ? "Close anyway" : "Done"}
        </button>
      </div>
    </div>
  );
}
