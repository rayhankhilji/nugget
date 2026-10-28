/** Daemon responses for jsdom tests, keyed by Tauri command name. */
import type {
  EgressEvent,
  EntitySummaryView,
  GraphView,
  IngestStatus,
  KnowledgeStatus,
  McpClientInfo,
  ModelInfo,
  PluginInfo,
  ProviderInfo,
  RuntimeInfo,
  SearchHit,
  SourceInfo,
  StatusResponse,
} from "../api";

const now = "2026-10-28T12:00:00Z";

export const providers: ProviderInfo[] = [
  { id: "local-chat", kind: "openai_compatible", model: "qwen3-4b", locality: "on_device", tiers: ["light", "triage"], capabilities: null },
  {
    id: "anthropic",
    kind: "anthropic",
    model: "claude-opus-5-5",
    locality: "cloud",
    tiers: ["synthesis"],
    capabilities: { streaming: true, vision: true, tools: true, json_schema: true, embeddings: false, context_tokens: 1_000_000 },
  },
];

export const models: ModelInfo[] = [
  { id: "qwen3-4b-q4", size_bytes: 2_500_000_000, installed: false, recommended: true, family: "qwen3", license: "Apache-2.0" },
  { id: "nomic-embed", size_bytes: 140_000_000, installed: true, recommended: true, family: "nomic", license: "Apache-2.0" },
  { id: "llama-8b-q4", size_bytes: 4_900_000_000, installed: false, recommended: false, family: "llama" },
];

export const sources: SourceInfo[] = [
  { id: "files-1", kind: "files", name: "Documents", enabled: true, state: "watching", items: 1234, chunks: 5678, last_sync: now, settings: { roots: ["/home/me/Documents"] } },
  { id: "gmail-1", kind: "gmail", name: "Work mail", enabled: true, state: "needs_auth", items: 0, chunks: 0, last_sync: null },
];

export const status: StatusResponse = { protocol: 1, version: "0.0.1", uptime_secs: 3725, items: 1234, chunks: 5678, providers, audit_chain_ok: true };

export const ingest: IngestStatus = { queue_depth: 3, freshness_p50_ms: 850, freshness_p95_ms: 4200, embedding_provider: "local-embed", triage_provider: "local-chat", sources };

export const runtime: RuntimeInfo[] = [{ model_id: "qwen3-4b-q4", role: "chat", state: "ready", provider: "local-chat" }];

export const egress: EgressEvent[] = [
  { at: now, host: "api.anthropic.com", feature: "chat", bytes_sent: 2048, bytes_received: 8192, items: [{ source: "files-1", key: "a.md" }] },
];

export const entities: EntitySummaryView[] = [
  { id: "e1", kind: "person", name: "Ada Lovelace", claims: 4, mentions: 9, updated_at: now },
  { id: "e2", kind: "project", name: "Analytical Engine", claims: 1, mentions: 2, updated_at: now },
];

export const knowledgeStatus: KnowledgeStatus = {
  entities: 2,
  claims: 5,
  mentions: 11,
  pending_items: 0,
  last_run: now,
  extractor_provider: null,
  vault: { path: "/home/me/Nugget vault", pages: 2, last_render: now, conflicts: [], invalid: [] },
};

export const graph: GraphView = {
  nodes: [
    { id: "e1", kind: "person", name: "Ada Lovelace", depth: 0 },
    { id: "e2", kind: "project", name: "Analytical Engine", depth: 1 },
  ],
  edges: [{ claim: 1, from: "e1", to: "e2", predicate: "works_on" }],
  truncated: false,
};

export const mcpClients: McpClientInfo[] = [
  { id: "c1", name: "Claude Desktop", scopes: { sources: null, tools: [], include_sensitive: false }, created_at: now, last_used: null },
];

export const hits: SearchHit[] = [{ item: { source: "files-1", key: "a.md" }, title: "Notes", snippet: "Ada wrote the first program", score: 0.92 }];

export const plugins: PluginInfo[] = [
  {
    id: "rss",
    name: "RSS feeds",
    version: "1.2.0",
    description: "Reads RSS and Atom feeds.",
    kinds: ["connector"],
    enabled: true,
    loaded: true,
    error: null,
    signature: "trusted",
    signer: "ab".repeat(32),
    approved: { hosts: ["feeds.example.com", "*.example.org"], secrets: [], kv_max_bytes: 65_536 },
    installed_at: now,
    connector_kind: "plugin:rss",
    tools: [],
  },
];

export const RESPONSES: Record<string, unknown> = {
  connection_info: { connected: true, socket: "/tmp/nuggetd.sock", error: null },
  status,
  providers_list: providers,
  models_list: models,
  egress_recent: egress,
  sources_list: sources,
  ingest_status: ingest,
  runtime_status: runtime,
  knowledge_entities: entities,
  knowledge_status: knowledgeStatus,
  knowledge_graph: graph,
  mcp_clients_list: mcpClients,
  plugins_list: { plugins },
  search: hits,
};
