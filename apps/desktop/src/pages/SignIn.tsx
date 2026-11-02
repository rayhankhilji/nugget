import { useState, type FormEvent } from "react";
import { cloudApi, type SignInProvider } from "../cloud";
import { ErrorNote, PageHeader, Spinner } from "../components/ui";
import { useT } from "../i18n";

type Waiting = { kind: "browser" } | { kind: "email"; email: string } | null;

/**
 * Nugget Cloud sign-in: Google in the browser, or an emailed magic link. Either way the
 * sign-in finishes when the link comes back to Nugget; the shell then emits
 * `nugget:cloud-session` and the gate re-checks the session. `failure` is that event's error.
 */
export function SignIn({ failure }: { failure?: string | null }) {
  const t = useT();
  const [email, setEmail] = useState("");
  const [busy, setBusy] = useState<SignInProvider | "email" | null>(null);
  const [waiting, setWaiting] = useState<Waiting>(null);
  const [error, setError] = useState<unknown>(null);

  const run = async (what: SignInProvider | "email", start: () => Promise<void>, then: Waiting) => {
    setBusy(what);
    setError(null);
    setWaiting(null);
    try {
      await start();
      setWaiting(then);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };

  const withProvider = (p: SignInProvider) => void run(p, () => cloudApi.signInWith(p), { kind: "browser" });
  const sendLink = (e: FormEvent) => {
    e.preventDefault();
    const address = email.trim();
    if (!address) return;
    void run("email", () => cloudApi.signInEmail(address), { kind: "email", email: address });
  };

  return (
    <div className="page cloud-signin">
      <PageHeader title={t("cloud.signIn.title")} description={t("cloud.signIn.description")} />
      {failure && (
        <p className="note note-error" role="alert">
          {t("cloud.signIn.failed", { error: failure })}
        </p>
      )}
      <ErrorNote error={error} />
      <div className="cloud-signin-providers">
        <button type="button" className="btn" disabled={busy !== null} onClick={() => withProvider("google")}>
          {busy === "google" ? <Spinner /> : t("cloud.signIn.google")}
        </button>
      </div>
      <form className="cloud-signin-email" onSubmit={sendLink}>
        <label htmlFor="cloud-email">{t("cloud.signIn.email")}</label>
        <input id="cloud-email" type="email" autoComplete="email" required value={email} onChange={(e) => setEmail(e.target.value)} disabled={busy !== null} />
        <button type="submit" className="btn btn-primary" disabled={busy !== null || !email.trim()}>
          {busy === "email" ? <Spinner /> : t("cloud.signIn.sendLink")}
        </button>
      </form>
      {waiting && (
        <p className="note" role="status">
          {waiting.kind === "email" ? t("cloud.signIn.checkEmail", { email: waiting.email }) : t("cloud.signIn.inBrowser")}
        </p>
      )}
    </div>
  );
}
