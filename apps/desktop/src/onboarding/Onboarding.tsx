import { useEffect, useReducer, useRef, useState, type RefObject } from "react";
import { api, type PageId } from "../api";
import { useT } from "../i18n";
import { LanguagePicker } from "../i18n/LanguagePicker";
import { Logo } from "../components/Logo";
import { loadState, reduce, saveState, stepGoalMet, STEPS, stepIndex, type OnboardingState } from "./machine";
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
  const mainRef = useRef<HTMLElement>(null);
  const footRef = useRef<HTMLElement>(null);
  const stuck = useContentUnder(mainRef, footRef);

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
  // Continue on a step that set nothing up counts as skipping it; say so next to the button.
  const unmet = !isLast && !stepGoalMet(state);

  return (
    <div className="onboarding">
      <header className="ob-top">
        <div className="brand">
          <Logo />
          <span>{t("onboarding.title")}</span>
        </div>
        <div className="ob-top-actions">
          <LanguagePicker />
          {/* Hidden, not removed, on the last step so the header and stepper don't shift. */}
          <button
            type="button"
            className={isLast ? "btn btn-ghost ob-invisible" : "btn btn-ghost"}
            onClick={() => dispatch({ type: "skipAll" })}
          >
            {t("onboarding.skipAll")}
          </button>
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
                    {status === "done" ? <StepGlyph kind="done" /> : status === "skipped" ? <StepGlyph kind="skipped" /> : n + 1}
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

      <main className="ob-main" aria-labelledby="ob-heading" ref={mainRef}>
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

      <footer className={stuck ? "ob-actions ob-actions-stuck" : "ob-actions"} ref={footRef}>
        {i > 0 && (
          <button type="button" className="btn" onClick={() => dispatch({ type: "back" })}>
            {t("onboarding.back")}
          </button>
        )}
        <span className="ob-spacer" />
        {unmet && (
          <span className="muted small ob-hint" id="ob-continue-hint">
            {t("onboarding.continueSkips")}
          </span>
        )}
        {!isLast && state.step !== "welcome" && (
          <button type="button" className="btn btn-ghost" onClick={() => dispatch({ type: "skip" })}>
            {t("onboarding.skipStep")}
          </button>
        )}
        <button
          type="button"
          className="btn btn-primary"
          aria-describedby={unmet ? "ob-continue-hint" : undefined}
          onClick={() => {
            exitTo.current = toSearch ? "search" : undefined;
            dispatch({ type: "continue" });
          }}
        >
          {toSearch ? t("onboarding.done.search") : isLast ? t("onboarding.done.start") : t("onboarding.continue")}
        </button>
      </footer>
    </div>
  );
}

/** Whether the step's content runs on under the sticky footer (then the footer casts a shadow). */
function useContentUnder(main: RefObject<HTMLElement | null>, foot: RefObject<HTMLElement | null>): boolean {
  const [under, setUnder] = useState(false);
  useEffect(() => {
    const check = () => {
      const m = main.current;
      const f = foot.current;
      if (!m || !f) return;
      // The main area's padding is breathing room, not content.
      const pad = parseFloat(getComputedStyle(m).paddingBlockEnd) || 0;
      setUnder(m.getBoundingClientRect().bottom - pad > f.getBoundingClientRect().top + 1);
    };
    check();
    window.addEventListener("scroll", check, { passive: true, capture: true });
    window.addEventListener("resize", check);
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(check);
    if (main.current) ro?.observe(main.current);
    return () => {
      window.removeEventListener("scroll", check, { capture: true });
      window.removeEventListener("resize", check);
      ro?.disconnect();
    };
  }, [main, foot]);
  return under;
}

/** A check for a done step, a dash for a skipped one (the step's number otherwise). */
function StepGlyph({ kind }: { kind: "done" | "skipped" }) {
  return (
    <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      {kind === "done" ? <path d="M2.5 6.5 5 9l4.5-5.5" /> : <path d="M3 6h6" />}
    </svg>
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
