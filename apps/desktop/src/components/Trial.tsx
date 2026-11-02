import { useState } from "react";
import { cloudApi, type CloudMe } from "../cloud";
import { useCloudSignedIn } from "../cloudSignedIn";
import { useAsync, useEvent, useNow } from "../hooks";
import { requestPage } from "../hooks.nav";
import { formatDate, useT } from "../i18n";
import { Dialog } from "./Dialog";

/**
 * The free trial, escalating gently as it ends: a quiet sidebar row while more than 3 days are
 * left, a card on Home for the last 3 days, and once, on the last day, a dialog. Each leads to
 * Plan & billing. Shown only while the subscription is trialing.
 */
export function trialDaysLeft(me: CloudMe | null | undefined, now: number): number | null {
  const s = me?.subscription;
  if (!s || s.status !== "trialing" || !s.trial_end) return null;
  const end = Date.parse(s.trial_end);
  if (Number.isNaN(end)) return null;
  return Math.max(0, Math.ceil((end - now) / 86_400_000));
}

function useTrial(): { days: number | null; end: string | null } {
  const signedIn = useCloudSignedIn();
  const me = useAsync(() => (signedIn ? cloudApi.me().catch(() => null) : Promise.resolve(null)), [signedIn]);
  useEvent(cloudApi.onBillingChanged, me.reload);
  const now = useNow();
  return { days: trialDaysLeft(me.data, now), end: me.data?.subscription?.trial_end ?? null };
}

const openBilling = () => requestPage("account");

/** The sidebar row, while more than 3 days are left. */
export function TrialRow() {
  const t = useT();
  const { days } = useTrial();
  if (days == null || days <= 3) return null;
  return (
    <button type="button" className="nav-item trial-row" onClick={openBilling}>
      <span className="trial-dot" aria-hidden="true" />
      {t("trial.row", { count: days })}
    </button>
  );
}

/** The Home card, for the last 3 days. */
export function TrialCard() {
  const t = useT();
  const { days, end } = useTrial();
  if (days == null || days > 3 || !end) return null;
  return (
    <section className="trial-card" aria-labelledby="trial-card-title">
      <div>
        <h2 id="trial-card-title">{t("trial.card.title", { date: formatDate(end, { weekday: "long", day: "numeric", month: "long" }) })}</h2>
        <p>{t("trial.body")}</p>
      </div>
      <button type="button" className="btn btn-primary" onClick={openBilling}>
        {t("trial.choose")}
      </button>
    </section>
  );
}

const SEEN_KEY = "heed.trial.lastDayShown";

/** The dialog, once, on the last day (under 24 hours left, or already over). */
export function TrialModal() {
  const t = useT();
  const { days, end } = useTrial();
  const today = end ? end.slice(0, 10) : "";
  const [dismissed, setDismissed] = useState(() => {
    try {
      return localStorage.getItem(SEEN_KEY);
    } catch {
      return null;
    }
  });
  if (days == null || days > 1 || !end || dismissed === today) return null;
  const close = () => {
    try {
      localStorage.setItem(SEEN_KEY, today);
    } catch {}
    setDismissed(today);
  };
  return (
    <Dialog title={t("trial.modal.title")} description={t("trial.body")} onClose={close}>
      <div className="dialog-actions">
        <button type="button" className="btn btn-ghost" onClick={close}>
          {t("trial.later")}
        </button>
        <button
          type="button"
          className="btn btn-primary"
          data-autofocus
          onClick={() => {
            close();
            openBilling();
          }}
        >
          {t("trial.choose")}
        </button>
      </div>
    </Dialog>
  );
}

