import { useMemo, useState, type FormEvent } from "react";
import { splitCitations } from "../citations";
import { cloudApi, type CloudAnswer, type CloudApp, type CloudConnection, type CloudSyncResult } from "../cloud";
import { formatRelative, formatTime } from "../format";
import { useAsync, useEvent, useNow, useOpener } from "../hooks";
import { AppLogo } from "../components/AppLogo";
import { Badge, EmptyState, ErrorNote, Loading, PageHeader, Spinner } from "../components/ui";
import { useT } from "../i18n";
import { SignIn } from "./SignIn";

/**
 * Nugget Cloud: Sign-in when signed out, else connected accounts, available apps, sync and a
 * cloud Ask. Works without the local background service. Markup is structural on purpose.
 */
export function CloudPage() {
  const t = useT();
  const session = useAsync(cloudApi.session);
  const [failure, setFailure] = useState<string | null>(null);
  useEvent(cloudApi.onSession, (e) => {
    setFailure(e.signed_in ? null : e.error);
    session.reload();
  });

  const s = session.data;
  if (!s) {
    return (
      <div className="page">
        <PageHeader title={t("cloud.title")} />
        {session.error ? <ErrorNote error={session.error} onRetry={session.reload} /> : <Loading label={t("cloud.loading")} lines={1} />}
      </div>
    );
  }
  if (!s.configured) {
    return (
      <div className="page">
        <PageHeader title={t("cloud.title")} />
        <EmptyState title={t("cloud.notConfigured.title")}>{t("cloud.notConfigured.body")}</EmptyState>
      </div>
    );
  }
  if (!s.signed_in) return <SignIn failure={failure} />;
  return <SignedIn email={s.email} onSignedOut={session.reload} />;
}

function SignedIn({ email, onSignedOut }: { email: string | null; onSignedOut: () => void }) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const signOut = async () => {
    setBusy(true);
    setError(null);
    try {
      await cloudApi.signOut();
      onSignedOut();
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };
  return (
    <div className="page">
      <PageHeader
        title={t("cloud.title")}
        description={email ? t("cloud.signedInAs", { email }) : t("cloud.signedIn")}
        actions={
          <button type="button" className="btn" onClick={() => void signOut()} disabled={busy}>
            {busy ? <Spinner /> : t("cloud.signOut")}
          </button>
        }
      />
      <ErrorNote error={error} />
      <Connections />
      <CloudAsk />
    </div>
  );
}

function Connections() {
  const t = useT();
  const now = useNow();
  const list = useAsync(cloudApi.connections);
  useEvent(cloudApi.onConnectionsChanged, list.reload);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [synced, setSynced] = useState<CloudSyncResult[] | null>(null);

  const data = list.data;
  const nameOf = (slug: string) => data?.available.find((a) => a.slug === slug)?.name ?? slug;

  const act = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    setError(null);
    setNotice(null);
    try {
      await fn();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };

  const connect = (app: CloudApp) =>
    act(`connect:${app.slug}`, async () => {
      await cloudApi.connect(app.slug);
      setNotice(t("cloud.connectInBrowser", { name: app.name }));
    });
  const disconnect = (c: CloudConnection) =>
    act(`disconnect:${c.id}`, async () => {
      await cloudApi.disconnect(c.id);
      list.reload();
    });
  const sync = () =>
    act("sync", async () => {
      setSynced(null);
      setSynced((await cloudApi.sync()).synced);
      list.reload();
    });

  const connected = new Set(data?.connections.filter((c) => c.status === "ACTIVE").map((c) => c.toolkit) ?? []);

  return (
    <>
      <section aria-labelledby="cloud-connections">
        <h2 id="cloud-connections">{t("cloud.connections.title")}</h2>
        <ErrorNote error={list.error} onRetry={list.reload} />
        <ErrorNote error={error} />
        {notice && (
          <p className="note" role="status">
            {notice}
          </p>
        )}
        {list.loading && !data && <Loading lines={2} />}
        {data && data.connections.length === 0 && <EmptyState title={t("cloud.connections.empty")}>{t("cloud.connections.emptyBody")}</EmptyState>}
        {data && data.connections.length > 0 && (
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">{t("cloud.col.app")}</th>
                  <th scope="col">{t("cloud.col.status")}</th>
                  <th scope="col">{t("cloud.col.lastSync")}</th>
                  <th scope="col">
                    <span className="visually-hidden">{t("cloud.col.actions")}</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {data.connections.map((c) => (
                  <tr key={c.id}>
                    <th scope="row">{nameOf(c.toolkit)}</th>
                    <td>
                      <Badge tone={c.status === "ACTIVE" ? "good" : c.status === "INITIATED" ? "neutral" : "bad"}>{c.status === "ACTIVE" ? t("cloud.connected") : c.status}</Badge>
                      {c.last_error && (
                        <div className="small muted" dir="auto">
                          {c.last_error}
                        </div>
                      )}
                    </td>
                    <td title={c.last_synced_at ? formatTime(c.last_synced_at) : undefined}>{formatRelative(c.last_synced_at, now)}</td>
                    <td>
                      <button type="button" className="btn btn-small btn-danger-ghost" disabled={busy !== null} onClick={() => void disconnect(c)} aria-label={t("cloud.disconnectLabel", { name: nameOf(c.toolkit) })}>
                        {busy === `disconnect:${c.id}` ? <Spinner /> : t("cloud.disconnect")}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {data && data.connections.length > 0 && (
          <p>
            <button type="button" className="btn" disabled={busy !== null} onClick={() => void sync()}>
              {busy === "sync" ? <Spinner label={t("cloud.syncing")} /> : t("cloud.sync")}
            </button>
          </p>
        )}
        {synced && (
          <ul className="cloud-sync-results" aria-label={t("cloud.syncResults")}>
            {synced.map((r) => (
              <li key={r.toolkit}>
                {r.error ? (
                  <span className="note-error">{t("cloud.syncFailed", { name: nameOf(r.toolkit), error: r.error })}</span>
                ) : (
                  t("cloud.syncResult", { name: nameOf(r.toolkit), added: r.added, updated: r.updated })
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
      {data && (
        <section aria-labelledby="cloud-available">
          <h2 id="cloud-available">{t("cloud.available.title")}</h2>
          <ul className="cloud-apps">
            {data.available.map((a) => (
              <li key={a.slug} className="cloud-app">
                <AppLogo app={a} />
                <span className="cloud-app-name">{a.name}</span>
                {connected.has(a.slug) ? (
                  <Badge tone="good">{t("cloud.connected")}</Badge>
                ) : a.connectable ? (
                  <button type="button" className="btn btn-small" disabled={busy !== null} onClick={() => void connect(a)} aria-label={t("cloud.connectLabel", { name: a.name })}>
                    {busy === `connect:${a.slug}` ? <Spinner /> : t("cloud.connect")}
                  </button>
                ) : (
                  <Badge>{t("cloud.comingSoon")}</Badge>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
    </>
  );
}

function CloudAsk() {
  const t = useT();
  const [question, setQuestion] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [answer, setAnswer] = useState<CloudAnswer | null>(null);
  const [focused, setFocused] = useState<number | null>(null);
  const opener = useOpener();
  const valid = useMemo(() => new Set(answer?.citations.map((c) => c.n) ?? []), [answer]);

  const ask = async (e: FormEvent) => {
    e.preventDefault();
    const q = question.trim();
    if (!q) return;
    setBusy(true);
    setError(null);
    try {
      setAnswer(await cloudApi.ask(q, answer?.conversation_id));
      setFocused(null);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const cite = (n: number) => {
    setFocused(n);
    document.getElementById(`cloud-cite-${n}`)?.focus();
  };

  return (
    <section aria-labelledby="cloud-ask">
      <h2 id="cloud-ask">{t("cloud.ask.title")}</h2>
      <form onSubmit={(e) => void ask(e)}>
        <label htmlFor="cloud-question">{t("cloud.ask.label")}</label>
        <input id="cloud-question" value={question} onChange={(e) => setQuestion(e.target.value)} disabled={busy} />
        <button type="submit" className="btn btn-primary" disabled={busy || !question.trim()}>
          {busy ? <Spinner label={t("cloud.ask.thinking")} /> : t("cloud.ask.submit")}
        </button>
        {answer && (
          <button type="button" className="btn" disabled={busy} onClick={() => setAnswer(null)}>
            {t("cloud.ask.new")}
          </button>
        )}
      </form>
      <ErrorNote error={error} />
      {answer && (
        <div className="cloud-answer">
          <p dir="auto">
            {splitCitations(answer.answer, valid).map((p, i) =>
              p.kind === "text" ? (
                <span key={i}>{p.text}</span>
              ) : (
                <button key={i} type="button" className="cite" onClick={() => cite(p.n)} aria-label={t("cloud.ask.citationLabel", { n: p.n })}>
                  {p.raw}
                </button>
              ),
            )}
          </p>
          {answer.citations.length > 0 && (
            <>
              <h3>{t("cloud.ask.sources")}</h3>
              <ol className="cloud-citations">
                {answer.citations.map((c) => {
                  const title = c.title || t("cloud.ask.untitled");
                  return (
                    <li key={c.n} id={`cloud-cite-${c.n}`} tabIndex={-1} aria-current={focused === c.n ? "true" : undefined}>
                      <span>[{c.n}]</span> <strong dir="auto">{title}</strong> <Badge>{c.source}</Badge>
                      {c.occurred_at && <span className="muted small"> {formatTime(c.occurred_at)}</span>}
                      {c.url && (
                        <button type="button" className="btn btn-small" onClick={() => void opener.open(c.url!, title)} aria-label={t("cloud.ask.openLabel", { title })}>
                          {t("cloud.ask.open")}
                        </button>
                      )}
                      <div className="small muted" dir="auto">
                        {c.snippet}
                      </div>
                    </li>
                  );
                })}
              </ol>
              {opener.status && (
                <p className={opener.status.error ? "note note-error" : "small muted"} role="status">
                  {opener.status.text}
                </p>
              )}
            </>
          )}
        </div>
      )}
    </section>
  );
}
