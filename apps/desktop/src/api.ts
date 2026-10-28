/**
 * Typed bridge to the Rust side. Every daemon call and event subscription in the UI goes
 * through this module; components never call `invoke`/`listen` directly.
 *
 * Wire types mirror `nugget_core::ipc` (snake_case fields, as serialized by serde).
 */
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Must match `nugget_core::ipc::PROTOCOL_VERSION`. */
export const PROTOCOL_VERSION = 1;

// ---- nugget_core types ----

export type Tier = "triage" | "light" | "synthesis" | "agentic" | "embedding";
export const CHAT_TIERS: readonly Tier[] = ["light", "synthesis", "agentic", "triage"];

export type Locality = "on_device" | "local_network" | "cloud";

export interface Capabilities {
  streaming: boolean;
  vision: boolean;
  tools: boolean;
  json_schema: boolean;
  embeddings: boolean;
  context_tokens: number | null;
}

export interface ProviderInfo {
  id: string;
  /** `anthropic`, `openai_responses`, `openai_compatible`. */
  kind: string;
  model: string;
  locality: Locality;
  tiers: Tier[];
  /** `null` until a probe has run. */
  capabilities: Capabilities | null;
}

export interface StatusResponse {
  protocol: number;
  version: string;
  uptime_secs: number;
  items: number;
  chunks: number;
  providers: ProviderInfo[];
}

export type Role = "system" | "user" | "assistant";

export interface ChatMessage {
  role: Role;
  text: string;
}

export interface ChatRequest {
  /** Explicit provider id, or `null` to route by `tier`. */
  provider: string | null;
  tier: Tier;
  messages: ChatMessage[];
}

export interface ChatStarted {
  stream_id: string;
  provider: string;
}

export interface ChatDelta {
  stream_id: string;
  text: string;
}

export interface ChatDone {
  stream_id: string;
  /** `null` on success. */
  error: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
}

export interface ItemRef {
  source: string;
  key: string;
}

export interface SearchHit {
  item: ItemRef;
  chunk_id?: number | null;
  title: string | null;
  snippet: string;
  score: number;
}

export interface ModelInfo {
  id: string;
  size_bytes: number;
  installed: boolean;
  recommended: boolean;
  family?: string;
  tiers?: Tier[];
  license?: string;
  revision?: string;
  update_available?: boolean;
}

export type ModelPhase = "downloading" | "verifying" | "installed" | "failed";

export interface ModelProgress {
  id: string;
  downloaded: number;
  total: number;
  phase: ModelPhase;
  error: string | null;
}

export interface EgressEvent {
  /** RFC 3339 timestamp. */
  at: string;
  host: string;
  feature: string;
  bytes_sent: number;
  bytes_received: number;
  items: ItemRef[];
}

// ---- app-side types (defined by the Rust shell) ----

export interface ConnectionInfo {
  connected: boolean;
  /** Socket path or pipe name the app connects to. */
  socket: string;
  /** Why the last connection attempt failed, if it did. */
  error: string | null;
}

export type ApiErrorKind =
  /** The daemon is not reachable. Offer "Start Nugget". */
  | "not_running"
  /** The connection dropped mid-call. */
  | "disconnected"
  /** The daemon answered with a JSON-RPC error (`code` is set). */
  | "rpc"
  /** Unexpected response shape (version skew between app and daemon). */
  | "protocol"
  /** `nuggetd` could not be found or started. */
  | "spawn"
  /** Not running inside the Tauri shell (e.g. plain `vite dev` in a browser). */
  | "unavailable"
  | "other";

interface ApiErrorPayload {
  kind: ApiErrorKind;
  message: string;
  code?: number | null;
}

export class ApiError extends Error {
  readonly kind: ApiErrorKind;
  readonly code: number | null;

  constructor(payload: ApiErrorPayload) {
    super(payload.message);
    this.name = "ApiError";
    this.kind = payload.kind;
    this.code = payload.code ?? null;
  }

  /** True when reconnecting or starting the daemon may help. */
  get isOffline(): boolean {
    return this.kind === "not_running" || this.kind === "disconnected" || this.kind === "unavailable";
  }

  static from(err: unknown): ApiError {
    if (err instanceof ApiError) return err;
    if (typeof err === "object" && err !== null && "kind" in err && "message" in err) {
      return new ApiError(err as ApiErrorPayload);
    }
    return new ApiError({ kind: "other", message: err instanceof Error ? err.message : String(err) });
  }
}

/** Human-readable message for any thrown value. */
export function errorMessage(err: unknown): string {
  return ApiError.from(err).message;
}

// ---- transport ----

export function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

async function call<T>(command: string, args?: InvokeArgs): Promise<T> {
  if (!inTauri()) {
    throw new ApiError({ kind: "unavailable", message: "Not running inside the Nugget app." });
  }
  try {
    return await invoke<T>(command, args);
  } catch (err) {
    throw ApiError.from(err);
  }
}

// ---- commands (see apps/desktop/src-tauri/src/commands.rs) ----

export const api = {
  /** Connect if needed and report the connection state. Never throws for "not running". */
  connection: () => call<ConnectionInfo>("connection_info"),
  /** Spawn `nuggetd` (next to the app binary, else from PATH) and wait until it answers. */
  startDaemon: () => call<ConnectionInfo>("start_daemon"),

  status: () => call<StatusResponse>("status"),
  providersList: () => call<ProviderInfo[]>("providers_list"),
  /** Result shape is not pinned by the protocol yet; usually `Capabilities`. */
  providersProbe: (provider: string) => call<unknown>("providers_probe", { provider }),

  chat: (request: ChatRequest) => call<ChatStarted>("chat", { request }),
  chatCancel: (streamId: string) => call<void>("chat_cancel", { streamId }),

  search: (query: string, limit: number) => call<SearchHit[]>("search", { query, limit }),

  modelsList: () => call<ModelInfo[]>("models_list"),
  modelsInstall: (id: string) => call<unknown>("models_install", { id }),

  egressRecent: (limit: number) => call<EgressEvent[]>("egress_recent", { limit }),
};

// ---- events (see apps/desktop/src-tauri/src/daemon.rs) ----

export const EVENT = {
  connection: "nugget:connection",
  chatDelta: "nugget:chat-delta",
  chatDone: "nugget:chat-done",
  modelsProgress: "nugget:models-progress",
  notification: "nugget:notification",
  navigate: "nugget:navigate",
} as const;

export interface RawNotification {
  method: string;
  params: unknown;
}

export type PageId = "chat" | "search" | "models" | "privacy" | "status";

async function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  return listen<T>(event, (e) => handler(e.payload));
}

export const events = {
  onConnection: (h: (info: ConnectionInfo) => void) => on(EVENT.connection, h),
  onChatDelta: (h: (d: ChatDelta) => void) => on(EVENT.chatDelta, h),
  onChatDone: (h: (d: ChatDone) => void) => on(EVENT.chatDone, h),
  onModelsProgress: (h: (p: ModelProgress) => void) => on(EVENT.modelsProgress, h),
  /** Every daemon notification, including ones without a dedicated event. */
  onNotification: (h: (n: RawNotification) => void) => on(EVENT.notification, h),
  /** Tray menu asked the window to show a page. */
  onNavigate: (h: (page: PageId) => void) => on(EVENT.navigate, h),
};
