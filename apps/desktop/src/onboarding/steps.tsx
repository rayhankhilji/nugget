import { useCallback, useEffect, useRef, useState, type Dispatch, type FormEvent, type ReactNode, type RefObject } from "react";
import { api, doubletapApi, events, needsFullDiskAccess, type ConnectorKind, type DoubleTapStatus, type SourceInfo } from "../api";
import { useEvent } from "../hooks";
import { formatList, Trans, useT, type MessageKey } from "../i18n";
import { AddSourceWizard } from "../components/AddSourceWizard";
import { FullDiskAccessSteps } from "../sources/MessagingForm";
import { Announce, useAnnouncer } from "../components/Announce";
import { CopyButton } from "../components/CopyButton";
import { UpdateQuestion } from "../components/UpdateChoice";
import { legacyDone, pendingSteps, type Action, type OnboardingState, type StepId } from "./machine";
import { indexPhase, isSettled, SkippedSummary, type IndexPhase } from "../sources/scan";
import { detectOs, type Os } from "./platform";
import { TriggerKeys } from "./TriggerStep";
import { loadTrigger, readTrigger } from "./trigger";
import { detectAiSetup } from "./detect";
import { DesktopOptIns } from "../platform/DesktopOptIns";
import { MacPermissionRecovery, missingPanes, useNeedsFullDiskAccess, useRecheckOnReturn } from "../platform/MacPermissions";
import { cloudApi, type SignInProvider } from "../cloud";
import { ErrorNote, Spinner } from "../components/ui";
import { ConnectApps, type CloudState } from "./ConnectApps";
import { ForkStep } from "./Fork";

export interface StepProps {
  state: OnboardingState;
  dispatch: Dispatch<Action>;
  connected: boolean;
  headingRef: RefObject<HTMLHeadingElement | null>;
  /** The Heed account: signed in, signed out, or no cloud in this build (`session.configured`). */
  cloud?: CloudState;
}

/** Signed in to a Heed account (cloud app connections work). */
export function signedIn(cloud: CloudState | undefined): boolean {
  return cloud?.session?.configured === true && cloud.session.signed_in;
}

/** A Heed account is offered but not signed in to: the cloud path asks to sign in. */
export function signInOffered(cloud: CloudState | undefined): boolean {
  return cloud?.session?.configured === true && !cloud.session.signed_in;
}

/** The first step shows the fork (Heed Cloud or run it yourself), not a sign-in or a greeting. */
export function forkShown(state: OnboardingState, cloud: CloudState | undefined): boolean {
  if (signedIn(cloud) || cloud?.session === null) return false;
  return !(state.path === "cloud" && signInOffered(cloud));
}

/** ChatGPT sign-in is for running Heed yourself: not on the cloud path, nor signed in to Heed Cloud. */
export function localMode(state: OnboardingState, cloud: CloudState | undefined): boolean {
  return state.path !== "cloud" && !signedIn(cloud);
}

export function StepHeading({ headingRef, children }: { headingRef: StepProps["headingRef"]; children: ReactNode }) {
  return (
    <h1 id="ob-heading" ref={headingRef} tabIndex={-1}>
      {children}
    </h1>
  );
}

export function OfflineNote() {
  const t = useT();
  return (
    <p className="note" role="status">
      {t("onboarding.offline")}
    </p>
  );
}

const STRONG = { strong: (c: ReactNode) => <strong>{c}</strong> };

// ---- 1. sign in ----

/**
 * The first step: the fork (Heed Cloud or run it yourself). On the cloud path, with a Heed account
 * on offer, it is the sign-in (Google in the browser or an emailed link, as on the Connections
 * page); the footer then offers "Continue without an account", the run-it-yourself way. Signed
 * in, it greets the account.
 */
export function WelcomeStep(props: StepProps) {
  const { headingRef, cloud, state } = props;
  const t = useT();
  if (state.path === "cloud" && signInOffered(cloud)) return <SignInPanel headingRef={headingRef} failure={cloud?.failure ?? null} />;
  if (signedIn(cloud)) {
    const email = cloud?.session?.email;
    return (
      <>
        <StepHeading headingRef={headingRef}>{t("onboarding.welcome.heading")}</StepHeading>
        <p className="ob-lead" role="status">
          {email ? t("cloud.signedInAs", { email }) : t("cloud.signedIn")}
        </p>
      </>
    );
  }
  if (cloud?.session === null) {
    // Checking for an account: don't flash the local welcome before the sign-in.
    return (
      <>
        <StepHeading headingRef={headingRef}>{t("onboarding.welcome.heading")}</StepHeading>
        <Spinner label={t("cloud.loading")} />
      </>
    );
  }
  return <ForkStep {...props} />;
}

type Waiting = { kind: "browser" } | { kind: "email"; email: string } | null;

/**
 * Sign in to Heed, the Connections page's sign-in (`pages/SignIn.tsx`) laid out as a setup step:
 * the step's own heading, one primary button. The sign-in finishes when the link comes back to
 * Heed; onboarding then moves on by itself.
 */
function SignInPanel({ headingRef, failure }: { headingRef: StepProps["headingRef"]; failure: string | null }) {
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
  const sendLink = (e: FormEvent) => {
    e.preventDefault();
    const address = email.trim();
    if (!address) return;
    void run("email", () => cloudApi.signInEmail(address), {
      kind: "email",
      email: address,
    });
  };

  return (
    <>
      <StepHeading headingRef={headingRef}>{t("cloud.signIn.title")}</StepHeading>
      <p className="ob-lead">{t("cloud.signIn.description")}</p>
      {failure && (
        <p className="note note-error" role="alert">
          {t("cloud.signIn.failed", { error: failure })}
        </p>
      )}
      <ErrorNote error={error} />
      <div className="ob-signin">
        <button
          type="button"
          className="btn btn-primary ob-signin-google"
          disabled={busy !== null}
          onClick={() =>
            void run("google", () => cloudApi.signInWith("google"), {
              kind: "browser",
            })
          }
        >
          {busy === "google" ? <Spinner /> : t("cloud.signIn.google")}
        </button>
        <form className="ob-signin-email" onSubmit={sendLink}>
          <label htmlFor="ob-email">{t("cloud.signIn.email")}</label>
          <div className="ob-signin-row">
            <input id="ob-email" type="email" autoComplete="email" required value={email} onChange={(e) => setEmail(e.target.value)} disabled={busy !== null} />
            <button type="submit" className="btn" disabled={busy !== null || !email.trim()}>
              {busy === "email" ? <Spinner /> : t("cloud.signIn.sendLink")}
            </button>
          </div>
        </form>
        <div role="status">
          {waiting && <p className="note">{waiting.kind === "email" ? t("cloud.signIn.checkEmail", { email: waiting.email }) : t("cloud.signIn.inBrowser")}</p>}
        </div>
      </div>
      <p className="muted small ob-signin-local">{t("onboarding.signin.localNote")}</p>
    </>
  );
}

// ---- 2. connect apps (or a first local source) ----

/** How often the source step re-reads `sources.list` while a new source indexes. */
export const INDEX_POLL_MS = 1000;

export { indexPhase, type IndexPhase };

/**
 * Signed in: a grid of one-click cloud app connections, and below it the local folder and other
 * sources ("Or add a folder on this computer"). Without an account: the local sources, with a
 * way back to sign in when an account is on offer.
 */
export function SourceStep({ headingRef, connected, dispatch, cloud }: StepProps) {
  const t = useT();
  const [open, setOpen] = useState<ConnectorKind | "pick" | null>(null);
  // Every source there is, not only those added on this visit: going Back, quitting and reopening,
  // or running setup again shows the folder added before, with its progress.
  const [added, setAdded] = useState<SourceInfo[]>([]);
  // Sources added on this visit: their progress is announced; earlier ones are just shown.
  const [fresh, setFresh] = useState<string[]>([]);
  const [announcement, announce] = useAnnouncer();

  useEffect(() => {
    if (!connected) return;
    let alive = true;
    api
      .sourcesList()
      .then((list) => {
        if (!alive) return;
        const shown = list.filter((s) => s.enabled);
        setAdded((cur) => [...shown, ...cur.filter((c) => !shown.some((s) => s.id === c.id))]);
        if (list.length > 0) dispatch({ type: "sourcesFound", names: list.map((s) => s.name) });
        if (list.some((s) => s.items > 0)) dispatch({ type: "sourcesIndexed" });
      })
      .catch(() => {
        /* unknown: adding one here still counts */
      });
    return () => {
      alive = false;
    };
  }, [connected, dispatch]);

  const apps = signedIn(cloud);
  return (
    <>
      <StepHeading headingRef={headingRef}>{t(apps ? "onboarding.connect.heading" : "onboarding.source.heading")}</StepHeading>
      <p className="ob-lead">{t(apps ? "onboarding.connect.lead" : "onboarding.source.lead")}</p>
      {apps && <ConnectApps dispatch={dispatch} announce={announce} />}
      {signInOffered(cloud) && (
        <div className="ob-signin-first">
          <p>{t("onboarding.connect.signInFirst")}</p>
          <button type="button" className="btn btn-small" onClick={() => dispatch({ type: "choosePath", path: "cloud" })}>
            {t("sources.signIn")}
          </button>
        </div>
      )}
      <div className="ob-source-local">
        {apps && <h2 className="ob-local-heading">{t("onboarding.connect.local")}</h2>}
        {connected ? (
          <div className="row ob-row-start">
            {/* A folder is the quickest first source: open its form directly, skipping the kind picker. */}
            <button type="button" className={added.length || apps ? "btn" : "btn btn-primary"} onClick={() => setOpen("files")}>
              {added.length ? t("onboarding.source.addAnother") : t("onboarding.source.addFolder")}
            </button>
            <button type="button" className="btn btn-ghost" onClick={() => setOpen("pick")}>
              {t("onboarding.source.addOther")}
            </button>
          </div>
        ) : (
          <OfflineNote />
        )}
        {added.length > 0 && <IndexProgress added={added} fresh={fresh} announce={announce} dispatch={dispatch} />}
        <p className="muted">{t("onboarding.source.later")}</p>
      </div>
      <Announce message={announcement} />
      {open && (
        <AddSourceWizard
          initialKind={open === "pick" ? undefined : open}
          onClose={() => setOpen(null)}
          onAdded={(s) => {
            setAdded((list) => [...list.filter((x) => x.id !== s.id), s]);
            setFresh((ids) => (ids.includes(s.id) ? ids : [...ids, s.id]));
            dispatch({ type: "sourcesFound", names: [s.name] });
            announce(t("onboarding.source.addedAnnounce", { name: s.name }));
          }}
        />
      )}
    </>
  );
}

const PHASE_KEY: Record<IndexPhase, MessageKey> = {
  waiting: "onboarding.source.waiting",
  indexing: "onboarding.source.indexing",
  ready: "onboarding.source.ready",
  failed: "onboarding.source.failed",
  empty: "onboarding.source.empty",
  denied: "onboarding.source.denied",
  missing: "onboarding.source.missing",
};

/** The line for a source: its phase, with the scan's total when it is known. */
function phaseText(t: ReturnType<typeof useT>, s: SourceInfo, phase: IndexPhase): string {
  const total = s.scan?.finished ? s.scan.found : 0;
  const args = { name: s.name, count: s.items, total, error: s.last_error ?? "" };
  if (phase === "indexing" && total > 0) return t("onboarding.source.indexingOf", args);
  if (phase === "ready" && total > s.items) return t("onboarding.source.readyPartial", args);
  return t(PHASE_KEY[phase], args);
}

/**
 * Live indexing progress for the sources there are, so the first run shows that something is
 * happening and says when search starts to work, or why it won't. Polls `sources.list` (and
 * refreshes on `sources.changed`) until every source is settled. The list is not a live region
 * (its counts change every second); each new source's start and end are announced.
 */
function IndexProgress({
  added,
  fresh,
  announce,
  dispatch,
}: {
  added: SourceInfo[];
  fresh: string[];
  announce: (text: string) => void;
  dispatch: Dispatch<Action>;
}) {
  const t = useT();
  const [live, setLive] = useState<Record<string, SourceInfo>>({});
  const ids = added.map((s) => s.id).join("\u0000");
  const current = added.map((s) => live[s.id] ?? s);
  const settled = current.every((s) => isSettled(indexPhase(s)));
  const anyIndexed = current.some((s) => s.items > 0);
  const [tick, setTick] = useState(0);

  useEvent(events.onSourcesChanged, () => setTick((n) => n + 1));

  useEffect(() => {
    if (anyIndexed) dispatch({ type: "sourcesIndexed" });
  }, [anyIndexed, dispatch]);

  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const poll = async () => {
      try {
        const list = await api.sourcesList();
        if (!alive) return;
        setLive((cur) => {
          const next = { ...cur };
          for (const s of list) if (ids.split("\u0000").includes(s.id)) next[s.id] = s;
          return next;
        });
      } catch {
        /* keep the last answer; the next poll tries again */
      }
      if (alive && !settled) timer = window.setTimeout(() => void poll(), INDEX_POLL_MS);
    };
    void poll();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [ids, settled, tick]);

  // The last phase announced per source: "added" is said by the step, then indexing and the end.
  // Sources from before this visit are not news: their phase is recorded without a word.
  const announced = useRef(new Map<string, IndexPhase>());
  useEffect(() => {
    for (const s of current) {
      const phase = indexPhase(s);
      if (!fresh.includes(s.id)) {
        announced.current.set(s.id, phase);
        continue;
      }
      const before = announced.current.get(s.id) ?? "waiting";
      // A later pass over a finished source is not news here.
      if (phase === before || phase === "waiting" || (phase === "indexing" && before !== "waiting")) continue;
      announced.current.set(s.id, phase);
      announce(phase === "indexing" ? t("announce.indexing", { name: s.name }) : phaseText(t, s, phase));
    }
  });

  const nothing = settled && current.every((s) => s.items === 0);
  return (
    <>
      <ul className="ob-index" aria-busy={!settled}>
        {current.map((s) => {
          const phase = indexPhase(s);
          const bad = phase === "failed" || phase === "empty" || phase === "denied" || phase === "missing";
          return (
            <li key={s.id} className={`note ob-index-${phase}${bad ? " note-error" : ""}`} data-phase={phase}>
              {!isSettled(phase) && <span className="spinner" aria-hidden="true" />}{" "}
              {phaseText(t, s, phase)}
              {phase === "failed" && s.kind === "imessage" && needsFullDiskAccess(s.last_error) && <FullDiskAccessSteps />}
              {(phase === "empty" || phase === "ready") && <SkippedSummary scan={s.scan} />}
            </li>
          );
        })}
      </ul>
      {nothing && <p className="note">{t("onboarding.source.nothingNext")}</p>}
    </>
  );
}

// ---- 4. permissions ----

// i18n-ignore-block: a shell command is code, not prose.
const GNOME_A11Y_COMMAND = "gsettings set org.gnome.desktop.interface toolkit-accessibility true";
// i18n-ignore-end

/** A command on its own line: left-to-right in every locale, never broken mid-token, with a copy button. */
export function CommandSnippet({ text, what }: { text: string; what: string }) {
  return (
    <div className="ob-snippet" dir="ltr">
      <pre tabIndex={0} aria-label={what}>
        {text}
      </pre>
      <CopyButton text={text} what={what} />
    </div>
  );
}

const OS_LABEL: Record<Exclude<Os, "other">, MessageKey> = {
  macos: "onboarding.perm.os.macos",
  windows: "onboarding.perm.os.windows",
  linux: "onboarding.perm.os.linux",
};

function PermissionList({ os }: { os: Exclude<Os, "other"> }) {
  const t = useT();
  switch (os) {
    case "macos":
      return (
        <ul className="ob-perm-list">
          <li>
            <Trans k="onboarding.perm.mac.ax" tags={STRONG} />
          </li>
          <li>
            <Trans k="onboarding.perm.mac.input" tags={STRONG} />
          </li>
          <li>{t("onboarding.perm.mac.revoke")}</li>
        </ul>
      );
    case "windows":
      return (
        <ul className="ob-perm-list">
          <li>{t("onboarding.perm.win.uia")}</li>
          <li>{t("onboarding.perm.win.admin")}</li>
        </ul>
      );
    case "linux":
      return (
        <ul className="ob-perm-list">
          <li>
            <Trans k="onboarding.perm.linux.atspiShort" tags={STRONG} />
            <CommandSnippet text={GNOME_A11Y_COMMAND} what={t("onboarding.perm.linux.copyCommand")} />
          </li>
          <li>{t("onboarding.perm.linux.wayland")}</li>
          <li>{t("onboarding.perm.linux.flatpak")}</li>
        </ul>
      );
  }
}

/**
 * On a Mac: what is on right now, and a button to the System Settings pane for each permission
 * that is off (Full Disk Access only once a Messages source is set up). Looks again when the user
 * comes back from System Settings. Nothing is shown when the shell can't say (a browser preview).
 */
export function MacPermissionStatus({ onGranted }: { onGranted?: () => void } = {}) {
  const t = useT();
  const [status, setStatus] = useState<DoubleTapStatus>();
  const wasMissing = useRef(false);
  const [tick, setTick] = useState(0);
  const recheck = useCallback(() => setTick((n) => n + 1), []);
  useEffect(() => {
    let alive = true;
    doubletapApi
      .status()
      .then((s) => alive && setStatus(s))
      .catch(() => {
        /* keep the last answer */
      });
    return () => {
      alive = false;
    };
  }, [tick]);
  const needFda = useNeedsFullDiskAccess();
  const panes = missingPanes(status, needFda);
  useRecheckOnReturn(recheck, panes.length > 0);
  // The step polls for the grant; once what was missing is on, it moves on by itself.
  useEffect(() => {
    if (status?.platform !== "macos") return;
    if (panes.length > 0) wasMissing.current = true;
    else if (wasMissing.current) {
      wasMissing.current = false;
      onGranted?.();
    }
  }, [status, panes.length, onGranted]);
  if (status?.platform !== "macos") return null;
  return (
    <>
      {needFda && (
        <p>
          <Trans k="onboarding.perm.mac.fda" tags={STRONG} />
        </p>
      )}
      {panes.length > 0 ? (
        <MacPermissionRecovery onCheck={recheck} panes={panes} firstRun />
      ) : (
        <p className="note" role="status">
          {t("onboarding.perm.mac.allSet")}
        </p>
      )}
    </>
  );
}

export function PermissionsStep({ headingRef, dispatch }: StepProps & { os?: Os }) {
  const t = useT();
  const detected = detectOs();
  const mine = detected === "other" ? null : detected;
  const others = (Object.keys(OS_LABEL) as Exclude<Os, "other">[]).filter((o) => o !== mine);
  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.perm.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.perm.lead")}</p>
      {mine && (
        <section className="ob-card" aria-labelledby="ob-perm-mine">
          <h2 id="ob-perm-mine">{t("onboarding.perm.yourSystem", { os: t(OS_LABEL[mine]) })}</h2>
          <PermissionList os={mine} />
          {mine === "macos" && <MacPermissionStatus onGranted={() => dispatch({ type: "next" })} />}
        </section>
      )}
      <details className="ob-details" open={!mine}>
        <summary>{t("onboarding.perm.otherSystems")}</summary>
        {others.map((o) => (
          <section key={o} aria-labelledby={`ob-perm-${o}`}>
            {/* Under "your system" (h2) when it is known; otherwise directly under the step's h1. */}
            <OtherHeading id={`ob-perm-${o}`} level={mine ? 3 : 2}>
              {t(OS_LABEL[o])}
            </OtherHeading>
            <PermissionList os={o} />
          </section>
        ))}
      </details>
      <p className="muted">{t("onboarding.perm.scope")}</p>
    </>
  );
}

function OtherHeading({ id, level, children }: { id: string; level: 2 | 3; children: ReactNode }) {
  return level === 2 ? <h2 id={id}>{children}</h2> : <h3 id={id}>{children}</h3>;
}

// ---- 6. first answer ----

export const STEP_TITLE: Record<StepId, MessageKey> = {
  welcome: "onboarding.step.welcome",
  ai: "onboarding.step.ai",
  source: "onboarding.step.source",
  permissions: "onboarding.step.permissions",
  trigger: "onboarding.step.trigger",
  done: "onboarding.step.done",
};

const SUGGESTIONS = ["onboarding.answer.suggestion1", "onboarding.answer.suggestion2", "onboarding.answer.suggestion3"] as const;

/**
 * The first question: a composer that hands the question to Chat (as quick ask's "Open in Chat"
 * does), with a few to try. Below it, what setup did and what is left.
 */
function FirstAnswer({ onAsk }: { onAsk: (question: string) => void }) {
  const t = useT();
  const [question, setQuestion] = useState("");
  const input = useRef<HTMLTextAreaElement>(null);
  const submit = (e?: FormEvent) => {
    e?.preventDefault();
    const q = question.trim();
    if (q) onAsk(q);
  };
  return (
    <>
      <form className="ob-ask" onSubmit={submit}>
        <label htmlFor="ob-question" className="visually-hidden">
          {t("quickAsk.inputLabel")}
        </label>
        <textarea
          id="ob-question"
          ref={input}
          rows={2}
          value={question}
          placeholder={t("quickAsk.placeholder")}
          onChange={(e) => setQuestion(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) submit(e);
          }}
          dir="auto"
        />
        <button type="submit" className="btn btn-primary ob-ask-send" disabled={!question.trim()}>
          {t("quickAsk.ask")}
        </button>
      </form>
      <ul className="ob-suggestions" aria-label={t("onboarding.answer.suggestionsLabel")}>
        {SUGGESTIONS.map((k) => (
          <li key={k}>
            <button
              type="button"
              className="ob-suggestion"
              onClick={() => {
                setQuestion(t(k));
                input.current?.focus();
              }}
            >
              {t(k)}
            </button>
          </li>
        ))}
      </ul>
    </>
  );
}

export function DoneStep({
  headingRef,
  state,
  dispatch,
  connected,
  searchable = 0,
  onAsk,
}: StepProps & { searchable?: number; onAsk: (question: string) => void }) {
  const t = useT();
  const pending = pendingSteps(state);
  const withAi = state.ai === "local" || state.ai === "byok" || state.ai === "chatgpt";
  const os = detectOs();
  // The shell's trigger is what Double Tap runs with; the local copy is only a first-render cache.
  const [trigger, setTrigger] = useState(() => loadTrigger(os));
  useEffect(() => {
    let alive = true;
    readTrigger(os)
      .then((p) => alive && setTrigger(p))
      .catch(() => {
        /* keep the cached copy */
      });
    return () => {
      alive = false;
    };
  }, [os]);

  // The summary follows what is really there (a key saved or a model installed before, sources
  // added earlier), not only what this run of setup recorded.
  const { ai: choice, aiSetup } = state;
  const knownSources = (state.sources?.length ?? 0) > 0;
  const indexed = state.indexed === true;
  useEffect(() => {
    if (!connected) return;
    let alive = true;
    if ((choice === "local" || choice === "byok" || choice === "chatgpt") && aiSetup?.ai !== choice) {
      void detectAiSetup(choice).then((what) => alive && what && dispatch({ type: "aiSetUp", ai: choice, what }));
    }
    if (!knownSources || !indexed) {
      api
        .sourcesList()
        .then((list) => {
          if (!alive) return;
          if (list.length > 0) dispatch({ type: "sourcesFound", names: list.map((s) => s.name) });
          if (list.some((s) => s.items > 0)) dispatch({ type: "sourcesIndexed" });
        })
        .catch(() => {});
    }
    return () => {
      alive = false;
    };
  }, [connected, choice, aiSetup?.ai, knownSources, indexed, dispatch]);

  const aiReady = withAi && aiSetup?.ai === state.ai ? aiSetup : null;
  // Progress saved before setups were recorded: the step was completed, so it was set up.
  const aiLegacy = withAi && !aiReady && legacyDone(state, "ai");
  const cloudApps = state.cloudApps ?? [];
  const sources = [...cloudApps, ...(state.sources ?? [])];
  const sourcesLegacy = sources.length === 0 && legacyDone(state, "source");
  // Sources were added but none has anything searchable (an empty folder, only unsupported,
  // unreadable or too-large files) and no cloud app is connected: say so, and don't claim search
  // works or every step is done.
  const nothingIndexed = sources.length > 0 && cloudApps.length === 0 && !indexed && searchable === 0;
  const pendingShown = nothingIndexed ? pending.filter((s) => s !== "source") : pending;
  const ai = aiReady
    ? aiReady.ai === "local"
      ? t("onboarding.done.summary.aiLocal", { model: aiReady.what })
      : aiReady.ai === "chatgpt"
        ? t("onboarding.done.summary.aiChatgpt")
        : t("onboarding.done.summary.aiByok", { provider: aiReady.what })
    : aiLegacy
      ? t(state.ai === "local" ? "onboarding.done.summary.aiLocalPlain" : "onboarding.done.summary.aiByokPlain")
      : withAi
        ? t("onboarding.done.summary.aiNotSetUp")
        : t("onboarding.done.summary.aiLater");
  const aiSet = aiReady !== null || aiLegacy;
  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.answer.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.answer.lead")}</p>
      <FirstAnswer onAsk={onAsk} />
      <section className="ob-setup" aria-labelledby="ob-setup-heading">
        <h2 id="ob-setup-heading">{t(nothingIndexed ? "onboarding.done.headingNothingIndexed" : "onboarding.done.heading")}</h2>
        <p>{t(nothingIndexed ? "onboarding.done.leadNothingIndexed" : aiSet ? "onboarding.done.lead" : "onboarding.done.leadNoAi")}</p>
        {nothingIndexed && (
          <p>
            <button type="button" className="btn btn-small" onClick={() => dispatch({ type: "goto", step: "source" })}>
              {t("onboarding.source.addAnother")}
            </button>
          </p>
        )}
        {searchable > 0 && (
          <p className="note ob-index-ready" role="status">
            {t("onboarding.done.searchable", { count: searchable })}
          </p>
        )}
        <dl className="ob-summary">
          <div className={aiSet ? "ob-summary-set" : "ob-summary-unset"}>
            <dt>{t("onboarding.done.summary.ai")}</dt>
            <dd>{ai}</dd>
          </div>
          <div className={(sources.length && !nothingIndexed) || sourcesLegacy ? "ob-summary-set" : "ob-summary-unset"}>
            <dt>{t("onboarding.done.summary.sources")}</dt>
            <dd>
              {sources.length ? (
                <>
                  <bdi>{formatList(sources)}</bdi>
                  {nothingIndexed && <span className="muted"> · {t("onboarding.done.summary.nothingIndexed")}</span>}
                </>
              ) : sourcesLegacy ? (
                t("onboarding.done.summary.sourcesAdded")
              ) : (
                t("onboarding.done.summary.sourcesNone")
              )}
            </dd>
          </div>
          <div className="ob-summary-set">
            <dt>{t("onboarding.done.summary.trigger")}</dt>
            <dd>
              <TriggerKeys pref={trigger} os={os} />
            </dd>
          </div>
        </dl>
        <DesktopOptIns />
        {pendingShown.length > 0 ? (
          <>
            <p>{t("onboarding.done.pending", { count: pendingShown.length })}</p>
            <ul className="ob-pending">
              {pendingShown.map((s) => (
                <li key={s}>
                  <button type="button" className="btn btn-small" onClick={() => dispatch({ type: "goto", step: s })}>
                    {t(STEP_TITLE[s])}
                  </button>
                </li>
              ))}
            </ul>
          </>
        ) : (
          !nothingIndexed && <p>{t("onboarding.done.allDone")}</p>
        )}
        <UpdateQuestion />
        <p className="muted">{t("onboarding.done.again")}</p>
      </section>
    </>
  );
}
