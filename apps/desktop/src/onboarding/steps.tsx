import { useEffect, useState, type Dispatch, type ReactNode, type RefObject } from "react";
import { api, events, type ConnectorKind, type SourceInfo } from "../api";
import { useEvent } from "../hooks";
import { Trans, useT, type MessageKey } from "../i18n";
import { LanguagePicker } from "../i18n/LanguagePicker";
import { AddSourceWizard } from "../components/AddSourceWizard";
import { pendingSteps, type Action, type OnboardingState, type StepId } from "./machine";
import { detectOs, type Os } from "./platform";

export interface StepProps {
  state: OnboardingState;
  dispatch: Dispatch<Action>;
  connected: boolean;
  headingRef: RefObject<HTMLHeadingElement | null>;
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
const CODE = { code: (c: ReactNode) => <code>{c}</code> };

// ---- 1. welcome ----

export function WelcomeStep({ headingRef }: StepProps) {
  const t = useT();
  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.welcome.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.welcome.lead")}</p>
      <div className="ob-columns">
        <section className="ob-card" aria-labelledby="ob-stays">
          <h2 id="ob-stays">{t("onboarding.welcome.staysHeading")}</h2>
          <ul>
            <li>{t("onboarding.welcome.stays1")}</li>
            <li>{t("onboarding.welcome.stays2")}</li>
            <li>{t("onboarding.welcome.stays3")}</li>
          </ul>
        </section>
        <section className="ob-card" aria-labelledby="ob-leaves">
          <h2 id="ob-leaves">{t("onboarding.welcome.leavesHeading")}</h2>
          <ul>
            <li>{t("onboarding.welcome.leaves1")}</li>
            <li>{t("onboarding.welcome.leaves2")}</li>
            <li>{t("onboarding.welcome.leaves3")}</li>
          </ul>
        </section>
      </div>
      <p>{t("onboarding.welcome.ledger")}</p>
      <p className="muted">{t("onboarding.welcome.time")}</p>
      <LanguagePicker className="field-inline ob-language" />
    </>
  );
}

// ---- 3. first source ----

/** How often the source step re-reads `sources.list` while a new source indexes. */
export const INDEX_POLL_MS = 1000;

/** Where a just-added source is: not started, indexing, searchable, or failed. */
export type IndexPhase = "waiting" | "indexing" | "ready" | "failed";

export function indexPhase(s: Pick<SourceInfo, "state" | "items" | "last_error">): IndexPhase {
  if (s.state === "error" || (s.last_error && s.items === 0)) return "failed";
  if (s.state === "syncing") return s.items > 0 ? "indexing" : "waiting";
  return s.items > 0 ? "ready" : "waiting";
}

export function SourceStep({ headingRef, connected }: StepProps) {
  const t = useT();
  const [open, setOpen] = useState<ConnectorKind | "pick" | null>(null);
  const [added, setAdded] = useState<SourceInfo[]>([]);
  const [announce, setAnnounce] = useState("");

  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.source.heading")}</StepHeading>
      <p className="ob-lead">{t("onboarding.source.lead")}</p>
      {connected ? (
        <div className="row ob-row-start">
          {/* A folder is the quickest first source: open its form directly, skipping the kind picker. */}
          <button type="button" className={added.length ? "btn" : "btn btn-primary"} onClick={() => setOpen("files")}>
            {added.length ? t("onboarding.source.addAnother") : t("onboarding.source.addFolder")}
          </button>
          <button type="button" className="btn btn-ghost" onClick={() => setOpen("pick")}>
            {t("onboarding.source.addOther")}
          </button>
        </div>
      ) : (
        <OfflineNote />
      )}
      {added.length > 0 && <IndexProgress added={added} />}
      <p className="muted">{t("onboarding.source.later")}</p>
      <div className="visually-hidden" aria-live="polite">
        {announce}
      </div>
      {open && (
        <AddSourceWizard
          initialKind={open === "pick" ? undefined : open}
          onClose={() => setOpen(null)}
          onAdded={(s) => {
            setAdded((list) => [...list.filter((x) => x.id !== s.id), s]);
            setAnnounce(t("onboarding.source.addedAnnounce", { name: s.name }));
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
};

/**
 * Live indexing progress for the sources added in this step, so the first run shows that
 * something is happening and says when search starts to work. Polls `sources.list` (and
 * refreshes on `sources.changed`) until every added source is searchable or failed.
 */
function IndexProgress({ added }: { added: SourceInfo[] }) {
  const t = useT();
  const [live, setLive] = useState<Record<string, SourceInfo>>({});
  const ids = added.map((s) => s.id).join("\u0000");
  const current = added.map((s) => live[s.id] ?? s);
  const settled = current.every((s) => {
    const p = indexPhase(s);
    return p === "ready" || p === "failed";
  });
  const [tick, setTick] = useState(0);

  useEvent(events.onSourcesChanged, () => setTick((n) => n + 1));

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

  return (
    <ul className="ob-index" aria-live="polite">
      {current.map((s) => {
        const phase = indexPhase(s);
        return (
          <li key={s.id} className={`note ob-index-${phase}${phase === "failed" ? " note-error" : ""}`} data-phase={phase}>
            {phase !== "ready" && phase !== "failed" && <span className="spinner" aria-hidden="true" />}{" "}
            {t(PHASE_KEY[phase], { name: s.name, count: s.items, error: s.last_error ?? "" })}
          </li>
        );
      })}
    </ul>
  );
}

// ---- 4. permissions ----

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
            <Trans k="onboarding.perm.linux.atspi" tags={{ ...STRONG, ...CODE }} />
          </li>
          <li>{t("onboarding.perm.linux.wayland")}</li>
          <li>{t("onboarding.perm.linux.flatpak")}</li>
        </ul>
      );
  }
}

export function PermissionsStep({ headingRef }: StepProps & { os?: Os }) {
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

// ---- 6. done ----

const AI_SUMMARY: Record<NonNullable<OnboardingState["ai"]>, MessageKey> = {
  local: "onboarding.done.ai.local",
  byok: "onboarding.done.ai.byok",
  later: "onboarding.done.ai.later",
};

export const STEP_TITLE: Record<StepId, MessageKey> = {
  welcome: "onboarding.step.welcome",
  ai: "onboarding.step.ai",
  source: "onboarding.step.source",
  permissions: "onboarding.step.permissions",
  trigger: "onboarding.step.trigger",
  done: "onboarding.step.done",
};

export function DoneStep({ headingRef, state, dispatch, searchable = 0 }: StepProps & { searchable?: number }) {
  const t = useT();
  const pending = pendingSteps(state);
  const withAi = state.ai === "local" || state.ai === "byok";
  return (
    <>
      <StepHeading headingRef={headingRef}>{t("onboarding.done.heading")}</StepHeading>
      <p className="ob-lead">{t(withAi ? "onboarding.done.lead" : "onboarding.done.leadNoAi")}</p>
      {searchable > 0 && (
        <p className="note" role="status">
          {t("onboarding.done.searchable", { count: searchable })}
        </p>
      )}
      {state.ai && <p>{t(AI_SUMMARY[state.ai])}</p>}
      {pending.length > 0 ? (
        <>
          <p>{t("onboarding.done.pending", { count: pending.length })}</p>
          <ul className="ob-pending">
            {pending.map((s) => (
              <li key={s}>
                <button type="button" className="btn btn-small" onClick={() => dispatch({ type: "goto", step: s })}>
                  {t(STEP_TITLE[s])}
                </button>
              </li>
            ))}
          </ul>
        </>
      ) : (
        <p>{t("onboarding.done.allDone")}</p>
      )}
      <p className="muted">{t("onboarding.done.again")}</p>
    </>
  );
}
