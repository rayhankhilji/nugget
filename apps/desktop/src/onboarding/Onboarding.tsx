import { useEffect, useReducer, useRef, useState } from "react";
import { api, type PageId } from "../api";
import { useT } from "../i18n";
import { LanguagePicker } from "../i18n/LanguagePicker";
import { Logo } from "../components/Logo";
import { loadState, reduce, saveState, STEPS, stepIndex, type OnboardingState } from "./machine";
import { AiStep } from "./AiStep";
import { TriggerStep } from "./TriggerStep";
import { DoneStep, PermissionsStep, SourceStep, STEP_TITLE, WelcomeStep } from "./steps";
import "./onboarding.css";

/**
 * First-run setup. Shown full-window until finished or skipped; works without the daemon (steps
 * that need it say so and can be skipped). Progress is saved after every change.
 */
export function Onboarding({
  connected,
  onExit,
  initial,
}: {
  /** Whether the background service is reachable (steps that need it adapt). */
  connected: boolean;
  /**
   * Called once the flow is finished or skipped, with the page to open: Search when the user
   * finished with a searchable source, else the app's default.
   */
  onExit: (page?: PageId) => void;
  /** Starting state (tests); defaults to the saved state. */
  initial?: OnboardingState;
}) {
  const t = useT();
  const [state, dispatch] = useReducer(reduce, initial ?? null, (i) => i ?? loadState());
  const headingRef = useRef<HTMLHeadingElement>(null);
  const first = useRef(true);
  const exitTo = useRef<PageId | undefined>(undefined);
  const searchable = useSearchableItems(connected && state.step === "done");

  useEffect(() => {
    saveState(state);
    if (state.finished) onExit(exitTo.current);
  }, [state, onExit]);

  // Move focus to the new step's heading so keyboard and screen-reader users start at the top.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    headingRef.current?.focus();
  }, [state.step]);

  const i = stepIndex(state.step);
  const isLast = state.step === "done";
  const stepProps = { state, dispatch, connected, headingRef };
  // Finishing with something indexed goes straight to Search: the first useful thing to do.
  const toSearch = isLast && searchable > 0;

  return (
    <div className="onboarding">
      <header className="ob-top">
        <div className="brand">
          <Logo />
          <span>{t("onboarding.title")}</span>
        </div>
        <div className="ob-top-actions">
          <LanguagePicker />
          {!isLast && (
            <button type="button" className="btn btn-ghost" onClick={() => dispatch({ type: "skipAll" })}>
              {t("onboarding.skipAll")}
            </button>
          )}
        </div>
      </header>

      <nav className="ob-progress" aria-label={t("onboarding.progressLabel")}>
        <ol>
          {STEPS.map((s, n) => {
            const status = s === state.step ? "current" : state.completed.includes(s) ? "done" : state.skipped.includes(s) ? "skipped" : "todo";
            return (
              <li key={s} className={`ob-step ob-step-${status}`}>
                <button
                  type="button"
                  aria-current={s === state.step ? "step" : undefined}
                  onClick={() => dispatch({ type: "goto", step: s })}
                >
                  <span className="ob-step-num" aria-hidden="true">
                    {n + 1}
                  </span>
                  <span>{t(STEP_TITLE[s])}</span>
                  {status !== "todo" && (
                    <span className="visually-hidden"> ({t(`onboarding.stepStatus.${status}`)})</span>
                  )}
                </button>
              </li>
            );
          })}
        </ol>
      </nav>

      <main className="ob-main" aria-labelledby="ob-heading">
        <p className="muted small ob-count">{t("onboarding.stepOf", { n: i + 1, total: STEPS.length })}</p>
        <div className="ob-panel" key={state.step}>
          {state.step === "welcome" && <WelcomeStep {...stepProps} />}
          {state.step === "ai" && <AiStep {...stepProps} />}
          {state.step === "source" && <SourceStep {...stepProps} />}
          {state.step === "permissions" && <PermissionsStep {...stepProps} />}
          {state.step === "trigger" && <TriggerStep {...stepProps} />}
          {state.step === "done" && <DoneStep {...stepProps} searchable={searchable} />}
        </div>
      </main>

      <footer className="ob-actions">
        {i > 0 && (
          <button type="button" className="btn" onClick={() => dispatch({ type: "back" })}>
            {t("onboarding.back")}
          </button>
        )}
        <span className="ob-spacer" />
        {!isLast && (
          <button type="button" className="btn btn-ghost" onClick={() => dispatch({ type: "skip" })}>
            {t("onboarding.skipStep")}
          </button>
        )}
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => {
            exitTo.current = toSearch ? "search" : undefined;
            dispatch({ type: "next" });
          }}
        >
          {toSearch ? t("onboarding.done.search") : isLast ? t("onboarding.done.start") : t("onboarding.continue")}
        </button>
      </footer>
    </div>
  );
}

/** Items indexed across all sources (0 until known, or while `active` is false). */
function useSearchableItems(active: boolean): number {
  const [n, setN] = useState(0);
  useEffect(() => {
    if (!active) return;
    let alive = true;
    api
      .sourcesList()
      .then((list) => alive && setN(list.reduce((sum, s) => sum + (s.enabled ? s.items : 0), 0)))
      .catch(() => {
        /* unknown: offer the default start page */
      });
    return () => {
      alive = false;
    };
  }, [active]);
  return n;
}
