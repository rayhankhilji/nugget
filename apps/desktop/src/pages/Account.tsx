import { useState } from "react";
import { cloudApi } from "../cloud";
import { useAsync, useEvent } from "../hooks";
import { requestPage } from "../hooks.nav";
import { useT } from "../i18n";
import { Dialog } from "../components/Dialog";
import { EmptyState, ErrorNote, Loading, PageHeader, Spinner } from "../components/ui";
import { Billing, CloudErrorNote } from "./Cloud";

/**
 * Account (Settings, the account button, Ctrl/⌘+,): who is signed in, Plan & billing, devices,
 * sign out and deleting the account. Works without the local background service.
 */
export function AccountPage() {
  const t = useT();
  const session = useAsync(cloudApi.session);
  useEvent(cloudApi.onSession, session.reload);
  const s = session.data;

  if (!s) {
    return (
      <div className="page">
        <PageHeader title={t("account.title")} />
        {session.error ? <ErrorNote error={session.error} onRetry={session.reload} /> : <Loading lines={1} />}
      </div>
    );
  }
  if (!s.configured || !s.signed_in) {
    return (
      <div className="page">
        <PageHeader title={t("account.title")} />
        <EmptyState
          title={t("account.signedOut.title")}
          action={
            s.configured ? (
              <button type="button" className="btn btn-primary" onClick={() => requestPage("connections")}>
                {t("account.signedOut.signIn")}
              </button>
            ) : undefined
          }
        >
          {s.configured ? t("account.signedOut.body") : t("cloud.notConfigured.body")}
        </EmptyState>
      </div>
    );
  }
  return <SignedInAccount email={s.email} onChanged={session.reload} />;
}

function SignedInAccount({ email, onChanged }: { email: string | null; onChanged: () => void }) {
  const t = useT();
  const me = useAsync(cloudApi.me);
  const [busy, setBusy] = useState<"signout" | "delete" | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [confirming, setConfirming] = useState(false);
  const [typed, setTyped] = useState("");

  const run = async (what: "signout" | "delete", fn: () => Promise<unknown>) => {
    setBusy(what);
    setError(null);
    try {
      await fn();
      setConfirming(false);
      onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };
  const devices = me.data?.entitlements?.devices;

  return (
    <div className="page">
      <PageHeader
        title={t("account.title")}
        description={email ? t("cloud.signedInAs", { email }) : t("cloud.signedIn")}
        actions={
          <button type="button" className="btn" disabled={busy !== null} onClick={() => void run("signout", cloudApi.signOut)}>
            {busy === "signout" ? <Spinner /> : t("cloud.signOut")}
          </button>
        }
      />
      <ErrorNote error={confirming ? null : error} />
      <Billing />
      <section aria-labelledby="account-devices">
        <h2 id="account-devices">{t("account.devices.title")}</h2>
        <CloudErrorNote error={me.error} onRetry={me.reload} />
        <p className="muted">{devices != null ? t("account.devices.body", { count: devices }) : t("account.devices.none")}</p>
      </section>
      <section aria-labelledby="account-delete">
        <h2 id="account-delete">{t("account.delete.title")}</h2>
        <p className="muted">{t("account.delete.body")}</p>
        <button
          type="button"
          className="btn btn-danger-ghost"
          onClick={() => {
            setTyped("");
            setConfirming(true);
          }}
        >
          {t("account.delete.button")}
        </button>
      </section>
      {confirming && (
        <Dialog
          title={t("account.delete.confirmTitle")}
          description={t("account.delete.confirmBody")}
          onClose={() => {
            if (busy === null) setConfirming(false);
          }}
        >
          <ErrorNote error={error} />
          {email && (
            <div className="field delete-confirm">
              <label htmlFor="delete-email">{t("account.delete.typeEmail", { email })}</label>
              <input
                id="delete-email"
                className="input"
                type="email"
                autoComplete="off"
                spellCheck={false}
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                disabled={busy !== null}
                data-autofocus
              />
            </div>
          )}
          <div className="dialog-actions">
            <button
              type="button"
              className="btn btn-danger-ghost"
              disabled={busy !== null || (email != null && typed.trim().toLowerCase() !== email.toLowerCase())}
              onClick={() => void run("delete", cloudApi.deleteAccount)}
            >
              {busy === "delete" ? <Spinner /> : t("account.delete.confirm")}
            </button>
            <button type="button" className="btn btn-primary" disabled={busy !== null} onClick={() => setConfirming(false)} data-autofocus={email ? undefined : true}>
              {t("account.delete.keep")}
            </button>
          </div>
        </Dialog>
      )}
    </div>
  );
}
