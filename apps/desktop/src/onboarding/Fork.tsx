import { useEffect, useState } from "react";
import { cloudApi, type CloudPlan } from "../cloud";
import { formatNumber, useT, type MessageKey } from "../i18n";
import { StepHeading, type StepProps } from "./steps";

/** The cheapest plan's monthly price in pounds, or `null` when there is no plan with a price. */
export function cheapestMonthly(plans: readonly CloudPlan[] | null | undefined): number | null {
  const prices = (plans ?? []).map((p) => p.monthly_gbp).filter((n) => typeof n === "number" && Number.isFinite(n) && n > 0);
  return prices.length ? Math.min(...prices) : null;
}

/** £39, or £39.50 when the price has pence. */
export function formatGbp(n: number): string {
  const digits = Number.isInteger(n) ? 0 : 2;
  return formatNumber(n, { style: "currency", currency: "GBP", minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/**
 * The Heed Cloud price line from the billing plans (`GET /v1/billing/plans`): never a hard-coded
 * price. `null` while loading, or when there is no price to show (no cloud in this build, offline,
 * or the call failed); the card then says only "14-day free trial".
 */
function useCloudPrice(configured: boolean | undefined): number | null {
  const [price, setPrice] = useState<number | null>(null);
  useEffect(() => {
    if (configured === false) return;
    let alive = true;
    cloudApi
      .plans()
      .then((r) => alive && setPrice(cheapestMonthly(r.plans)))
      .catch(() => {
        /* no price: the trial line alone */
      });
    return () => {
      alive = false;
    };
  }, [configured]);
  return price;
}

const CLOUD_POINTS: MessageKey[] = ["onboarding.fork.cloud.point1", "onboarding.fork.cloud.point2", "onboarding.fork.cloud.point3"];
const LOCAL_POINTS: MessageKey[] = ["onboarding.fork.local.point1", "onboarding.fork.local.point2", "onboarding.fork.local.point3"];

/**
 * The first question on first run: Heed Cloud (sign in, then connect apps) or run it yourself
 * (choose your AI). Nothing is preselected and choosing moves on; going back shows the choice
 * made (`aria-pressed`). Picking Heed Cloud in a build without it says so and stays here.
 */
export function ForkStep({ headingRef, state, dispatch, cloud }: StepProps) {
  const t = useT();
  const configured = cloud?.session?.configured;
  const price = useCloudPrice(configured);
  const unavailable = state.path === "cloud" && configured === false;
  const card = (path: "cloud" | "local", kicker: MessageKey, title: MessageKey, body: MessageKey, points: MessageKey[], foot: string) => (
    <button
      type="button"
      className="ob-fork-card"
      aria-pressed={state.path === path}
      onClick={() => dispatch({ type: "choosePath", path })}
      data-path={path}
    >
      <span className="ob-fork-kicker">
        {t(kicker)}
      </span>
      <h2>{t(title)}</h2>
      <p>{t(body)}</p>
      <ul className="ob-fork-points">
        {points.map((k) => (
          <li key={k}>{t(k)}</li>
        ))}
      </ul>
      <span className="ob-fork-foot">
        {foot}
      </span>
    </button>
  );
  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.fork.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.fork.lead")}</p>
      <div className="ob-fork">
        {card(
          "cloud",
          "onboarding.fork.cloud.kicker",
          "onboarding.fork.cloud.title",
          "onboarding.fork.cloud.body",
          CLOUD_POINTS,
          price === null ? t("onboarding.fork.cloud.trial") : t("onboarding.fork.cloud.price", { price: formatGbp(price) }),
        )}
        {card(
          "local",
          "onboarding.fork.local.kicker",
          "onboarding.fork.local.title",
          "onboarding.fork.local.body",
          LOCAL_POINTS,
          t("onboarding.fork.local.free"),
        )}
      </div>
      {unavailable && (
        <p className="note ob-fork-note" role="status">
          {t("onboarding.fork.cloudUnavailable")}
        </p>
      )}
    </>
  );
}
