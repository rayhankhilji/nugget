import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, chatgptApi, isMethodNotFound, isOpenableUrl, type ChatgptStatus } from "../api";
import { useT, type MessageKey } from "../i18n";
import { ErrorNote, Spinner } from "../components/ui";
import "./open-core.css";

/** How often the status is read again while the sign-in waits for the browser. */
export const CHATGPT_POLL_MS = 1000;

// i18n-ignore-next-line: a URL is not prose.
export const MANAGE_USAGE_URL = "https://chatgpt.com/settings/usage";

const ERROR_KEYS: Record<string, MessageKey> = {
  access_denied: "chatgpt.error.access_denied",
  scope_not_granted: "chatgpt.error.scope_not_granted",
  invalid_id_token: "chatgpt.error.invalid_id_token",
  state_mismatch: "chatgpt.error.state_mismatch",
  timeout: "chatgpt.error.timeout",
  network: "chatgpt.error.network",
  token_exchange_failed: "chatgpt.error.token_exchange_failed",
  usage_limit: "chatgpt.error.usage_limit",
  reauth_required: "chatgpt.error.reauth_required",
  cancelled: "chatgpt.error.cancelled",
  no_keychain: "chatgpt.error.no_keychain",
  cloud_mode: "chatgpt.error.cloud_mode",
};

/** The sentence for an `ai.chatgpt.*` error code (a newer, unknown one gets a general line). */
export function chatgptErrorKey(code: string): MessageKey {
  return ERROR_KEYS[code] ?? "chatgpt.error.other";
}

/** A refusal because Heed Cloud is active, whether it came as a status error or an RPC error. */
function isCloudMode(status: ChatgptStatus | null, error: unknown): boolean {
  if (status?.error?.code === "cloud_mode") return true;
  if (error == null) return false;
  return /\bcloud_mode\b/.test(ApiError.from(error).message);
}

/** Signed in, with plan usage on and not paused: ChatGPT answers. */
export function chatgptReady(s: ChatgptStatus | null | undefined): boolean {
  return s?.state === "signed_in" && s.plan_usage && !s.paused;
}

export type ChatgptAction = "sign_in" | "cancel" | "resume" | "sign_out";

export interface Chatgpt {
  /** `null` until the first answer. */
  status: ChatgptStatus | null;
  /** The last call that failed (shown with {@link ErrorNote}), or a refusal. */
  error: unknown;
  /** The daemon predates ChatGPT sign-in. */
  unsupported: boolean;
  /** Heed Cloud is active, so the daemon refuses ChatGPT. */
  cloudMode: boolean;
  busy: ChatgptAction | null;
  signIn: () => void;
  cancel: () => void;
  resume: () => void;
  signOut: () => void;
  manageUsage: () => void;
}

/**
 * The ChatGPT sign-in's state, read from the daemon and polled about once a second while the
 * browser is open. Actions run one at a time; each answers with the new status.
 */
export function useChatgpt(): Chatgpt {
  const [status, setStatus] = useState<ChatgptStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState<ChatgptAction | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    chatgptApi
      .status()
      .then((s) => alive.current && setStatus(s))
      .catch((e: unknown) => alive.current && setError(e));
    return () => {
      alive.current = false;
    };
  }, []);

  const waiting = status?.state === "signing_in";
  useEffect(() => {
    if (!waiting) return;
    const timer = window.setInterval(() => {
      chatgptApi
        .status()
        .then((s) => alive.current && setStatus(s))
        .catch(() => {
          /* keep the last answer; the next poll tries again */
        });
    }, CHATGPT_POLL_MS);
    return () => window.clearInterval(timer);
  }, [waiting]);

  const run = useCallback(
    (what: ChatgptAction, call: () => Promise<ChatgptStatus>) => {
      if (busy) return;
      setBusy(what);
      setError(null);
      call()
        .then((s) => alive.current && setStatus(s))
        .catch((e: unknown) => alive.current && setError(e))
        .finally(() => alive.current && setBusy(null));
    },
    [busy],
  );

  const manageUsage = useCallback(() => {
    const url = isOpenableUrl(status?.manage_usage_url) ? status!.manage_usage_url : MANAGE_USAGE_URL;
    void api.openUrl(url).catch((e: unknown) => setError(e));
  }, [status]);

  return {
    status,
    error,
    unsupported: isMethodNotFound(error) && status === null,
    cloudMode: isCloudMode(status, error),
    busy,
    signIn: () => run("sign_in", chatgptApi.signIn),
    cancel: () => run("cancel", chatgptApi.cancel),
    resume: () => run("resume", chatgptApi.resume),
    signOut: () => run("sign_out", chatgptApi.signOut),
    manageUsage,
  };
}

/** What the provider row says: signed in as whom, paused, or plan usage off. */
function detailText(t: ReturnType<typeof useT>, s: ChatgptStatus): string {
  if (s.state === "signing_in") return t("chatgpt.waiting");
  if (s.state !== "signed_in") return t("chatgpt.signedOut");
  if (s.paused) return t("chatgpt.detail.paused");
  if (!s.plan_usage) return t("chatgpt.detail.planOff");
  const who = s.account?.email ?? s.account?.name;
  return who ? t("chatgpt.signedInAs", { account: who }) : t("chatgpt.signedIn");
}

/**
 * ChatGPT as a `.provider-status` row: a green dot only when it answers, amber when paused, none
 * otherwise; the model in mono; one action (sign in, cancel, or sign out).
 */
export function ChatgptRow({ chatgpt }: { chatgpt: Chatgpt }) {
  const t = useT();
  const s = chatgpt.status;
  if (!s) return null;
  const health = chatgptReady(s) ? "ok" : s.state === "signed_in" && s.paused ? "warn" : undefined;
  const model = s.state === "signed_in" ? (s.models.smart ?? s.models.light) : null;
  return (
    <li data-provider="chatgpt">
      <span className="provider-dot" data-health={health} aria-hidden="true" />
      <span>
        {/* i18n-ignore-next-line: a product name. */}
        <span className="provider-name">ChatGPT</span>
        <span className="provider-detail">
          <bdi>{detailText(t, s)}</bdi>
        </span>
      </span>
      <span className="provider-model" dir="ltr">
        {model ?? ""}
      </span>
      {s.state === "signed_in" ? (
        <button type="button" className="btn btn-small" disabled={chatgpt.busy !== null} onClick={chatgpt.signOut}>
          {t("chatgpt.signOut")}
        </button>
      ) : s.state === "signing_in" ? (
        <button type="button" className="btn btn-small" disabled={chatgpt.busy !== null} onClick={chatgpt.cancel}>
          {t("common.cancel")}
        </button>
      ) : (
        <button type="button" className="btn btn-small" disabled={chatgpt.busy !== null || chatgpt.cloudMode} onClick={chatgpt.signIn}>
          {t("chatgpt.signInShort")}
        </button>
      )}
    </li>
  );
}

/**
 * What needs saying about the sign-in beyond its row: waiting for the browser (with Cancel), plan
 * usage off, the usage limit (Manage usage, Resume, Use another AI) and errors. Never switches to
 * another AI by itself: every way out is the user's.
 */
export function ChatgptNotes({
  chatgpt,
  onUseKey,
  onOtherAi,
}: {
  chatgpt: Chatgpt;
  /** "Use an API key instead" (plan usage off). Hidden when not given. */
  onUseKey?: () => void;
  /** "Use another AI" (usage limit). Hidden when not given. */
  onOtherAi?: () => void;
}) {
  const t = useT();
  const { status: s, busy } = chatgpt;
  if (chatgpt.unsupported) {
    return (
      <p className="note" role="status">
        {t("chatgpt.unsupported")}
      </p>
    );
  }
  if (chatgpt.cloudMode) {
    return (
      <p className="note" role="status">
        {t("chatgpt.error.cloud_mode")}
      </p>
    );
  }
  const code = s?.error?.code;
  // OpenAI refused this account (registration or plan usage): not available for it yet.
  const unavailable = code === "token_exchange_failed" || code === "scope_not_granted";
  const planOff = s?.state === "signed_in" && !s.plan_usage;
  // The usage limit, plan usage and refusals have their own notes; a cancel the user pressed needs none.
  const shownError = code && code !== "usage_limit" && code !== "cancelled" && !unavailable;
  return (
    <>
      {s?.state === "signing_in" && (
        <div className="note" role="status">
          <p>
            <span className="spinner" aria-hidden="true" /> {t("chatgpt.waiting")}
          </p>
          <p className="muted small">{t("chatgpt.waitingHint")}</p>
          <div className="row">
            <button type="button" className="btn" disabled={busy !== null} onClick={chatgpt.cancel}>
              {t("common.cancel")}
            </button>
          </div>
        </div>
      )}
      {(planOff || unavailable) && (
        <div className="note note-error" role="alert">
          <p>
            <strong>{t(unavailable ? "chatgpt.unavailable.title" : "chatgpt.planOff.title")}</strong>
          </p>
          <p>{t(planOff ? "chatgpt.planOff.body" : "chatgpt.unavailable.body")}</p>
          {/* A new sign-in starts only from this click: never by itself, never in a loop. */}
          <div className="row">
            {onUseKey && (
              <button type="button" className={unavailable ? "btn btn-primary" : "btn"} onClick={onUseKey}>
                {t("chatgpt.useKey")}
              </button>
            )}
            <button type="button" className={unavailable && onUseKey ? "btn" : "btn btn-primary"} disabled={busy !== null} onClick={chatgpt.signIn}>
              {busy === "sign_in" ? t("chatgpt.signingIn") : t("chatgpt.tryAgain")}
            </button>
          </div>
        </div>
      )}
      {s?.state === "signed_in" && s.plan_usage && s.paused && (
        <div className="note note-warn" role="alert">
          <p>
            <strong>{t("chatgpt.paused.title")}</strong>
          </p>
          <p>{t("chatgpt.paused.body")}</p>
          <div className="row">
            <button type="button" className="btn btn-primary" onClick={chatgpt.manageUsage}>
              {t("chatgpt.manageUsage")}
            </button>
            <button type="button" className="btn" disabled={busy !== null} onClick={chatgpt.resume}>
              {busy === "resume" ? t("chatgpt.resuming") : t("chatgpt.resume")}
            </button>
            {onOtherAi && (
              <button type="button" className="btn btn-ghost" onClick={onOtherAi}>
                {t("chatgpt.otherAi")}
              </button>
            )}
          </div>
        </div>
      )}
      {shownError && (
        <p className="note note-error" role="alert">
          {t(chatgptErrorKey(code))}
        </p>
      )}
      {code === "cancelled" && s?.state === "signed_out" && (
        <p className="muted small" role="status">
          {t("chatgpt.error.cancelled")}
        </p>
      )}
      <ErrorNote error={chatgpt.unsupported ? null : chatgpt.error} />
    </>
  );
}

/**
 * Sign in with ChatGPT, for "Choose your AI": the button and its small print when signed out,
 * the provider row once signed in, and the notes. `onReady` runs once ChatGPT answers.
 */
export function ChatgptPanel({
  onReady,
  onUseKey,
  onOtherAi,
}: {
  onReady?: (what: string) => void;
  onUseKey?: () => void;
  onOtherAi?: () => void;
}) {
  const t = useT();
  const chatgpt = useChatgpt();
  const s = chatgpt.status;
  const ready = chatgptReady(s);
  const readyRef = useRef(onReady);
  readyRef.current = onReady;
  useEffect(() => {
    // i18n-ignore-next-line: a product name.
    if (ready) readyRef.current?.("ChatGPT");
  }, [ready]);

  if (!s && !chatgpt.error) return <Spinner />;
  const signedOut = s?.state === "signed_out" && !chatgpt.cloudMode;
  return (
    <div className="chatgpt-panel">
      {(s?.state === "signed_in" || s?.state === "signing_in") && (
        <ul className="provider-status">
          <ChatgptRow chatgpt={chatgpt} />
        </ul>
      )}
      {signedOut && (
        <>
          <div className="row">
            <button type="button" className="btn btn-primary" disabled={chatgpt.busy !== null} onClick={chatgpt.signIn}>
              {chatgpt.busy === "sign_in" ? t("chatgpt.signingIn") : t("chatgpt.signIn")}
            </button>
          </div>
          <p className="muted small chatgpt-fineprint">{t("chatgpt.fineprint")}</p>
        </>
      )}
      <ChatgptNotes chatgpt={chatgpt} onUseKey={onUseKey} onOtherAi={onOtherAi} />
    </div>
  );
}
