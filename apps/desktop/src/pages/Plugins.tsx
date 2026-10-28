import { useEffect, useState } from "react";
import {
  approvePlugin,
  installPlugin,
  isMethodNotFound,
  listPlugins,
  newPluginCapabilities,
  parsePluginHost,
  pickPluginFile,
  pluginCapabilitiesEmpty,
  removePlugin,
  setPluginEnabled,
  type PluginCapabilities,
  type PluginInfo,
  type PluginInstallResult,
  type PluginSignature,
} from "../api";
import { useAsync, useNow } from "../hooks";
import { formatBytes, formatRelative, formatTime } from "../format";
import { useT, type MessageKey, type TFunction } from "../i18n";
import { Dialog } from "../components/Dialog";
import { Badge, Empty, ErrorNote, NotAvailable, PageHeader, Spinner } from "../components/ui";

const SIGNATURE_TONE: Record<PluginSignature, "good" | "warn" | "bad"> = { trusted: "good", dev: "warn", untrusted: "bad" };

const SIGNATURE_LABEL: Record<PluginSignature, MessageKey> = {
  trusted: "plugins.signature.trusted",
  dev: "plugins.signature.dev",
  untrusted: "plugins.signature.untrusted",
};

function kindLabel(kind: string, t: TFunction): string {
  if (kind === "connector") return t("plugins.kind.connector");
  if (kind === "tool") return t("plugins.kind.tool");
  return kind;
}

/** Each capability as one plain-language sentence. */
export function capabilityLines(caps: PluginCapabilities, t: TFunction): string[] {
  const lines = caps.hosts.map((entry) => {
    const h = parsePluginHost(entry);
    if (h.wildcard) return t(h.insecure ? "plugins.cap.hostWildcardInsecure" : "plugins.cap.hostWildcard", { host: h.host });
    return t(h.insecure ? "plugins.cap.hostInsecure" : "plugins.cap.host", { host: h.host });
  });
  for (const name of caps.secrets) lines.push(t("plugins.cap.secret", { name }));
  if (caps.kv_max_bytes > 0) lines.push(t("plugins.cap.kv", { size: formatBytes(caps.kv_max_bytes) }));
  return lines;
}

/** Installed plugins: sandboxed source connectors and assistant tools, with install, approve, enable and remove. */
export function PluginsPage() {
  const t = useT();
  const loaded = useAsync(listPlugins);
  const [plugins, setPlugins] = useState<PluginInfo[] | null>(null);
  const [staged, setStaged] = useState<PluginInstallResult | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installError, setInstallError] = useState<unknown>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [removing, setRemoving] = useState<PluginInfo | null>(null);
  const [announce, setAnnounce] = useState("");
  const now = useNow();

  useEffect(() => {
    if (loaded.data) setPlugins(loaded.data);
  }, [loaded.data]);

  const replace = (p: PluginInfo) => setPlugins((list) => [...(list ?? []).filter((x) => x.id !== p.id), p]);

  const installed = (name: string, version: string, kinds: string[], update: boolean) => {
    setAnnounce(update ? t("plugins.announce.updated", { name, version }) : t("plugins.announce.installed", { name }));
    setNotice(kinds.includes("connector") ? t("plugins.notice.connectorRestart", { name }) : null);
  };

  const installFromFile = async () => {
    setInstallError(null);
    setNotice(null);
    setInstalling(true);
    try {
      const path = await pickPluginFile();
      if (!path) return;
      const r = await installPlugin(path);
      if (r.needs_approval || !r.installed) {
        setStaged(r);
      } else {
        // An update that asks for nothing new is committed at once.
        installed(r.name, r.version, r.kinds, true);
        loaded.reload();
      }
    } catch (e) {
      setInstallError(e);
    } finally {
      setInstalling(false);
    }
  };

  const unavailable = isMethodNotFound(loaded.error);
  const list = [...(plugins ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  return (
    <div className="page">
      <PageHeader
        title={t("plugins.title")}
        description={t("plugins.description")}
        actions={
          !unavailable && (
            <button type="button" className="btn btn-primary" onClick={() => void installFromFile()} disabled={installing}>
              {installing ? <Spinner label={t("plugins.installing")} /> : t("plugins.install")}
            </button>
          )
        }
      />
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
      <ErrorNote error={installError} />
      {notice && (
        <p className="note" role="status">
          {notice}
        </p>
      )}

      {unavailable ? (
        <NotAvailable what={t("plugins.title")} />
      ) : (
        <>
          <ErrorNote error={loaded.error} onRetry={loaded.reload} />
          {loaded.loading && !plugins && <Spinner />}
          {plugins && list.length === 0 && <Empty title={t("plugins.emptyTitle")}>{t("plugins.emptyBody")}</Empty>}
          {list.length > 0 && (
            <ul className="source-list" aria-label={t("plugins.listLabel")}>
              {list.map((p) => (
                <PluginRow
                  key={p.id}
                  plugin={p}
                  now={now}
                  onChanged={replace}
                  onRemove={() => setRemoving(p)}
                  onAnnounce={setAnnounce}
                />
              ))}
            </ul>
          )}
          <details className="help">
            <summary>{t("plugins.help.summary")}</summary>
            <ul>
              <li>{t("plugins.help.sandbox")}</li>
              <li>{t("plugins.help.signature")}</li>
              <li>{t("plugins.help.restart")}</li>
            </ul>
          </details>
        </>
      )}

      {staged && (
        <ApprovalDialog
          staged={staged}
          onClose={(plugin) => {
            const s = staged;
            setStaged(null);
            if (plugin) {
              replace(plugin);
              installed(plugin.name, plugin.version, plugin.kinds, Boolean(s.installed_version));
            } else {
              setAnnounce(t("plugins.announce.cancelled", { name: s.name }));
            }
          }}
        />
      )}
      {removing && (
        <RemoveDialog
          plugin={removing}
          onClose={() => setRemoving(null)}
          onRemoved={() => {
            setPlugins((l) => (l ?? []).filter((x) => x.id !== removing.id));
            setAnnounce(t("plugins.announce.removed", { name: removing.name }));
            setRemoving(null);
          }}
        />
      )}
    </div>
  );
}

function PluginIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <path
        d="M10 3a2 2 0 0 1 4 0v2h4a1 1 0 0 1 1 1v4h-2a2 2 0 0 0 0 4h2v4a1 1 0 0 1-1 1h-4v-2a2 2 0 0 0-4 0v2H6a1 1 0 0 1-1-1v-4h2a2 2 0 0 0 0-4H5V6a1 1 0 0 1 1-1h4z"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function PluginRow({
  plugin: p,
  now,
  onChanged,
  onRemove,
  onAnnounce,
}: {
  plugin: PluginInfo;
  now: number;
  onChanged: (p: PluginInfo) => void;
  onRemove: () => void;
  onAnnounce: (text: string) => void;
}) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const headingId = `plugin-${p.id}`;
  const access = capabilityLines(p.approved, t);
  const tools = p.tools ?? [];

  const toggle = async () => {
    setBusy(true);
    setError(null);
    try {
      const next = await setPluginEnabled(p.id, !p.enabled);
      onChanged(next);
      onAnnounce(t(next.enabled ? "plugins.announce.enabled" : "plugins.announce.disabled", { name: p.name }));
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const state = !p.enabled ? "off" : p.loaded ? "running" : "notRunning";

  return (
    <li className="source">
      <div className="source-icon-wrap">
        <PluginIcon />
      </div>
      <div className="source-body">
        <div className="source-head">
          <h2 id={headingId} className="source-name">
            {p.name}
          </h2>
          <Badge tone={SIGNATURE_TONE[p.signature]}>{t(SIGNATURE_LABEL[p.signature])}</Badge>
          <Badge tone={state === "running" ? "good" : state === "off" ? "neutral" : "bad"}>{t(`plugins.state.${state}`)}</Badge>
        </div>
        {p.description && <p className="small source-meta">{p.description}</p>}
        <p className="muted small source-meta">
          <span>{t("plugins.version", { version: p.version })}</span>
          <span>{p.kinds.map((k) => kindLabel(k, t)).join(t("common.listSeparator"))}</span>
          <code>{p.id}</code>
          <span title={formatTime(p.installed_at)}>{t("plugins.installedAt", { when: formatRelative(p.installed_at, now) })}</span>
        </p>
        {tools.length > 0 && <p className="muted small source-meta">{t("plugins.tools", { list: tools.join(t("common.listSeparator")) })}</p>}
        {p.enabled && !p.loaded && p.error && (
          <p className="note note-error small" role="status">
            {p.error}
          </p>
        )}
        <details className="help">
          <summary>{t("plugins.access.summary")}</summary>
          {access.length === 0 ? (
            <p className="small muted">{t("plugins.access.none")}</p>
          ) : (
            <ul className="small">
              {access.map((line) => (
                <li key={line}>{line}</li>
              ))}
            </ul>
          )}
        </details>
        <ErrorNote error={error} />
        <div className="source-actions">
          <label className="switch">
            <input
              type="checkbox"
              role="switch"
              checked={p.enabled}
              onChange={() => void toggle()}
              disabled={busy}
              aria-describedby={headingId}
            />
            <span className="switch-track" aria-hidden="true" />
            <span>{t("plugins.enabled")}</span>
          </label>
          <button
            type="button"
            className="btn btn-small btn-danger-ghost"
            onClick={onRemove}
            disabled={busy}
            aria-label={t("plugins.removeLabel", { name: p.name })}
          >
            {t("plugins.remove")}
          </button>
        </div>
      </div>
    </li>
  );
}

/**
 * Shows what a staged package asks for, in plain language, and commits or discards it. On an
 * update, access the user hasn't approved before is marked as new. Closing any other way than
 * Allow discards the staged package.
 */
function ApprovalDialog({ staged, onClose }: { staged: PluginInstallResult; onClose: (installed: PluginInfo | null) => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const update = Boolean(staged.installed_version);
  const fresh = newPluginCapabilities(staged.requested, staged.previously_approved);
  const freshLines = new Set(capabilityLines(fresh, t));
  const lines = capabilityLines(staged.requested, t);
  const empty = pluginCapabilitiesEmpty(staged.requested);

  const cancel = () => {
    if (busy) return;
    // Discard the staged package; it expires on its own if this fails.
    approvePlugin(staged.token, false).catch(() => {});
    onClose(null);
  };

  const approve = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await approvePlugin(staged.token, true);
      if (!r.plugin) throw new Error(t("plugins.approve.failed"));
      onClose(r.plugin);
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={t(update ? "plugins.approve.titleUpdate" : "plugins.approve.title", { name: staged.name })}
      description={t("plugins.approve.description")}
      onClose={cancel}
      wide
    >
      <div className="source-head">
        <Badge tone={SIGNATURE_TONE[staged.signature]}>{t(SIGNATURE_LABEL[staged.signature])}</Badge>
        <span className="muted small">
          {update
            ? t("plugins.approve.versionUpdate", { from: staged.installed_version ?? "", to: staged.version })
            : t("plugins.version", { version: staged.version })}
        </span>
        <span className="muted small">{staged.kinds.map((k) => kindLabel(k, t)).join(t("common.listSeparator"))}</span>
      </div>
      {staged.description && <p>{staged.description}</p>}
      {staged.signature === "untrusted" && <p className="note note-warn">{t("plugins.approve.untrusted")}</p>}
      {staged.signature === "dev" && <p className="note note-warn">{t("plugins.approve.dev")}</p>}

      <h3 className="small">{t(update ? "plugins.approve.asksUpdate" : "plugins.approve.asks")}</h3>
      {empty ? (
        <p>{t("plugins.cap.none")}</p>
      ) : (
        <ul>
          {lines.map((line) => (
            <li key={line}>
              {line}
              {update && freshLines.has(line) && (
                <>
                  {" "}
                  <Badge tone="accent">{t("plugins.cap.new")}</Badge>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
      <p className="muted small">
        {t("plugins.approve.signer", { key: staged.signer.slice(0, 16) })}
      </p>
      {staged.kinds.includes("connector") && <p className="muted small">{t("plugins.approve.connectorNote")}</p>}
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={cancel} disabled={busy} data-autofocus="">
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-primary" onClick={() => void approve()} disabled={busy}>
          {busy ? (
            <Spinner label={t("plugins.approve.approving")} />
          ) : (
            t(update ? "plugins.approve.allowUpdate" : "plugins.approve.allow")
          )}
        </button>
      </div>
    </Dialog>
  );
}

function RemoveDialog({ plugin, onClose, onRemoved }: { plugin: PluginInfo; onClose: () => void; onRemoved: () => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      await removePlugin(plugin.id);
      onRemoved();
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };

  return (
    <Dialog
      title={t("plugins.removeDialog.title", { name: plugin.name })}
      description={t("plugins.removeDialog.description")}
      onClose={onClose}
    >
      <ErrorNote error={error} />
      <div className="dialog-actions">
        <button type="button" className="btn" onClick={onClose} disabled={busy} data-autofocus="">
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-danger" onClick={() => void remove()} disabled={busy}>
          {busy ? <Spinner label={t("plugins.removeDialog.removing")} /> : t("plugins.remove")}
        </button>
      </div>
    </Dialog>
  );
}
