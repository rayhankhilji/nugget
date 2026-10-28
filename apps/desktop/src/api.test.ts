import { describe, expect, it } from "vitest";
import {
  ApiError,
  changeAffects,
  isClaimStatus,
  isClaimView,
  isEntityKind,
  isEntitySummaryView,
  isEvidenceView,
  isGraphView,
  isKnowledgeChanged,
  isKnowledgeStatus,
  isMcpClientCreated,
  isMcpClientInfo,
  isMcpScopes,
  isIngestProgress,
  isMethodNotFound,
  isOpenableUrl,
  isSourceInfo,
  isSourceKind,
  isSourceState,
  mergeSources,
  METHOD_NOT_FOUND,
  sourcesFromChange,
  type SourceInfo,
} from "./api";

const source = (over: Partial<SourceInfo> = {}): SourceInfo => ({
  id: "files-1",
  kind: "files",
  name: "Documents",
  enabled: true,
  state: "watching",
  items: 12,
  chunks: 40,
  last_sync: "2026-10-28T09:00:00Z",
  last_error: null,
  settings: { roots: ["/home/me/Documents"] },
  ...over,
});

describe("isSourceState / isSourceKind", () => {
  it("accepts exactly the protocol's snake_case values", () => {
    for (const s of ["idle", "syncing", "watching", "needs_auth", "disabled", "error"]) expect(isSourceState(s)).toBe(true);
    for (const s of ["NeedsAuth", "needsAuth", "", "ready", 1, null, undefined]) expect(isSourceState(s)).toBe(false);
    expect(isSourceKind("gcal")).toBe(true);
    expect(isSourceKind("dropbox")).toBe(false);
  });
});

describe("isSourceInfo", () => {
  it("accepts a full SourceInfo and one with optional fields omitted", () => {
    expect(isSourceInfo(source())).toBe(true);
    const { last_sync: _a, last_error: _b, settings: _c, ...minimal } = source();
    expect(isSourceInfo(minimal)).toBe(true);
  });

  it("rejects wrong shapes", () => {
    expect(isSourceInfo(null)).toBe(false);
    expect(isSourceInfo([source()])).toBe(false);
    expect(isSourceInfo({ ...source(), state: "Watching" })).toBe(false);
    expect(isSourceInfo({ ...source(), items: -1 })).toBe(false);
    expect(isSourceInfo({ ...source(), items: "12" })).toBe(false);
    expect(isSourceInfo({ ...source(), enabled: "yes" })).toBe(false);
    expect(isSourceInfo({ ...source(), last_sync: 1700000000 })).toBe(false);
    expect(isSourceInfo({ ...source(), settings: ["x"] })).toBe(false);
    const { id: _id, ...noId } = source();
    expect(isSourceInfo(noId)).toBe(false);
  });
});

describe("isIngestProgress", () => {
  it("validates notification payloads", () => {
    expect(isIngestProgress({ source: "files-1", phase: "indexing", done: 3, total: 10 })).toBe(true);
    expect(isIngestProgress({ source: "files-1", phase: "scanning", done: 0 })).toBe(true);
    expect(isIngestProgress({ source: "files-1", phase: "error", done: 0, total: null, error: "boom" })).toBe(true);
    expect(isIngestProgress({ source: "files-1", phase: "paused", done: 0 })).toBe(false);
    expect(isIngestProgress({ source: "files-1", phase: "indexing", done: Number.NaN })).toBe(false);
    expect(isIngestProgress({ phase: "indexing", done: 1 })).toBe(false);
    expect(isIngestProgress("indexing")).toBe(false);
  });
});

describe("sourcesFromChange", () => {
  it("extracts sources from the payload shapes the daemon might send", () => {
    expect(sourcesFromChange(source())).toEqual([source()]);
    expect(sourcesFromChange([source(), source({ id: "gmail-1", kind: "gmail" })])).toHaveLength(2);
    expect(sourcesFromChange({ sources: [source()] })).toEqual([source()]);
  });

  it("returns null (refetch) for anything else", () => {
    for (const p of [null, undefined, {}, [], { id: "files-1" }, [source(), { id: "x" }], "files-1"]) {
      expect(sourcesFromChange(p)).toBeNull();
    }
  });
});

describe("mergeSources", () => {
  it("replaces by id in place and appends new ones", () => {
    const a = source({ id: "a" });
    const b = source({ id: "b" });
    const b2 = source({ id: "b", state: "syncing" });
    const c = source({ id: "c" });
    expect(mergeSources([a, b], [b2, c]).map((s) => [s.id, s.state])).toEqual([
      ["a", "watching"],
      ["b", "syncing"],
      ["c", "watching"],
    ]);
  });
});

describe("method not found", () => {
  it("is recognised from the serialized command error", () => {
    const wire = { kind: "rpc", message: "method not found: sources.list", code: METHOD_NOT_FOUND };
    expect(isMethodNotFound(wire)).toBe(true);
    expect(ApiError.from(wire).isMethodNotFound).toBe(true);
    expect(isMethodNotFound({ kind: "rpc", message: "nope", code: -32001 })).toBe(false);
    expect(isMethodNotFound({ kind: "not_running", message: "down", code: null })).toBe(false);
    expect(isMethodNotFound(null)).toBe(false);
    expect(isMethodNotFound(new Error("x"))).toBe(false);
  });
});

describe("isOpenableUrl", () => {
  it("allows https, obsidian and local file URLs", () => {
    for (const u of [
      "https://mail.google.com/mail/u/0/#inbox/abc",
      "HTTPS://example.com/",
      "obsidian://open?vault=Notes&file=Daily%2F2026-10-28",
      "file:///home/me/Notes/a%20b.md",
      "file://localhost/tmp/x.txt",
      "file:///C:/Users/me/a.md",
    ]) {
      expect(isOpenableUrl(u), u).toBe(true);
    }
  });

  it("refuses script, data and other schemes", () => {
    for (const u of [
      "javascript:alert(1)",
      "JavaScript:alert(1)",
      " javascript:alert(1)",
      "java\tscript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vbscript:msgbox(1)",
      "http://example.com/",
      "mailto:me@example.com",
      "smb://server/share",
      "ms-msdt:/id",
      "blob:https://example.com/x",
      "about:blank",
    ]) {
      expect(isOpenableUrl(u), u).toBe(false);
    }
  });

  it("refuses network shares, malformed and oversized input", () => {
    for (const u of [
      "file://server/share/doc.txt",
      "file:////server/share/doc.txt",
      "https://",
      "not a url",
      "/etc/passwd",
      "https://example.com/a b",
      "https://example.com/\n",
      "",
      null,
      undefined,
      `https://example.com/${"a".repeat(9000)}`,
    ]) {
      expect(isOpenableUrl(u), String(u).slice(0, 40)).toBe(false);
    }
  });
});

// ---- knowledge & MCP (M2) ----

const claim = (over: Record<string, unknown> = {}) => ({
  id: 7,
  subject: { id: "ent_sam", name: "Sam" },
  predicate: "works_at",
  object: { entity: { id: "ent_acme", name: "Acme" }, literal: null },
  text: "Sam works at Acme.",
  confidence: 0.92,
  status: "active",
  evidence: [{ item: { source: "gmail-1", key: "m1" }, chunk_id: 3, quote: "joined Acme", title: "Offer", url: null }],
  ...over,
});

const mcpClient = (over: Record<string, unknown> = {}) => ({
  id: "mcp-1",
  name: "Claude Desktop",
  scopes: { sources: null, tools: [], include_sensitive: false },
  created_at: "2026-10-28T09:00:00Z",
  last_used: null,
  ...over,
});

describe("knowledge guards", () => {
  it("accept exactly the protocol's kinds and statuses", () => {
    for (const k of ["person", "organization", "project", "commitment", "preference", "event", "place", "topic"]) {
      expect(isEntityKind(k)).toBe(true);
    }
    for (const k of ["Person", "org", "", null, 3]) expect(isEntityKind(k)).toBe(false);
    for (const s of ["active", "superseded", "retracted", "user_asserted"]) expect(isClaimStatus(s)).toBe(true);
    for (const s of ["userAsserted", "rejected", undefined]) expect(isClaimStatus(s)).toBe(false);
  });

  it("validate entity summaries", () => {
    const e = { id: "ent_1", kind: "person", name: "Sam", claims: 3, mentions: 10, updated_at: "2026-10-28T09:00:00Z" };
    expect(isEntitySummaryView(e)).toBe(true);
    expect(isEntitySummaryView({ ...e, kind: "robot" })).toBe(false);
    expect(isEntitySummaryView({ ...e, claims: -1 })).toBe(false);
    expect(isEntitySummaryView({ ...e, updated_at: 0 })).toBe(false);
  });

  it("validate claims, with exactly one object form", () => {
    expect(isClaimView(claim())).toBe(true);
    expect(isClaimView(claim({ object: { literal: "London" } }))).toBe(true);
    expect(isClaimView(claim({ object: {} }))).toBe(false);
    expect(isClaimView(claim({ object: { literal: "x", entity: { id: "a", name: "A" } } }))).toBe(false);
    expect(isClaimView(claim({ status: "rejected" }))).toBe(false);
    expect(isClaimView(claim({ confidence: Number.NaN }))).toBe(false);
    expect(isClaimView(claim({ evidence: [{ item: { source: "x" } }] }))).toBe(false);
    expect(isEvidenceView({ item: { source: "a", key: "b" } })).toBe(true);
    expect(isEvidenceView({ item: { source: "a", key: "b" }, url: 5 })).toBe(false);
  });

  it("validate graphs and change notifications", () => {
    const g = {
      nodes: [{ id: "a", kind: "person", name: "A", depth: 0 }],
      edges: [{ claim: 1, from: "a", to: "b", predicate: "knows" }],
      truncated: false,
    };
    expect(isGraphView(g)).toBe(true);
    expect(isGraphView({ ...g, truncated: "no" })).toBe(false);
    expect(isGraphView({ ...g, edges: [{ from: "a", to: "b" }] })).toBe(false);
    expect(isKnowledgeChanged({ entities: ["a", "b"] })).toBe(true);
    expect(isKnowledgeChanged({ entities: [1] })).toBe(false);
    expect(isKnowledgeChanged(null)).toBe(false);
    expect(changeAffects({ entities: [] }, "a")).toBe(true);
    expect(changeAffects({ entities: ["a"] }, "a")).toBe(true);
    expect(changeAffects({ entities: ["b"] }, "a")).toBe(false);
    expect(changeAffects({ entities: ["b"] }, null)).toBe(false);
  });

  it("validate knowledge status", () => {
    const s = {
      entities: 10,
      claims: 40,
      mentions: 100,
      pending_items: 2,
      last_run: null,
      extractor_provider: "local-chat",
      vault: { path: "/home/me/vault", pages: 10, last_render: null, conflicts: ["people/sam.md"], invalid: [] },
    };
    expect(isKnowledgeStatus(s)).toBe(true);
    expect(isKnowledgeStatus({ ...s, vault: { ...s.vault, conflicts: [1] } })).toBe(false);
    expect(isKnowledgeStatus({ ...s, vault: null })).toBe(false);
  });
});

describe("MCP guards", () => {
  it("validate scopes and clients", () => {
    expect(isMcpScopes({})).toBe(true);
    expect(isMcpScopes({ sources: ["files-1"], tools: ["search"], include_sensitive: true })).toBe(true);
    expect(isMcpScopes({ sources: "all" })).toBe(false);
    expect(isMcpScopes({ include_sensitive: "no" })).toBe(false);
    expect(isMcpClientInfo(mcpClient())).toBe(true);
    expect(isMcpClientInfo(mcpClient({ last_used: "2026-10-28T10:00:00Z" }))).toBe(true);
    expect(isMcpClientInfo(mcpClient({ created_at: undefined }))).toBe(false);
  });

  it("validate the one-time create result", () => {
    const created = {
      client: mcpClient(),
      token: "nmcp_secret",
      setup: [{ app: "claude-desktop", config_path: null, text: "{}" }],
    };
    expect(isMcpClientCreated(created)).toBe(true);
    expect(isMcpClientCreated({ ...created, token: "" })).toBe(false);
    expect(isMcpClientCreated({ ...created, setup: [{ app: "cursor" }] })).toBe(false);
    expect(isMcpClientCreated({ ...created, client: {} })).toBe(false);
  });
});
