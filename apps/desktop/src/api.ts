/**
 * Typed bridge to the Rust side. Every daemon call and event subscription in the UI goes
 * through this module; components never call `invoke`/`listen` directly.
 *
 * Wire types mirror `nugget_core::ipc` (snake_case fields, as serialized by serde).
 */
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { t, type MessageKey } from "./i18n";

/** Must match `nugget_core::ipc::PROTOCOL_VERSION`. */
export const PROTOCOL_VERSION = 1;

// ---- nugget_core types ----

export type Tier = "triage" | "light" | "synthesis" | "agentic" | "embedding" | "rerank";
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
export type SourceKind = "files" | "obsidian" | "gmail" | "gcal" | "gdrive" | "imap" | "caldav";
export const SOURCE_KINDS: readonly SourceKind[] = ["files", "obsidian", "gmail", "gcal", "gdrive", "imap", "caldav"];

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
  /** Device flows (GitHub): the code the user enters on the page at `url`. */
  user_code?: string | null;
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
  /** Earlier turns, oldest first: a follow-up is rewritten for retrieval and they go to the model. */
  history?: ChatMessage[];
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
  /** The standalone query actually searched, when a follow-up was rewritten. */
  query_rewrite?: string | null;
  /** Nothing relevant in the sources: the daemon answers as an ordinary chat (Z3). */
  general?: boolean;
}

// ---- local runtime ----

export type RuntimeRole = "chat" | "embedding" | "rerank" | "draft";
export type RuntimeState = "not_installed" | "stopped" | "starting" | "ready" | "stopping" | "failed";

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

/** `retracted`: lost its evidence. `rejected`: rejected by the user (sticky). */
export type ClaimStatus = "active" | "superseded" | "retracted" | "user_asserted" | "rejected";
export const CLAIM_STATUSES: readonly ClaimStatus[] = ["active", "superseded", "retracted", "user_asserted", "rejected"];

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
  /** RFC 3339: when the source said it (the item's modified time), if the daemon sends it. */
  observed_at?: string | null;
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
  /** RFC 3339: when the fact last changed, if the daemon sends it. */
  updated_at?: string | null;
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
  /** Only nodes and edges supported by items of this source. */
  source?: string | null;
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
  status: ClaimDecision;
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
export const MCP_TOOLS: readonly { name: McpToolName; label: MessageKey; description: MessageKey }[] = (
  ["search", "get_context", "get_entity", "find_entities", "get_neighbourhood", "get_item", "recent_changes"] as const
).map((name) => ({ name, label: `mcp.tool.${name}.label`, description: `mcp.tool.${name}.description` }) as const);

export type McpToolName =
  | "search"
  | "get_context"
  | "get_entity"
  | "find_entities"
  | "get_neighbourhood"
  | "get_item"
  | "recent_changes";

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
/** JSON-RPC code for a connection at its in-flight cap: the call did not run; retry shortly. */
export const SERVER_BUSY = -32010;
/** Waits before each retry of a busy call, about a second in all (the shell retries too). */
export const BUSY_BACKOFF_MS = [100, 300, 600] as const;

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
  /** The daemon turned the call away as busy; it did not run. */
  get isBusy(): boolean {
    return this.kind === "rpc" && this.code === SERVER_BUSY;
  }

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

/** Shown wherever a feature needs a newer daemon (a getter, so it follows the current locale). */
export function notAvailable(): string {
  return t("common.notAvailable");
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
    throw new ApiError({ kind: "unavailable", message: t("common.notInApp") });
  }
  return withBusyRetry(() => invoke<T>(command, args));
}

const sleepMs = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/**
 * Run `fn`, retrying with {@link BUSY_BACKOFF_MS} while the daemon answers "busy"
 * ({@link SERVER_BUSY}), so a momentary overload is not shown as an error. Throws an `ApiError`.
 */
export async function withBusyRetry<T>(fn: () => Promise<T>, sleep: (ms: number) => Promise<void> = sleepMs): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await fn();
    } catch (err) {
      const e = ApiError.from(err);
      if (!e.isBusy || attempt >= BUSY_BACKOFF_MS.length) throw e;
      await sleep(BUSY_BACKOFF_MS[attempt]!);
    }
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
  /** Reject, restore, assert or un-assert a fact (see `ClaimDecision`). */
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

export type PageId = "today" | "chat" | "search" | "knowledge" | "sources" | "apps" | "plugins" | "agent" | "safety" | "doubletap" | "models" | "privacy" | "status";

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

// ---- W7: knowledge ----

/**
 * What `knowledge.claim.status` accepts: `rejected` (sticky rejection), `active` (restore; a fact
 * without sources comes back as `user_asserted`), `user_asserted` (the user vouches for it) and
 * `unasserted` (undo the user's assertion: `active` if sources support it, else `retracted`).
 */
export type ClaimDecision = "rejected" | "active" | "user_asserted" | "unasserted";

/** `knowledge.split`: move facts (and named aliases/identifiers) off an entity into a new one. */
export interface SplitRequest {
  entity: string;
  claims: number[];
  name: string;
  /** Defaults to the split entity's kind. */
  kind?: EntityKind | null;
  aliases?: string[];
  identifiers?: [string, string][];
}

/** Split facts off an entity; resolves to the new entity's id. */
export async function knowledgeSplit(request: SplitRequest): Promise<string> {
  const r = await call<EntityRef>("knowledge_split", { request });
  return r.id;
}

// ---- W4 proactive ----
//
// Card inbox, morning brief and reminders (`nugget_core::ipc`, `---- Proactive (M5) ----`).
// Card titles, details, evidence quotes and diff lines are untrusted source text: render them as
// plain text only.

/** `unanswered_thread`, `commitment`, `meeting_prep`, `renewal`, `follow_up` (or newer kinds). */
export type CardKind = string;
/** `new`, `seen`, `snoozed`, `accepted`, `dismissed`, `done`, `expired`. */
export type CardState = string;
export type CardFeedbackKind = "accept" | "dismiss" | "snooze" | "seen" | "done";
export type DismissReason = "not_relevant" | "already_handled" | "wrong_info" | "not_important" | "other";
export const DISMISS_REASONS: readonly DismissReason[] = ["not_relevant", "already_handled", "wrong_info", "not_important", "other"];
/** `executed`, `needs_confirmation` (ask, then retry with `confirmed`), `denied`, `halted`. */
export type ExecuteOutcome = "executed" | "needs_confirmation" | "denied" | "halted";

export interface CardsListRequest {
  /** Also return snoozed, accepted and closed cards (default: the open inbox only). */
  include_closed?: boolean;
}

export interface CardsList {
  cards: CardView[];
  /** RFC 3339. */
  last_refresh?: string | null;
  /** `[proactive] enabled`. */
  enabled: boolean;
}

/** One feature's share of a card's score. */
export interface CardFactor {
  /** `urgency`, `importance`, `freshness`, `kind:<kind>`, ... */
  name: string;
  value: number;
  weight: number;
  /** `value × weight` (log-odds). */
  contribution: number;
}

/** A source item backing a card. `quote` is untrusted: render as plain text. */
export interface CardEvidence {
  item: ItemRef;
  quote?: string | null;
  url?: string | null;
}

export interface CardActionView {
  /** Position in the card's actions (`cards.preview`/`cards.execute` take it). */
  index: number;
  /** `draft_reply`, `create_event`, `reminder`, `archive_thread`, `research_note`. */
  kind: string;
  label: string;
  /** `observe`, `edit`, `send`, `destroy`, `spend`, `security`. */
  risk: string;
  /** Whether an executor is configured for it. */
  supported: boolean;
}

export interface CardView {
  id: string;
  kind: CardKind;
  title: string;
  detail: string;
  summary?: string | null;
  people?: string[];
  /** RFC 3339. */
  due?: string | null;
  score: number;
  rank: number;
  /** Top factors of the score (why it is here). */
  factors: CardFactor[];
  evidence: CardEvidence[];
  actions: CardActionView[];
  state: CardState;
  sensitive: boolean;
  created_at: string;
  snoozed_until?: string | null;
}

export interface CardFeedbackRequest {
  card_id: string;
  feedback: CardFeedbackKind;
  reason?: DismissReason | null;
  reason_text?: string | null;
  /** RFC 3339, for `snooze` (default: tomorrow at the brief time). */
  until?: string | null;
}

export interface CardPreviewRequest {
  card_id: string;
  action: number;
}

export interface PreviewLine {
  /** `add`, `remove`, `context`. */
  op: string;
  text: string;
}

/** A dry run: what the action would change. Nothing has executed. */
export interface CardPreview {
  card_id: string;
  action: number;
  kind: string;
  summary: string;
  risk: string;
  /** Where the change lands, e.g. `mail:drafts`, `calendar`, `vault`. */
  target: string;
  diff: PreviewLine[];
  /** Pass back to `cards.execute`; approvals are bound to it. */
  digest: string;
}

export interface CardExecuteRequest {
  card_id: string;
  action: number;
  digest: string;
  confirmed?: boolean;
}

/** Something the desktop finishes on the user's behalf after an action ran. */
export interface ActionArtifact {
  /** `draft`, `file`, `note`, `event`, `reminder`, `archived`. */
  kind: string;
  path?: string | null;
  text?: string | null;
  url?: string | null;
  /** RFC 3339. */
  at?: string | null;
}

export interface CardExecuteResult {
  outcome: ExecuteOutcome | string;
  /** The executor's result id, or the policy's reason. */
  detail?: string | null;
  artifact?: ActionArtifact | null;
}

export interface BriefCardView {
  card_id: string;
  kind: CardKind;
  title: string;
  summary: string;
  due?: string | null;
  rank: number;
  factors: CardFactor[];
  /** Offered action kinds, best first. */
  actions: string[];
}

export interface BriefMeetingView {
  title: string;
  start: string;
  end?: string | null;
  all_day: boolean;
  location?: string | null;
  attendees: string[];
  /** Attendees outside the user's organisation. */
  external: string[];
  prep_card?: string | null;
  prep: CardEvidence[];
}

export interface BriefView {
  /** The user's local date, `YYYY-MM-DD`. */
  date: string;
  generated_at: string;
  top: BriefCardView[];
  meetings: BriefMeetingView[];
  overdue: BriefCardView[];
  renewals: BriefCardView[];
  open_cards: number;
}

export interface KindMetricsView {
  shown: number;
  accepted: number;
  dismissed: number;
  precision?: number | null;
}

export interface ProactiveMetrics {
  total: number;
  by_state: Record<string, number>;
  shown: number;
  accepted: number;
  dismissed: number;
  snoozed: number;
  precision?: number | null;
  by_kind: Record<string, KindMetricsView>;
  median_time_to_action_secs?: number | null;
  mean_time_to_action_secs?: number | null;
  dismiss_reasons: Record<string, number>;
  model_updates: number;
}

/** `cards.changed`: refetch `cards.list`. */
export interface CardsChanged {
  /** `refresh`, `feedback`, `execute`. */
  reason: string;
  open: number;
  created: number;
  resolved: number;
}

export interface ReminderView {
  id: string;
  text: string;
  at: string;
  about?: ItemRef | null;
  card_id?: string | null;
}

export const PROACTIVE_NOTIFICATION = {
  cardsChanged: "cards.changed",
  briefReady: "brief.ready",
  reminderDue: "reminder.due",
} as const;

export function isBriefView(v: unknown): v is BriefView {
  return (
    isRecord(v) &&
    typeof v.date === "string" &&
    Array.isArray(v.top) &&
    Array.isArray(v.meetings) &&
    Array.isArray(v.overdue) &&
    Array.isArray(v.renewals) &&
    isCount(v.open_cards)
  );
}

export function isReminderView(v: unknown): v is ReminderView {
  return isRecord(v) && typeof v.id === "string" && typeof v.text === "string" && typeof v.at === "string";
}

/**
 * Whether a link from card evidence may be offered: `https:` only (`open_url` also checks).
 * `file:`/`obsidian:` links in untrusted evidence are not offered.
 */
export function isSafeEvidenceUrl(url: string | null | undefined): url is string {
  return isOpenableUrl(url) && url.toLowerCase().startsWith("https:");
}

/** A `mailto:` link `open_mailto` would accept (the Rust side is the real check). */
export function isMailtoUrl(url: string | null | undefined): url is string {
  if (!url || url.length > 32 * 1024) return false;
  if (/[\s\u0000-\u001f\u007f-\u009f]/.test(url)) return false;
  return /^mailto:/i.test(url);
}

export const proactiveApi = {
  cardsList: (request: CardsListRequest = {}) => call<CardsList>("cards_list", { request }),
  cardsFeedback: (request: CardFeedbackRequest) => call<CardView>("cards_feedback", { request }),
  /** Dry run; nothing executes. */
  cardsPreview: (request: CardPreviewRequest) => call<CardPreview>("cards_preview", { request }),
  /** Executes through the policy gate, bound to the approved preview's `digest`. */
  cardsExecute: (request: CardExecuteRequest) => call<CardExecuteResult>("cards_execute", { request }),
  briefGet: () => call<BriefView>("brief_get"),
  metrics: () => call<ProactiveMetrics>("proactive_metrics"),
  refresh: () => call<CardsChanged>("proactive_refresh"),
  remindersList: () => call<ReminderView[]>("reminders_list"),
  /** OS notification, shown by the Rust shell. With `openToday`, clicking it opens Today. */
  notify: (title: string, body: string, openToday = false) => call<void>("notify", { title, body, openToday }),
  /** Open a drafted reply (`mailto:` only) in the default mail app. */
  openMailto: (url: string) => call<void>("open_mailto", { url }),
};

export const proactiveEvents = {
  /** `cards.changed` (malformed payloads still trigger a refetch). */
  onCardsChanged: (h: (c: CardsChanged | null) => void) =>
    on<RawNotification>(EVENT.notification, (n) => {
      if (n?.method === PROACTIVE_NOTIFICATION.cardsChanged) h(isRecord(n.params) ? (n.params as unknown as CardsChanged) : null);
    }),
  /** `brief.ready`; malformed payloads are dropped. */
  onBriefReady: (h: (b: BriefView) => void) =>
    on<RawNotification>(EVENT.notification, (n) => {
      if (n?.method === PROACTIVE_NOTIFICATION.briefReady && isBriefView(n.params)) h(n.params);
    }),
  /** `reminder.due`; malformed payloads are dropped. */
  onReminderDue: (h: (r: ReminderView) => void) =>
    on<RawNotification>(EVENT.notification, (n) => {
      if (n?.method === PROACTIVE_NOTIFICATION.reminderDue && isReminderView(n.params)) h(n.params);
    }),
};

// ---- W11: chat history ----

/** One saved chat message (mirrors `src-tauri/src/history.rs`). */
export interface StoredChatMessage {
  role: "user" | "assistant";
  text: string;
  provider?: string | null;
  /** How an assistant turn ended. */
  state?: "done" | "error" | "cancelled" | null;
  error?: string | null;
  output_tokens?: number | null;
  /** Sources the reply cites as `[n]`, when it came with any. */
  citations?: Citation[] | null;
  /** Answered without sources because none matched (with "Use my sources" on). */
  general_answer?: boolean | null;
}

export interface StoredConversation {
  id: string;
  title: string;
  /** Milliseconds since the epoch; set by the shell on save. */
  updated_at: number;
  messages: StoredChatMessage[];
}

/**
 * Chat history is kept by the desktop shell (not the daemon) in its app data folder, as plain
 * unencrypted JSON. Older shells lack these commands ("method not found").
 */
export const chatHistory = {
  /** Most recent first. */
  load: () => call<StoredConversation[]>("chat_history_load"),
  save: (conversation: StoredConversation) => call<void>("chat_history_save", { conversation }),
  remove: (id: string) => call<void>("chat_history_delete", { id }),
  clear: () => call<void>("chat_history_clear"),
};

/** A shell without the chat history commands answers like an unknown method. */
export function isCommandMissing(err: unknown): boolean {
  if (isMethodNotFound(err)) return true;
  const e = ApiError.from(err);
  return /not found|unknown command|not allowed/i.test(e.message) && e.kind !== "rpc";
}

// ---- W11: desktop shell ----

/** "Open Nugget at login" (the desktop app itself, hidden in the tray); see `autostart.rs`. */
export interface AutostartStatus {
  /** This platform can register a login entry. */
  supported: boolean;
  enabled: boolean;
}

export const autostart = {
  get: () => call<AutostartStatus>("autostart_get"),
  set: (enabled: boolean) => call<AutostartStatus>("autostart_set", { enabled }),
};

// W5: connectors

/** Microsoft 365 connector kinds (OAuth with the user's own Entra app registration). */
export type MicrosoftKind = "outlook" | "outlook-calendar" | "onedrive";
export const MICROSOFT_KINDS: readonly MicrosoftKind[] = ["outlook", "outlook-calendar", "onedrive"];

/** SaaS connector kinds (a pasted token, or OAuth when settings carry `client_id`). */
export type SaasKind = "slack" | "notion" | "github" | "linear";
export const SAAS_KINDS: readonly SaasKind[] = ["slack", "notion", "github", "linear"];

/** Every kind the Add source wizard offers. */
export type ConnectorKind = SourceKind | MicrosoftKind | SaasKind;
export const CONNECTOR_KINDS: readonly ConnectorKind[] = [...SOURCE_KINDS, ...MICROSOFT_KINDS, ...SAAS_KINDS];

export function isConnectorKind(v: unknown): v is ConnectorKind {
  return typeof v === "string" && (CONNECTOR_KINDS as readonly string[]).includes(v);
}

// ---- W1 Double Tap ----
// Tauri commands in apps/desktop/src-tauri/src/doubletap/ (the settings page and the overlay window).

/** Modifier keys a double tap can use (mirrors `nugget_hotkey::Modifier`). */
export type DoubleTapModifier =
  | "left_command"
  | "right_command"
  | "left_control"
  | "right_control"
  | "left_option"
  | "right_option"
  | "left_shift"
  | "right_shift";

export type TriggerSpec =
  | { kind: "double_tap"; modifier: DoubleTapModifier; window_ms: number }
  /** A chord such as `Ctrl+Alt+Space`. */
  | { kind: "chord"; chord: string };

export interface DoubleTapSettings {
  enabled: boolean;
  trigger: TriggerSpec;
  /** App id (bundle id, executable) or page host → provider id. */
  app_providers: Record<string, string>;
  /** Apps Double Tap never drafts in (password managers and banking by default). */
  excluded_apps: string[];
  /** Allow drafting in terminal emulators (off by default: text there can run as a command). */
  allow_terminals: boolean;
  /**
   * Read ahead (CC1): when a reply field keeps focus for a moment, prefill its prompt into the
   * local model server's cache so the trigger only reads the remainder. Local models only; never
   * in excluded apps, terminals or secure fields. Absent from an older shell: treated as on.
   */
  read_ahead?: boolean;
}

export interface ModifierOption {
  id: DoubleTapModifier;
  /** As printed on this platform's keyboards. */
  label: string;
}

export type PermissionState = "granted" | "denied" | "not_required" | "unknown";

export interface DoubleTapStatus {
  platform: "macos" | "windows" | "linux";
  /** `macos`, `windows`, `x11`, `wayland` or `unknown`. */
  session: string;
  /** A listener is running with the current trigger. */
  listening: boolean;
  /** Why the trigger could not start (unsupported, taken…). */
  trigger_error: string | null;
  accessibility: "granted" | "denied" | "not_required";
  /** macOS only; `not_required` elsewhere. */
  input_monitoring: PermissionState;
  /** i18n key of an onboarding hint, e.g. `doubletap.hint.atspi`. */
  hint: string | null;
  modifiers: ModifierOption[];
  recommended_trigger: TriggerSpec;
}

/** `nugget_core::ipc::VoiceStatus`. */
export interface VoiceStatus {
  /** Messages written by the user that the profile was built from. */
  samples: number;
  /** Channels with enough samples for their own style (e.g. `email`, `chat`). */
  channels: string[];
  /** RFC 3339, or `null` when never built. */
  last_built: string | null;
}

export type OverlayPhase = "reading" | "refused" | "streaming" | "done" | "inserting" | "inserted" | "error";
export type InsertMethod = "accessibility" | "clipboard" | "typing";

/** What the caret overlay shows; sent with every `doubletap:overlay` event. */
export interface OverlayView {
  phase: OverlayPhase;
  /** Draft so far. */
  text: string;
  provider: string | null;
  locality: Locality | null;
  citations: number;
  excluded_sensitive: number;
  /** i18n key for refused/error (e.g. `doubletap.refused.secure`), or raw error text. */
  message: string | null;
  /** After insertion: whether reading the field back matched. */
  verified: boolean | null;
  method: InsertMethod | null;
}

export const DOUBLETAP_OVERLAY_EVENT = "doubletap:overlay";
/** The saved settings, sent to every window after a change (from the settings page or onboarding). */
export const DOUBLETAP_SETTINGS_EVENT = "doubletap:settings";

export const doubletapApi = {
  settingsGet: () => call<DoubleTapSettings>("doubletap_settings_get"),
  /** Validates, saves and restarts the listener; returns what was saved. */
  settingsSet: (settings: DoubleTapSettings) => call<DoubleTapSettings>("doubletap_settings_set", { settings }),
  status: () => call<DoubleTapStatus>("doubletap_status"),
  /** macOS: opens the Accessibility / Input Monitoring prompts. Elsewhere just reports status. */
  requestPermissions: () => call<DoubleTapStatus>("doubletap_request_permissions"),
  defaultExcluded: () => call<string[]>("doubletap_default_excluded"),
  /** Start a draft as if the trigger fired. */
  fire: () => call<void>("doubletap_fire"),
  voiceStatus: () => call<VoiceStatus>("voice_status"),

  // Overlay window only.
  overlayState: () => call<OverlayView>("overlay_state"),
  /** `text` is `null` when accepted unchanged, else the edited text. */
  overlayAccept: (text: string | null) => call<void>("overlay_accept", { text }),
  overlayCancel: () => call<void>("overlay_cancel"),
  onOverlay: (h: (view: OverlayView) => void) => on<OverlayView>(DOUBLETAP_OVERLAY_EVENT, h),
  onSettings: (h: (settings: DoubleTapSettings) => void) => on<DoubleTapSettings>(DOUBLETAP_SETTINGS_EVENT, h),
};

// ---- W6 plugins ----
// Wire types mirror the `// ---- Plugins (M6) ----` block of `nugget_core::ipc`.

/** What a plugin may use. The host denies everything not listed. */
export interface PluginCapabilities {
  /** `example.com`, `example.com:8443`, `*.example.com` (any subdomain) or `http://host:port`. */
  hosts: string[];
  /** Names of the plugin's own secrets in the OS keychain. */
  secrets: string[];
  /** Size limit of the plugin's own key-value store; `0` means none. */
  kv_max_bytes: number;
}

/** `trusted`: signer is in `[plugins] trusted_keys`; `dev`: this device's dev key; else `untrusted`. */
export type PluginSignature = "trusted" | "dev" | "untrusted";
export const PLUGIN_SIGNATURES: readonly PluginSignature[] = ["trusted", "dev", "untrusted"];

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  /** `connector` and/or `tool`. */
  kinds: string[];
  enabled: boolean;
  /** Running in the plugin host right now. */
  loaded: boolean;
  /** Why it is not loaded (untrusted signer, load failure, …). */
  error?: string | null;
  signature: PluginSignature;
  /** Hex ed25519 public key. */
  signer: string;
  approved: PluginCapabilities;
  /** RFC 3339. */
  installed_at: string;
  /** `plugin:<id>` when the plugin provides a connector. */
  connector_kind?: string | null;
  /** Agent tool names. */
  tools?: string[];
}

export interface PluginListResult {
  plugins: PluginInfo[];
}

export interface PluginInspectResult {
  plugin: PluginInfo;
  requested: PluginCapabilities;
  connector_display_name?: string | null;
}

/** A verified package staged under `token` (for 10 minutes) until approved or rejected. */
export interface PluginInstallResult {
  token: string;
  id: string;
  name: string;
  version: string;
  description: string;
  kinds: string[];
  signature: PluginSignature;
  signer: string;
  requested: PluginCapabilities;
  /** What the user approved for the installed version, when this is an update. */
  previously_approved?: PluginCapabilities | null;
  installed_version?: string | null;
  needs_approval: boolean;
  /** Already committed (an update within the approved capabilities). */
  installed: boolean;
}

export interface PluginApproveResult {
  plugin: PluginInfo | null;
}

export interface PluginRemoveResult {
  removed: boolean;
}

/** One `hosts` entry, split for plain-language display. */
export interface PluginHost {
  /** Host name (without `*.`), with `:port` when one is given. */
  host: string;
  /** Any subdomain of `host` (not `host` itself). */
  wildcard: boolean;
  /** Plain `http` (unencrypted) was spelled out. */
  insecure: boolean;
}

export function parsePluginHost(entry: string): PluginHost {
  let rest = entry.trim();
  let insecure = false;
  const scheme = /^([a-z][a-z0-9+.-]*):\/\//i.exec(rest);
  if (scheme) {
    insecure = scheme[1]!.toLowerCase() === "http";
    rest = rest.slice(scheme[0].length);
  }
  const wildcard = rest.startsWith("*.");
  if (wildcard) rest = rest.slice(2);
  return { host: rest.toLowerCase().replace(/\.$/, ""), wildcard, insecure };
}

function hostKey(entry: string): string {
  const h = parsePluginHost(entry);
  return `${h.insecure ? "http" : "https"}://${h.wildcard ? "*." : ""}${h.host}`;
}

/** What `requested` adds over `previous` (everything, for a new install). */
export function newPluginCapabilities(
  requested: PluginCapabilities,
  previous: PluginCapabilities | null | undefined,
): PluginCapabilities {
  if (!previous) return requested;
  const oldHosts = new Set(previous.hosts.map(hostKey));
  return {
    hosts: requested.hosts.filter((h) => !oldHosts.has(hostKey(h))),
    secrets: requested.secrets.filter((s) => !previous.secrets.includes(s)),
    kv_max_bytes: requested.kv_max_bytes > previous.kv_max_bytes ? requested.kv_max_bytes : 0,
  };
}

export function pluginCapabilitiesEmpty(c: PluginCapabilities): boolean {
  return c.hosts.length === 0 && c.secrets.length === 0 && c.kv_max_bytes <= 0;
}

export function isPluginInfo(v: unknown): v is PluginInfo {
  if (typeof v !== "object" || v === null) return false;
  const p = v as Record<string, unknown>;
  return (
    typeof p.id === "string" &&
    typeof p.name === "string" &&
    typeof p.version === "string" &&
    Array.isArray(p.kinds) &&
    typeof p.enabled === "boolean" &&
    typeof p.loaded === "boolean" &&
    (PLUGIN_SIGNATURES as readonly unknown[]).includes(p.signature)
  );
}

/** Installed plugins (`plugins.list`). */
export const listPlugins = async (): Promise<PluginInfo[]> => {
  const r = await call<PluginListResult>("plugins_list");
  return Array.isArray(r?.plugins) ? r.plugins.filter(isPluginInfo) : [];
};
export const inspectPlugin = (id: string) => call<PluginInspectResult>("plugins_inspect", { request: { id } });
/** Verify and stage a `.nuggetplugin` file; commit it with `approvePlugin` when `needs_approval`. */
export const installPlugin = (path: string) => call<PluginInstallResult>("plugins_install", { request: { path } });
/** `approve: false` discards the staged package. */
export const approvePlugin = (token: string, approve: boolean) =>
  call<PluginApproveResult>("plugins_approve", { request: { token, approve } });
export const removePlugin = (id: string) => call<PluginRemoveResult>("plugins_remove", { request: { id } });
export const setPluginEnabled = (id: string, enabled: boolean) =>
  call<PluginInfo>("plugins_enable", { request: { id, enabled } });
/** Native file picker for `.nuggetplugin` packages; `null` when cancelled. */
export const pickPluginFile = () => call<string | null>("pick_plugin_file");
// ---- W3: agent and safety ----
//
// Mirrors the `// ---- Agent (M4) ----` block of `nugget_core::ipc`. Values the daemon carries as
// JSON (`nugget_agent::AgentEvent`, `nugget_policy::PolicyConfig`, ...) are typed here from their
// serde form. Each method maps to the Tauri command of the same name with `.` → `_`
// (`agent.run` → `agent_run`), whose argument is `{ request }` like the other commands.

/** `nugget_policy::RiskClass`, lowest to highest. */
export type RiskClass = "observe" | "edit" | "send" | "destroy" | "spend" | "security";
export const RISK_CLASSES: readonly RiskClass[] = ["observe", "edit", "send", "destroy", "spend", "security"];
/** Classes a confirmation can be approved for the rest of a task (`ConfirmChoice::Task`). */
export const TASK_APPROVABLE_RISKS: readonly RiskClass[] = ["send", "destroy"];

/** `nugget_policy::ActionDescriptor`. */
export interface ActionDescriptor {
  target_app: string;
  verb: string;
  target_label?: string | null;
  text_preview?: string | null;
  declared_risk: RiskClass;
  task_id: string;
}

/** `nugget_agent::ConfirmationRequest`. */
export interface ConfirmationRequest {
  request_id: string;
  action_id: string;
  tool: string;
  action: ActionDescriptor;
  reason: string;
  forced_by_injection: boolean;
  timeout_ms: number;
}

/** `nugget_agent::Verification` (adjacently tagged: `{ status, detail }`). */
export type Verification = { status: "passed"; detail: string } | { status: "failed"; detail: string } | { status: "unverifiable"; detail?: undefined };

/** `nugget_agent::Budget`. */
export type AgentBudget =
  | { budget: "steps"; limit: number }
  | { budget: "deadline"; limit_ms: number }
  | { budget: "tokens"; limit: number }
  | { budget: "tool_rate"; tool: string; max_calls: number; window_ms: number };

/** `nugget_agent::Outcome` (adjacently tagged: `{ outcome, detail }`). */
export type AgentOutcome =
  | { outcome: "succeeded" | "partial" | "halted" | "cancelled"; detail?: undefined }
  | { outcome: "unverified" | "failed" | "needs_input" | "refused" | "error"; detail: string }
  | { outcome: "budget_exhausted"; detail: AgentBudget };

export interface InjectionFinding {
  patterns: string[];
  excerpt: string;
}

export type ConfirmAnswer = "approved" | "rejected" | "timed_out";

/** `nugget_agent::AgentEvent`, tagged by `event`. */
export type AgentEvent =
  | { event: "run_started"; task_id: string; goal: string }
  | { event: "step_started"; task_id: string; step: number }
  | { event: "model_replied"; task_id: string; step: number; text: string; tool_calls: string[] }
  | { event: "action_proposed"; task_id: string; step: number; action_id: string; tool: string; action: ActionDescriptor }
  | { event: "confirmation_needed"; task_id: string; request: ConfirmationRequest }
  | { event: "confirmation_resolved"; task_id: string; request_id: string; answer: ConfirmAnswer }
  | { event: "action_denied"; task_id: string; action_id: string; tool: string; reason: string }
  | { event: "action_done"; task_id: string; action_id: string; tool: string; is_error: boolean; summary: string; replayed: boolean }
  | { event: "verification"; task_id: string; action_id: string; tool: string; verification: Verification }
  | { event: "injection_suspected"; task_id: string; action_id: string; tool: string; finding: InjectionFinding }
  | { event: "suspended"; task_id: string; question: string; options?: string[] }
  | { event: "finished"; task_id: string; outcome: AgentOutcome; summary: string | null };

/** One `agent.event` notification. */
export interface AgentEventNotice {
  task_id: string;
  event: AgentEvent;
}

export interface AgentBudgets {
  max_steps?: number | null;
  deadline_secs?: number | null;
  max_tokens?: number | null;
}

export interface AgentRunRequest {
  goal: string;
  context?: string | null;
  /** `null`/absent: every registered tool. */
  tools?: string[] | null;
  budgets?: AgentBudgets;
  provider?: string | null;
}

export interface AgentStarted {
  task_id: string;
  tools: string[];
}

export type ConfirmChoice = "once" | "task" | "deny";

export type AgentTaskState = "running" | "confirming" | "needs_input" | "finished" | "interrupted";

export interface AgentVerificationCounts {
  passed: number;
  failed: number;
  unverifiable: number;
}

export interface AgentTaskSummary {
  task_id: string;
  goal: string;
  state: AgentTaskState;
  /** `Outcome::label()` once finished. */
  outcome?: string | null;
  detail?: string | null;
  /** Unix milliseconds. */
  started_ms: number;
  finished_ms?: number | null;
  steps: number;
  verification: AgentVerificationCounts;
}

export interface AgentTaskDetail {
  summary: AgentTaskSummary;
  /** `nugget_agent::LogEntry`s (tagged by `kind`). */
  entries: Record<string, unknown>[];
  explain: string;
  pending: ConfirmationRequest[];
  question?: string | null;
  /** Candidate answers offered with `question` (untrusted screen text: render as plain text). */
  options?: string[];
}

export type PolicyEffect = "allow" | "confirm" | "deny";
export const POLICY_EFFECTS: readonly PolicyEffect[] = ["allow", "confirm", "deny"];

/** `nugget_policy::Rule`. Empty lists are omitted on the wire. */
export interface PolicyRule {
  id: string;
  description?: string | null;
  targets?: string[];
  verbs?: string[];
  risks?: RiskClass[];
  labels?: string[];
  effect: PolicyEffect;
}

/** `nugget_policy::PolicyConfig`. Unknown fields are kept when it is edited and sent back. */
export interface PolicyConfig {
  version?: number;
  rules: PolicyRule[];
  disabled_defaults: string[];
  /** Per-task maximum per risk class; a missing class is unlimited. */
  rate_limits: Partial<Record<RiskClass, number>>;
  confirm_timeout_secs: number;
  [extra: string]: unknown;
}

export interface DefaultRuleView {
  rule: PolicyRule;
  enabled: boolean;
}

export interface ActionLogStatus {
  entries: number;
  /** `[seq, hash]` of the last entry. */
  head?: [number, string] | null;
  chain_ok: boolean;
  error?: string | null;
}

export interface PolicyState {
  config: PolicyConfig;
  defaults: DefaultRuleView[];
  halted: boolean;
  log: ActionLogStatus;
}

export interface TaskApprovalView {
  task_id: string;
  risk: string;
  target?: string | null;
}

export interface PolicyApprovals {
  pending: ConfirmationRequest[];
  task: TaskApprovalView[];
}

export interface KillState {
  halted: boolean;
}

export interface PolicyLogExport {
  jsonl: string;
  status: ActionLogStatus;
}

export interface BrowserStatus {
  connected: boolean;
  origin?: string | null;
  host_version?: string | null;
  tools: string[];
}

/** Daemon notification methods (`nugget_core::ipc::agent_notification`). */
export const AGENT_NOTIFICATION = {
  agentEvent: "agent.event",
  killChanged: "policy.kill.changed",
} as const;

export function isAgentEventNotice(v: unknown): v is AgentEventNotice {
  return isRecord(v) && typeof v.task_id === "string" && isRecord(v.event) && typeof v.event.event === "string";
}

export function isKillState(v: unknown): v is KillState {
  return isRecord(v) && typeof v.halted === "boolean";
}

export const agentApi = {
  /** Events for the returned task arrive as `agent.event` notifications (see `agentEvents`). */
  agentRun: (request: AgentRunRequest) => call<AgentStarted>("agent_run", { request }),
  agentCancel: (taskId: string) => call<void>("agent_cancel", { request: { task_id: taskId } }),
  agentConfirm: (taskId: string, requestId: string, choice: ConfirmChoice) =>
    call<void>("agent_confirm", { request: { task_id: taskId, request_id: requestId, choice } }),
  agentAnswer: (taskId: string, answer: string) => call<void>("agent_answer", { request: { task_id: taskId, answer } }),
  agentList: (limit?: number) => call<AgentTaskSummary[]>("agent_list", { request: { limit: limit ?? null } }),
  agentGet: (taskId: string) => call<AgentTaskDetail>("agent_get", { request: { task_id: taskId } }),

  policyGet: () => call<PolicyState>("policy_get"),
  policySet: (config: PolicyConfig) => call<PolicyState>("policy_set", { request: { config } }),
  policyApprovals: () => call<PolicyApprovals>("policy_approvals"),
  policyKill: () => call<KillState>("policy_kill"),
  policyResume: () => call<KillState>("policy_resume"),
  policyLogExport: () => call<PolicyLogExport>("policy_log_export"),
  browserStatus: () => call<BrowserStatus>("browser_status"),
};

export const agentEvents = {
  /** `agent.event` notifications; malformed payloads are dropped. */
  onAgentEvent: (h: (n: AgentEventNotice) => void) =>
    events.onNotification((n) => {
      if (n.method === AGENT_NOTIFICATION.agentEvent && isAgentEventNotice(n.params)) h(n.params);
    }),
  /** The kill switch was engaged or released. */
  onKillChanged: (h: (k: KillState) => void) =>
    events.onNotification((n) => {
      if (n.method === AGENT_NOTIFICATION.killChanged && isKillState(n.params)) h(n.params);
    }),
};

// ---- X4: conversational ask ----

/** Context budget (tokens) for a Chat reply that uses the user's sources, as Search's default. */
export const CHAT_ASK_BUDGET = 2000;
/** Earlier turns such a reply sends along (the daemon bounds them again). */
export const CHAT_ASK_HISTORY = 24;

/**
 * `retrieve.ask` for the newest message of a conversation: the daemon rewrites a follow-up into
 * a standalone query (with a local model only) and gives the model the earlier turns.
 */
export function askInConversation(
  query: string,
  history: readonly ChatMessage[],
  route: { provider: string | null; tier: Tier },
): Promise<AskStarted> {
  return api.retrieveAsk({
    query,
    budget_tokens: CHAT_ASK_BUDGET,
    provider: route.provider,
    tier: route.tier,
    history: history.filter((m) => m.role !== "system").slice(-CHAT_ASK_HISTORY),
  });
}

// ---- Z3: bring your own key ----

/** The `kind` of a `[[providers]]` table a key is for. */
export type AiKeyKind = "openai_compatible" | "openai_responses" | "anthropic";

/** Why a key check (or save) did not succeed; `ok` when it did. */
export type AiKeyReason = "ok" | "bad_key" | "wrong_url" | "no_network" | "server_error" | "exists";

/** `ai.key.test` params. The key is sent to the daemon only; it is never stored by this call. */
export interface AiKeyTestRequest {
  kind: AiKeyKind;
  /** Required for `openai_compatible`; defaults to the provider's own API otherwise. */
  base_url?: string | null;
  api_key: string;
  model?: string | null;
}

export interface AiKeyTestResult {
  ok: boolean;
  reason: AiKeyReason;
  /** Plain-English explanation from the daemon (never contains the key). */
  message: string;
  /** Model ids the provider listed. */
  models: string[];
}

/** `ai.key.save` params: the key goes to the OS keychain, a `keychain:NAME` reference to config.toml. */
export interface AiKeySaveRequest {
  /** Provider id: 1–32 of `a-z 0-9 _ -`. */
  name: string;
  kind: AiKeyKind;
  base_url?: string | null;
  api_key: string;
  model: string;
}

export interface AiKeySaveResult extends AiKeyTestResult {
  saved: boolean;
  provider_id: string | null;
  /** The daemon reads providers at start: always true. */
  restart_required: boolean;
}

export const aiKeyApi = {
  /** Check a key against the provider's model list. Validation outcomes are results, not errors. */
  aiKeyTest: (request: AiKeyTestRequest) => call<AiKeyTestResult>("ai_key_test", { request }),
  /** Check, then save the key (keychain) and add the provider to config.toml. */
  aiKeySave: (request: AiKeySaveRequest) => call<AiKeySaveResult>("ai_key_save", { request }),
};

// ---- Z3: model fit ----

/** Coarse GPU class the daemon detected (`unknown`: could not tell, or none). */
export type GpuKind = "apple_silicon" | "nvidia" | "amd" | "intel" | "unknown";

/** Why a model is (or is not) recommended for this machine. */
export type FitReason =
  | "ram_fits_largest"
  | "ram_fits"
  | "ram_too_small"
  | "rerank_opt_in"
  | "needs_newer_app"
  | "gpu_accelerated"
  | "cpu_only_slower";

/** How one manifest model fits this machine (`models.fit`). */
export interface ModelFit {
  id: string;
  /** Same answer as `models.list`'s `recommended`. */
  recommended: boolean;
  tiers: Tier[];
  size_bytes: number;
  /** Nominal RAM the model needs, in GB. */
  min_ram_gb: number;
  /** Free disk space a fresh install needs (size + 10% + 1 GiB). */
  disk_needed_bytes: number;
  fits_ram: boolean;
  /** Whether the models folder has room; `null` when free space could not be read. */
  fits_disk: boolean | null;
  /** This machine's memory as a nominal figure in GB. */
  machine_ram_gb: number;
  gpu: GpuKind;
  /** Expected compute backend: `metal`, `cuda`, `vulkan` or `cpu`. */
  accel: string;
  reasons: FitReason[];
}

export interface FitReport {
  hardware: { total_ram: number; available_ram: number; cpu_cores: number; gpu: GpuKind };
  /** Nominal memory in GB (16 for a "16 GB" machine). */
  ram_gb: number;
  free_disk_bytes: number | null;
  models: ModelFit[];
}

export const modelFitApi = {
  /** Why each model is recommended and what it needs. Older daemons answer "method not found". */
  modelsFit: () => call<FitReport>("models_fit"),
};

// ---- BB2 proactive learning ----
//
// What the user's own Accept/Dismiss feedback taught the card ranking, and forgetting it
// (`proactive.learning`, `proactive.reset_learning` in `nugget_core::ipc`). Learning only changes
// ranking and suppression, never what an action does. Labels are untrusted source text.

/** `sender`, `domain`, `kind`, `thread` (or a newer scope). */
export type LearningScope = "sender" | "domain" | "kind" | "thread";
/** `less` (ranked lower), `more` (ranked higher), `hidden` (suppressed). */
export type LearnedDirection = "less" | "more" | "hidden";

/** One way feedback changed a card's ranking (`CardView.learned`, strongest first). */
export interface CardLearned {
  scope: LearningScope | string;
  /** The sender address, domain or card kind (`thread`: an opaque key). */
  key: string;
  direction: LearnedDirection | string;
  /** Accepts and dismissals behind it (decayed, rounded). */
  accepted: number;
  dismissed: number;
  /** The most common dismiss reason behind it (`DismissReason` wire name), when dismissals dominate. */
  reason?: DismissReason | string | null;
  /** Change in log-odds of the score. */
  contribution: number;
}

/** One learned adjustment in `proactive.learning`. */
export interface LearnedView {
  scope: LearningScope | string;
  key: string;
  /** A human label (a thread's card title) when the key is opaque. Untrusted: plain text only. */
  label?: string | null;
  /** Decayed counts. */
  accepted: number;
  dismissed: number;
  /** Log-odds; negative ranks lower. */
  adjustment: number;
  /** Suppresses matching cards (a thread marked already handled). */
  suppresses?: boolean;
  /** RFC 3339. */
  last_at: string;
}

export interface ProactiveLearning {
  /** Strongest adjustment first. */
  entries: LearnedView[];
}

export interface ResetLearningRequest {
  scope: "all" | LearningScope;
  /** Required unless `scope` is `all`. */
  key?: string | null;
}

export interface ResetLearningResult {
  /** Learned entries forgotten. */
  cleared: number;
}

// Declaration merge: `CardView` (W4 block) gains the learned effects.
export interface CardView {
  /** How the user's feedback changed this card's ranking, strongest first (absent from older daemons). */
  learned?: CardLearned[] | null;
}

/** A card's learned effects; older daemons send none. */
export function cardLearned(card: CardView): CardLearned[] {
  return Array.isArray(card.learned) ? card.learned : [];
}

export const learningApi = {
  /** Older daemons answer "method not found". */
  learning: () => call<ProactiveLearning>("proactive_learning"),
  /** Audited; the daemon then emits `cards.changed` with reason `reset_learning`. */
  resetLearning: (request: ResetLearningRequest) => call<ResetLearningResult>("proactive_reset_learning", { request }),
};
