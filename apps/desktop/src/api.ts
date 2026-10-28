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
  /** Why the provider is unavailable (e.g. it failed to build from config); not routed to. */
  error?: string | null;
}

export interface StatusResponse {
  protocol: number;
  version: string;
  uptime_secs: number;
  items: number;
  chunks: number;
  providers: ProviderInfo[];
  /** Audit hash-chain check at daemon start: `null` until it has run. */
  audit_chain_ok?: boolean | null;
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
  /** Items whose content the messages include; recorded in the egress ledger. */
  items?: ItemRef[];
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
  /** Where to open the original (`file:`, `https:`, `obsidian:` URL), if known. */
  url?: string | null;
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

// ---- sources (M1) ----

/** Connector kinds shipped in M1. The daemon may report others; treat `kind` as a string. */
export type SourceKind = "files" | "obsidian" | "gmail" | "gcal" | "imap" | "caldav";
export const SOURCE_KINDS: readonly SourceKind[] = ["files", "obsidian", "gmail", "gcal", "imap", "caldav"];

export type SourceState = "idle" | "syncing" | "watching" | "needs_auth" | "disabled" | "error";
export const SOURCE_STATES: readonly SourceState[] = ["idle", "syncing", "watching", "needs_auth", "disabled", "error"];

/** Kind-specific settings. Never contains secrets. */
export type SourceSettings = Record<string, unknown>;

export interface SourceSpec {
  /** Chosen by the daemon when `null` (e.g. `files-1`). */
  id?: string | null;
  kind: string;
  /** Human-readable label. */
  name: string;
  settings: SourceSettings;
}

export interface SourceInfo {
  id: string;
  kind: string;
  name: string;
  enabled: boolean;
  state: SourceState;
  items: number;
  chunks: number;
  /** RFC 3339 timestamp of the last finished sync. */
  last_sync?: string | null;
  last_error?: string | null;
  settings?: SourceSettings;
}

export interface SourceRef {
  id: string;
}

export interface SourceUpdate {
  id: string;
  name?: string | null;
  enabled?: boolean | null;
  settings?: SourceSettings | null;
}

export interface SourceRemove {
  id: string;
  /** Also delete everything indexed from this source. */
  purge: boolean;
}

export interface SourceSync {
  /** `null` syncs every enabled source. */
  id: string | null;
}

export interface AuthBegin {
  url: string;
}

/** Write-only: the password goes to the OS keychain through the daemon. */
export interface SourceSecret {
  id: string;
  secret: string;
}

export type IngestPhase = "scanning" | "indexing" | "idle" | "error";

export interface IngestProgress {
  source: string;
  phase: IngestPhase;
  done: number;
  total?: number | null;
  error?: string | null;
}

export interface IngestStatus {
  /** Changes observed but not yet searchable. */
  queue_depth: number;
  /** Change-to-searchable latency over the recent window, in milliseconds. */
  freshness_p50_ms?: number | null;
  freshness_p95_ms?: number | null;
  /** Provider used for embeddings (otherwise text-only search). */
  embedding_provider?: string | null;
  triage_provider?: string | null;
  sources: SourceInfo[];
}

// ---- retrieval ----

export interface ContextRequest {
  query: string;
  /** Approximate token budget for `text`. */
  budget_tokens: number;
}

export interface Citation {
  /** The `[n]` marker used in `ContextResponse.text`, from 1. */
  n: number;
  item: ItemRef;
  chunk_id: number;
  title: string | null;
  url?: string | null;
  /** The item is marked sensitive (PII or triage): it is never sent to cloud models. */
  sensitive?: boolean;
}

export interface ContextResponse {
  /** Packed passages, each prefixed with its `[n]` marker. */
  text: string;
  citations: Citation[];
  /** Relevant sensitive items left out of `text`. */
  excluded_sensitive?: number;
}

/** `retrieve.ask`: the daemon picks the model first, then packs context fit for it. */
export interface AskRequest {
  query: string;
  /** Approximate token budget for the packed context. */
  budget_tokens: number;
  /** Explicit provider id, or `null` to route by `tier`. */
  provider?: string | null;
  /** Default `synthesis`. */
  tier?: Tier;
}

/** Answer streaming as `chat.delta` / `chat.done`, plus the context the model was given. */
export interface AskStarted {
  stream_id: string;
  provider: string;
  locality: Locality;
  /** Passages sent to the model, numbered as the answer cites them. */
  citations: Citation[];
  /** Relevant sensitive items withheld because the model is a cloud model. */
  excluded_sensitive?: number;
}

// ---- local runtime ----

export type RuntimeRole = "chat" | "embedding";
export type RuntimeState = "not_installed" | "stopped" | "starting" | "ready" | "failed";

export interface RuntimeInfo {
  model_id: string;
  role: RuntimeRole;
  state: RuntimeState;
  /** Provider id the router knows this runtime by (e.g. `local-chat`). */
  provider: string;
  error?: string | null;
}

// ---- knowledge graph (M2) ----

/** Entity kinds, as `nugget_core::ipc` sends them. */
export type EntityKind =
  | "person"
  | "organization"
  | "project"
  | "commitment"
  | "preference"
  | "event"
  | "place"
  | "topic";
export const ENTITY_KINDS: readonly EntityKind[] = [
  "person",
  "organization",
  "project",
  "commitment",
  "preference",
  "event",
  "place",
  "topic",
];

export type ClaimStatus = "active" | "superseded" | "retracted" | "user_asserted";
export const CLAIM_STATUSES: readonly ClaimStatus[] = ["active", "superseded", "retracted", "user_asserted"];

/** `knowledge.entities`: page through entities, optionally filtered by text and kind. */
export interface EntitiesRequest {
  text?: string | null;
  kind?: EntityKind | null;
  limit: number;
  offset?: number;
}

export interface EntitySummaryView {
  id: string;
  kind: EntityKind;
  name: string;
  claims: number;
  mentions: number;
  /** RFC 3339 timestamp. */
  updated_at: string;
}

export interface EntityRef {
  id: string;
}

/** One source passage supporting a fact. */
export interface EvidenceView {
  item: ItemRef;
  chunk_id?: number | null;
  quote?: string | null;
  title?: string | null;
  url?: string | null;
}

export interface EntityLink {
  id: string;
  name: string;
}

/** Exactly one of `entity` / `literal` is set. */
export interface ClaimObjectView {
  entity?: EntityLink | null;
  literal?: string | null;
}

export interface ClaimView {
  id: number;
  subject: EntityLink;
  predicate: string;
  object: ClaimObjectView;
  text: string;
  confidence: number;
  status: ClaimStatus;
  evidence: EvidenceView[];
}

export interface MentionView {
  item: ItemRef;
  chunk_id?: number | null;
  surface: string;
  title?: string | null;
  url?: string | null;
}

export interface EntityDetail {
  id: string;
  kind: EntityKind;
  name: string;
  aliases: string[];
  /** `[scheme, value]` pairs, e.g. `["email", "sam@example.com"]`. */
  identifiers: [string, string][];
  attributes: unknown;
  claims: ClaimView[];
  mentions: MentionView[];
  /** The entity's vault page, relative to the vault root. */
  vault_path?: string | null;
}

export interface GraphRequest {
  /** Neighbourhood of this entity, or an overview of the most connected entities when `null`. */
  center?: string | null;
  depth: number;
  limit: number;
  kinds?: EntityKind[];
}

export interface GraphNodeView {
  id: string;
  kind: EntityKind;
  name: string;
  depth: number;
}

export interface GraphEdgeView {
  claim: number;
  from: string;
  to: string;
  predicate: string;
}

export interface GraphView {
  nodes: GraphNodeView[];
  edges: GraphEdgeView[];
  truncated: boolean;
}

export interface MergeRequest {
  /** The entity that stays. */
  into: string;
  /** The entity merged away; its id keeps resolving to `into`. */
  from: string;
}

export interface ClaimStatusRequest {
  claim: number;
  /** `retracted` rejects the fact; `active` restores it. */
  status: "retracted" | "active";
}

export interface VaultStatus {
  path: string | null;
  pages: number;
  last_render?: string | null;
  /** Pages (relative to the vault) whose user edits could not be merged automatically. */
  conflicts?: string[];
  /** Pages failing validation (broken links, missing citations, bad frontmatter). */
  invalid?: string[];
}

export interface KnowledgeStatus {
  entities: number;
  claims: number;
  mentions: number;
  /** Items changed but not yet processed by extraction. */
  pending_items: number;
  last_run?: string | null;
  /** Provider used for LLM extraction; `null` means deterministic extractors only. */
  extractor_provider?: string | null;
  vault: VaultStatus;
}

/** `knowledge.changed`. An empty list means "anything may have changed". */
export interface KnowledgeChanged {
  entities: string[];
}

// ---- MCP clients (M2) ----

/** What an MCP client may see and call. */
export interface McpScopes {
  /** Source ids the client may read; `null` means all. */
  sources?: string[] | null;
  /** Tool names the client may call; empty means all read-only tools. */
  tools?: string[];
  /** Whether items flagged sensitive may be returned. */
  include_sensitive?: boolean;
}

export interface McpClientInfo {
  id: string;
  name: string;
  scopes: McpScopes;
  created_at: string;
  last_used?: string | null;
}

export interface McpClientCreate {
  name: string;
  scopes: McpScopes;
}

export interface McpClientRef {
  id: string;
}

export type McpSetupApp = "claude-desktop" | "claude-code" | "cursor" | "generic-http" | "generic-stdio";
export const MCP_SETUP_APPS: readonly McpSetupApp[] = [
  "claude-desktop",
  "claude-code",
  "cursor",
  "generic-http",
  "generic-stdio",
];

export interface McpSetupSnippet {
  /** Usually an `McpSetupApp`; unknown apps are shown generically. */
  app: string;
  /** Config file path hint, if the app uses one. */
  config_path?: string | null;
  text: string;
}

/**
 * Result of `mcp.clients.create`. `token` is shown once: keep it in the dialog that shows it
 * and nowhere else, and never log it.
 */
export interface McpClientCreated {
  client: McpClientInfo;
  token: string;
  setup: McpSetupSnippet[];
}

/** Read-only tools the Nugget MCP server offers (docs/m2-spec.md, M2-D). */
export const MCP_TOOLS: readonly { name: string; label: string; description: string }[] = [
  { name: "search", label: "Search", description: "Search everything the app may read." },
  { name: "get_context", label: "Get context", description: "Cited passages for a question." },
  { name: "get_entity", label: "Get entity", description: "A person, project or other entity, with cited facts." },
  { name: "find_entities", label: "Find entities", description: "Look entities up by name." },
  { name: "get_neighbourhood", label: "Get neighbourhood", description: "Entities connected to an entity." },
  { name: "get_item", label: "Get item", description: "One indexed document, message or event." },
  { name: "recent_changes", label: "Recent changes", description: "What changed in the knowledge graph lately." },
];

// ---- type guards (for event payloads, which are not type-checked on the way in) ----

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isCount(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v) && v >= 0;
}

function isOptString(v: unknown): boolean {
  return v === undefined || v === null || typeof v === "string";
}

export function isSourceState(v: unknown): v is SourceState {
  return typeof v === "string" && (SOURCE_STATES as readonly string[]).includes(v);
}

export function isSourceKind(v: unknown): v is SourceKind {
  return typeof v === "string" && (SOURCE_KINDS as readonly string[]).includes(v);
}

export function isSourceInfo(v: unknown): v is SourceInfo {
  return (
    isRecord(v) &&
    typeof v.id === "string" &&
    typeof v.kind === "string" &&
    typeof v.name === "string" &&
    typeof v.enabled === "boolean" &&
    isSourceState(v.state) &&
    isCount(v.items) &&
    isCount(v.chunks) &&
    isOptString(v.last_sync) &&
    isOptString(v.last_error) &&
    (v.settings === undefined || v.settings === null || isRecord(v.settings))
  );
}

const INGEST_PHASES: readonly string[] = ["scanning", "indexing", "idle", "error"];

export function isIngestProgress(v: unknown): v is IngestProgress {
  return (
    isRecord(v) &&
    typeof v.source === "string" &&
    typeof v.phase === "string" &&
    INGEST_PHASES.includes(v.phase) &&
    isCount(v.done) &&
    (v.total === undefined || v.total === null || isCount(v.total)) &&
    isOptString(v.error)
  );
}

/**
 * `sources.changed` has no fixed payload in the protocol. When it carries a `SourceInfo` (or a
 * list of them, possibly as `{ sources: [...] }`) they are returned so the UI can update in
 * place; otherwise `null`, meaning "refetch `sources.list`".
 */
export function sourcesFromChange(payload: unknown): SourceInfo[] | null {
  if (isSourceInfo(payload)) return [payload];
  const list = isRecord(payload) && Array.isArray(payload.sources) ? payload.sources : payload;
  if (Array.isArray(list) && list.length > 0 && list.every(isSourceInfo)) return list;
  return null;
}

/** Merge updated sources into a list, keeping order; new ids are appended. */
export function mergeSources(list: readonly SourceInfo[], updates: readonly SourceInfo[]): SourceInfo[] {
  const byId = new Map(updates.map((s) => [s.id, s]));
  const merged = list.map((s) => byId.get(s.id) ?? s);
  for (const u of updates) if (!list.some((s) => s.id === u.id)) merged.push(u);
  return merged;
}

// ---- knowledge & MCP guards ----

function isStringArray(v: unknown): v is string[] {
  return Array.isArray(v) && v.every((x) => typeof x === "string");
}

function isItemRef(v: unknown): v is ItemRef {
  return isRecord(v) && typeof v.source === "string" && typeof v.key === "string";
}

export function isEntityKind(v: unknown): v is EntityKind {
  return typeof v === "string" && (ENTITY_KINDS as readonly string[]).includes(v);
}

export function isClaimStatus(v: unknown): v is ClaimStatus {
  return typeof v === "string" && (CLAIM_STATUSES as readonly string[]).includes(v);
}

function isEntityLink(v: unknown): v is EntityLink {
  return isRecord(v) && typeof v.id === "string" && typeof v.name === "string";
}

export function isEntitySummaryView(v: unknown): v is EntitySummaryView {
  return (
    isRecord(v) &&
    typeof v.id === "string" &&
    isEntityKind(v.kind) &&
    typeof v.name === "string" &&
    isCount(v.claims) &&
    isCount(v.mentions) &&
    typeof v.updated_at === "string"
  );
}

export function isEvidenceView(v: unknown): v is EvidenceView {
  return (
    isRecord(v) &&
    isItemRef(v.item) &&
    (v.chunk_id === undefined || v.chunk_id === null || typeof v.chunk_id === "number") &&
    isOptString(v.quote) &&
    isOptString(v.title) &&
    isOptString(v.url)
  );
}

export function isClaimView(v: unknown): v is ClaimView {
  if (!isRecord(v) || !isRecord(v.object)) return false;
  const o = v.object;
  const hasEntity = o.entity !== undefined && o.entity !== null;
  const hasLiteral = o.literal !== undefined && o.literal !== null;
  return (
    typeof v.id === "number" &&
    isEntityLink(v.subject) &&
    typeof v.predicate === "string" &&
    hasEntity !== hasLiteral &&
    (!hasEntity || isEntityLink(o.entity)) &&
    (!hasLiteral || typeof o.literal === "string") &&
    typeof v.text === "string" &&
    typeof v.confidence === "number" &&
    Number.isFinite(v.confidence) &&
    isClaimStatus(v.status) &&
    Array.isArray(v.evidence) &&
    v.evidence.every(isEvidenceView)
  );
}

export function isGraphNodeView(v: unknown): v is GraphNodeView {
  return isRecord(v) && typeof v.id === "string" && isEntityKind(v.kind) && typeof v.name === "string" && isCount(v.depth);
}

export function isGraphEdgeView(v: unknown): v is GraphEdgeView {
  return (
    isRecord(v) &&
    typeof v.claim === "number" &&
    typeof v.from === "string" &&
    typeof v.to === "string" &&
    typeof v.predicate === "string"
  );
}

/** Strict: every node must have a known kind and every edge the full shape. */
export function isGraphView(v: unknown): v is GraphView {
  return (
    isRecord(v) &&
    Array.isArray(v.nodes) &&
    v.nodes.every(isGraphNodeView) &&
    Array.isArray(v.edges) &&
    v.edges.every(isGraphEdgeView) &&
    typeof v.truncated === "boolean"
  );
}

export function isKnowledgeChanged(v: unknown): v is KnowledgeChanged {
  return isRecord(v) && isStringArray(v.entities);
}

export function isKnowledgeStatus(v: unknown): v is KnowledgeStatus {
  if (!isRecord(v) || !isRecord(v.vault)) return false;
  const vault = v.vault;
  return (
    isCount(v.entities) &&
    isCount(v.claims) &&
    isCount(v.mentions) &&
    isCount(v.pending_items) &&
    isOptString(v.last_run) &&
    isOptString(v.extractor_provider) &&
    isOptString(vault.path) &&
    isCount(vault.pages) &&
    isOptString(vault.last_render) &&
    (vault.conflicts === undefined || isStringArray(vault.conflicts)) &&
    (vault.invalid === undefined || isStringArray(vault.invalid))
  );
}

export function isMcpScopes(v: unknown): v is McpScopes {
  return (
    isRecord(v) &&
    (v.sources === undefined || v.sources === null || isStringArray(v.sources)) &&
    (v.tools === undefined || isStringArray(v.tools)) &&
    (v.include_sensitive === undefined || typeof v.include_sensitive === "boolean")
  );
}

export function isMcpClientInfo(v: unknown): v is McpClientInfo {
  return (
    isRecord(v) &&
    typeof v.id === "string" &&
    typeof v.name === "string" &&
    isMcpScopes(v.scopes) &&
    typeof v.created_at === "string" &&
    isOptString(v.last_used)
  );
}

export function isMcpSetupSnippet(v: unknown): v is McpSetupSnippet {
  return isRecord(v) && typeof v.app === "string" && typeof v.text === "string" && isOptString(v.config_path);
}

export function isMcpClientCreated(v: unknown): v is McpClientCreated {
  return (
    isRecord(v) &&
    isMcpClientInfo(v.client) &&
    typeof v.token === "string" &&
    v.token.length > 0 &&
    Array.isArray(v.setup) &&
    v.setup.every(isMcpSetupSnippet)
  );
}

/** Whether a `knowledge.changed` payload may affect `id` (an empty list means "anything"). */
export function changeAffects(change: KnowledgeChanged, id: string | null | undefined): boolean {
  if (change.entities.length === 0) return true;
  return id != null && change.entities.includes(id);
}

/** URL schemes `open_url` accepts (mirrors `src-tauri/src/open.rs`). */
export const OPENABLE_SCHEMES = ["https:", "obsidian:", "file:"] as const;
const MAX_URL_LEN = 8 * 1024;

/**
 * Whether `open_url` would accept this URL. The Rust side is the real check (it also refuses
 * network shares and reveals runnable files instead of opening them); this decides whether to
 * offer an Open button at all.
 */
export function isOpenableUrl(url: string | null | undefined): url is string {
  if (!url || url.length > MAX_URL_LEN) return false;
  // Whitespace or control characters anywhere: the Rust side refuses these.
  if (/[\s\u0000-\u001f\u007f-\u009f]/.test(url)) return false;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  const scheme = parsed.protocol.toLowerCase();
  if (!(OPENABLE_SCHEMES as readonly string[]).includes(scheme)) return false;
  if (scheme === "https:") return parsed.hostname.length > 0;
  if (scheme === "file:") {
    const host = parsed.hostname;
    if (host !== "" && host !== "localhost") return false;
    // `file:////server/share` keeps the share in the path.
    return !parsed.pathname.startsWith("//");
  }
  return true;
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

/** JSON-RPC "method not found": the daemon predates this feature. */
export const METHOD_NOT_FOUND = -32601;

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

  /** True when the daemon doesn't implement the method (an older daemon version). */
  get isMethodNotFound(): boolean {
    return this.kind === "rpc" && this.code === METHOD_NOT_FOUND;
  }

  static from(err: unknown): ApiError {
    if (err instanceof ApiError) return err;
    if (typeof err === "object" && err !== null && "kind" in err && "message" in err) {
      return new ApiError(err as ApiErrorPayload);
    }
    return new ApiError({ kind: "other", message: err instanceof Error ? err.message : String(err) });
  }
}

/** True when `err` says the daemon doesn't implement the method yet. */
export function isMethodNotFound(err: unknown): boolean {
  return err != null && ApiError.from(err).isMethodNotFound;
}

/** Shown wherever a feature needs a newer daemon. */
export const NOT_AVAILABLE = "Not available in this daemon version";

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

  sourcesList: () => call<SourceInfo[]>("sources_list"),
  sourcesAdd: (request: SourceSpec) => call<SourceInfo>("sources_add", { request }),
  sourcesUpdate: (request: SourceUpdate) => call<SourceInfo>("sources_update", { request }),
  sourcesRemove: (request: SourceRemove) => call<void>("sources_remove", { request }),
  /** Progress arrives as `ingest.progress` events. `id: null` syncs every enabled source. */
  sourcesSync: (request: SourceSync) => call<void>("sources_sync", { request }),
  /** Open the returned URL with `openUrl`; completion arrives as `sources.changed`. */
  sourcesAuthBegin: (request: SourceRef) => call<AuthBegin>("sources_auth_begin", { request }),
  /** Write-only. Callers should drop the secret from their state right after this resolves. */
  sourcesSecretSet: (request: SourceSecret) => call<void>("sources_secret_set", { request }),
  ingestStatus: () => call<IngestStatus>("ingest_status"),
  retrieveContext: (request: ContextRequest) => call<ContextResponse>("retrieve_context", { request }),
  /** Answer from retrieved context; output arrives as chat events for the returned stream. */
  retrieveAsk: (request: AskRequest) => call<AskStarted>("retrieve_ask", { request }),
  runtimeStatus: () => call<RuntimeInfo[]>("runtime_status"),

  knowledgeEntities: (request: EntitiesRequest) => call<EntitySummaryView[]>("knowledge_entities", { request }),
  knowledgeEntity: (id: string) => call<EntityDetail>("knowledge_entity", { request: { id } satisfies EntityRef }),
  knowledgeGraph: (request: GraphRequest) => call<GraphView>("knowledge_graph", { request }),
  /** Merge `from` into `into`; `from`'s id keeps resolving to `into`. */
  knowledgeMerge: (request: MergeRequest) => call<void>("knowledge_merge", { request }),
  /** Reject (`retracted`) or restore (`active`) a fact. */
  knowledgeClaimStatus: (request: ClaimStatusRequest) => call<void>("knowledge_claim_status", { request }),
  knowledgeStatus: () => call<KnowledgeStatus>("knowledge_status"),

  mcpClientsList: () => call<McpClientInfo[]>("mcp_clients_list"),
  /**
   * The result carries the client's token, shown once. Keep it only in the dialog showing it.
   * A response of the wrong shape is refused rather than half-shown.
   */
  mcpClientsCreate: async (request: McpClientCreate) => {
    const created = await call<unknown>("mcp_clients_create", { request });
    if (!isMcpClientCreated(created)) {
      throw new ApiError({ kind: "protocol", message: "Unexpected response from Nugget when creating the app." });
    }
    return created;
  },
  mcpClientsRevoke: (id: string) => call<void>("mcp_clients_revoke", { request: { id } satisfies McpClientRef }),

  /** Open an `https:`, `obsidian:` or local `file:` URL with the OS default handler. */
  openUrl: (url: string) => call<OpenResult>("open_url", { url }),
  /** Native folder picker; `[]` when cancelled. */
  pickFolders: (multiple: boolean) => call<string[]>("pick_folders", { multiple }),
};

/** `revealed`: the file could run code, so its folder was shown instead of opening it. */
export type OpenResult = "opened" | "revealed";

// ---- events (see apps/desktop/src-tauri/src/daemon.rs) ----

export const EVENT = {
  connection: "nugget:connection",
  chatDelta: "nugget:chat-delta",
  chatDone: "nugget:chat-done",
  modelsProgress: "nugget:models-progress",
  ingestProgress: "nugget:ingest-progress",
  sourcesChanged: "nugget:sources-changed",
  knowledgeChanged: "nugget:knowledge-changed",
  notification: "nugget:notification",
  navigate: "nugget:navigate",
} as const;

export interface RawNotification {
  method: string;
  params: unknown;
}

export type PageId = "chat" | "search" | "knowledge" | "sources" | "apps" | "models" | "privacy" | "status";

async function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (!inTauri()) return () => {};
  return listen<T>(event, (e) => handler(e.payload));
}

export const events = {
  onConnection: (h: (info: ConnectionInfo) => void) => on(EVENT.connection, h),
  onChatDelta: (h: (d: ChatDelta) => void) => on(EVENT.chatDelta, h),
  onChatDone: (h: (d: ChatDone) => void) => on(EVENT.chatDone, h),
  onModelsProgress: (h: (p: ModelProgress) => void) => on(EVENT.modelsProgress, h),
  /** Malformed payloads are dropped. */
  onIngestProgress: (h: (p: IngestProgress) => void) =>
    on<unknown>(EVENT.ingestProgress, (p) => {
      if (isIngestProgress(p)) h(p);
    }),
  /** Payload is whatever the daemon sent; see `sourcesFromChange`. */
  onSourcesChanged: (h: (payload: unknown) => void) => on(EVENT.sourcesChanged, h),
  /**
   * Entities whose facts, mentions or pages changed. A malformed payload is passed on as
   * `{ entities: [] }` ("anything may have changed"), so views refetch instead of going stale.
   */
  onKnowledgeChanged: (h: (c: KnowledgeChanged) => void) =>
    on<unknown>(EVENT.knowledgeChanged, (p) => h(isKnowledgeChanged(p) ? p : { entities: [] })),
  /** Every daemon notification, including ones without a dedicated event. */
  onNotification: (h: (n: RawNotification) => void) => on(EVENT.notification, h),
  /** Tray menu asked the window to show a page. */
  onNavigate: (h: (page: PageId) => void) => on(EVENT.navigate, h),
};
