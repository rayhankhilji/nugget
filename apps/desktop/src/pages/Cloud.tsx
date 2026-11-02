import { useEffect, useMemo, useState, type FormEvent } from "react";
import { splitCitations } from "../citations";
import { CloudError, cloudApi, type BillingInterval, type CloudAnswer, type CloudApp, type CloudConnection, type CloudPlan, type CloudSubscription, type CloudSyncResult } from "../cloud";
import { formatRelative, formatTime } from "../format";
import { useAsync, useEvent, useNow, useOpener } from "../hooks";
import { AppLogo } from "../components/AppLogo";
import { Badge, EmptyState, ErrorNote, Loading, PageHeader, Spinner } from "../components/ui";
import { formatDate, formatNumber, useT, type TFunction } from "../i18n";
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
      <Billing />
      <Connections />
      <CloudAsk />
    </div>
  );
}

/** The server reports `active`; older builds and Composio say `ACTIVE`. */
function isActive(status: string): boolean {
  return status.toLowerCase() === "active";
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
      const r = await cloudApi.sync();
      setSynced(r.synced);
      if (r.running) setNotice(t("cloud.syncRunning"));
      list.reload();
    });

  // While the cloud imports in the background, refresh the list every few seconds.
  const importing = data?.connections.some((c) => c.syncing) ?? false;
  const reload = list.reload;
  useEffect(() => {
    if (!importing) return;
    const timer = setInterval(reload, 3000);
    return () => clearInterval(timer);
  }, [importing, reload]);

  const connected = new Set(data?.connections.filter((c) => isActive(c.status)).map((c) => c.toolkit) ?? []);

  return (
    <>
      <section aria-labelledby="cloud-connections">
        <h2 id="cloud-connections">{t("cloud.connections.title")}</h2>
        <CloudErrorNote error={list.error} onRetry={list.reload} />
        <CloudErrorNote error={error} />
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
                      <Badge tone={isActive(c.status) ? "good" : c.status.toLowerCase() === "initiated" ? "neutral" : "bad"}>{isActive(c.status) ? t("cloud.connected") : c.status}</Badge>
                      {c.last_error && (
                        <div className="small muted" dir="auto">
                          {c.last_error}
                        </div>
                      )}
                    </td>
                    <td title={c.last_synced_at ? formatTime(c.last_synced_at) : undefined}>
                      {c.syncing ? (
                        <span role="status">{t("cloud.importing", { count: formatNumber(c.items_synced ?? 0) })}</span>
                      ) : (
                        formatRelative(c.last_synced_at, now)
                      )}
                    </td>
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
      <CloudErrorNote error={error} />
      {answer && (
        <div className="cloud-answer">
          {answer.downgraded && (
            <p className="note" role="status">
              {t("cloud.ask.downgraded")}
            </p>
          )}
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

const BILLING_HEADING = "cloud-billing";

/** Plan errors get their own sentence; `subscription_required` points at Plan & billing. */
function CloudErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const t = useT();
  const code = error instanceof CloudError ? error.cloud : null;
  if (code === "subscription_required") {
    const go = () => {
      const h = document.getElementById(BILLING_HEADING);
      h?.scrollIntoView?.();
      h?.focus();
    };
    return (
      <div className="note" role="alert">
        <span className="note-text">{t("cloud.billing.required")}</span>
        <button type="button" className="btn btn-small" onClick={go}>
          {t("cloud.billing.seePlans")}
        </button>
      </div>
    );
  }
  if (code === "device_limit_reached" || code === "connection_limit_reached") {
    return (
      <div className="note note-error" role="alert">
        <span className="note-text">{t(code === "device_limit_reached" ? "cloud.billing.deviceLimit" : "cloud.billing.connectionLimit")}</span>
      </div>
    );
  }
  return <ErrorNote error={error} onRetry={onRetry} />;
}

const gbp = (n: number) => formatNumber(n, { style: "currency", currency: "GBP", maximumFractionDigits: 0 });
const day = (iso: string | null) => (iso && !Number.isNaN(new Date(iso).getTime()) ? formatDate(iso) : null);
/** Stripe statuses that no longer grant access: offer the plans again. */
const ENDED = ["canceled", "incomplete_expired", "unpaid"];

function statusText(s: CloudSubscription, t: TFunction): string {
  const end = day(s.current_period_end);
  if (s.cancel_at_period_end && end) return t("cloud.billing.status.cancels", { date: end });
  if (s.status === "trialing") {
    const until = day(s.trial_end);
    return until ? t("cloud.billing.status.trialUntil", { date: until }) : t("cloud.billing.status.trialing");
  }
  if (s.status === "active") return t("cloud.billing.status.active");
  if (s.status === "past_due") return t("cloud.billing.status.pastDue");
  return s.status;
}

function Billing() {
  const t = useT();
  const me = useAsync(cloudApi.me);
  const plans = useAsync(cloudApi.plans);
  useEvent(cloudApi.onBillingChanged, () => {
    me.reload();
    plans.reload();
  });
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const act = async (key: string, fn: () => Promise<void>) => {
    setBusy(key);
    setError(null);
    setNotice(null);
    try {
      await fn();
      setNotice(t("cloud.billing.inBrowser"));
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };
  const checkout = (p: CloudPlan, interval: BillingInterval) => act(`checkout:${p.id}:${interval}`, () => cloudApi.checkout(p.id, interval));
  const portal = (key: string) => act(key, cloudApi.portal);

  const sub = me.data?.subscription;
  const subscribed = sub && !ENDED.includes(sub.status);
  const ent = me.data?.entitlements;
  const offers = plans.data?.plans ?? [];
  const maxPlan = offers.find((p) => p.id === "heed_max");

  return (
    <section aria-labelledby={BILLING_HEADING}>
      <h2 id={BILLING_HEADING} tabIndex={-1}>
        {t("cloud.billing.title")}
      </h2>
      <CloudErrorNote error={me.error} onRetry={me.reload} />
      <CloudErrorNote error={error} />
      {notice && (
        <p className="note" role="status">
          {notice}
        </p>
      )}
      {me.loading && !me.data && <Loading lines={2} />}
      {me.data && subscribed && sub && (
        <div className="cloud-subscription">
          <p>
            <strong>{ent?.name ?? offers.find((p) => p.id === sub.plan)?.name ?? sub.plan}</strong>{" "}
            <Badge tone={sub.status === "past_due" ? "bad" : sub.cancel_at_period_end ? "warn" : "good"}>{statusText(sub, t)}</Badge>
          </p>
          {ent && <p>{t("cloud.billing.smartAsks", { used: me.data.usage.smart_asks_used, limit: ent.smart_asks_per_month })}</p>}
          <p>
            <button type="button" className="btn" disabled={busy !== null} onClick={() => void portal("portal")}>
              {busy === "portal" ? <Spinner /> : t("cloud.billing.manage")}
            </button>{" "}
            {sub.plan !== "heed_max" && (
              <button type="button" className="btn btn-primary" disabled={busy !== null} onClick={() => void portal("upgrade")}>
                {/* i18n-ignore: the plan's brand name, when the plans list hasn't loaded */}
                {busy === "upgrade" ? <Spinner /> : t("cloud.billing.upgrade", { name: maxPlan?.name ?? "Heed Max" })}
              </button>
            )}
          </p>
        </div>
      )}
      {me.data && !subscribed && (
        <>
          <p>{t("cloud.billing.choose")}</p>
          <CloudErrorNote error={plans.error} onRetry={plans.reload} />
          {plans.loading && !plans.data && <Loading lines={2} />}
          <ul className="cloud-plans">
            {offers.map((p) => (
              <li key={p.id}>
                <h3>{p.name}</h3>
                <p>{t("cloud.billing.prices", { monthly: gbp(p.monthly_gbp), annual: gbp(p.annual_gbp) })}</p>
                {p.trial_days > 0 && <p className="small muted">{t("cloud.billing.trial", { days: p.trial_days })}</p>}
                <button type="button" className="btn btn-primary" disabled={busy !== null} onClick={() => void checkout(p, "month")} aria-label={t("cloud.billing.startMonthlyLabel", { name: p.name })}>
                  {busy === `checkout:${p.id}:month` ? <Spinner /> : t("cloud.billing.startMonthly")}
                </button>{" "}
                <button type="button" className="btn" disabled={busy !== null} onClick={() => void checkout(p, "year")} aria-label={t("cloud.billing.startAnnualLabel", { name: p.name })}>
                  {busy === `checkout:${p.id}:year` ? <Spinner /> : t("cloud.billing.startAnnual")}
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}
