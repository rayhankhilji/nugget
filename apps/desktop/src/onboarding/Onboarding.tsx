import { useCallback, useEffect, useReducer, useRef, useState, type RefObject } from "react";
import { api, type PageId } from "../api";
import { requestChatAsk } from "../hooks.nav";
import { useT, type MessageKey } from "../i18n";
import { LanguagePicker } from "../i18n/LanguagePicker";
import { Logo } from "../components/Logo";
import { loadState, reduce, saveState, stepGoalMet, STEPS, stepIndex, type OnboardingState, type StepId } from "./machine";
import { AiStep } from "./AiStep";
import { TriggerStep } from "./TriggerStep";
import { DoneStep, PermissionsStep, signInOffered, SourceStep, WelcomeStep } from "./steps";
import { useCloudSession } from "./ConnectApps";
import "./onboarding.css";

/** The three stages the progress bar shows: sign in, connect apps (and the setup after it), first answer. */
const SEGMENTS: { label: MessageKey; steps: StepId[] }[] = [
  { label: "onboarding.progress.signin", steps: ["welcome"] },
  {
    label: "onboarding.progress.connect",
    steps: ["source", "ai", "permissions", "trigger"],
  },
  { label: "onboarding.progress.answer", steps: ["done"] },
];

/**
 * First-run setup: sign in → connect apps → first answer. Shown full-window until finished or
 * skipped; works without the daemon (steps that need it say so and can be skipped) and without an
 * account (the local-first way). Progress is saved after every change.
 */
export function Onboarding({
  connected,
  onExit,
  initial,
}: {
  /** Whether the background service is reachable (steps that need it adapt). */
  connected: boolean;
  /**
   * Called once the flow is finished or skipped, with the page to open: Chat when the user asked
   * a first question, Search when they finished with a searchable source, else the app's default.
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
  const stepRef = useRef(state.step);
  stepRef.current = state.step;
  // A sign-in that finishes while the sign-in step is open moves on to connecting apps.
  const cloud = useCloudSession(
    useCallback(() => {
      if (stepRef.current === "welcome") dispatch({ type: "next" });
    }, []),
  );

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
  const stepProps = { state, dispatch, connected, headingRef, cloud };
  // Finishing with something indexed goes straight to Search: the first useful thing to do.
  const toSearch = isLast && searchable > 0;
  // Continue on a step that set nothing up counts as skipping it; say so next to the button.
  const unmet = !isLast && !stepGoalMet(state);
  // One primary action per step: the step's own (sign in, add a folder, ask) until it is done.
  const withoutAccount = state.step === "welcome" && signInOffered(cloud);
  const primary = !withoutAccount && !isLast && !(state.step === "source" && unmet);
  const seg = SEGMENTS.findIndex((g) => g.steps.includes(state.step));
  const ask = (question: string) => {
    requestChatAsk(question);
    exitTo.current = "chat";
    dispatch({ type: "finish" });
  };

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
          {SEGMENTS.map((g, n) => {
            const status = n === seg ? "current" : n < seg || g.steps.every((s) => state.completed.includes(s)) ? "done" : "todo";
            return (
              <li key={g.label} className={`ob-seg ob-seg-${status}`}>
                <button type="button" aria-current={n === seg ? "step" : undefined} onClick={() => n !== seg && dispatch({ type: "goto", step: g.steps[0]! })}>
                  <span className="ob-seg-bar" aria-hidden="true" />
                  <span className="visually-hidden">
                    {t(g.label)}
                    {status !== "todo" && ` (${t(`onboarding.stepStatus.${status}`)})`}
                  </span>
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
          {state.step === "done" && <DoneStep {...stepProps} searchable={searchable} onAsk={ask} />}
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
          className={primary ? "btn btn-primary" : "btn"}
          aria-describedby={unmet ? "ob-continue-hint" : undefined}
          onClick={() => {
            exitTo.current = toSearch ? "search" : undefined;
            dispatch({ type: "continue" });
          }}
        >
          {toSearch
            ? t("onboarding.done.search")
            : isLast
              ? t("onboarding.done.start")
              : withoutAccount
                ? t("onboarding.signin.withoutAccount")
                : t("onboarding.continue")}
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
      const footer = f.getBoundingClientRect();
      setUnder(m.getBoundingClientRect().bottom - pad > footer.top + 1);
      // Focus scrolling (Tab) keeps the focused control clear of the sticky footer.
      document.documentElement.style.scrollPaddingBlockEnd = `${Math.ceil(footer.height) + 12}px`;
    };
    check();
    window.addEventListener("scroll", check, { passive: true, capture: true });
    window.addEventListener("resize", check);
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(check);
    if (main.current) ro?.observe(main.current);
    // The footer grows when the hint appears or it wraps.
    if (foot.current) ro?.observe(foot.current);
    return () => {
      document.documentElement.style.scrollPaddingBlockEnd = "";
      window.removeEventListener("scroll", check, { capture: true });
      window.removeEventListener("resize", check);
      ro?.disconnect();
    };
  }, [main, foot]);
  return under;
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
