/**
 * Nugget Cloud: typed wrappers for the shell's `cloud_*` commands (`src-tauri/src/cloud.rs`).
 * Tokens never reach the page: sign-in finishes in the shell, which then emits
 * {@link CLOUD_SESSION_EVENT} without any secret.
 */
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { ApiError, inTauri } from "./api";
import { t } from "./i18n";

export interface CloudSession {
  /** False when this build has no cloud configuration. */
  configured: boolean;
  signed_in: boolean;
  email: string | null;
}

export interface CloudConnection {
  id: string;
  toolkit: string;
  /** Composio's status, e.g. `ACTIVE`, `INITIATED`, `FAILED`, `EXPIRED`. */
  status: string;
  last_synced_at: string | null;
  last_error: string | null;
}

export interface CloudApp {
  slug: string;
  name: string;
  logo: string | null;
  connectable: boolean;
}

export interface CloudConnections {
  connections: CloudConnection[];
  available: CloudApp[];
}

export interface CloudSyncResult {
  toolkit: string;
  added: number;
  updated: number;
  error: string | null;
}

export interface CloudCitation {
  n: number;
  item_id: string;
  title: string | null;
  source: string;
  url: string | null;
  occurred_at: string | null;
  snippet: string;
}

export interface CloudAnswer {
  conversation_id: string;
  /** Contains `[n]` markers for {@link CloudCitation.n}. */
  answer: string;
  citations: CloudCitation[];
  usage: { input_tokens: number; output_tokens: number };
}

/** Sign in with Apple is not in M1 (no Apple Developer account yet). */
export type SignInProvider = "google";

/** `nugget:cloud-session`: the session changed. */
export interface CloudSessionEvent {
  signed_in: boolean;
  error: string | null;
}

export const CLOUD_SESSION_EVENT = "nugget:cloud-session";
/** No payload: Composio sent the browser back; reload the connections. */
export const CLOUD_CONNECTIONS_EVENT = "nugget:cloud-connections";

/** A cloud error: the shell's `ApiError` plus the cloud's own code (`not_signed_in`, ...). */
export class CloudError extends ApiError {
  readonly cloud: string | null;
  constructor(err: unknown) {
    const base = ApiError.from(err);
    super({ kind: base.kind, message: base.message, code: base.code });
    const c = typeof err === "object" && err !== null ? (err as { cloud?: unknown }).cloud : undefined;
    this.cloud = typeof c === "string" ? c : null;
  }
  get notSignedIn(): boolean {
    return this.cloud === "not_signed_in";
  }
}

async function call<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (!inTauri()) throw new ApiError({ kind: "unavailable", message: t("common.notInApp") });
  try {
    return await invoke<T>(command, args);
  } catch (e) {
    throw new CloudError(e);
  }
}

async function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  return listen<T>(event, (e) => handler(e.payload));
}

function isSessionEvent(p: unknown): p is CloudSessionEvent {
  return typeof p === "object" && p !== null && typeof (p as CloudSessionEvent).signed_in === "boolean";
}

export const cloudApi = {
  session: () => call<CloudSession>("cloud_session"),
  /** Opens the browser; the sign-in finishes when the link comes back ({@link CLOUD_SESSION_EVENT}). */
  signInWith: (provider: SignInProvider) => call<void>("cloud_sign_in", { provider }),
  /** Emails a magic link; the sign-in finishes when it is opened on this computer. */
  signInEmail: (email: string) => call<void>("cloud_sign_in", { email }),
  signOut: () => call<{ revoked: boolean }>("cloud_sign_out"),
  connections: () => call<CloudConnections>("cloud_connections_list"),
  /** Opens the app's consent page in the browser. */
  connect: (toolkit: string) => call<void>("cloud_connect", { toolkit }),
  disconnect: (id: string) => call<void>("cloud_disconnect", { id }),
  sync: () => call<{ synced: CloudSyncResult[] }>("cloud_sync"),
  ask: (question: string, conversationId?: string | null) =>
    call<CloudAnswer>("cloud_ask", conversationId ? { question, conversationId } : { question }),
  /** Malformed payloads are dropped. */
  onSession: (h: (e: CloudSessionEvent) => void) =>
    on<unknown>(CLOUD_SESSION_EVENT, (p) => {
      if (isSessionEvent(p)) h({ signed_in: p.signed_in, error: typeof p.error === "string" ? p.error : null });
    }),
  onConnectionsChanged: (h: (payload: unknown) => void) => on<unknown>(CLOUD_CONNECTIONS_EVENT, h),
};
